use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rspotify::model::Token;
use rspotify::{
    AuthCodePkceSpotify, Config as RSpotifyConfig, Credentials, OAuth, prelude::*, scopes,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::config::{self, Config};

/// Token cache file name (under the config directory).
const TOKEN_CACHE_FILE: &str = "token.json";
/// Maximum time to wait for authorization to complete. Login is aborted past this.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
/// Upper bound for reading the request line (a safeguard against oversized headers / malformed input).
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// Build a PKCE-authenticated rspotify client (the token is cached on disk).
pub fn build_client(cfg: &Config) -> Result<AuthCodePkceSpotify> {
    let creds = Credentials::new_pkce(&cfg.client_id);
    let oauth = OAuth {
        redirect_uri: cfg.redirect_uri.clone(),
        scopes: scopes!(
            "user-read-playback-state",
            "user-modify-playback-state",
            "playlist-read-private",
            "playlist-read-collaborative",
            "user-library-read",
            // Needed to save/remove the current track (the TUI `s` action). After adding it,
            // log in again to grant the new scope.
            "user-library-modify"
        ),
        ..Default::default()
    };

    let cache_path = token_cache_path()?;
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create the config directory: {}",
                parent.display()
            )
        })?;
        // This directory holds a secret token, so restrict it to the owner only.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            {
                eprintln!("warning: failed to set permissions on the config directory: {e}");
            }
        }
    }

    let rconf = RSpotifyConfig {
        token_cached: true,
        // Disable automatic refresh and control it ourselves in authed_client. rspotify's
        // auto-refresh has a bug: when Spotify returns a response that omits the refresh_token,
        // it overwrites the stored refresh_token with null (via auto_reauth → write_token_cache
        // before every request), making all later refreshes impossible.
        // Note: as a result, refresh happens at only one place — the entry of authed_client.
        // One-shot commands complete in a few requests, so this is fine; but if a command that
        // makes many requests over a long time (e.g. paging) is added later, provide a separate
        // 401 → re-fetch retry to handle mid-flight expiry.
        token_refreshing: false,
        cache_path,
        ..Default::default()
    };

    Ok(AuthCodePkceSpotify::with_config(creds, oauth, rconf))
}

/// Return an authenticated client with the cached token loaded.
/// The shared entry point for the API commands (status/search/devices/…).
/// If the token is expired, refresh it ourselves and update the cache at 0600 while keeping the
/// refresh_token (rspotify's auto-refresh is disabled because it has a bug that loses the refresh_token).
pub async fn authed_client(cfg: &Config) -> Result<AuthCodePkceSpotify> {
    let spotify = build_client(cfg)?;

    let token = spotify
        .read_token_cache(true)
        .await
        .context("failed to read the token cache")?
        .context("not logged in. Run `spoterm login` first")?;

    let token = if token.is_expired() {
        refresh_expired_token(&spotify, token).await?
    } else {
        token
    };

    // Set the loaded (or refreshed) token on the client.
    set_client_token(&spotify, token).await;

    Ok(spotify)
}

/// For a long-lived client (the TUI), refresh the token only when needed.
/// Unlike [`authed_client`], this does not re-read the disk every time and can reuse reqwest's
/// connection pool. It does nothing while the token is valid, so it is cheap and may be called
/// on every poll or consecutive operation.
///
/// Precondition: it must be **called sequentially from a single task** (the TUI event loop awaits
/// sequentially without spawning, which satisfies this). The expiry check → refresh is not atomic,
/// so sharing one client across multiple tasks and calling it concurrently could race into a
/// double refresh.
pub async fn ensure_fresh_token(spotify: &AuthCodePkceSpotify) -> Result<()> {
    let current = {
        let token_mutex = spotify.get_token();
        let guard = token_mutex
            .lock()
            .await
            .expect("token mutex poisoned (implies a prior panic)");
        guard.clone()
    };
    let token = current.context("not logged in. Run `spoterm login` first")?;
    if token.is_expired() {
        let refreshed = refresh_expired_token(spotify, token).await?;
        set_client_token(spotify, refreshed).await;
    }
    Ok(())
}

/// Set the authenticated client's token via the lock. This CLI runs one-shot and does not share
/// the token lock with other threads, so poisoning cannot happen (poison implies a prior panic).
async fn set_client_token(spotify: &AuthCodePkceSpotify, token: Token) {
    let token_mutex = spotify.get_token();
    let mut guard = token_mutex
        .lock()
        .await
        .expect("token mutex poisoned (implies a prior panic)");
    *guard = Some(token);
}

/// Explicitly refresh an expired token, then save it to the cache at 0600 while keeping the refresh_token, and return it.
async fn refresh_expired_token(spotify: &AuthCodePkceSpotify, expired: Token) -> Result<Token> {
    // Keep the pre-refresh value in case the response omits the refresh_token.
    let previous_refresh_token = expired.refresh_token.clone();

    // refetch_token refreshes using the refresh_token of the currently locked token, so set it first.
    set_client_token(spotify, expired).await;

    let refreshed = spotify
        .refetch_token()
        .await
        .context("failed to refresh the token")?
        .context(
            "cannot refresh because there is no refresh_token. Log in again with `spoterm login`",
        )?;

    let token = preserve_refresh_token(refreshed, previous_refresh_token);
    persist_token(&token)?;
    Ok(token)
}

/// If the refresh response omits the refresh_token, carry over the previous one.
/// Spotify's PKCE refresh response sometimes does not include a refresh_token, and saving that
/// as-is would make later refreshes impossible. If the response does return a new refresh_token,
/// prefer that.
fn preserve_refresh_token(mut refreshed: Token, previous_refresh_token: Option<String>) -> Token {
    if refreshed.refresh_token.is_none() {
        refreshed.refresh_token = previous_refresh_token;
    }
    refreshed
}

/// Return the token cache path (under the config directory).
fn token_cache_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join(TOKEN_CACHE_FILE))
}

/// Save the refreshed token to the cache (protected at 0600). Pre-create it at 0600 before
/// write_cache to close the window where a newly created file is briefly world-readable depending
/// on umask (write_cache reuses the existing inode via create+truncate, so the permissions are
/// preserved). Also re-restrict after writing in case an existing file had loose permissions.
fn persist_token(token: &Token) -> Result<()> {
    let cache = token_cache_path()?;
    precreate_token_cache_secure(&cache)?;
    token
        .write_cache(&cache)
        .with_context(|| format!("failed to save the token cache: {}", cache.display()))?;
    restrict_token_perms(&cache)
}

/// Pre-create the token cache at 0600 (if it already exists, leave its permissions and contents
/// unchanged). Call this right before writing the plaintext access/refresh tokens to close the
/// permission window on creation.
fn precreate_token_cache_secure(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // mode(0o600) applies only "on creation" (it does not affect an existing file). We open in
        // append mode to avoid losing an existing token via truncate (we close without writing anything).
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("failed to create the token cache: {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Restrict the token cache to owner read/write only (0600). Because it contains plaintext
/// access/refresh tokens, set this explicitly rather than relying on umask.
fn restrict_token_perms(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).with_context(
            || {
                format!(
                    "failed to set permissions on the token cache: {}",
                    path.display()
                )
            },
        )?;
    }
    Ok(())
}

/// `spoterm login`: authorize in the browser → receive the redirect on a local server → obtain and save the token.
pub async fn login(cfg: &Config) -> Result<()> {
    let mut spotify = build_client(cfg)?;

    // get_authorize_url stores the PKCE code_verifier on the client.
    // It must be called before the later request_token.
    let url = spotify
        .get_authorize_url(None)
        .context("failed to generate the authorization URL")?;
    // The CSRF state generated by rspotify. It is matched in the callback.
    let expected_state = spotify.get_oauth().state.clone();

    println!("Open the following URL in your browser and approve the Spotify authorization:\n");
    println!("  {url}\n");

    let code = wait_for_code(&cfg.redirect_uri, &expected_state)
        .await
        .context("failed to receive the redirect")?;

    // The cache written by request_token holds the access/refresh tokens in plaintext. Pre-create
    // it at 0600 before writing so rspotify's write_token_cache reuses this inode.
    let cache = token_cache_path()?;
    precreate_token_cache_secure(&cache)?;

    spotify
        .request_token(&code)
        .await
        .context("failed to obtain the access token")?;

    // Also finalize the permissions after writing, just in case an existing file had loose permissions.
    restrict_token_perms(&cache)?;

    match spotify.current_user().await {
        Ok(user) => {
            let who = user.display_name.unwrap_or_else(|| "(no name)".to_string());
            println!("✅ Login successful: {who}");
        }
        // The token itself is already saved. Failing to fetch user info alone is not fatal, but keep it for diagnostics.
        Err(e) => {
            eprintln!("failed to fetch user info: {e}");
            println!("✅ Login successful (token obtained)");
        }
    }
    println!("   Token saved: {}", cache.display());
    Ok(())
}

/// Listen on the redirect_uri's port, accept the callback carrying the correct state, and return the authorization code.
/// Binds to 0.0.0.0 because it may run inside a container. Docker's publish (127.0.0.1:8888->8888)
/// delivers packets to the container's eth0, so binding to 127.0.0.1 would not receive the forward.
async fn wait_for_code(redirect_uri: &str, expected_state: &str) -> Result<String> {
    let port = parse_port(redirect_uri)?;
    let expected_path = parse_path(redirect_uri);
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("failed to listen on port {port}"))?;

    println!(
        "Waiting for the redirect... (127.0.0.1:{port}) up to {}s",
        LOGIN_TIMEOUT.as_secs()
    );

    match timeout(
        LOGIN_TIMEOUT,
        accept_code(&listener, expected_state, &expected_path),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => bail!(
            "authorization timed out ({}s). Run `spoterm login` again",
            LOGIN_TIMEOUT.as_secs()
        ),
    }
}

/// Keep accepting connections until a callback matching the expected path and state arrives.
/// Unrelated connections (favicon fetches, leftovers from a previous run, state mismatches) get a 4xx and listening continues.
async fn accept_code(
    listener: &TcpListener,
    expected_state: &str,
    expected_path: &str,
) -> Result<String> {
    loop {
        let (mut stream, _) = listener.accept().await?;

        let request = match read_request(&mut stream).await {
            Ok(req) => req,
            Err(e) => {
                eprintln!("warning: failed to read the request: {e}");
                respond_status(&mut stream, 400, "Bad Request").await;
                continue;
            }
        };

        let Some(target) = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
        else {
            respond_status(&mut stream, 400, "Bad Request").await;
            continue;
        };

        let path = target.split('?').next().unwrap_or("");
        if path != expected_path {
            respond_status(&mut stream, 404, "Not Found").await;
            continue;
        }

        match decide(&parse_callback(target), expected_state) {
            Decision::Code(code) => {
                respond_html(
                    &mut stream,
                    "Authentication complete. You can return to the terminal.",
                )
                .await;
                return Ok(code);
            }
            Decision::Denied(err) => {
                respond_html(
                    &mut stream,
                    "Authentication failed. Please check the terminal.",
                )
                .await;
                bail!("Spotify returned an authorization error: {err}");
            }
            // Ignore state mismatches and incomplete requests, and keep waiting for the real callback.
            Decision::Ignore => {
                respond_status(&mut stream, 400, "Bad Request").await;
                continue;
            }
        }
    }
}

/// The result of interpreting a callback.
enum Decision {
    /// An authorization code carrying the correct state.
    Code(String),
    /// An error, e.g. the user denied authorization.
    Denied(String),
    /// An unrelated / invalid request (listening should continue).
    Ignore,
}

/// Interpret the callback query and decide the next action. A state mismatch is treated as ignore.
fn decide(cb: &Callback, expected_state: &str) -> Decision {
    if cb.state.as_deref() != Some(expected_state) {
        return Decision::Ignore;
    }
    if let Some(err) = &cb.error {
        return Decision::Denied(err.clone());
    }
    match &cb.code {
        Some(code) => Decision::Code(code.clone()),
        None => Decision::Ignore,
    }
}

/// The values extracted from the callback query.
#[derive(Default)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Parse the query of "/callback?code=...&state=...&error=...".
fn parse_callback(target: &str) -> Callback {
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut cb = Callback::default();
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "code" => cb.code = Some(percent_decode(value)),
            "state" => cb.state = Some(percent_decode(value)),
            "error" => cb.error = Some(percent_decode(value)),
            _ => {}
        }
    }
    cb
}

/// Read until the request line is available (up to `\r\n` or the limit).
async fn read_request(stream: &mut TcpStream) -> Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if find_subslice(&buf, b"\r\n").is_some() || buf.len() >= MAX_REQUEST_BYTES {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Return a simple HTML page reporting success/failure (the body is a fixed string with no user input).
async fn respond_html(stream: &mut TcpStream, message: &str) {
    let body = format!("<!doctype html><meta charset=utf-8><h2>{message}</h2>");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    if let Err(e) = write_all(stream, response.as_bytes()).await {
        eprintln!("warning: failed to send the redirect response: {e}");
    }
}

/// Return a status response with no body.
async fn respond_status(stream: &mut TcpStream, code: u16, reason: &str) {
    let response =
        format!("HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    if let Err(e) = write_all(stream, response.as_bytes()).await {
        eprintln!("warning: failed to send the response: {e}");
    }
}

async fn write_all(stream: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    stream.write_all(bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Extract the port number from "http://127.0.0.1:8888/callback".
fn parse_port(redirect_uri: &str) -> Result<u16> {
    let after_scheme = redirect_uri.split("://").nth(1).unwrap_or(redirect_uri);
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    let port_str = host_port
        .rsplit_once(':')
        .map(|(_, p)| p)
        .with_context(|| format!("no port specified in redirect_uri: {redirect_uri}"))?;
    port_str
        .parse::<u16>()
        .with_context(|| format!("failed to parse the port number: {port_str}"))
}

/// Extract the path ("/callback") from "http://127.0.0.1:8888/callback".
fn parse_path(redirect_uri: &str) -> String {
    let after_scheme = redirect_uri.split("://").nth(1).unwrap_or(redirect_uri);
    match after_scheme.find('/') {
        Some(i) => after_scheme[i..].to_string(),
        None => "/".to_string(),
    }
}

/// Minimal application/x-www-form-urlencoded decoding (no external crate needed).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserve_refresh_token_keeps_previous_when_response_omits_it() {
        let refreshed = Token {
            access_token: "new-access".into(),
            refresh_token: None,
            ..Default::default()
        };

        let merged = preserve_refresh_token(refreshed, Some("old-refresh".into()));

        assert_eq!(merged.refresh_token.as_deref(), Some("old-refresh"));
        assert_eq!(merged.access_token, "new-access");
    }

    #[test]
    fn preserve_refresh_token_prefers_rotated_value() {
        let refreshed = Token {
            refresh_token: Some("rotated-refresh".into()),
            ..Default::default()
        };

        let merged = preserve_refresh_token(refreshed, Some("old-refresh".into()));

        assert_eq!(merged.refresh_token.as_deref(), Some("rotated-refresh"));
    }

    #[cfg(unix)]
    #[test]
    fn restrict_token_perms_sets_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let path =
            std::env::temp_dir().join(format!("spoterm-perm-test-{}.json", std::process::id()));
        std::fs::write(&path, b"{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        restrict_token_perms(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn parses_port_from_redirect_uri() {
        assert_eq!(parse_port("http://127.0.0.1:8888/callback").unwrap(), 8888);
        assert_eq!(parse_port("http://localhost:9000/cb").unwrap(), 9000);
    }

    #[test]
    fn parse_port_fails_without_port() {
        assert!(parse_port("http://127.0.0.1/callback").is_err());
    }

    #[test]
    fn parses_path_from_redirect_uri() {
        assert_eq!(parse_path("http://127.0.0.1:8888/callback"), "/callback");
        assert_eq!(parse_path("http://127.0.0.1:8888"), "/");
    }

    #[test]
    fn parse_callback_extracts_fields() {
        let cb = parse_callback("/callback?code=AQD123&state=xyz");
        assert_eq!(cb.code.as_deref(), Some("AQD123"));
        assert_eq!(cb.state.as_deref(), Some("xyz"));
        assert!(cb.error.is_none());
    }

    #[test]
    fn decide_returns_code_when_state_matches() {
        let cb = parse_callback("/callback?code=AQD123&state=good");
        assert!(matches!(decide(&cb, "good"), Decision::Code(c) if c == "AQD123"));
    }

    #[test]
    fn decide_ignores_state_mismatch() {
        let cb = parse_callback("/callback?code=AQD123&state=bad");
        assert!(matches!(decide(&cb, "good"), Decision::Ignore));
    }

    #[test]
    fn decide_reports_denied_when_state_matches() {
        let cb = parse_callback("/callback?error=access_denied&state=good");
        assert!(matches!(decide(&cb, "good"), Decision::Denied(e) if e == "access_denied"));
    }

    #[test]
    fn decide_ignores_missing_code() {
        let cb = parse_callback("/callback?state=good");
        assert!(matches!(decide(&cb, "good"), Decision::Ignore));
    }

    #[test]
    fn percent_decode_handles_escapes_and_plus() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("hello+world"), "hello world");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[tokio::test]
    async fn accept_code_validates_state_and_ignores_spurious() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move { accept_code(&listener, "good", "/callback").await });

        // 1) A spurious request with a state mismatch → ignored, listening continues
        let mut spurious = TcpStream::connect(addr).await.unwrap();
        spurious
            .write_all(b"GET /callback?code=NOPE&state=bad HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut discard = Vec::new();
        let _ = spurious.read_to_end(&mut discard).await;

        // 2) The real callback carrying the correct state → returns the code
        let mut real = TcpStream::connect(addr).await.unwrap();
        real.write_all(b"GET /callback?code=REAL123&state=good HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut ok = Vec::new();
        let _ = real.read_to_end(&mut ok).await;

        let code = server.await.unwrap().unwrap();
        assert_eq!(code, "REAL123");
    }
}
