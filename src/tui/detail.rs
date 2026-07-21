//! Detail pane data model (issue #26 Phase 4). Drives the lower-right dashboard pane: given the
//! library item currently selected, fetch the tracks that belong to it (album tracks / playlist
//! items / artist top tracks / the track's own album) and expose them as flat, playable rows. It does
//! not touch `App`; rendering lives in `mod.rs`, row formatting in `view.rs`. Loaders stay thin —
//! they map API models to primitives and hand off to the pure formatter, like `browse.rs`.

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{
    AlbumId, ArtistId, Market, PlayableItem, PlaylistId, SimplifiedArtist, SimplifiedTrack, TrackId,
};
use rspotify::prelude::*;

use crate::auth;
use crate::format::join_artists;
use crate::tui::browse::PlayTarget;

/// Number of detail rows fetched (first page only).
const DETAIL_LIMIT: u32 = 50;

/// One playable track row in the detail pane.
#[derive(Clone)]
pub struct DetailRow {
    /// Album track number (`Some` for album context); playlist/artist rows use their list position.
    pub track_no: Option<u32>,
    pub title: String,
    pub artists: String,
    pub duration_ms: u128,
    pub uri: String,
}

/// A loaded detail: the context header title plus its track rows. Cloneable so the caller can cache
/// it per library-item URI and reuse it when the selection returns to that item.
#[derive(Clone)]
pub struct DetailData {
    pub title: String,
    pub rows: Vec<DetailRow>,
    /// The full track count when the list was truncated to the first page (`Some(total)` when the
    /// context has more tracks than were fetched), so the pane can say "first N of total" instead of
    /// silently implying the shown rows are everything. `None` when the full list is shown.
    pub truncated_total: Option<u32>,
}

/// State of the always-visible detail pane. All rows are selectable (no headers), so selection is a
/// simple clamped index. `key` is the library-item URI the current rows correspond to, used to skip
/// re-fetching while the selection stays put. The `Default` (empty, no key) is the "nothing loaded
/// yet" state shown before the first selection resolves.
#[derive(Default)]
pub struct DetailState {
    pub key: Option<String>,
    pub title: String,
    pub rows: Vec<DetailRow>,
    pub selected: usize,
    pub message: Option<String>,
}

impl DetailState {
    /// Populate from a loaded detail, resetting the selection to the top. An empty track list and a
    /// truncated (first-page-only) list are both messaged, so the pane never silently implies the
    /// shown rows are complete.
    pub fn set(&mut self, key: String, data: DetailData) {
        self.message = if data.rows.is_empty() {
            Some("No tracks".to_string())
        } else if let Some(total) = data.truncated_total {
            Some(format!(
                "first {} of {total} tracks — ↑↓ select / Enter play",
                data.rows.len()
            ))
        } else {
            None
        };
        self.key = Some(key);
        self.title = data.title;
        self.rows = data.rows;
        self.selected = 0;
    }

    /// Record a load failure for `key` (keeps the key so it is not retried every tick, and shows the
    /// message so the failure is never silent).
    pub fn set_error(&mut self, key: String, message: String) {
        self.key = Some(key);
        self.title = String::new();
        self.rows = Vec::new();
        self.selected = 0;
        self.message = Some(message);
    }

    /// Clear the pane when there is nothing to show a detail for (empty library selection).
    pub fn clear(&mut self, message: Option<String>) {
        self.key = None;
        self.title = String::new();
        self.rows = Vec::new();
        self.selected = 0;
        self.message = message;
    }

    pub fn select_next(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }
}

/// Fetch the detail for the given library target. `fallback_title` is the library item's own label,
/// used as the header for album/playlist/artist contexts (the `Track` context overrides it with the
/// album name). Loaders hard-fail on error; the caller surfaces the message (never silent).
pub async fn fetch(
    spotify: &AuthCodePkceSpotify,
    target: &PlayTarget,
    fallback_title: &str,
) -> Result<DetailData> {
    auth::ensure_fresh_token(spotify).await?;
    match target {
        PlayTarget::Album(uri) => {
            let id = AlbumId::from_uri(uri).context("failed to parse the album URI")?;
            let (rows, truncated_total) = album_rows(spotify, id).await?;
            Ok(DetailData {
                title: fallback_title.to_string(),
                rows,
                truncated_total,
            })
        }
        PlayTarget::Playlist(uri) => {
            let id = PlaylistId::from_uri(uri).context("failed to parse the playlist URI")?;
            let page = spotify
                .playlist_items_manual(id, None, None, Some(DETAIL_LIMIT), None)
                .await
                .context("failed to fetch playlist items")?;
            let truncated_total = truncated_total(page.total, page.items.len());
            let rows = page
                .items
                .into_iter()
                .enumerate()
                .filter_map(|(i, item)| match item.item {
                    Some(PlayableItem::Track(t)) => {
                        track_row(i, t.id, t.name, t.artists, t.duration)
                    }
                    _ => None,
                })
                .collect();
            Ok(DetailData {
                title: fallback_title.to_string(),
                rows,
                truncated_total,
            })
        }
        PlayTarget::Artist(uri) => {
            let id = ArtistId::from_uri(uri).context("failed to parse the artist URI")?;
            // rspotify 0.16.1 marks `artist_top_tracks` deprecated citing removal (issue #550).
            // Whether Spotify has actually retired `GET /artists/{id}/top-tracks` needs runtime
            // verification (see docs/manual-tests.md); it is the natural playable content for the
            // Artists tab, and if the endpoint does 404 the call errors and is surfaced on screen
            // (never silent), so this degrades safely rather than breaking the app.
            #[allow(deprecated)]
            let tracks = spotify
                .artist_top_tracks(id, Some(Market::FromToken))
                .await
                .context("failed to fetch artist top tracks")?;
            // Top tracks is a fixed short list (no paging), so it is never truncated.
            let rows = tracks
                .into_iter()
                .enumerate()
                .filter_map(|(i, t)| track_row(i, t.id, t.name, t.artists, t.duration))
                .collect();
            Ok(DetailData {
                title: fallback_title.to_string(),
                rows,
                truncated_total: None,
            })
        }
        PlayTarget::Track(uri) => {
            // A saved track's context is its album, so resolve the album (one call) then list it.
            let id = TrackId::from_uri(uri).context("failed to parse the track URI")?;
            let full = spotify
                .track(id, None)
                .await
                .context("failed to fetch the track")?;
            match full.album.id {
                Some(album_id) => {
                    let (rows, truncated_total) = album_rows(spotify, album_id).await?;
                    Ok(DetailData {
                        title: full.album.name,
                        rows,
                        truncated_total,
                    })
                }
                None => {
                    // No album context (e.g. a local track): show just the track itself, not blank.
                    let artists: Vec<String> = full.artists.into_iter().map(|a| a.name).collect();
                    Ok(DetailData {
                        title: full.name.clone(),
                        rows: vec![DetailRow {
                            track_no: Some(full.track_number),
                            title: full.name,
                            artists: join_artists(&artists),
                            duration_ms: full.duration.num_milliseconds().max(0) as u128,
                            uri: uri.clone(),
                        }],
                        truncated_total: None,
                    })
                }
            }
        }
    }
}

/// `Some(total)` when the context has more tracks than the single page we fetched, so the caller can
/// tell the user the list is only the first page; `None` when everything fits.
fn truncated_total(total: u32, fetched: usize) -> Option<u32> {
    (total as usize > fetched).then_some(total)
}

/// Album tracks as detail rows (track number preserved, tracks without a URI skipped). Returns the
/// rows and, when the album has more tracks than one page, the full total for a truncation notice.
async fn album_rows(
    spotify: &AuthCodePkceSpotify,
    album_id: AlbumId<'_>,
) -> Result<(Vec<DetailRow>, Option<u32>)> {
    let page = spotify
        .album_track_manual(album_id, None, Some(DETAIL_LIMIT), None)
        .await
        .context("failed to fetch album tracks")?;
    let truncated = truncated_total(page.total, page.items.len());
    let rows = page.items.into_iter().filter_map(simplified_row).collect();
    Ok((rows, truncated))
}

/// Map a `SimplifiedTrack` (album context) to a row, keeping its real track number.
fn simplified_row(t: SimplifiedTrack) -> Option<DetailRow> {
    let uri = t.id.as_ref()?.uri();
    let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
    Some(DetailRow {
        track_no: Some(t.track_number),
        title: t.name,
        artists: join_artists(&artists),
        duration_ms: t.duration.num_milliseconds().max(0) as u128,
        uri,
    })
}

/// Map a list-context track (playlist/artist) to a row, numbering it by its 0-based list position
/// (playlist/artist ordering, not the album track number, is what the user sees). Returns `None` for
/// tracks without a playable URI.
fn track_row(
    index: usize,
    id: Option<TrackId<'static>>,
    name: String,
    artists: Vec<SimplifiedArtist>,
    duration: chrono::Duration,
) -> Option<DetailRow> {
    let uri = id.as_ref()?.uri();
    let artist_names: Vec<String> = artists.into_iter().map(|a| a.name).collect();
    Some(DetailRow {
        track_no: Some((index + 1) as u32),
        title: name,
        artists: join_artists(&artist_names),
        duration_ms: duration.num_milliseconds().max(0) as u128,
        uri,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(title: &str) -> DetailRow {
        DetailRow {
            track_no: Some(1),
            title: title.to_string(),
            artists: String::new(),
            duration_ms: 1000,
            uri: format!("spotify:track:{title}"),
        }
    }

    #[test]
    fn set_empty_shows_no_tracks() {
        let mut s = DetailState::default();
        s.set(
            "k".to_string(),
            DetailData {
                title: "Album".to_string(),
                rows: vec![],
                truncated_total: None,
            },
        );
        assert_eq!(s.message.as_deref(), Some("No tracks"));
        assert_eq!(s.key.as_deref(), Some("k"));
    }

    #[test]
    fn set_truncated_reports_first_of_total() {
        let mut s = DetailState::default();
        s.set(
            "k".to_string(),
            DetailData {
                title: "Big Playlist".to_string(),
                rows: vec![row("a"), row("b")],
                truncated_total: Some(187),
            },
        );
        let msg = s.message.unwrap();
        assert!(msg.starts_with("first 2 of 187 tracks"), "got: {msg}");
    }

    #[test]
    fn set_full_list_has_no_message() {
        let mut s = DetailState::default();
        s.set(
            "k".to_string(),
            DetailData {
                title: "Album".to_string(),
                rows: vec![row("a")],
                truncated_total: None,
            },
        );
        assert!(s.message.is_none());
    }

    #[test]
    fn set_error_keeps_key_and_message() {
        let mut s = DetailState::default();
        s.set_error("k".to_string(), "failed: boom".to_string());
        assert_eq!(s.key.as_deref(), Some("k"));
        assert_eq!(s.message.as_deref(), Some("failed: boom"));
        assert!(s.rows.is_empty());
    }

    #[test]
    fn selection_clamps_within_rows() {
        let mut s = DetailState::default();
        s.set(
            "k".to_string(),
            DetailData {
                title: "A".to_string(),
                rows: vec![row("a"), row("b")],
                truncated_total: None,
            },
        );
        s.select_prev(); // already at 0, stays
        assert_eq!(s.selected, 0);
        s.select_next();
        assert_eq!(s.selected, 1);
        s.select_next(); // clamps at last
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn truncated_total_only_when_more_than_fetched() {
        assert_eq!(truncated_total(50, 50), None);
        assert_eq!(truncated_total(51, 50), Some(51));
        assert_eq!(truncated_total(10, 20), None);
    }
}
