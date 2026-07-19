//! TUI display calculations (pure functions) and the Now Playing snapshot. Independent of
//! ratatui: progress interpolation and ratio computation take/return primitives so they are unit
//! testable (rendering is on the `mod.rs` side).

use std::time::Instant;

use crate::format::{display_width, format_ms, truncate};

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
fn saved_marker(saved: Option<bool>) -> &'static str {
    match saved {
        Some(true) => "   ♥ Saved",
        Some(false) => "   ♡ Not saved",
        None => "",
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
            title: "  (press p to resume / `spoterm play` to start)".to_string(),
            artist: String::new(),
            album: String::new(),
            ratio: 0.0,
            progress_label: "-".to_string(),
            device: String::new(),
        };
    };

    let prog = interpolate_progress(n.progress_ms, elapsed_ms, n.duration_ms, n.is_playing);
    let head = if n.is_playing {
        "▶ Playing"
    } else {
        "⏸ Paused"
    };
    let vol = n
        .volume
        .map(|v| format!("{v}%"))
        .unwrap_or_else(|| "-".to_string());

    RenderLines {
        state: format!("{head}{}", saved_marker(saved)),
        title: line("♪ ", &n.title),
        artist: line("  ", &n.artists),
        album: n
            .album
            .as_deref()
            .map(|a| line("  ", a))
            .unwrap_or_default(),
        ratio: progress_ratio(prog, n.duration_ms),
        progress_label: format!("{} / {}", format_ms(prog), format_ms(n.duration_ms)),
        device: format!("🔈 {} (vol {vol})", n.device),
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

/// Pure function that formats one device row (the TUI version of `commands::devices::render_device`).
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
        ("2", "browse library"),
        ("d", "select device"),
        ("r", "refresh (resume auto-refresh)"),
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

/// Pure function that classifies a status string by kind. Starting with `⚠` is a warning, starting
/// with a success symbol is Ok, and anything else is Info (startup, hint messages, etc.).
pub fn status_kind(s: &str) -> StatusKind {
    let trimmed = s.trim_start();
    if trimmed.starts_with('⚠') {
        StatusKind::Warn
    } else if ["▶", "⏸", "⏭", "⏮", "🔊", "♥", "♡", "⏩"]
        .iter()
        .any(|p| trimmed.starts_with(p))
    {
        StatusKind::Ok
    } else {
        StatusKind::Info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(out.state, "▶ Playing");
        assert!(out.title.contains("Song"));
        assert!(out.artist.contains("Artist"));
        assert_eq!(out.progress_label, "1:00 / 3:00");
        assert_eq!(out.ratio, 60_000.0 / 180_000.0);
        assert!(out.device.contains("MacBook Pro"));
        assert!(out.device.contains("40%"));
    }

    #[test]
    fn render_lines_empty_state_when_nothing_playing() {
        let out = render_lines(None, 0, 80, None);
        assert_eq!(out.state, "Nothing is playing");
        assert!(out.artist.is_empty());
        assert_eq!(out.ratio, 0.0);
    }

    #[test]
    fn render_lines_shows_saved_marker() {
        let n = sample(true);
        // Saved is ♥, not saved is ♡, unknown shows nothing
        assert!(
            render_lines(Some(&n), 0, 80, Some(true))
                .state
                .contains("♥")
        );
        assert!(
            render_lines(Some(&n), 0, 80, Some(false))
                .state
                .contains("♡")
        );
        let unknown = render_lines(Some(&n), 0, 80, None).state;
        assert!(!unknown.contains('♥') && !unknown.contains('♡'));
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
    fn help_entries_cover_all_keys() {
        let keys: Vec<&str> = help_entries().iter().map(|(k, _)| *k).collect();
        for k in [
            "space",
            "n / p",
            "← / →",
            "+ / -",
            "s",
            "/",
            "2",
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
        assert_eq!(status_kind("⚠ refresh failed: x"), StatusKind::Warn);
        assert_eq!(status_kind("▶ play"), StatusKind::Ok);
        assert_eq!(status_kind("♥ saved to your library"), StatusKind::Ok);
        assert_eq!(status_kind("⏩ seek 1:23"), StatusKind::Ok);
        assert_eq!(status_kind("starting…"), StatusKind::Info);
    }
}
