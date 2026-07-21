//! TUI display calculations (pure functions) and the Now Playing snapshot. Independent of
//! ratatui: progress interpolation and ratio computation take/return primitives so they are unit
//! testable (rendering is on the `mod.rs` side).

use std::time::Instant;

use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::format::{display_width, format_ms, truncate};
use crate::theme;
use crate::tui::browse::BrowseTab;

/// A snapshot of the playback status from the most recent poll. Using `fetched_at` as the base,
/// progress between polls is interpolated locally to look smooth.
pub struct NowPlaying {
    pub is_playing: bool,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub progress_ms: u128,
    pub duration_ms: u128,
    pub device: String,
    pub volume: Option<u8>,
    /// The current track's URI (`spotify:track:…`). Used for the save action and track-change detection.
    /// `None` for episodes or when track info is unknown.
    pub track_uri: Option<String>,
    /// The (already selected) cover-art image URL. Used for art fetching and track-change detection. `None` if absent.
    pub album_image_url: Option<String>,
    /// The time this snapshot was fetched (the base for progress interpolation).
    pub fetched_at: Instant,
}

/// If playing, return the progress advanced from the base `base_ms` by the elapsed time (capped at
/// `duration_ms`). While paused, stays at the base. When `duration_ms == 0` (unknown length), no cap.
pub fn interpolate_progress(
    base_ms: u128,
    elapsed_ms: u128,
    duration_ms: u128,
    playing: bool,
) -> u128 {
    if !playing {
        return base_ms;
    }
    let advanced = base_ms.saturating_add(elapsed_ms);
    if duration_ms == 0 {
        advanced
    } else {
        advanced.min(duration_ms)
    }
}

/// Return the progress ratio 0.0..=1.0. When `duration_ms == 0`, returns 0.0.
pub fn progress_ratio(progress_ms: u128, duration_ms: u128) -> f64 {
    if duration_ms == 0 {
        return 0.0;
    }
    (progress_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
}

/// The lines needed for rendering (primitive strings + progress ratio). Kept independent of
/// ratatui so the render logic is a pure function and unit testable (`mod.rs::draw` just feeds
/// these into widgets).
pub struct RenderLines {
    pub state: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub ratio: f64,
    pub progress_label: String,
    pub device: String,
}

/// A short marker for the current track's saved state (appended to the end of the state line).
/// `None` means the state is unknown and nothing is shown.
fn saved_marker(saved: Option<bool>) -> String {
    match saved {
        Some(true) => format!("   {} Saved", theme::HEART),
        Some(false) => format!("   {} Not saved", theme::HEART_O),
        None => String::new(),
    }
}

/// Build the Now Playing display lines. `elapsed_ms` is the time since the last fetch (the base for
/// progress interpolation), `width` is the wrap width per line, and `saved` is the current track's
/// library saved state (`None` if unknown). When nothing is playing (`None`), returns a hint message.
pub fn render_lines(
    now: Option<&NowPlaying>,
    elapsed_ms: u128,
    width: usize,
    saved: Option<bool>,
) -> RenderLines {
    let line = |label: &str, value: &str| -> String {
        format!(
            "{label}{}",
            truncate(value, width.saturating_sub(display_width(label)))
        )
    };

    let Some(n) = now else {
        return RenderLines {
            state: "Nothing is playing".to_string(),
            title: "  (press / to search, 2 to browse, d to select a device)".to_string(),
            artist: String::new(),
            album: String::new(),
            ratio: 0.0,
            progress_label: "-".to_string(),
            device: String::new(),
        };
    };

    let prog = interpolate_progress(n.progress_ms, elapsed_ms, n.duration_ms, n.is_playing);
    let head = if n.is_playing {
        format!("{} Playing", theme::PLAY)
    } else {
        format!("{} Paused", theme::PAUSE)
    };
    let vol = n
        .volume
        .map(|v| format!("{v}%"))
        .unwrap_or_else(|| "-".to_string());

    RenderLines {
        state: format!("{head}{}", saved_marker(saved)),
        title: line(&format!("{} ", theme::MUSIC), &n.title),
        artist: line(&format!("{} ", theme::ARTIST), &n.artists),
        album: n
            .album
            .as_deref()
            .map(|a| line(&format!("{} ", theme::ALBUM), a))
            .unwrap_or_default(),
        ratio: progress_ratio(prog, n.duration_ms),
        progress_label: format!("{} / {}", format_ms(prog), format_ms(n.duration_ms)),
        device: format!("{} {} (vol {vol})", theme::VOLUME, n.device),
    }
}

/// Pure function returning the target position (ms) after a seek. Adds `delta_ms` (negative to
/// rewind) to `current_ms` and clamps to `[0, duration_ms]`. When `duration_ms == 0` (unknown
/// length), there is no upper bound.
pub fn seek_target(current_ms: u128, duration_ms: u128, delta_ms: i64) -> u128 {
    let target = if delta_ms >= 0 {
        current_ms.saturating_add(delta_ms as u128)
    } else {
        current_ms.saturating_sub(delta_ms.unsigned_abs() as u128)
    };
    if duration_ms == 0 {
        target
    } else {
        target.min(duration_ms)
    }
}

/// The supplementary line of the search overlay (the default hint when there is no `message`).
/// Varies by phase and result count.
pub fn search_hint(is_input: bool, results_len: usize) -> String {
    if is_input {
        "Enter to search / Esc to go back".to_string()
    } else {
        format!("{results_len} results — ↑↓ select / Enter play / Esc edit query")
    }
}

/// Format one search-result row (`name — artists`, truncated to the width). Selection highlighting
/// is done by the caller. Truncates at a width reduced by the 2 columns of the selection marker `"▶ "`.
pub fn search_row(name: &str, artists: &str, width: usize) -> String {
    let text = if artists.is_empty() {
        name.to_string()
    } else {
        format!("{name} — {artists}")
    };
    truncate(&text, width.saturating_sub(2))
}

/// The library pane tab header, e.g. `[All] Artists  Albums  Playlists  Tracks`. The current tab is
/// wrapped in brackets so it reads as selected even without color. Pure and testable.
pub fn library_tab_header(current: BrowseTab) -> String {
    BrowseTab::ALL
        .iter()
        .map(|t| {
            if *t == current {
                format!("[{}]", t.label())
            } else {
                format!(" {} ", t.label())
            }
        })
        .collect()
}

/// The library pane's supplementary line (the default hint when there is no `message`). `count` is
/// the number of playable items (headers excluded).
pub fn library_hint(count: usize) -> String {
    format!("{count} items — ↑↓ select / [ ] tab / Enter play")
}

/// The detail pane's supplementary line (the default hint when there is no `message`). `count` is the
/// number of tracks shown.
pub fn detail_hint(count: usize) -> String {
    format!("{count} tracks — ↑↓ select / Enter play")
}

/// Turn a failed detail fetch into a concise, non-alarming pane message. `what` is the content label
/// ("artist top tracks", "playlist tracks", …) and `status` is the HTTP status when the failure was an
/// HTTP response. `403` is the common case for content Spotify's Web API restricts for this app's
/// access tier (artist top-tracks and other users' / editorial playlists have been restricted since
/// late 2024) — surface it as an expected limitation, not a scary error, while still never staying
/// silent. Pure (primitives in, `String` out) so the mapping is unit-tested without HTTP models.
pub fn detail_error_message(status: Option<u16>, what: &str) -> String {
    match status {
        Some(403) => format!("{what} unavailable — restricted by Spotify Web API (403)"),
        Some(404) => format!("{what} not found (404)"),
        Some(code) => format!("failed to load {what} (HTTP {code})"),
        None => format!("failed to load {what}"),
    }
}

/// Format one detail-pane track row: `{▶ }{no} {title} — {artists}` on the left with the duration
/// right-aligned to the pane width. The currently-playing track is prefixed with the play glyph
/// (distinct from the list's `▶ ` selection marker). Truncates at a width reduced by the 2 columns of
/// the selection marker, and reserves room for the right-aligned duration so it is never clipped.
/// Pure (primitives in, `String` out) so it is unit-tested without building API models.
pub fn detail_row(
    track_no: Option<u32>,
    title: &str,
    artists: &str,
    duration_ms: u128,
    is_current: bool,
    width: usize,
) -> String {
    let marker = if is_current {
        format!("{} ", theme::PLAY)
    } else {
        String::new()
    };
    let no = track_no.map(|n| format!("{n} ")).unwrap_or_default();
    let body = if artists.is_empty() {
        format!("{marker}{no}{title}")
    } else {
        format!("{marker}{no}{title} — {artists}")
    };
    let dur = format_ms(duration_ms);
    // Reserve the 2 columns the list highlight symbol occupies, then keep the duration (plus a gap)
    // pinned to the right by truncating the body and padding the middle.
    let avail = width.saturating_sub(2);
    let dur_w = display_width(&dur);
    let body_max = avail.saturating_sub(dur_w + 1);
    let body = truncate(&body, body_max);
    let pad = avail.saturating_sub(display_width(&body) + dur_w);
    format!("{body}{}{dur}", " ".repeat(pad))
}

/// Pure function that formats one device row for the device picker.
/// Active is shown with `● (active)`, inactive with `○`, and restricted is annotated.
/// Truncates at a width reduced by the 2 columns of the selection marker `"▶ "`.
pub fn device_row(
    name: &str,
    type_label: &str,
    vol: Option<u32>,
    is_active: bool,
    is_restricted: bool,
    width: usize,
) -> String {
    let mark = if is_active { "●" } else { "○" };
    let vol_s = match vol {
        Some(v) => format!("vol {v}%"),
        None => "vol -".to_string(),
    };
    let mut text = format!("{mark} {name} [{type_label}]  {vol_s}");
    if is_active {
        text.push_str("  (active)");
    }
    if is_restricted {
        text.push_str(" (restricted)");
    }
    truncate(&text, width.saturating_sub(2))
}

/// The list of key bindings (key, description). The single source of truth for the footer and the
/// help overlay (both reference this to prevent notation drift).
pub fn help_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("space", "play / pause"),
        ("n / p", "next / previous track"),
        ("← / →", "seek 5s (back / forward)"),
        ("+ / -", "volume ±5"),
        ("s", "save / unsave the current track"),
        ("/", "search and play"),
        ("tab", "focus panel (library / detail)"),
        ("[ / ]", "library: previous / next tab"),
        ("↑ / ↓", "library / detail: move selection"),
        ("enter", "library / detail: play selection"),
        ("d", "select device"),
        ("r", "refresh (playback / focused library tab or detail)"),
        ("?", "this help"),
        ("q / Esc", "quit"),
        ("Ctrl-C", "quit (from any screen)"),
    ]
}

/// The kind of a status line. Used to decide coloring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatusKind {
    Warn,
    Ok,
    Info,
}

/// Pure function that classifies a status string by kind. Starting with the warning glyph is a
/// warning, starting with a success glyph is Ok, and anything else is Info (startup, hints, etc.).
/// The glyphs come from [`theme`] so the classifier stays in sync with the strings the app emits.
pub fn status_kind(s: &str) -> StatusKind {
    let trimmed = s.trim_start();
    if trimmed.starts_with(theme::WARN) {
        StatusKind::Warn
    } else if theme::OK_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
        StatusKind::Ok
    } else {
        StatusKind::Info
    }
}

// ---- Dashboard layout (pure region splitting) -------------------------------

/// Below this inner width the dashboard collapses to a single column (visualizer and detail are
/// dropped) so the remaining panes stay legible on narrow terminals. The boundary is inclusive:
/// a width of exactly this many columns still shows two columns.
const MIN_TWO_COL_WIDTH: u16 = 60;
/// Below this inner height the footer row is dropped first (its keys also live in the help overlay).
/// The Now Playing pane needs ~5 text rows (state / title / artist / album / device) but only gets
/// ~45% of the body, so the footer is only worth showing once the body is comfortably tall.
const MIN_FOOTER_HEIGHT: u16 = 12;
/// Below this inner height the visualizer pane is dropped (checked after the footer, i.e. 12 → 10),
/// so degradation removes the footer first and the visualizer second.
const MIN_VISUALIZER_HEIGHT: u16 = 10;

/// Which lower dashboard pane currently holds keyboard focus. Only the lower two panes (library and
/// detail) are navigable; the upper Now Playing / Visualizer panes are display-only, so they are not
/// part of the focus cycle. Kept a small, exhaustively-matched enum so adding a future focus target
/// surfaces missing branches as compile errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The lower-left library pane.
    Library,
    /// The lower-right detail pane.
    Detail,
}

impl Focus {
    /// The focus after a `tab` press, given whether the detail pane is currently shown. With the
    /// detail pane hidden (narrow terminal) there is only one navigable pane, so focus stays on the
    /// library — the stored focus is clamped *at the moment of navigation*, not just at render time.
    /// This keeps `App.focus` always consistent with what is on screen, so widening the terminal
    /// later never makes focus "jump" to a pane the user did not deliberately move to.
    pub fn next(self, detail_visible: bool) -> Focus {
        if !detail_visible {
            return Focus::Library;
        }
        match self {
            Focus::Library => Focus::Detail,
            Focus::Detail => Focus::Library,
        }
    }

    /// The focus that should actually be rendered, given whether the detail pane is currently shown.
    /// A render-time safety belt: even though `next` already clamps on navigation, the terminal can
    /// be resized between a key press and the next draw, so clamp here too. On a narrow terminal the
    /// detail pane is hidden, so focus can never rest on a pane the user cannot see — it clamps back
    /// to the library instead of silently highlighting nothing.
    pub fn effective(self, detail_visible: bool) -> Focus {
        if detail_visible { self } else { Focus::Library }
    }
}

/// The dashboard regions carved out of the inner area (inside the outer border). Optional regions
/// are `None` when the terminal is too small (or, for `search_bar`, when search is inactive) so the
/// caller simply skips drawing them.
pub struct DashboardAreas {
    /// Upper-left Now Playing pane (always present).
    pub now_playing: Rect,
    /// Upper-right visualizer pane (`None` on narrow or short terminals).
    pub visualizer: Option<Rect>,
    /// The search input row (`Some` only while search is active). Populated by the pure splitter and
    /// asserted by unit tests; the Phase 1 `draw` calls with search inactive and does not render it
    /// yet (search still uses its own overlay view), so it is not read from the binary target.
    #[allow(dead_code)]
    pub search_bar: Option<Rect>,
    /// Lower-left library pane (always present).
    pub library: Rect,
    /// Lower-right detail pane (`None` on narrow terminals).
    pub detail: Option<Rect>,
    /// The status line. Its height is reserved arithmetically before any other row, so it is the
    /// last thing to disappear as the terminal shrinks (it is the only non-silent output). It is
    /// `>= 1` row whenever `inner.height >= 1`.
    pub status: Rect,
    /// The playback bar row. Reserved right after `status`, so it is `>= 1` row whenever
    /// `inner.height >= 2`.
    pub playbar: Rect,
    /// The footer/key-hint row (`None` on short terminals).
    pub footer: Option<Rect>,
}

/// Split the inner area (inside the outer border) into dashboard regions. Pure and
/// terminal-independent (works on hand-built `Rect`s), so it is unit-testable without a backend.
///
/// The mandatory bottom rows are reserved with plain arithmetic (not the layout solver, which can
/// collapse rows to height 0 on short terminals). Highest priority first: `status`, then `playbar`,
/// then `footer` (only when tall enough); whatever remains becomes the body. Pseudocode:
///
/// ```text
/// remaining = inner.height
/// status_h  = min(remaining, 1); remaining -= status_h   // survives down to height 1
/// playbar_h = min(remaining, 1); remaining -= playbar_h   // survives down to height 2
/// footer_h  = show_footer ? min(remaining, 1) : 0; remaining -= footer_h
/// body_h    = remaining                                   // upper + search bar + lower
/// ```
///
/// The body is then split with the layout solver: vertically into `upper / (search_bar) / lower`
/// (search bar only when `search_active`), and each of those horizontally into
/// `now_playing / visualizer` and `library / detail` (single column when narrow/short).
pub fn dashboard_areas(inner: Rect, search_active: bool) -> DashboardAreas {
    let two_col = inner.width >= MIN_TWO_COL_WIDTH;
    let show_footer = inner.height >= MIN_FOOTER_HEIGHT;
    let show_visualizer = two_col && inner.height >= MIN_VISUALIZER_HEIGHT;

    // 1. Reserve the mandatory bottom rows arithmetically, status first (see the doc comment). This
    //    guarantees status/playbar never collapse to height 0 while the terminal can still show them,
    //    which keeps the status line — the only non-silent output — visible.
    let mut remaining = inner.height;
    let status_h = remaining.min(1);
    remaining -= status_h;
    let playbar_h = remaining.min(1);
    remaining -= playbar_h;
    let footer_h = if show_footer { remaining.min(1) } else { 0 };
    remaining -= footer_h;
    let body_h = remaining;

    // Stack the rows from the top: body, then status, playbar, footer along the bottom.
    let x = inner.x;
    let w = inner.width;
    let mut y = inner.y;
    let body = Rect::new(x, y, w, body_h);
    y += body_h;
    let status = Rect::new(x, y, w, status_h);
    y += status_h;
    let playbar = Rect::new(x, y, w, playbar_h);
    y += playbar_h;
    let footer = if show_footer {
        Some(Rect::new(x, y, w, footer_h))
    } else {
        None
    };

    // 2. Split the body into upper / (search_bar) / lower.
    let (upper, search_bar, lower) = if search_active {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(45),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(body);
        (rows[0], Some(rows[1]), rows[2])
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(body);
        (rows[0], None, rows[1])
    };

    // 3. Split the upper row into now_playing / visualizer.
    let (now_playing, visualizer) = if show_visualizer {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(upper);
        (cols[0], Some(cols[1]))
    } else {
        (upper, None)
    };

    // 4. Split the lower row into library / detail.
    let (library, detail) = if two_col {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(41), Constraint::Percentage(59)])
            .split(lower);
        (cols[0], Some(cols[1]))
    } else {
        (lower, None)
    };

    DashboardAreas {
        now_playing,
        visualizer,
        search_bar,
        library,
        detail,
        status,
        playbar,
        footer,
    }
}

/// Cover-art columns are square; terminal cells are about twice as tall as wide, so a column of
/// `height * 2` columns renders roughly square.
const ART_COL_ASPECT: u16 = 2;
/// Never let the cover-art column exceed this many columns (keep room for the text).
const ART_COL_MAX: u16 = 24;
/// Below this many columns an art column is not worth showing; fall back to full-width text.
const ART_COL_MIN: u16 = 4;

/// The width (in columns) of the cover-art column inside the Now Playing pane, or `0` when no
/// column is shown. Pure so the caller can subtract it from the pane width *before* building the
/// text lines, keeping the truncation width in sync with the actual text rectangle. `want_art` is
/// false when nothing is playing (no art to show). The column is square-ish (`pane_h * 2`), capped
/// at half the pane width and [`ART_COL_MAX`]; anything under [`ART_COL_MIN`] collapses to `0`.
pub fn art_col_width(pane_w: u16, pane_h: u16, want_art: bool) -> u16 {
    if !want_art {
        return 0;
    }
    let cols = pane_h
        .saturating_mul(ART_COL_ASPECT)
        .min(pane_w / 2)
        .min(ART_COL_MAX);
    if cols >= ART_COL_MIN { cols } else { 0 }
}

/// Allocate up to `count` single-row `Rect`s stacked from the top of `area`, dropping any that do
/// not fit (so the caller can lay out priority-ordered rows without the layout solver collapsing an
/// arbitrary one to height 0). Each returned rect is exactly one row tall and inside `area`. Reused
/// for the Now Playing text rows (state / title / artist / album / device, highest priority first).
pub fn stack_rows(area: Rect, count: usize) -> Vec<Rect> {
    (0..count)
        .map(|i| area.y.saturating_add(i as u16))
        .take_while(|&y| y < area.bottom())
        .map(|y| Rect::new(area.x, y, area.width, 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// True when `r` sits entirely inside `outer` (no boundary overflow).
    fn within(outer: Rect, r: Rect) -> bool {
        r.x >= outer.x
            && r.y >= outer.y
            && r.right() <= outer.right()
            && r.bottom() <= outer.bottom()
    }

    #[test]
    fn dashboard_normal_has_four_panes_and_stacked_bottom_rows() {
        // Arrange
        let inner = Rect::new(0, 0, 100, 30);

        // Act
        let a = dashboard_areas(inner, false);

        // Assert: all optional panes present, no search bar.
        let vis = a.visualizer.expect("visualizer present when wide/tall");
        let detail = a.detail.expect("detail present when wide");
        let footer = a.footer.expect("footer present when tall");
        assert!(a.search_bar.is_none(), "no search bar when inactive");

        // Upper panes are left/right neighbors on the same row.
        assert_eq!(vis.x, a.now_playing.x + a.now_playing.width);
        assert_eq!(vis.y, a.now_playing.y);
        // Lower panes are left/right neighbors.
        assert_eq!(detail.x, a.library.x + a.library.width);
        assert_eq!(detail.y, a.library.y);
        // Upper sits above lower.
        assert!(a.now_playing.y < a.library.y);

        // Bottom rows are each one line tall and stacked status → playbar → footer.
        assert_eq!(a.status.height, 1);
        assert_eq!(a.playbar.height, 1);
        assert_eq!(footer.height, 1);
        assert_eq!(a.playbar.y, a.status.y + 1);
        assert_eq!(footer.y, a.playbar.y + 1);
        // Footer is the bottom-most row.
        assert_eq!(footer.bottom(), inner.bottom());
    }

    #[test]
    fn dashboard_narrow_collapses_to_single_column() {
        // Arrange
        let inner = Rect::new(0, 0, 50, 30);

        // Act
        let a = dashboard_areas(inner, false);

        // Assert
        assert!(a.visualizer.is_none(), "no visualizer when narrow");
        assert!(a.detail.is_none(), "no detail when narrow");
        assert_eq!(a.now_playing.width, inner.width);
        assert_eq!(a.library.width, inner.width);
    }

    #[test]
    fn dashboard_short_drops_footer_before_visualizer() {
        // Height 11: footer gone (< 12) but visualizer kept (>= 10).
        let a = dashboard_areas(Rect::new(0, 0, 100, 11), false);
        assert!(a.footer.is_none(), "footer dropped below 12 rows");
        assert!(a.visualizer.is_some(), "visualizer kept at 11 rows");

        // Height 9: both footer and visualizer gone (< 10).
        let b = dashboard_areas(Rect::new(0, 0, 100, 9), false);
        assert!(b.footer.is_none());
        assert!(b.visualizer.is_none(), "visualizer dropped below 10 rows");
    }

    #[test]
    fn dashboard_search_inserts_bar_between_upper_and_lower() {
        // Arrange
        let inner = Rect::new(0, 0, 100, 30);

        // Act
        let a = dashboard_areas(inner, true);

        // Assert
        let bar = a.search_bar.expect("search bar present when active");
        assert_eq!(bar.height, 1);
        assert!(a.now_playing.y < bar.y);
        assert!(bar.y < a.library.y);
    }

    #[test]
    fn dashboard_regions_stay_within_inner() {
        let inner = Rect::new(3, 2, 100, 30);
        let a = dashboard_areas(inner, true);
        for r in [
            Some(a.now_playing),
            a.visualizer,
            a.search_bar,
            Some(a.library),
            a.detail,
            Some(a.status),
            Some(a.playbar),
            a.footer,
        ]
        .into_iter()
        .flatten()
        {
            assert!(within(inner, r), "region {r:?} escapes inner {inner:?}");
        }
    }

    #[test]
    fn dashboard_guarantees_status_then_playbar_on_tiny_heights() {
        // Height 0 is degenerate: nothing can be drawn, so every row collapses to 0.
        let a0 = dashboard_areas(Rect::new(0, 0, 100, 0), false);
        assert_eq!(a0.status.height, 0, "no rows fit at height 0");
        assert_eq!(a0.playbar.height, 0);

        // From height 1 up, status must always survive (the only non-silent output); from height 2
        // up, the playbar must too. Neither may overflow the inner area.
        for h in 1..=4u16 {
            let a = dashboard_areas(Rect::new(0, 0, 100, h), false);
            assert!(a.status.height >= 1, "status must stay >= 1 at height {h}");
            assert!(a.status.bottom() <= h, "status overflows at height {h}");
            if h >= 2 {
                assert!(
                    a.playbar.height >= 1,
                    "playbar must stay >= 1 at height {h}"
                );
                assert!(a.playbar.bottom() <= h, "playbar overflows at height {h}");
            }
        }
    }

    #[test]
    fn dashboard_thresholds_are_inclusive_boundaries() {
        // Width exactly at the two-column boundary keeps both columns; one below collapses.
        let wide = dashboard_areas(Rect::new(0, 0, MIN_TWO_COL_WIDTH, 30), false);
        assert!(wide.visualizer.is_some() && wide.detail.is_some());
        let narrow = dashboard_areas(Rect::new(0, 0, MIN_TWO_COL_WIDTH - 1, 30), false);
        assert!(narrow.visualizer.is_none() && narrow.detail.is_none());

        // Height exactly at the footer threshold keeps the footer; one below drops it.
        let with_footer = dashboard_areas(Rect::new(0, 0, 100, MIN_FOOTER_HEIGHT), false);
        assert!(with_footer.footer.is_some());
        let no_footer = dashboard_areas(Rect::new(0, 0, 100, MIN_FOOTER_HEIGHT - 1), false);
        assert!(no_footer.footer.is_none());

        // Height exactly at the visualizer threshold keeps it; one below drops it.
        let with_vis = dashboard_areas(Rect::new(0, 0, 100, MIN_VISUALIZER_HEIGHT), false);
        assert!(with_vis.visualizer.is_some());
        let no_vis = dashboard_areas(Rect::new(0, 0, 100, MIN_VISUALIZER_HEIGHT - 1), false);
        assert!(no_vis.visualizer.is_none());
    }

    #[test]
    fn art_col_width_columnizes_only_when_wide_enough() {
        // No art wanted (nothing playing) → no column.
        assert_eq!(art_col_width(100, 20, false), 0);
        // Wide and tall pane → capped at ART_COL_MAX (24) columns.
        assert_eq!(art_col_width(100, 50, true), 24);
        // Aspect-driven width (height*2) below the cap, bounded by half the pane width.
        assert_eq!(art_col_width(40, 3, true), 6);
        // Too narrow to host a >= ART_COL_MIN column → full-width text (0).
        assert_eq!(art_col_width(6, 10, true), 0);
    }

    #[test]
    fn art_col_width_lets_text_width_stay_positive() {
        // Regression guard: the text width the caller derives (pane_w - art) must stay > 0 so
        // render_lines truncates against the real text rectangle, not the whole pane.
        let pane_w = 100u16;
        let art = art_col_width(pane_w, 30, true);
        assert!(
            art > 0 && art < pane_w,
            "art column must leave room for text"
        );
        assert!(pane_w - art > 0);
    }

    #[test]
    fn stack_rows_allocates_from_top_and_drops_overflow() {
        // Enough height: all rows fit, each one line tall, stacked top-down.
        let full = stack_rows(Rect::new(0, 0, 10, 5), 5);
        assert_eq!(full.len(), 5);
        assert!(
            full.iter()
                .enumerate()
                .all(|(i, r)| r.y == i as u16 && r.height == 1)
        );
        // Short area: only the top rows fit; lower (lower-priority) ones are dropped.
        assert_eq!(stack_rows(Rect::new(0, 0, 10, 3), 5).len(), 3);
        // Zero height: nothing fits.
        assert!(stack_rows(Rect::new(2, 1, 10, 0), 5).is_empty());
        // Respects the area offset.
        let offset = stack_rows(Rect::new(3, 7, 10, 2), 5);
        assert_eq!(offset.len(), 2);
        assert_eq!((offset[0].x, offset[0].y), (3, 7));
        assert_eq!(offset[1].y, 8);
    }

    #[test]
    fn now_playing_rows_keep_top_priority_at_footer_boundary() {
        // At the footer-threshold height the Now Playing pane is short, but its top rows
        // (state / title / artist) must survive; only device/album may drop.
        let a = dashboard_areas(Rect::new(0, 0, 100, MIN_FOOTER_HEIGHT), false);
        let rows = stack_rows(a.now_playing, 5);
        assert_eq!(
            rows.len(),
            4,
            "state/title/artist/album survive, only device drops"
        );
        assert!(rows.iter().all(|r| r.height == 1), "no row is crushed to 0");
        for pair in rows.windows(2) {
            assert_eq!(pair[1].y, pair[0].y + 1, "rows stack contiguously");
        }
        // Rows stay inside the pane.
        assert!(rows.iter().all(|r| within(a.now_playing, *r)));
    }

    #[test]
    fn now_playing_rows_survive_tiny_pane_heights() {
        // Sweep pane heights 1..=6: allocated row count == min(height, 5) and none is crushed.
        for h in 1..=6u16 {
            let pane = Rect::new(0, 0, 40, h);
            let rows = stack_rows(pane, 5);
            assert_eq!(rows.len() as u16, h.min(5), "row count at height {h}");
            assert!(rows.iter().all(|r| r.height == 1), "no crush at height {h}");
        }
    }

    #[test]
    fn interpolate_advances_only_while_playing() {
        // While playing, advances by the elapsed time
        assert_eq!(interpolate_progress(10_000, 3_000, 200_000, true), 13_000);
        // While paused, stays at the base
        assert_eq!(interpolate_progress(10_000, 3_000, 200_000, false), 10_000);
    }

    #[test]
    fn interpolate_clamps_to_duration() {
        // Does not exceed the length
        assert_eq!(
            interpolate_progress(195_000, 10_000, 200_000, true),
            200_000
        );
        // Unknown length (0) is not capped
        assert_eq!(interpolate_progress(195_000, 10_000, 0, true), 205_000);
    }

    #[test]
    fn progress_ratio_bounds() {
        assert_eq!(progress_ratio(0, 200_000), 0.0);
        assert_eq!(progress_ratio(100_000, 200_000), 0.5);
        assert_eq!(progress_ratio(200_000, 200_000), 1.0);
        // Length 0 is 0.0 (avoids division by zero)
        assert_eq!(progress_ratio(50_000, 0), 0.0);
        // Even if progress > duration, clamp to 1.0
        assert_eq!(progress_ratio(250_000, 200_000), 1.0);
    }

    fn sample(is_playing: bool) -> NowPlaying {
        NowPlaying {
            is_playing,
            title: "Song".to_string(),
            artists: "Artist".to_string(),
            album: Some("Album".to_string()),
            progress_ms: 60_000,
            duration_ms: 180_000,
            device: "MacBook Pro".to_string(),
            volume: Some(40),
            track_uri: Some("spotify:track:xxxx".to_string()),
            album_image_url: None,
            fetched_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn render_lines_shows_track_and_progress() {
        let n = sample(true);
        let out = render_lines(Some(&n), 0, 80, None);
        assert_eq!(out.state, format!("{} Playing", theme::PLAY));
        assert!(out.title.contains("Song"));
        assert!(out.artist.contains("Artist"));
        assert_eq!(out.progress_label, "1:00 / 3:00");
        assert_eq!(out.ratio, 60_000.0 / 180_000.0);
        assert!(out.device.contains("MacBook Pro"));
        assert!(out.device.contains("40%"));
    }

    #[test]
    fn render_lines_prefixes_info_lines_with_icons() {
        let n = sample(true);
        let out = render_lines(Some(&n), 0, 80, None);
        assert!(
            out.title.starts_with(theme::MUSIC),
            "title needs music icon"
        );
        assert!(
            out.artist.starts_with(theme::ARTIST),
            "artist needs artist icon"
        );
        assert!(
            out.album.starts_with(theme::ALBUM),
            "album needs album icon"
        );
        assert!(
            out.device.starts_with(theme::VOLUME),
            "device needs volume icon"
        );
    }

    #[test]
    fn render_lines_empty_state_when_nothing_playing() {
        let out = render_lines(None, 0, 80, None);
        assert_eq!(out.state, "Nothing is playing");
        assert!(out.artist.is_empty());
        assert_eq!(out.ratio, 0.0);
        // The hint must guide with real TUI keys, not a removed CLI command (issue #27).
        assert!(
            !out.title.contains("spotterm"),
            "must not reference a removed CLI command"
        );
        assert!(
            out.title.contains("/ to search") && out.title.contains("d to select"),
            "should point to real TUI keys (search / device)"
        );
    }

    #[test]
    fn render_lines_shows_saved_marker() {
        let n = sample(true);
        // Saved shows the filled heart, not-saved the empty heart, unknown shows neither.
        assert!(
            render_lines(Some(&n), 0, 80, Some(true))
                .state
                .contains(theme::HEART)
        );
        assert!(
            render_lines(Some(&n), 0, 80, Some(false))
                .state
                .contains(theme::HEART_O)
        );
        let unknown = render_lines(Some(&n), 0, 80, None).state;
        assert!(!unknown.contains(theme::HEART) && !unknown.contains(theme::HEART_O));
    }

    #[test]
    fn seek_target_advances_and_rewinds() {
        // Forward
        assert_eq!(seek_target(60_000, 180_000, 5_000), 65_000);
        // Backward
        assert_eq!(seek_target(60_000, 180_000, -5_000), 55_000);
    }

    #[test]
    fn seek_target_clamps_bounds() {
        // Lower bound 0 (rewound too far)
        assert_eq!(seek_target(3_000, 180_000, -5_000), 0);
        // Upper bound duration (advanced too far)
        assert_eq!(seek_target(178_000, 180_000, 5_000), 180_000);
        // Unknown length (0) has no upper bound
        assert_eq!(seek_target(178_000, 0, 5_000), 183_000);
    }

    #[test]
    fn search_row_joins_name_and_artists() {
        assert_eq!(search_row("Song", "Artist", 80), "Song — Artist");
        // With no artist, just the track name
        assert_eq!(search_row("Song", "", 80), "Song");
    }

    #[test]
    fn search_row_truncates_with_symbol_margin() {
        // width 10 → truncate to 8 chars after subtracting the 2 columns of the marker (ends with …)
        let out = search_row("abcdefghij", "", 10);
        assert_eq!(out.chars().count(), 8);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn search_hint_varies_by_phase() {
        assert!(search_hint(true, 0).contains("Enter to search"));
        let results = search_hint(false, 3);
        assert!(results.starts_with("3 results"));
        assert!(results.contains("Enter play"));
    }

    #[test]
    fn library_tab_header_brackets_current_only() {
        let out = library_tab_header(BrowseTab::All);
        assert!(out.contains("[All]"));
        assert!(out.contains(" Artists "));
        assert!(!out.contains("[Artists]"));
        // Every tab label appears exactly once.
        for tab in BrowseTab::ALL {
            assert!(out.contains(tab.label()));
        }
    }

    #[test]
    fn library_hint_reports_item_count() {
        assert!(library_hint(7).starts_with("7 items"));
        assert!(library_hint(7).contains("[ ] tab"));
    }

    #[test]
    fn detail_row_right_aligns_duration_and_numbers() {
        let out = detail_row(Some(4), "Weird Fishes", "Radiohead", 318_000, false, 40);
        assert!(out.contains("4 Weird Fishes — Radiohead"));
        assert!(out.trim_end().ends_with("5:18"));
        // Fits within the pane width minus the 2-column selection marker.
        assert!(crate::format::display_width(&out) <= 38);
    }

    #[test]
    fn detail_row_marks_current_track_with_play_glyph() {
        let out = detail_row(Some(1), "15 Step", "Radiohead", 237_000, true, 40);
        assert!(out.starts_with(theme::PLAY));
    }

    #[test]
    fn detail_row_without_track_number_omits_it() {
        let out = detail_row(None, "Song", "", 60_000, false, 30);
        assert!(out.starts_with("Song"));
        assert!(out.trim_end().ends_with("1:00"));
    }

    #[test]
    fn detail_hint_reports_track_count() {
        assert!(detail_hint(12).starts_with("12 tracks"));
        assert!(detail_hint(12).contains("Enter play"));
    }

    #[test]
    fn detail_error_403_reads_as_restricted_not_failure() {
        let msg = detail_error_message(Some(403), "artist top tracks");
        assert!(msg.contains("artist top tracks"));
        assert!(msg.contains("restricted"));
        assert!(msg.contains("403"));
        // Framed as a limitation, not a scary failure.
        assert!(!msg.contains("failed"));
    }

    #[test]
    fn detail_error_404_reports_not_found() {
        assert_eq!(
            detail_error_message(Some(404), "playlist tracks"),
            "playlist tracks not found (404)"
        );
    }

    #[test]
    fn detail_error_other_status_shows_code() {
        assert_eq!(
            detail_error_message(Some(500), "album tracks"),
            "failed to load album tracks (HTTP 500)"
        );
    }

    #[test]
    fn detail_error_no_status_is_generic() {
        assert_eq!(detail_error_message(None, "track"), "failed to load track");
    }

    #[test]
    fn device_row_active_marks_and_notes() {
        let out = device_row("MacBook Pro", "Computer", Some(65), true, false, 80);
        assert!(out.starts_with("● MacBook Pro [Computer]"));
        assert!(out.contains("vol 65%"));
        assert!(out.contains("(active)"));
    }

    #[test]
    fn device_row_inactive_without_volume() {
        let out = device_row("Speaker", "Speaker", None, false, false, 80);
        assert!(out.starts_with("○ Speaker [Speaker]"));
        assert!(out.contains("vol -"));
        assert!(!out.contains("(active)"));
    }

    #[test]
    fn device_row_restricted_is_annotated() {
        let out = device_row("TV", "Tv", Some(40), false, true, 80);
        assert!(out.starts_with("○ TV [Tv]"));
        assert!(out.contains("(restricted)"));
    }

    #[test]
    fn device_row_truncates_with_symbol_margin() {
        // width 10 → truncate to 8 columns after subtracting the 2 columns of the marker
        let out = device_row("abcdefghij", "X", None, false, false, 10);
        assert!(crate::format::display_width(&out) <= 8);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn focus_next_swaps_lower_panes_when_detail_visible() {
        assert_eq!(Focus::Library.next(true), Focus::Detail);
        assert_eq!(Focus::Detail.next(true), Focus::Library);
    }

    #[test]
    fn focus_next_stays_on_library_when_detail_hidden() {
        // Narrow terminal: only one navigable pane, so tab clamps focus at navigation time (no
        // hidden drift that would surface as a focus "jump" after the terminal is widened again).
        assert_eq!(Focus::Library.next(false), Focus::Library);
        assert_eq!(Focus::Detail.next(false), Focus::Library);
    }

    #[test]
    fn focus_effective_clamps_to_library_when_detail_hidden() {
        // A narrow terminal hides the detail pane, so focus must never rest on it.
        assert_eq!(Focus::Detail.effective(false), Focus::Library);
        assert_eq!(Focus::Library.effective(false), Focus::Library);
    }

    #[test]
    fn focus_effective_keeps_stored_when_detail_visible() {
        assert_eq!(Focus::Detail.effective(true), Focus::Detail);
        assert_eq!(Focus::Library.effective(true), Focus::Library);
    }

    #[test]
    fn help_entries_cover_all_keys() {
        let keys: Vec<&str> = help_entries().iter().map(|(k, _)| *k).collect();
        for k in [
            "space",
            "n / p",
            "← / →",
            "+ / -",
            "s",
            "/",
            "tab",
            "[ / ]",
            "↑ / ↓",
            "enter",
            "d",
            "r",
            "?",
            "q / Esc",
        ] {
            assert!(keys.contains(&k), "help is missing {k}");
        }
        // Descriptions are non-empty
        assert!(help_entries().iter().all(|(_, desc)| !desc.is_empty()));
    }

    #[test]
    fn status_kind_classifies() {
        assert_eq!(
            status_kind(&format!("{} refresh failed: x", theme::WARN)),
            StatusKind::Warn
        );
        assert_eq!(
            status_kind(&format!("{} play", theme::PLAY)),
            StatusKind::Ok
        );
        assert_eq!(
            status_kind(&format!("{} saved to your library", theme::HEART)),
            StatusKind::Ok
        );
        assert_eq!(
            status_kind(&format!("{} seek 1:23", theme::SEEK)),
            StatusKind::Ok
        );
        assert_eq!(status_kind("starting…"), StatusKind::Info);
    }

    #[test]
    fn status_kind_recognizes_every_theme_prefix() {
        // Guards the theme<->classifier contract: every glyph the app prefixes an "ok" status with
        // (declared in theme::OK_PREFIXES) must classify as Ok, and the warning glyph as Warn. This
        // catches drift where a status line is emitted with a glyph the classifier no longer matches.
        for icon in theme::OK_PREFIXES {
            assert_eq!(
                status_kind(&format!("{icon} action done")),
                StatusKind::Ok,
                "ok prefix {icon:?} must classify as Ok"
            );
        }
        assert_eq!(
            status_kind(&format!("{} boom", theme::WARN)),
            StatusKind::Warn
        );
    }
}
