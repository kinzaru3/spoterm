//! 表示整形の純粋関数。rspotify のモデル型に依存せず、プリミティブのみを受け取り
//! `String` を返すことで単体テスト可能にしている（コマンド本体はモデル→プリミティブの写像に徹する）。

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

/// `max` 文字を超える文字列を末尾省略（`…`）する。マルチバイトを壊さないよう文字単位で数える。
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let head: String = s.chars().take(keep).collect();
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
        assert_eq!(join_artists(&[]), "(不明なアーティスト)");
    }

    #[test]
    fn truncate_shortens_only_when_over_limit() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        // マルチバイト（各文字 1 count）
        assert_eq!(truncate("あいうえお", 3), "あい…");
        assert_eq!(truncate("あいうえお", 5), "あいうえお");
    }
}
