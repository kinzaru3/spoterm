//! TUI の表示計算（純粋関数）と Now Playing スナップショット。ratatui に依存せず、
//! 進捗補間・比率計算をプリミティブ入出力で行い単体テスト可能にする（描画は mod.rs 側）。

use std::time::Instant;

use crate::format::{display_width, format_ms, truncate};

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
    /// 現在曲のトラック URI（`spotify:track:…`）。保存操作・曲変化検知に使う。
    /// エピソードや曲情報不明のときは `None`。
    pub track_uri: Option<String>,
    /// カバーアート画像の URL（選択済み）。アート取得・曲変化検知に使う。無い場合は `None`。
    pub album_image_url: Option<String>,
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

/// 現在曲の保存状態を表す短いマーカー（state 行末尾に付す）。`None` は状態不明で無表示。
fn saved_marker(saved: Option<bool>) -> &'static str {
    match saved {
        Some(true) => "   ♥ 保存済み",
        Some(false) => "   ♡ 未保存",
        None => "",
    }
}

/// Now Playing の表示行を組み立てる。`elapsed_ms` は前回取得からの経過（進捗補間の基点）、
/// `width` は各行の折り返し幅、`saved` は現在曲のライブラリ保存状態（`None` は不明）。
/// 無再生（`None`）時は案内文を返す。
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

/// シーク後の目標位置（ms）を返す純粋関数。`current_ms` に `delta_ms`（負で後退）を足し、
/// `[0, duration_ms]` にクランプする。`duration_ms == 0`（尺不明）のときは上限を設けない。
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

/// キーバインド一覧（キー, 説明）。フッターとヘルプオーバーレイの唯一の情報源
/// （両者がここを参照することで表記のドリフトを防ぐ）。
pub fn help_entries() -> &'static [(&'static str, &'static str)] {
    &[
        ("space", "再生 / 一時停止"),
        ("n / p", "次の曲 / 前の曲"),
        ("← / →", "5 秒シーク（戻る / 進む）"),
        ("+ / -", "音量 ±5"),
        ("s", "現在曲を保存 / 解除"),
        ("/", "検索して再生"),
        ("2", "ライブラリ閲覧"),
        ("d", "デバイス選択"),
        ("r", "更新（自動更新の再開）"),
        ("?", "このヘルプ"),
        ("q / Esc", "終了"),
        ("Ctrl-C", "終了（どの画面でも）"),
    ]
}

/// ステータス行の種別。色分けの判断に使う。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StatusKind {
    Warn,
    Ok,
    Info,
}

/// ステータス文字列を種別に分類する純粋関数。`⚠` 始まりは警告、操作成功の記号始まりは Ok、
/// それ以外は Info（起動中・案内文など）。
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
            track_uri: Some("spotify:track:xxxx".to_string()),
            album_image_url: None,
            fetched_at: std::time::Instant::now(),
        }
    }

    #[test]
    fn render_lines_shows_track_and_progress() {
        let n = sample(true);
        let out = render_lines(Some(&n), 0, 80, None);
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
        let out = render_lines(None, 0, 80, None);
        assert_eq!(out.state, "再生中の曲はありません");
        assert!(out.artist.is_empty());
        assert_eq!(out.ratio, 0.0);
    }

    #[test]
    fn render_lines_shows_saved_marker() {
        let n = sample(true);
        // 保存済みは ♥、未保存は ♡、不明は無表示
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
        // 前進
        assert_eq!(seek_target(60_000, 180_000, 5_000), 65_000);
        // 後退
        assert_eq!(seek_target(60_000, 180_000, -5_000), 55_000);
    }

    #[test]
    fn seek_target_clamps_bounds() {
        // 下限 0（後退しすぎ）
        assert_eq!(seek_target(3_000, 180_000, -5_000), 0);
        // 上限 duration（前進しすぎ）
        assert_eq!(seek_target(178_000, 180_000, 5_000), 180_000);
        // 尺不明（0）は上限なし
        assert_eq!(seek_target(178_000, 0, 5_000), 183_000);
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
        // width 10 → 記号 2 桁ぶんを引いた 8 列で末尾省略
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
            assert!(keys.contains(&k), "help に {k} が無い");
        }
        // 説明は空でない
        assert!(help_entries().iter().all(|(_, desc)| !desc.is_empty()));
    }

    #[test]
    fn status_kind_classifies() {
        assert_eq!(status_kind("⚠ 更新失敗: x"), StatusKind::Warn);
        assert_eq!(status_kind("▶ 再生"), StatusKind::Ok);
        assert_eq!(status_kind("♥ ライブラリに保存しました"), StatusKind::Ok);
        assert_eq!(status_kind("⏩ シーク 1:23"), StatusKind::Ok);
        assert_eq!(status_kind("起動中…"), StatusKind::Info);
    }
}
