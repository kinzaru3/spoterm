//! Cover-art support. Handles selecting the image URL to display (a pure function) and
//! the HTTP fetch + decode of the image. Rendering to the terminal (protocol detection and
//! rendering) is done on the `mod.rs` side using `ratatui-image`'s `Picker`/`StatefulProtocol`.
//!
//! - Fetching reuses the retained `reqwest::Client` (does not discard the connection pool).
//! - Decoding is CPU-bound, so it is offloaded to `spawn_blocking` to avoid stalling the UI loop.

use std::io::Cursor;

use anyhow::{Context, Result, bail};
use image::{DynamicImage, ImageReader, Limits};

/// Target width (px) for cover art. A medium size is enough for terminal display and is light to fetch/decode.
const TARGET_WIDTH: u32 = 300;
/// Maximum image dimension (px) at decode time. Cover art is really about 640×640; this is a
/// generous cap against decompression bombs.
const MAX_IMAGE_DIM: u32 = 4096;

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
