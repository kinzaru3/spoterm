//! Now Playing pane (issue #34 pane split). Owns the playback poll/fetch, the transport controls
//! (`space`/`n`/`p`/`+`/`-`/`←`/`→`/`s`), the saved-state and cover-art refreshes, and the Now
//! Playing pane rendering. Extracted verbatim from the former `mod.rs` monolith — behavior is
//! unchanged. It reads/writes `super::App`, uses the shared `super::finish` / `super::ensure_ready`
//! helpers, and honors the `super::MAX_POLL_FAILURES` auto-refresh cap.

use std::time::Instant;

use anyhow::{Context, Result};
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui_image::StatefulImage;
use rspotify::model::{
    CurrentPlaybackContext, LibraryId, Offset, PlayableId, PlayableItem, TrackId,
};
use rspotify::prelude::*;

use crate::auth;
use crate::format::join_artists;
use crate::theme;
use crate::tui::art;
use crate::tui::view::{self, NowPlaying};

use super::{App, MAX_POLL_FAILURES};

pub(super) async fn start_playback_queue(
    app: &App,
    uris: &[String],
    selected: usize,
) -> Result<()> {
    let (ids, offset) = queue_from_uris(uris, selected)?;
    auth::ensure_fresh_token(&app.client).await?;
    app.client
        .start_uris_playback(ids.into_iter().map(PlayableId::Track), None, offset, None)
        .await
        .context("failed to start playback (an active device may be required)")?;
    Ok(())
}

/// Parse result URIs into track ids and compute the play offset for the selected index.
/// Queueing every hit (not just the selected one) is what gives `next`/`prev` somewhere to go.
/// A URI that fails to parse aborts the whole play rather than silently dropping a track.
fn queue_from_uris(uris: &[String], selected: usize) -> Result<(Vec<TrackId<'_>>, Option<Offset>)> {
    let ids = uris
        .iter()
        .map(|u| TrackId::from_uri(u))
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse a track URI")?;
    let offset = uris.get(selected).map(|u| Offset::Uri(u.clone()));
    Ok((ids, offset))
}

// ---- API integration --------------------------------------------------------

/// Fetch the playback status and update `app.now`. Failures are shown on the status line.
pub(super) async fn poll_playback(app: &mut App) {
    // Detect recovery (failures were ongoing until just now) to clear a lingering warning.
    let was_failing = app.poll_failures > 0;
    match fetch_playback(app).await {
        Ok(Some(np)) => {
            // On track change, discard the saved state and re-fetch it next (only on change, not every poll).
            let prev_uri = app.now.as_ref().and_then(|n| n.track_uri.clone());
            if np.track_uri != prev_uri {
                app.saved = None;
                app.saved_checked = false;
            }
            app.now = Some(np);
            app.poll_failures = 0;
            // On recovery, clear only if what remains is a stale ⚠ warning (do not clear a
            // legitimate message from the user's last operation, i.e. Ok/Info).
            if was_failing && view::status_kind(&app.status) == view::StatusKind::Warn {
                app.status.clear();
            }
            refresh_saved(app).await;
            refresh_art(app).await;
        }
        Ok(None) => {
            app.now = None;
            app.saved = None;
            app.saved_checked = false;
            app.art = None;
            app.art_url = None;
            app.poll_failures = 0;
            if was_failing && view::status_kind(&app.status) == view::StatusKind::Warn {
                app.status.clear();
            }
        }
        Err(e) => {
            app.poll_failures = app.poll_failures.saturating_add(1);
            app.status = if app.poll_failures >= MAX_POLL_FAILURES {
                format!(
                    "{} auto-refresh stopped ({e}). Press r to retry / q to quit",
                    theme::WARN
                )
            } else {
                format!("{} refresh failed: {e}", theme::WARN)
            };
        }
    }
}

async fn fetch_playback(app: &App) -> Result<Option<NowPlaying>> {
    auth::ensure_fresh_token(&app.client).await?;
    let ctx = app
        .client
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("failed to fetch playback status")?;
    Ok(ctx.map(snapshot_from_context))
}

/// Map rspotify's playback context into a display snapshot.
fn snapshot_from_context(ctx: CurrentPlaybackContext) -> NowPlaying {
    let device = ctx.device.name;
    // By Spotify's contract this is 0-100, but as an external boundary, cap at 100 before casting to u8 (avoids a silent wraparound).
    let volume = ctx.device.volume_percent.map(|v| v.min(100) as u8);
    let progress_ms = ctx
        .progress
        .map(|d| d.num_milliseconds().max(0) as u128)
        .unwrap_or(0);
    let is_playing = ctx.is_playing;

    // track_uri is used for the save action and track-change detection; album_image_url for cover-art fetching.
    // Track uses the typed model; Unknown is extracted from raw JSON.
    let (title, artists, album, duration_ms, track_uri, album_image_url) = match ctx.item {
        Some(PlayableItem::Track(t)) => {
            let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            let dur = t.duration.num_milliseconds().max(0) as u128;
            let uri = t.id.as_ref().map(|id| id.uri());
            let images: Vec<(String, u32, u32)> = t
                .album
                .images
                .into_iter()
                .map(|im| (im.url, im.width.unwrap_or(0), im.height.unwrap_or(0)))
                .collect();
            let art_url = art::pick_image_url(&images);
            (t.name, artists, Some(t.album.name), dur, uri, art_url)
        }
        Some(PlayableItem::Episode(e)) => {
            let dur = e.duration.num_milliseconds().max(0) as u128;
            (e.name, vec!["(podcast)".to_string()], None, dur, None, None)
        }
        // Like the status command, extract a fallback from the raw JSON that fell to Unknown.
        Some(PlayableItem::Unknown(v)) => {
            let (title, artists, album, dur) = crate::np_json::track_from_json(&v);
            let images = crate::np_json::album_images_from_json(&v);
            (
                title,
                artists,
                album,
                dur,
                crate::np_json::track_id_from_json(&v),
                art::pick_image_url(&images),
            )
        }
        None => (
            "(no track info while playing)".to_string(),
            Vec::new(),
            None,
            0,
            None,
            None,
        ),
    };

    NowPlaying {
        is_playing,
        title,
        artists: join_artists(&artists),
        album,
        progress_ms,
        duration_ms,
        device,
        volume,
        track_uri,
        album_image_url,
        fetched_at: Instant::now(),
    }
}

pub(super) async fn control_toggle(app: &mut App) {
    let playing = app.now.as_ref().is_some_and(|n| n.is_playing);
    if !super::ensure_ready(app).await {
        return;
    }
    // To avoid a borrow conflict, settle the result first, then pass it to finish (&mut app).
    if playing {
        let res = app.client.pause_playback(None).await;
        super::finish(app, res, &format!("{} Paused", theme::PAUSE));
    } else {
        let res = app.client.resume_playback(None, None).await;
        super::finish(app, res, &format!("{} Playing", theme::PLAY));
    }
}

pub(super) async fn control_next(app: &mut App) {
    if !super::ensure_ready(app).await {
        return;
    }
    let res = app.client.next_track(None).await;
    super::finish(app, res, &format!("{} Next track", theme::NEXT));
}

pub(super) async fn control_prev(app: &mut App) {
    if !super::ensure_ready(app).await {
        return;
    }
    let res = app.client.previous_track(None).await;
    super::finish(app, res, &format!("{} Previous track", theme::PREV));
}

pub(super) async fn control_volume(app: &mut App, delta: i16) {
    let Some(cur) = app.now.as_ref().and_then(|n| n.volume) else {
        app.status = format!(
            "{} device volume is unavailable (press d to select a device)",
            theme::WARN
        );
        return;
    };
    let next = (cur as i16 + delta).clamp(0, 100) as u8;
    if !super::ensure_ready(app).await {
        return;
    }
    let res = app.client.volume(next, None).await;
    super::finish(app, res, &format!("{} Volume {next}%", theme::VOLUME));
}

/// Fetch the current track's saved state and update `app.saved`. Best-effort: query only when
/// `saved` is undetermined and a URI exists, and do not surface a status on failure (the main poll
/// reports network/token errors, so do not overwrite the status and confuse the user here). The
/// marker simply does not appear.
async fn refresh_saved(app: &mut App) {
    // Query only once per track (`saved_checked`). Do not hammer every poll even on persistent failure.
    if app.saved_checked {
        return;
    }
    let Some(uri) = app.now.as_ref().and_then(|n| n.track_uri.clone()) else {
        return; // Unknown track (episodes, etc.) is not queried. No API call either.
    };
    let Ok(id) = TrackId::from_uri(&uri) else {
        app.saved_checked = true;
        return;
    };
    // A token-refresh failure is not a hard stop (the main poll reports the failure, and past the threshold auto-refresh itself stops).
    if auth::ensure_fresh_token(&app.client).await.is_err() {
        return;
    }
    // Regardless of success, stop re-querying for this track (best-effort).
    app.saved_checked = true;
    if let Ok(mut flags) = app.client.library_contains([LibraryId::Track(id)]).await {
        app.saved = flags.pop();
    }
}

/// Refresh the cover art. Re-fetch only when the current track's art URL differs from `art_url`
/// (once per track, no retry on failure = best-effort). A fetch failure keeps the metadata display
/// and is shown on the status line (no silent failures).
async fn refresh_art(app: &mut App) {
    let url = app.now.as_ref().and_then(|n| n.album_image_url.clone());
    if url == app.art_url {
        return; // No change (same track) → do not re-fetch
    }
    // Regardless of success, stop re-fetching for this URL (prevents hammering every poll).
    app.art_url = url.clone();
    let Some(url) = url else {
        app.art = None; // No art (episodes, etc.)
        return;
    };
    match art::fetch_decode(&app.http, &url).await {
        Ok(img) => app.art = Some(app.picker.new_resize_protocol(img)),
        Err(e) => {
            app.art = None;
            app.status = format!("{} failed to fetch cover art: {e}", theme::WARN);
        }
    }
}

/// Seek the current track by ±`delta_ms`. The target is computed from local progress (with
/// interpolation), and on success progress is updated immediately and reflected on screen (no
/// forced poll, to avoid appearing to rewind due to Connect's propagation delay). Repeated presses
/// accumulate from the locally updated progress.
pub(super) async fn control_seek(app: &mut App, delta_ms: i64) {
    let Some(n) = app.now.as_ref() else {
        app.status = format!("{} nothing is playing", theme::WARN);
        return;
    };
    let elapsed = n.fetched_at.elapsed().as_millis();
    let current = view::interpolate_progress(n.progress_ms, elapsed, n.duration_ms, n.is_playing);
    let target = view::seek_target(current, n.duration_ms, delta_ms);
    if !super::ensure_ready(app).await {
        return;
    }
    // target as i64: target is already clamped by duration_ms (and even when length is unknown, within a
    // realistic number of presses), so it will not reach i64::MAX (~290 million years) — safe.
    let res = app
        .client
        .seek_track(chrono::Duration::milliseconds(target as i64), None)
        .await;
    match res {
        Ok(()) => {
            // Reflect local progress immediately (no forced poll).
            if let Some(n) = app.now.as_mut() {
                n.progress_ms = target;
                n.fetched_at = Instant::now();
            }
            app.status = format!("{} Seek {}", theme::SEEK, crate::format::format_ms(target));
        }
        Err(e) => {
            app.status = format!(
                "{} seek failed: {e} (press d to select and activate a device)",
                theme::WARN
            );
        }
    }
}

/// Save/unsave the current track in the library (`s`). Toggles to the opposite of the current saved state, updating it on success.
pub(super) async fn control_save(app: &mut App) {
    let Some(uri) = app.now.as_ref().and_then(|n| n.track_uri.clone()) else {
        app.status = format!(
            "{} cannot save the current track (track info is unknown)",
            theme::WARN
        );
        return;
    };
    let id = match TrackId::from_uri(&uri) {
        Ok(id) => id,
        Err(e) => {
            app.status = format!("{} failed to parse the track URI: {e}", theme::WARN);
            return;
        }
    };
    if !super::ensure_ready(app).await {
        return;
    }
    // If undetermined, interpret as "save".
    let want_save = !app.saved.unwrap_or(false);
    let res = if want_save {
        app.client.library_add([LibraryId::Track(id)]).await
    } else {
        app.client.library_remove([LibraryId::Track(id)]).await
    };
    match res {
        Ok(()) => {
            app.saved = Some(want_save);
            app.saved_checked = true;
            app.status = if want_save {
                format!("{} Saved to your library", theme::HEART)
            } else {
                format!("{} Removed from your library", theme::HEART_O)
            };
        }
        Err(e) => {
            app.status = format!("{} save operation failed: {e}", theme::WARN);
        }
    }
}

// ---- Rendering --------------------------------------------------------------

/// Draw the Now Playing pane: an optional cover-art column of `art_cols` columns on the left (0 =
/// none) and the text lines on the right. The text rows are placed by `view::stack_rows` in priority
/// order (state / title / artist / album / device), so a short pane drops the lower rows first
/// instead of letting the layout solver crush an arbitrary one to height 0. Progress is shown by the
/// bottom playbar, not here. The cover art is rendered last so `&mut app.art` is the final borrow.
pub(super) fn draw_now_playing_pane(
    frame: &mut ratatui::Frame,
    app: &mut App,
    area: ratatui::layout::Rect,
    art_cols: u16,
    v: &view::RenderLines,
) {
    // A placeholder is shown even when art is absent/not yet fetched, to make the empty state
    // explicit. `art_cols` (from `view::art_col_width`) is 0 when no column should be shown.
    let (art_area, text_area) = if art_cols > 0 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(art_cols), Constraint::Min(1)])
            .split(area);
        (Some(cols[0]), cols[1])
    } else {
        (None, area)
    };

    // Priority-ordered rows: highest first, so `stack_rows` drops device/album before title.
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let accent = Style::default()
        .fg(theme::GREEN)
        .add_modifier(Modifier::BOLD);
    let plain = Style::default();
    let lines = [
        (v.state.as_str(), accent),
        (v.title.as_str(), bold),
        (v.artist.as_str(), plain),
        (v.album.as_str(), plain),
        (v.device.as_str(), plain),
    ];
    for ((text, style), rect) in lines.iter().zip(view::stack_rows(text_area, lines.len())) {
        frame.render_widget(Paragraph::new(*text).style(*style), rect);
    }

    // Cover art last (first `&mut app.art` borrow). Placeholder makes the empty state explicit.
    if let Some(art_rect) = art_area {
        if let Some(art) = app.art.as_mut() {
            frame.render_stateful_widget(StatefulImage::default(), art_rect, art);
        } else {
            let art_placeholder = Paragraph::new(format!("{}\n\n(no art)", theme::MUSIC))
                .alignment(Alignment::Center)
                .style(Style::default().add_modifier(Modifier::DIM))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(theme::GREEN)),
                );
            frame.render_widget(art_placeholder, art_rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(id: &str) -> String {
        format!("spotify:track:{id}")
    }

    #[test]
    fn queue_from_uris_queues_all_hits_and_offsets_to_selection() {
        let uris = vec![uri("4iV5W9uYEdYUVa79Axb7Rh"), uri("1301WleyT98MSxVHPZCA6M")];

        let (ids, offset) = queue_from_uris(&uris, 1).unwrap();

        // Every hit is queued so `next`/`prev` have somewhere to go...
        assert_eq!(ids.len(), 2);
        // ...and playback starts at the selected track, not the first one.
        assert_eq!(offset, Some(Offset::Uri(uris[1].clone())));
    }

    #[test]
    fn queue_from_uris_rejects_an_unparseable_uri() {
        let uris = vec![uri("4iV5W9uYEdYUVa79Axb7Rh"), "not-a-uri".to_string()];

        assert!(queue_from_uris(&uris, 0).is_err());
    }

    #[test]
    fn queue_from_uris_without_a_matching_selection_omits_the_offset() {
        let uris = vec![uri("4iV5W9uYEdYUVa79Axb7Rh")];

        // `selected` past the end yields no offset (Spotify then starts at the queue head).
        let (ids, offset) = queue_from_uris(&uris, 9).unwrap();

        assert_eq!(ids.len(), 1);
        assert_eq!(offset, None);
    }
}
