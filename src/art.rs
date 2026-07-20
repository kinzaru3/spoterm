//! Cover-art support, shared by the TUI and the CLI commands.
//!
//! - Selecting the image URL to display ([`pick_image_url`]) and the SSRF allow-check
//!   ([`is_allowed_art_url`]) are pure functions.
//! - [`fetch_decode`] performs the HTTP fetch + decode.
//! - The TUI renders continuously via `ratatui-image`'s `Picker`/`StatefulProtocol` (see
//!   `tui::mod`), while [`show`] renders a single cover art inline below the CLI text output.
//!
//! - Fetching reuses a retained `reqwest::Client` (does not discard the connection pool).
//! - Decoding is CPU-bound, so it is offloaded to `spawn_blocking` to avoid stalling the caller.

use std::io::{Cursor, IsTerminal};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use image::{DynamicImage, ImageReader, Limits};

/// Target width (px) for cover art. A medium size is enough for terminal display and is light to fetch/decode.
const TARGET_WIDTH: u32 = 300;
/// Maximum image dimension (px) at decode time. Cover art is really about 640×640; this is a
/// generous cap against decompression bombs.
const MAX_IMAGE_DIM: u32 = 4096;
/// Height (terminal rows) of the inline cover art the CLI prints. Width is derived from this
/// (`ART_ROWS * 2`) so that, with a ~1:2 cell aspect ratio, the image renders roughly square.
const ART_ROWS: u16 = 12;
/// Timeout for the one-shot cover-art fetch in the CLI (mirrors the TUI's HTTP client).
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Allow-check for cover-art URLs (SSRF protection). Allows only `https` and Spotify's CDN (`*.scdn.co`).
/// The URL comes from Spotify API responses, but as defense in depth this pure function validates it before fetching.
pub fn is_allowed_art_url(url: &str) -> bool {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            u.scheme() == "https"
                && u.host_str()
                    .is_some_and(|h| h == "scdn.co" || h.ends_with(".scdn.co"))
        }
        Err(_) => false,
    }
}

/// Pure function that picks the URL to display from the art candidates (`url`, `width`, `height`).
/// Chooses the one whose width is closest to `TARGET_WIDTH`. Returns `None` if empty. Still selects
/// even when the width is unknown (0) if there is nothing else.
pub fn pick_image_url(images: &[(String, u32, u32)]) -> Option<String> {
    images
        .iter()
        .min_by_key(|(_, w, _)| w.abs_diff(TARGET_WIDTH))
        .map(|(url, _, _)| url.clone())
}

/// Pick the cover-art URL from an rspotify album's images. Convenience wrapper over
/// [`pick_image_url`] shared by the CLI commands that hold typed `FullTrack`s.
pub fn pick_from_images(images: &[rspotify::model::Image]) -> Option<String> {
    let mapped: Vec<(String, u32, u32)> = images
        .iter()
        .map(|im| {
            (
                im.url.clone(),
                im.width.unwrap_or(0),
                im.height.unwrap_or(0),
            )
        })
        .collect();
    pick_image_url(&mapped)
}

/// Fetch and decode the image. Fetch the body with `reqwest`, decode with `spawn_blocking`, and
/// return a `DynamicImage` (the caller passes it to `Picker::new_resize_protocol`).
pub async fn fetch_decode(client: &reqwest::Client, url: &str) -> Result<DynamicImage> {
    // SSRF protection: do not fetch anything other than the Spotify CDN (redirects are already disabled by the caller).
    if !is_allowed_art_url(url) {
        bail!("cover-art URL is not allowed");
    }
    let bytes = client
        .get(url)
        .send()
        .await
        .context("failed to fetch the cover art")?
        .error_for_status()
        .context("failed to fetch the cover art")?
        .bytes()
        .await
        .context("failed to read the cover-art body")?;
    // Decoding is CPU-bound. Impose a dimension cap (decompression-bomb protection) and run it in spawn_blocking.
    let img = tokio::task::spawn_blocking(move || -> Result<DynamicImage> {
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .context("failed to detect the image format")?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_DIM);
        limits.max_image_height = Some(MAX_IMAGE_DIM);
        reader.limits(limits);
        reader.decode().context("failed to decode the image")
    })
    .await
    .context("failed to run the image-decode task")??;
    Ok(img)
}

/// Fetch the cover art at `url` and render it inline (below the current text) for a CLI command.
///
/// Best-effort and non-fatal:
/// - `url == None` (episode / ad / no image) → does nothing.
/// - stdout or stdin is not a TTY → does nothing. Protocol detection and the inline viewport both
///   write escape sequences to stdout and read the reply from stdin; if either is redirected we
///   would leak noise into a pipe and/or block for seconds waiting for a reply that never comes.
/// - On fetch/decode/render failure → prints a short warning to stderr (no silent failure) and
///   returns. The command's text output is unaffected.
pub async fn show(url: Option<&str>) {
    let Some(url) = url else {
        return; // No art for this item; the text Now Playing already covers the state.
    };
    // Require an interactive terminal on both ends: the render path queries the terminal
    // (writes to stdout, reads from stdin), which would block or emit noise otherwise.
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        return;
    }
    // Fresh client per invocation (one-shot CLI). Timeout + no redirects = SSRF hardening the
    // fetch relies on. If the (static) config somehow fails to build, warn and skip rather than
    // fall back to an unhardened client that follows redirects and never times out.
    let http = match reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            eprintln!("warning: failed to build the cover-art HTTP client: {e:?}");
            return;
        }
    };
    match fetch_decode(&http, url).await {
        Ok(img) => {
            if let Err(e) = render_inline(img) {
                eprintln!("warning: failed to render cover art: {e:?}");
            }
        }
        Err(e) => eprintln!("warning: failed to fetch cover art: {e:?}"),
    }
}

/// Render a decoded image once into the terminal scrollback, below the current cursor line.
///
/// Uses the same rendering parts as the TUI (`Picker` + `StatefulImage`), so graphics-capable
/// terminals show a real image and others fall back to colored half-blocks. A tiny inline viewport
/// plus `insert_before` leaves the image in the normal scrollback and keeps the cursor beneath it.
fn render_inline(img: DynamicImage) -> Result<()> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::layout::Rect;
    use ratatui::widgets::StatefulWidget;
    use ratatui::{Terminal, TerminalOptions, Viewport};
    use ratatui_image::StatefulImage;
    use ratatui_image::picker::Picker;

    // Detect the terminal's image protocol (queries stdin/stdout); fall back to half-blocks.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    let mut protocol = picker.new_resize_protocol(img);

    // A 1-row inline viewport keeps a single blank separator line and leaves the cursor below the
    // inserted image; `insert_before` writes the ART_ROWS-tall image into the scrollback above it.
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(1),
        },
    )
    .context("failed to initialize the inline terminal for cover art")?;

    terminal
        .insert_before(ART_ROWS, |buf| {
            let width = buf.area.width.min(ART_ROWS.saturating_mul(2));
            let area = Rect::new(buf.area.x, buf.area.y, width, ART_ROWS);
            StatefulImage::default().render(area, buf, &mut protocol);
        })
        .context("failed to render cover art inline")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(url: &str, w: u32) -> (String, u32, u32) {
        (url.to_string(), w, w)
    }

    #[test]
    fn picks_width_closest_to_target() {
        // Of 640 / 300 / 64, 300 is closest to TARGET(300)
        let imgs = [img("a640", 640), img("b300", 300), img("c64", 64)];
        assert_eq!(pick_image_url(&imgs).as_deref(), Some("b300"));
    }

    #[test]
    fn picks_nearest_when_no_exact() {
        // Between 200 and 500, 200 is closer to 300
        let imgs = [img("big", 500), img("small", 200)];
        assert_eq!(pick_image_url(&imgs).as_deref(), Some("small"));
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(pick_image_url(&[]), None);
    }

    #[test]
    fn unknown_width_still_selectable() {
        // Selects even with unknown width (0) if it is the only one
        let imgs = [img("only", 0)];
        assert_eq!(pick_image_url(&imgs).as_deref(), Some("only"));
    }

    #[test]
    fn allows_only_https_spotify_cdn() {
        assert!(is_allowed_art_url("https://i.scdn.co/image/ab67616d"));
        assert!(is_allowed_art_url("https://scdn.co/x"));
    }

    #[test]
    fn rejects_non_cdn_and_non_https() {
        // http is not allowed
        assert!(!is_allowed_art_url("http://i.scdn.co/image/x"));
        // A different host (SSRF target) is not allowed
        assert!(!is_allowed_art_url(
            "https://169.254.169.254/latest/meta-data"
        ));
        assert!(!is_allowed_art_url("https://evil.com/scdn.co"));
        // Contains scdn.co but is a different domain (suffix spoofing) — not allowed
        assert!(!is_allowed_art_url("https://scdn.co.evil.com/x"));
        // Invalid URL
        assert!(!is_allowed_art_url("not a url"));
    }
}
