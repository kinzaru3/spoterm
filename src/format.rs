//! Pure formatting helpers. They depend only on primitives (not rspotify model types) and
//! return `String`, which keeps them unit-testable (the TUI does the model→primitive mapping).

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Format milliseconds as `m:ss` (keeps counting as `mm:ss` past 60 minutes).
pub fn format_ms(ms: u128) -> String {
    let total_secs = ms / 1000;
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{minutes}:{seconds:02}")
}

/// Join artist names with `", "`. Returns a placeholder when the list is empty.
pub fn join_artists(names: &[String]) -> String {
    if names.is_empty() {
        "(unknown artist)".to_string()
    } else {
        names.join(", ")
    }
}

/// Display width (terminal columns) of a string. Full-width chars/emoji count as 2 columns,
/// matching how ratatui lays out the buffer.
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate with a trailing `…` when the display width exceeds `max` columns. Counts by column
/// width and reserves 1 column for the `…`. Stops before a full-width char that would cross the
/// budget (prevents column overflow).
pub fn truncate(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new(); // too narrow for even the `…` (1 col): return empty
    }
    let budget = max - 1; // subtract the trailing `…` (1 col); max >= 1 guaranteed here
    let mut width = 0;
    let mut head = String::new();
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + w > budget {
            break;
        }
        width += w;
        head.push(c);
    }
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_ms_pads_seconds_and_handles_minutes() {
        assert_eq!(format_ms(0), "0:00");
        assert_eq!(format_ms(5_000), "0:05");
        assert_eq!(format_ms(65_000), "1:05");
        assert_eq!(format_ms(187_000), "3:07");
        assert_eq!(format_ms(3_600_000), "60:00");
    }

    #[test]
    fn join_artists_joins_or_reports_unknown() {
        assert_eq!(join_artists(&["A".to_string()]), "A");
        assert_eq!(join_artists(&["A".to_string(), "B".to_string()]), "A, B");
        assert_eq!(join_artists(&[]), "(unknown artist)");
    }

    #[test]
    fn truncate_shortens_only_when_over_limit() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        // Full-width chars are 2 columns each. Count by columns and reserve 1 col for `…`.
        // "あいうえお" is 10 cols. max=3 → budget 2 → "あ"(2)+…, max=5 → budget 4 → "あい"(4)+…
        assert_eq!(truncate("あいうえお", 3), "あ…");
        assert_eq!(truncate("あいうえお", 5), "あい…");
        // A full-width char crossing the budget boundary stops before it (no overflow).
        assert_eq!(truncate("あいうえお", 4), "あ…");
        // max=0 (not even room for `…`) → empty string (never overflow the column budget)
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(display_width(&truncate("あいうえお", 0)), 0);
    }

    #[test]
    fn display_width_counts_columns() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("あ"), 2); // full-width = 2 cols
        assert_eq!(display_width("🎵"), 2); // emoji = 2 cols
        assert_eq!(display_width("a あ"), 4); // 1 + 1(space) + 2
    }
}
