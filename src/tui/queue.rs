//! Queue pane (issue #26 Phase 7). Owns the playback-queue poll/fetch and the display-only pane
//! rendering that replaced the former Visualizer placeholder. The pane shows the currently-playing
//! track followed by the upcoming queue, mirroring the detail pane's block/hint/list layout but
//! without focus or selection (the upper dashboard row is display-only).
//!
//! Following the project's test policy, string formatting lives in pure functions in `view`
//! (`view::queue_row` / `view::queue_hint`), so this module stays a thin layer that maps the API
//! response to primitives and wires them to the formatters. Fetch failures are never silent: they
//! surface as an in-pane message.

use anyhow::{Context, Result};
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use rspotify::model::{CurrentUserQueue, PlayableItem};
use rspotify::prelude::*;

use crate::auth;
use crate::format::join_artists;
use crate::theme;
use crate::tui::view;

use super::App;

/// Consecutive queue-fetch failures tolerated before the last good rows are dropped for a visible
/// "unavailable" message. Mirrors `MAX_POLL_FAILURES` for the playback poll: a single 2-second
/// hiccup keeps the pane steady, but a persistent problem is still surfaced rather than hidden
/// behind stale rows.
const MAX_QUEUE_FAILURES: u32 = 3;

/// One row of the queue pane, already mapped to display primitives. `is_current` marks the
/// currently-playing track (rendered with the play glyph instead of a queue number).
pub struct QueueRow {
    pub title: String,
    pub artists: String,
    pub is_current: bool,
}

/// The queue pane's state: the rows to show, plus an optional message shown instead of the list
/// (empty queue, nothing playing, or a fetch failure) so the pane is never silently blank.
/// Invariant kept by the constructors below: whenever `rows` is empty, `message` is `Some` (the draw
/// path also guards this defensively).
pub(super) struct QueueState {
    pub rows: Vec<QueueRow>,
    pub message: Option<String>,
    /// Consecutive fetch failures since the last success. Reset to 0 on every successful poll.
    pub failures: u32,
}

impl Default for QueueState {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            // Before the first poll resolves, say so rather than implying an empty queue.
            message: Some("Loading…".to_string()),
            failures: 0,
        }
    }
}

/// Poll the user's playback queue and store the result on `app`. Called on the same cadence as the
/// playback poll. On success the pane is fully refreshed (and the failure count reset); on failure the
/// error is never silently swallowed — a transient hiccup keeps the last good rows to avoid a
/// 2-second flicker, and a persistent failure escalates to a visible message (see `degrade_on_failure`).
pub(super) async fn poll_queue(app: &mut App) {
    match fetch_queue(app).await {
        Ok(state) => app.queue = state,
        // `{e:#}` includes the anyhow context chain (the bare `{e}` would show only the top wrapper).
        Err(e) => degrade_on_failure(&mut app.queue, format!("{e:#}")),
    }
}

/// Record a queue-fetch failure. The last good rows are kept until `MAX_QUEUE_FAILURES` consecutive
/// failures accumulate; past that threshold they are dropped for a visible "unavailable" message so a
/// persistent problem is surfaced rather than hidden behind stale rows.
fn degrade_on_failure(queue: &mut QueueState, error: String) {
    queue.failures = queue.failures.saturating_add(1);
    if queue.failures >= MAX_QUEUE_FAILURES {
        queue.rows = Vec::new();
        queue.message = Some(format!("{} queue unavailable ({error})", theme::WARN));
    }
}

async fn fetch_queue(app: &App) -> Result<QueueState> {
    auth::ensure_fresh_token(&app.client).await?;
    let queue = app
        .client
        .current_user_queue()
        .await
        .context("failed to fetch playback queue")?;
    Ok(build_queue_state(queue))
}

/// Map the API response to display rows: the currently-playing track first (if any), then the
/// upcoming queue. When nothing resolves, leave a message so the pane stays non-silent.
fn build_queue_state(queue: CurrentUserQueue) -> QueueState {
    let mut rows = Vec::new();
    if let Some(current) = queue.currently_playing {
        rows.push(playable_to_row(current, true));
    }
    for item in queue.queue {
        rows.push(playable_to_row(item, false));
    }
    let message = rows.is_empty().then(|| "Queue is empty".to_string());
    QueueState {
        rows,
        message,
        failures: 0,
    }
}

/// Extract `(title, artists)` from a queue item, mirroring `snapshot_from_context`'s mapping so the
/// queue and Now Playing panes format the same track identically. Episodes show `(podcast)`;
/// items rspotify dropped to `Unknown` fall back to the raw-JSON extractor.
fn playable_to_row(item: PlayableItem, is_current: bool) -> QueueRow {
    let (title, artists) = match item {
        PlayableItem::Track(t) => {
            let names: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            (t.name, join_artists(&names))
        }
        PlayableItem::Episode(e) => (e.name, "(podcast)".to_string()),
        PlayableItem::Unknown(v) => {
            let (title, names, _album, _dur) = crate::np_json::track_from_json(&v);
            (title, join_artists(&names))
        }
    };
    QueueRow {
        title,
        artists,
        is_current,
    }
}

/// Draw the display-only queue pane: a dim-bordered block titled " Queue " with an "N up next" hint
/// row above the list. The currently-playing track leads with the play glyph and is bolded; upcoming
/// tracks are numbered from 1. When there is a message (empty/loading/failure) it is shown centered
/// instead of the list, so the pane is never silently blank.
pub(super) fn draw_queue_pane(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(dim) // Display-only pane: never focused, so always dim (matches the old placeholder).
        .title(" Queue ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // A message (loading / empty / failure) replaces the list. The `rows.is_empty()` fallback is a
    // defensive belt-and-suspenders guard for the type invariant so the pane can never read as a
    // silently blank box even if a future construction path forgets to set `message`.
    if let Some(message) = &app.queue.message {
        render_message(frame, inner, dim, message);
        return;
    }
    if app.queue.rows.is_empty() {
        render_message(frame, inner, dim, "Queue is empty");
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)]) // hint / list
        .split(inner);

    let upcoming = app.queue.rows.iter().filter(|r| !r.is_current).count();
    frame.render_widget(
        Paragraph::new(view::queue_hint(upcoming)).style(dim),
        rows[0],
    );

    // A running position (loop, not a side-effecting iterator) numbers the upcoming tracks 1..N while
    // the currently-playing row is left unnumbered (rendered with the play glyph by `queue_row`).
    let width = inner.width as usize;
    let mut position = 0;
    let mut items: Vec<ListItem> = Vec::with_capacity(app.queue.rows.len());
    for r in &app.queue.rows {
        let number = if r.is_current {
            None
        } else {
            position += 1;
            Some(position)
        };
        let text = view::queue_row(number, &r.title, &r.artists, width);
        let item = ListItem::new(text);
        // Display-only: no selection state / highlight symbol (unlike the detail pane).
        items.push(if r.is_current { item.style(bold) } else { item });
    }
    frame.render_widget(List::new(items), rows[1]);
}

fn render_message(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    style: Style,
    message: &str,
) {
    frame.render_widget(
        Paragraph::new(message.to_string())
            .alignment(Alignment::Center)
            .style(style),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upcoming(title: &str) -> QueueRow {
        QueueRow {
            title: title.to_string(),
            artists: "Artist".to_string(),
            is_current: false,
        }
    }

    #[test]
    fn build_queue_state_empty_reports_empty_message() {
        let state = build_queue_state(CurrentUserQueue {
            currently_playing: None,
            queue: Vec::new(),
        });
        assert!(state.rows.is_empty());
        assert_eq!(state.message.as_deref(), Some("Queue is empty"));
        assert_eq!(state.failures, 0);
    }

    #[test]
    fn transient_failure_keeps_last_good_rows() {
        let mut state = QueueState {
            rows: vec![upcoming("Song")],
            message: None,
            failures: 0,
        };
        degrade_on_failure(&mut state, "boom".to_string());
        // Below the threshold: the last good rows stay, no scary message yet (avoids 2s flicker).
        assert_eq!(state.rows.len(), 1);
        assert!(state.message.is_none());
        assert_eq!(state.failures, 1);
    }

    #[test]
    fn persistent_failure_escalates_to_visible_message() {
        let mut state = QueueState {
            rows: vec![upcoming("Song")],
            message: None,
            failures: 0,
        };
        for _ in 0..MAX_QUEUE_FAILURES {
            degrade_on_failure(&mut state, "boom".to_string());
        }
        // Past the threshold: stale rows dropped, failure surfaced (never silent).
        assert!(state.rows.is_empty());
        let message = state.message.expect("message set after threshold");
        assert!(message.contains("queue unavailable"));
        assert!(message.contains("boom"));
    }

    #[test]
    fn build_queue_state_always_starts_from_zero_failures() {
        // A successful poll fully replaces `app.queue`, so a freshly built state must start clean —
        // this is what resets the failure count after a recovery.
        let rebuilt = build_queue_state(CurrentUserQueue {
            currently_playing: None,
            queue: Vec::new(),
        });
        assert_eq!(rebuilt.failures, 0);
    }
}
