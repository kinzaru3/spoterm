//! TUI の表示計算（純粋関数）と Now Playing スナップショット。ratatui に依存せず、
//! 進捗補間・比率計算をプリミティブ入出力で行い単体テスト可能にする（描画は mod.rs 側）。

use std::time::Instant;

use crate::format::{format_ms, truncate};

/// 直近のポーリングで得た再生状況のスナップショット。`fetched_at` を基点に、
/// ポーリング間の進捗をローカルで補間して滑らかに見せる。
pub struct NowPlaying {
    pub is_playing: bool,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub progress_ms: u128,
    pub duration_ms: u128,
    pub device: String,
    pub volume: Option<u8>,
    /// このスナップショットを取得した時刻（進捗補間の基点）。
    pub fetched_at: Instant,
}

/// 再生中なら基点 `base_ms` から経過ぶんを進めた進捗を返す（`duration_ms` で頭打ち）。
/// 一時停止中は基点のまま。`duration_ms == 0`（尺不明）のときは頭打ちしない。
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

/// 進捗比率 0.0..=1.0 を返す。`duration_ms == 0` のときは 0.0。
pub fn progress_ratio(progress_ms: u128, duration_ms: u128) -> f64 {
    if duration_ms == 0 {
        return 0.0;
    }
    (progress_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
}

/// 描画に必要な各行（プリミティブ文字列 + 進捗比率）。ratatui に依存させず、描画ロジックを
/// 純粋関数化して単体テスト可能にする（`mod.rs::draw` はこれを widget に流し込むだけ）。
pub struct RenderLines {
    pub state: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub ratio: f64,
    pub progress_label: String,
    pub device: String,
}

/// Now Playing の表示行を組み立てる。`elapsed_ms` は前回取得からの経過（進捗補間の基点）、
/// `width` は各行の折り返し幅。無再生（`None`）時は案内文を返す。
pub fn render_lines(now: Option<&NowPlaying>, elapsed_ms: u128, width: usize) -> RenderLines {
    let line = |label: &str, value: &str| -> String {
        format!(
            "{label}{}",
            truncate(value, width.saturating_sub(label.chars().count()))
        )
    };

    let Some(n) = now else {
        return RenderLines {
            state: "再生中の曲はありません".to_string(),
            title: "  （p で再開 / spoterm play で開始）".to_string(),
            artist: String::new(),
            album: String::new(),
            ratio: 0.0,
            progress_label: "-".to_string(),
            device: String::new(),
        };
    };

    let prog = interpolate_progress(n.progress_ms, elapsed_ms, n.duration_ms, n.is_playing);
    let head = if n.is_playing {
        "▶ 再生中"
    } else {
        "⏸ 一時停止"
    };
    let vol = n
        .volume
        .map(|v| format!("{v}%"))
        .unwrap_or_else(|| "-".to_string());

    RenderLines {
        state: head.to_string(),
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

/// 検索オーバーレイの補足行（`message` が無いときの既定案内）。フェーズと件数で出し分ける。
pub fn search_hint(is_input: bool, results_len: usize) -> String {
    if is_input {
        "Enter で検索 / Esc で戻る".to_string()
    } else {
        format!("{results_len} 件 — ↑↓ 選択 / Enter 再生 / Esc でクエリ修正")
    }
}

/// 検索結果 1 行を整形する（`name — artists`、幅で末尾省略）。選択強調は呼び出し側で行う。
/// 選択記号 `"▶ "` の 2 桁ぶんを差し引いた幅で省略する。
pub fn search_row(name: &str, artists: &str, width: usize) -> String {
    let text = if artists.is_empty() {
        name.to_string()
    } else {
        format!("{name} — {artists}")
    };
    truncate(&text, width.saturating_sub(2))
}

/// デバイス 1 行を整形する純粋関数（`commands::devices::render_device` の TUI 版）。
/// アクティブは `● (active)`、非アクティブは `○` で明示し、操作不可は注記する。
/// 選択記号 `"▶ "` の 2 桁ぶんを差し引いた幅で末尾省略する。
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
        text.push_str(" (操作不可)");
    }
    truncate(&text, width.saturating_sub(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_advances_only_while_playing() {
        // 再生中は経過ぶん進む
        assert_eq!(interpolate_progress(10_000, 3_000, 200_000, true), 13_000);
        // 一時停止中は基点のまま
        assert_eq!(interpolate_progress(10_000, 3_000, 200_000, false), 10_000);
    }

    #[test]
    fn interpolate_clamps_to_duration() {
        // 尺を超えない
        assert_eq!(
            interpolate_progress(195_000, 10_000, 200_000, true),
            200_000
        );
        // 尺不明（0）なら頭打ちしない
        assert_eq!(interpolate_progress(195_000, 10_000, 0, true), 205_000);
    }

    #[test]
    fn progress_ratio_bounds() {
        assert_eq!(progress_ratio(0, 200_000), 0.0);
        assert_eq!(progress_ratio(100_000, 200_000), 0.5);
        assert_eq!(progress_ratio(200_000, 200_000), 1.0);
        // 尺 0 は 0.0（ゼロ割回避）
        assert_eq!(progress_ratio(50_000, 0), 0.0);
        // 万一 progress > duration でも 1.0 に収める
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
            device: "spotifyd".to_string(),
            volume: Some(40),
            fetched_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn render_lines_shows_track_and_progress() {
        let n = sample(true);
        let out = render_lines(Some(&n), 0, 80);
        assert_eq!(out.state, "▶ 再生中");
        assert!(out.title.contains("Song"));
        assert!(out.artist.contains("Artist"));
        assert_eq!(out.progress_label, "1:00 / 3:00");
        assert_eq!(out.ratio, 60_000.0 / 180_000.0);
        assert!(out.device.contains("spotifyd"));
        assert!(out.device.contains("40%"));
    }

    #[test]
    fn render_lines_empty_state_when_nothing_playing() {
        let out = render_lines(None, 0, 80);
        assert_eq!(out.state, "再生中の曲はありません");
        assert!(out.artist.is_empty());
        assert_eq!(out.ratio, 0.0);
    }

    #[test]
    fn search_row_joins_name_and_artists() {
        assert_eq!(search_row("Song", "Artist", 80), "Song — Artist");
        // アーティスト無しは曲名のみ
        assert_eq!(search_row("Song", "", 80), "Song");
    }

    #[test]
    fn search_row_truncates_with_symbol_margin() {
        // width 10 → 記号 2 桁ぶんを引いた 8 文字で省略（末尾は …）
        let out = search_row("abcdefghij", "", 10);
        assert_eq!(out.chars().count(), 8);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn search_hint_varies_by_phase() {
        assert!(search_hint(true, 0).contains("Enter で検索"));
        let results = search_hint(false, 3);
        assert!(results.starts_with("3 件"));
        assert!(results.contains("Enter 再生"));
    }

    #[test]
    fn device_row_active_marks_and_notes() {
        let out = device_row("MacBook-spotifyd", "Computer", Some(65), true, false, 80);
        assert!(out.starts_with("● MacBook-spotifyd [Computer]"));
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
        assert!(out.contains("(操作不可)"));
    }

    #[test]
    fn device_row_truncates_with_symbol_margin() {
        // width 10 → 記号 2 桁ぶんを引いた 8 文字で末尾省略
        let out = device_row("abcdefghij", "X", None, false, false, 10);
        assert_eq!(out.chars().count(), 8);
        assert!(out.ends_with('…'));
    }
}
