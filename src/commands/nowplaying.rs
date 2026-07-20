//! Shared "now playing → cover art" helpers for the CLI commands.
//!
//! Maps the current playback context to the cover-art URL to display and renders it inline
//! (via [`crate::art::show`]). Kept separate from each command so the mapping is testable with
//! `wiremock` while the terminal rendering stays a thin side-effecting layer.

use std::time::Duration;

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::PlayableItem;
use rspotify::prelude::*;

use super::status;
use crate::art;

/// How long to wait after a control command (`next`/`prev`/resume) before reading the now-playing
/// track. Spotify Connect propagates the change asynchronously, so an immediate read can still
/// return the previous track. This is best-effort (see `docs/design/cli-cover-art.md`).
const SETTLE_DELAY: Duration = Duration::from_millis(300);

/// Pick the cover-art URL to display for a playback item. `None` for episodes / items without art.
/// Exposed within the crate so `status` reuses it without a second API call.
pub(crate) fn pick_art_url(item: &PlayableItem) -> Option<String> {
    match item {
        PlayableItem::Track(t) => art::pick_from_images(&t.album.images),
        PlayableItem::Episode(_) => None,
        // Spotify's /me/player track JSON lacks external_ids, so rspotify drops it to Unknown;
        // extract the album images from the raw JSON (same fallback the status display uses).
        PlayableItem::Unknown(v) => art::pick_image_url(&status::album_images_from_json(v)),
    }
}

/// Fetch the current playback and return the cover-art URL to display (`None` when nothing is
/// playing or the item has no art). The request building + response mapping is testable via wiremock.
pub(crate) async fn current_art_url(spotify: &AuthCodePkceSpotify) -> Result<Option<String>> {
    let ctx = spotify
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("failed to fetch playback status")?;
    Ok(ctx.and_then(|c| c.item).as_ref().and_then(pick_art_url))
}

/// Fetch the now-playing cover art and render it inline immediately (no wait). For paths where the
/// track does not change (resume), so `current_playback` already reflects the right track.
/// Best-effort: a fetch failure prints a warning (no silent failure); the text output is unaffected.
pub(crate) async fn show_current_art(spotify: &AuthCodePkceSpotify) {
    match current_art_url(spotify).await {
        Ok(url) => art::show(url.as_deref()).await,
        Err(e) => eprintln!("warning: failed to fetch cover art: {e:?}"),
    }
}

/// Like [`show_current_art`], but first waits `SETTLE_DELAY` for Spotify Connect to propagate the
/// track change. For `next`/`prev`, where the now-playing track changes asynchronously.
pub(crate) async fn show_after_control(spotify: &AuthCodePkceSpotify) {
    tokio::time::sleep(SETTLE_DELAY).await;
    show_current_art(spotify).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn current_art_url_picks_from_playing_track() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/player"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                crate::test_fixtures::playback_unknown_track("Song", "Artist"),
            ))
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        // The fixture's album has a 300px image, which is the target width → it is chosen.
        let url = current_art_url(&client).await.unwrap();
        assert_eq!(url.as_deref(), Some("https://i.scdn.co/image/cover300"));
    }

    #[tokio::test]
    async fn current_art_url_is_none_when_nothing_playing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/player"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert_eq!(current_art_url(&client).await.unwrap(), None);
    }

    #[tokio::test]
    async fn current_art_url_propagates_fetch_error() {
        // A 5xx from the playback endpoint must surface as Err (so callers warn, not silently skip).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/player"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert!(current_art_url(&client).await.is_err());
    }
}
