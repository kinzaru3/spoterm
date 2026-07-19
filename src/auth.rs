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

/// トークンキャッシュのファイル名（設定ディレクトリ配下）。
const TOKEN_CACHE_FILE: &str = "token.json";
/// 認可完了を待つ最大時間。これを過ぎたらログインを中断する。
const LOGIN_TIMEOUT: Duration = Duration::from_secs(180);
/// リクエスト行の読み取り上限（ヘッダ肥大化・不正入力に対する保険）。
const MAX_REQUEST_BYTES: usize = 8 * 1024;

/// PKCE 認証済みの rspotify クライアントを組み立てる（トークンはディスクにキャッシュ）。
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
            // 現在曲の保存/解除（TUI の `s`）に必要。追加後は再ログインで新スコープを付与する。
            "user-library-modify"
        ),
        ..Default::default()
    };

    let cache_path = token_cache_path()?;
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("設定ディレクトリの作成に失敗: {}", parent.display()))?;
        // 機密トークンを置くディレクトリなので所有者のみに制限する。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            {
                eprintln!("警告: 設定ディレクトリの権限設定に失敗: {e}");
            }
        }
    }

    let rconf = RSpotifyConfig {
        token_cached: true,
        // 自動リフレッシュは無効化し、authed_client で自前制御する。rspotify の自動更新は
        // Spotify が refresh_token を省略した応答を返すと refresh_token を null で上書き
        // 保存してしまい（毎リクエスト前の auto_reauth → write_token_cache）、以降リフレッシュ
        // 不能になる不具合があるため。
        // 注意: この結果リフレッシュ点は authed_client の入口 1 箇所のみになる。単発コマンドは
        // 数リクエストで完結するため問題ないが、将来ページング等で長時間・多数リクエストを行う
        // コマンドを追加する場合は、途中失効に備えた 401→再取得のリトライを別途用意すること。
        token_refreshing: false,
        cache_path,
        ..Default::default()
    };

    Ok(AuthCodePkceSpotify::with_config(creds, oauth, rconf))
}

/// キャッシュ済みトークンを読み込んだ認証済みクライアントを返す。
/// Phase 3 以降の API コマンド（status/search/devices/…）の共通入口。
/// 期限切れなら自前でリフレッシュし、refresh_token を保持したままキャッシュを 0600 で更新する
/// （rspotify の自動リフレッシュは refresh_token を失う不具合があるため無効化している）。
pub async fn authed_client(cfg: &Config) -> Result<AuthCodePkceSpotify> {
    let spotify = build_client(cfg)?;

    let token = spotify
        .read_token_cache(true)
        .await
        .context("トークンキャッシュの読み込みに失敗しました")?
        .context("未ログインです。先に `spoterm login` を実行してください")?;

    let token = if token.is_expired() {
        refresh_expired_token(&spotify, token).await?
    } else {
        token
    };

    // 読み込んだ（または更新した）トークンをクライアントに設定する。
    set_client_token(&spotify, token).await;

    Ok(spotify)
}

/// 長時間保持するクライアント（TUI）向けに、必要なときだけトークンを更新する。
/// [`authed_client`] と違い毎回ディスクを読み直さず、reqwest の接続プールを使い回せる。
/// 期限内なら何もしないので安価で、ポーリングや連続操作のたびに呼んでよい。
///
/// 前提: **単一タスクからの逐次呼び出し**であること（TUI のイベントループは spawn せず
/// 逐次 await するのでこれを満たす）。期限判定〜リフレッシュはアトミックでないため、同一
/// クライアントを複数タスクで共有して並行に呼ぶと二重リフレッシュの競合になり得る。
pub async fn ensure_fresh_token(spotify: &AuthCodePkceSpotify) -> Result<()> {
    let current = {
        let token_mutex = spotify.get_token();
        let guard = token_mutex
            .lock()
            .await
            .expect("token mutex poisoned (implies a prior panic)");
        guard.clone()
    };
    let token = current.context("未ログインです。先に `spoterm login` を実行してください")?;
    if token.is_expired() {
        let refreshed = refresh_expired_token(spotify, token).await?;
        set_client_token(spotify, refreshed).await;
    }
    Ok(())
}

/// 認証済みクライアントのトークンをロック経由で設定する。この CLI は単発実行でトークンロックを
/// 他スレッドと共有しないため poison は起こらない（poison は先行 panic を意味する）。
async fn set_client_token(spotify: &AuthCodePkceSpotify, token: Token) {
    let token_mutex = spotify.get_token();
    let mut guard = token_mutex
        .lock()
        .await
        .expect("token mutex poisoned (implies a prior panic)");
    *guard = Some(token);
}

/// 期限切れトークンを明示的に更新し、refresh_token を保持したままキャッシュへ 0600 で保存して返す。
async fn refresh_expired_token(spotify: &AuthCodePkceSpotify, expired: Token) -> Result<Token> {
    // 応答が refresh_token を省略した場合に備え、更新前の値を控える。
    let previous_refresh_token = expired.refresh_token.clone();

    // refetch_token は現在ロック中のトークンの refresh_token を使って更新するため、先に入れる。
    set_client_token(spotify, expired).await;

    let refreshed = spotify
        .refetch_token()
        .await
        .context("トークンの更新に失敗しました")?
        .context(
            "refresh_token が無いため更新できません。`spoterm login` で再ログインしてください",
        )?;

    let token = preserve_refresh_token(refreshed, previous_refresh_token);
    persist_token(&token)?;
    Ok(token)
}

/// リフレッシュ応答が refresh_token を省略した場合、直前の refresh_token を引き継ぐ。
/// Spotify の PKCE リフレッシュ応答は refresh_token を含まないことがあり、そのまま保存すると
/// 以降リフレッシュ不能になるため。応答が新しい refresh_token を返した場合はそれを優先する。
fn preserve_refresh_token(mut refreshed: Token, previous_refresh_token: Option<String>) -> Token {
    if refreshed.refresh_token.is_none() {
        refreshed.refresh_token = previous_refresh_token;
    }
    refreshed
}

/// トークンキャッシュのパス（設定ディレクトリ配下）を返す。
fn token_cache_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join(TOKEN_CACHE_FILE))
}

/// 更新済みトークンをキャッシュへ保存する（0600 で保護）。write_cache 前に 0600 で事前作成し、
/// 新規作成時に umask 依存で一時的に他者可読になる窓を塞ぐ（write_cache は create+truncate で
/// 既存 inode を再利用するため、権限は維持される）。既存ファイルが緩い権限だった場合に備え
/// 書き込み後にも制限をかける。
fn persist_token(token: &Token) -> Result<()> {
    let cache = token_cache_path()?;
    precreate_token_cache_secure(&cache)?;
    token
        .write_cache(&cache)
        .with_context(|| format!("トークンキャッシュの保存に失敗: {}", cache.display()))?;
    restrict_token_perms(&cache)
}

/// トークンキャッシュを 0600 で事前作成する（既存なら権限は変えず内容も保持）。平文の
/// access/refresh トークンを書き込む直前に呼び、新規作成時の権限窓を塞ぐ。
fn precreate_token_cache_secure(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // mode(0o600) は「新規作成時」のみ適用される（既存ファイルには影響しない）。append で開くのは
        // truncate による既存トークンの消失を避けるため（何も書かずに閉じる）。
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("トークンキャッシュの作成に失敗: {}", path.display()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// トークンキャッシュを所有者のみ読み書き可（0600）に制限する。平文の access/refresh トークンを
/// 含むため umask 依存にせず明示的に設定する。
fn restrict_token_perms(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("トークンキャッシュの権限設定に失敗: {}", path.display()))?;
    }
    Ok(())
}

/// `spoterm login`: ブラウザで認可 → ローカルサーバで redirect を受け取り → トークンを取得・保存。
pub async fn login(cfg: &Config) -> Result<()> {
    let mut spotify = build_client(cfg)?;

    // get_authorize_url は PKCE の code_verifier をクライアントに保存する。
    // 以降の request_token より前に必ず呼ぶ必要がある。
    let url = spotify
        .get_authorize_url(None)
        .context("認可URLの生成に失敗しました")?;
    // rspotify が生成した CSRF 用 state。コールバックで照合する。
    let expected_state = spotify.get_oauth().state.clone();

    println!("ブラウザで次のURLを開き、Spotify の認可を許可してください:\n");
    println!("  {url}\n");

    let code = wait_for_code(&cfg.redirect_uri, &expected_state)
        .await
        .context("リダイレクトの受信に失敗しました")?;

    // request_token が書き出すキャッシュには access/refresh トークンが平文で入る。書き込み前に
    // 0600 で事前作成し、rspotify の write_token_cache がこの inode を再利用するようにする。
    let cache = token_cache_path()?;
    precreate_token_cache_secure(&cache)?;

    spotify
        .request_token(&code)
        .await
        .context("アクセストークンの取得に失敗しました")?;

    // 念のため書き込み後にも権限を確定させる（既存ファイルが緩い権限だった場合の保険）。
    restrict_token_perms(&cache)?;

    match spotify.current_user().await {
        Ok(user) => {
            let who = user
                .display_name
                .unwrap_or_else(|| "(名前なし)".to_string());
            println!("✅ ログイン成功: {who}");
        }
        // トークン自体は保存済み。ユーザー情報取得だけ失敗しても致命的ではないが、診断用に残す。
        Err(e) => {
            eprintln!("ユーザー情報の取得に失敗しました: {e}");
            println!("✅ ログイン成功（トークン取得済み）");
        }
    }
    println!("   トークンを保存しました: {}", cache.display());
    Ok(())
}

/// redirect_uri のポートで待ち受け、正しい state を伴うコールバックを受けて認可コードを返す。
/// コンテナ内で動くため 0.0.0.0 にバインドする。docker の publish (127.0.0.1:8888->8888) は
/// パケットをコンテナの eth0 に届けるため、127.0.0.1 にバインドすると転送を受け取れない。
async fn wait_for_code(redirect_uri: &str, expected_state: &str) -> Result<String> {
    let port = parse_port(redirect_uri)?;
    let expected_path = parse_path(redirect_uri);
    let listener = TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("ポート {port} の待受に失敗しました"))?;

    println!(
        "リダイレクト待機中... (127.0.0.1:{port}) 最大{}秒",
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
            "認可がタイムアウトしました（{}秒）。もう一度 `spoterm login` を実行してください",
            LOGIN_TIMEOUT.as_secs()
        ),
    }
}

/// 期待するパスと state に一致するコールバックが来るまで受け付け続ける。
/// 無関係な接続（favicon 取得・前回の残骸・state 不一致）は 4xx を返して待受を継続する。
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
                eprintln!("警告: リクエストの読み取りに失敗: {e}");
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
                    "認証が完了しました。ターミナルに戻ってください。",
                )
                .await;
                return Ok(code);
            }
            Decision::Denied(err) => {
                respond_html(
                    &mut stream,
                    "認証に失敗しました。ターミナルを確認してください。",
                )
                .await;
                bail!("Spotify から認可エラーが返されました: {err}");
            }
            // state 不一致や不完全なリクエストは無視して本来のコールバックを待ち続ける。
            Decision::Ignore => {
                respond_status(&mut stream, 400, "Bad Request").await;
                continue;
            }
        }
    }
}

/// コールバックの判定結果。
enum Decision {
    /// 正しい state を伴う認可コード。
    Code(String),
    /// ユーザーが認可を拒否した等のエラー。
    Denied(String),
    /// 無関係・不正なリクエスト（待受を継続すべき）。
    Ignore,
}

/// コールバックのクエリを解釈して次の動作を決める。state 不一致は無視扱いにする。
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

/// コールバックのクエリから取り出した値。
#[derive(Default)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// "/callback?code=...&state=...&error=..." のクエリを解析する。
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

/// リクエスト行が読めるところまで（`\r\n` または上限まで）読み取る。
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

/// 成功/失敗を伝える簡単な HTML を返す（本文は固定文字列でユーザー入力を含めない）。
async fn respond_html(stream: &mut TcpStream, message: &str) {
    let body = format!("<!doctype html><meta charset=utf-8><h2>{message}</h2>");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    if let Err(e) = write_all(stream, response.as_bytes()).await {
        eprintln!("警告: リダイレクト応答の送信に失敗: {e}");
    }
}

/// ボディなしのステータス応答を返す。
async fn respond_status(stream: &mut TcpStream, code: u16, reason: &str) {
    let response =
        format!("HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    if let Err(e) = write_all(stream, response.as_bytes()).await {
        eprintln!("警告: 応答の送信に失敗: {e}");
    }
}

async fn write_all(stream: &mut TcpStream, bytes: &[u8]) -> Result<()> {
    stream.write_all(bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// "http://127.0.0.1:8888/callback" からポート番号を取り出す。
fn parse_port(redirect_uri: &str) -> Result<u16> {
    let after_scheme = redirect_uri.split("://").nth(1).unwrap_or(redirect_uri);
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    let port_str = host_port
        .rsplit_once(':')
        .map(|(_, p)| p)
        .with_context(|| format!("redirect_uri にポートが指定されていません: {redirect_uri}"))?;
    port_str
        .parse::<u16>()
        .with_context(|| format!("ポート番号の解析に失敗: {port_str}"))
}

/// "http://127.0.0.1:8888/callback" からパス("/callback")を取り出す。
fn parse_path(redirect_uri: &str) -> String {
    let after_scheme = redirect_uri.split("://").nth(1).unwrap_or(redirect_uri);
    match after_scheme.find('/') {
        Some(i) => after_scheme[i..].to_string(),
        None => "/".to_string(),
    }
}

/// 最小限の application/x-www-form-urlencoded デコード（外部クレート不要）。
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

        // 1) state 不一致の偽リクエスト → 無視され待受は継続する
        let mut spurious = TcpStream::connect(addr).await.unwrap();
        spurious
            .write_all(b"GET /callback?code=NOPE&state=bad HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut discard = Vec::new();
        let _ = spurious.read_to_end(&mut discard).await;

        // 2) 正しい state を伴う本物のコールバック → コードを返す
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
