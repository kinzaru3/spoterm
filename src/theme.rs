//! Central theme: Nerd Font icon glyphs and the Spotify-like accent color, shared by the TUI and
//! the `login` command's console output.
//!
//! This is the single source of truth so that icon rendering, the status-line classifier
//! (`tui::view::status_kind`), and their tests all reference the *same* constants. Changing a glyph
//! here must keep those in sync — hence the shared definitions.
//!
//! Icons are Nerd Font (v3) Private Use Area glyphs and require a patched Nerd Font in the user's
//! terminal (documented as a requirement in the README). Without one, they render as tofu (□).

use ratatui::style::Color;

/// Spotify classic brand green (#1DB954). Used as the accent across borders, titles, the progress
/// gauge, list selection, and "ok" status lines. Rendered accurately on truecolor terminals;
/// approximated on 256-color terminals by ratatui.
pub const GREEN: Color = Color::Rgb(0x1D, 0xB9, 0x54);

// --- Now Playing / library icons ---
/// Music note — track title and the cover-art placeholder.
pub const MUSIC: &str = "\u{f001}"; // nf-fa-music
/// Microphone — artist line.
pub const ARTIST: &str = "\u{f130}"; // nf-fa-microphone
/// Compact disc — album line.
pub const ALBUM: &str = "\u{f51f}"; // nf-fa-compact_disc
/// Magnifier — the search input bar.
pub const SEARCH: &str = "\u{f002}"; // nf-fa-search
/// Speaker — device / volume line.
pub const VOLUME: &str = "\u{f028}"; // nf-fa-volume_up

// --- State / status icons ---
/// Warning — the prefix for every warning status line (matched by `status_kind`).
pub const WARN: &str = "\u{f071}"; // nf-fa-exclamation_triangle
/// Play — playing state.
pub const PLAY: &str = "\u{f04b}"; // nf-fa-play
/// Pause — paused state.
pub const PAUSE: &str = "\u{f04c}"; // nf-fa-pause
/// Step forward — next track.
pub const NEXT: &str = "\u{f051}"; // nf-fa-step_forward
/// Step backward — previous track.
pub const PREV: &str = "\u{f048}"; // nf-fa-step_backward
/// Fast forward — seek.
pub const SEEK: &str = "\u{f04e}"; // nf-fa-forward
/// Filled heart — track saved to library.
pub const HEART: &str = "\u{f004}"; // nf-fa-heart
/// Empty heart — track not saved.
pub const HEART_O: &str = "\u{f08a}"; // nf-fa-heart_o
/// Check mark — login success (CLI output).
pub const CHECK: &str = "\u{f00c}"; // nf-fa-check

// --- Playback / volume bar glyphs ---
// Decorative box-drawing glyphs for the bottom playbar. Deliberately *not* Nerd Font PUA (they are
// ordinary Unicode and render without a patched font) and deliberately kept out of `OK_PREFIXES` so
// the status-line classifier never mistakes a bar glyph for a status prefix.
/// Played portion of the progress slider.
pub const PROGRESS_FILLED: &str = "▬";
/// Current-position knob on the progress slider.
pub const PROGRESS_KNOB: &str = "●";
/// Unplayed portion of the progress slider.
pub const PROGRESS_TRACK: &str = "─";
/// Filled volume segment.
pub const VOL_FILLED: &str = "▮";
/// Empty volume segment.
pub const VOL_EMPTY: &str = "▯";

/// The set of status prefixes classified as [`super::view::StatusKind::Ok`]. `status_kind` matches
/// against these, so this list is the single definition shared by the classifier and its tests.
pub const OK_PREFIXES: &[&str] = &[PLAY, PAUSE, NEXT, PREV, SEEK, VOLUME, HEART, HEART_O];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_nonempty_and_distinct() {
        let all = [
            MUSIC, ARTIST, ALBUM, SEARCH, VOLUME, WARN, PLAY, PAUSE, NEXT, PREV, SEEK, HEART,
            HEART_O, CHECK,
        ];
        assert!(all.iter().all(|g| !g.is_empty()), "no icon may be empty");
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b, "icons must be distinct: {a:?}");
            }
        }
    }

    #[test]
    fn warn_is_not_an_ok_prefix() {
        // A warning line must never be misclassified as ok.
        assert!(!OK_PREFIXES.contains(&WARN));
    }

    #[test]
    fn bar_glyphs_are_nonempty_and_distinct() {
        let bars = [
            PROGRESS_FILLED,
            PROGRESS_KNOB,
            PROGRESS_TRACK,
            VOL_FILLED,
            VOL_EMPTY,
        ];
        assert!(
            bars.iter().all(|g| !g.is_empty()),
            "no bar glyph may be empty"
        );
        for (i, a) in bars.iter().enumerate() {
            for b in &bars[i + 1..] {
                assert_ne!(a, b, "bar glyphs must be distinct: {a:?}");
            }
        }
    }

    #[test]
    fn bar_glyphs_are_not_ok_prefixes() {
        // Decorative bar glyphs must never leak into the status-line classifier.
        for g in [
            PROGRESS_FILLED,
            PROGRESS_KNOB,
            PROGRESS_TRACK,
            VOL_FILLED,
            VOL_EMPTY,
        ] {
            assert!(
                !OK_PREFIXES.contains(&g),
                "bar glyph must not be a status prefix: {g:?}"
            );
        }
    }
}
