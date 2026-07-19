//! 表示整形の純粋関数。rspotify のモデル型に依存せず、プリミティブのみを受け取り
//! `String` を返すことで単体テスト可能にしている（コマンド本体はモデル→プリミティブの写像に徹する）。

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// ミリ秒を `m:ss` 形式に整形する（60 分を超えても `mm:ss` を継続）。
pub fn format_ms(ms: u128) -> String {
    let total_secs = ms / 1000;
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{minutes}:{seconds:02}")
}

/// アーティスト名を `", "` で連結する。空なら不明表記を返す。
pub fn join_artists(names: &[String]) -> String {
    if names.is_empty() {
        "(不明なアーティスト)".to_string()
    } else {
        names.join(", ")
    }
}

/// 文字列の表示幅（端末の列数）。全角/絵文字は 2 列として数える（ratatui の描画計算と一致）。
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// 表示幅が `max` 列を超える文字列を末尾省略（`…`）する。列幅で数え、`…`（1 列）ぶんを確保する。
/// 全角文字が予算境界をまたぐ場合はその手前で止める（列あふれを防ぐ）。
pub fn truncate(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new(); // `…`(1列) すら入らない極端に狭い幅では空にする
    }
    let budget = max - 1; // 末尾 `…`（1 列）ぶんを引く（max >= 1 が保証済み）
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

/// 一覧の 1 行を整形する純粋関数。`subtitle` が空なら省略し、`uri` が空なら末尾に付けない。
/// search / playlist ls / lib の各一覧で共用する。
pub fn render_entry(index: usize, title: &str, subtitle: &str, uri: &str) -> String {
    let head = if subtitle.is_empty() {
        format!("  {index}. {title}")
    } else {
        format!("  {index}. {title}  —  {subtitle}")
    };
    if uri.is_empty() {
        head
    } else {
        format!("{head}    {uri}")
    }
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
        assert_eq!(join_artists(&[]), "(不明なアーティスト)");
    }

    #[test]
    fn truncate_shortens_only_when_over_limit() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        // 全角は 1 文字 2 列。列幅で数え、`…`(1列) ぶんを確保する。
        // "あいうえお" は 10 列。max=3 → 予算 2 列 → "あ"(2)＋… 、max=5 → 予算 4 列 → "あい"(4)＋…
        assert_eq!(truncate("あいうえお", 3), "あ…");
        assert_eq!(truncate("あいうえお", 5), "あい…");
        // 全角が予算境界をまたぐ場合は手前で止める（列あふれ防止）
        assert_eq!(truncate("あいうえお", 4), "あ…");
        // max=0（`…` すら入らない）は空文字（列あふれさせない）
        assert_eq!(truncate("hello", 0), "");
        assert_eq!(display_width(&truncate("あいうえお", 0)), 0);
    }

    #[test]
    fn display_width_counts_columns() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width("あ"), 2); // 全角は 2 列
        assert_eq!(display_width("🎵"), 2); // 絵文字も 2 列
        assert_eq!(display_width("a あ"), 4); // 1 + 1(空白) + 2
    }

    #[test]
    fn render_entry_with_subtitle_and_uri() {
        let out = render_entry(1, "Song", "Artist", "spotify:track:abc");
        assert_eq!(out, "  1. Song  —  Artist    spotify:track:abc");
    }

    #[test]
    fn render_entry_without_subtitle() {
        let out = render_entry(2, "Artist", "", "spotify:artist:xyz");
        assert_eq!(out, "  2. Artist    spotify:artist:xyz");
    }

    #[test]
    fn render_entry_without_uri() {
        let out = render_entry(3, "My Mix", "120曲", "");
        assert_eq!(out, "  3. My Mix  —  120曲");
    }
}
