//! `spoterm status`: 現在の再生状況（Now Playing）を表示する。

use anyhow::{Context, Result};
use rspotify::model::PlayableItem;
use rspotify::prelude::*;
use serde_json::Value;

use crate::auth;
use crate::config::Config;
use crate::format::{format_ms, join_artists};

pub async fn run(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    let ctx = spotify
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("再生状況の取得に失敗しました")?;

    let Some(ctx) = ctx else {
        println!("再生中の曲はありません（spoterm play で再生を開始できます）");
        return Ok(());
    };

    let device = ctx.device.name;
    let vol = ctx.device.volume_percent;
    // rspotify の Duration（chrono）を非負ミリ秒へ。型名を出さず method 経由で変換する。
    let progress_ms = ctx.progress.map(|d| d.num_milliseconds().max(0) as u128);

    match ctx.item {
        Some(PlayableItem::Track(track)) => {
            let artists: Vec<String> = track.artists.into_iter().map(|a| a.name).collect();
            let line = render_track(
                ctx.is_playing,
                &track.name,
                &join_artists(&artists),
                Some(&track.album.name),
                progress_ms,
                track.duration.num_milliseconds().max(0) as u128,
                &device,
                vol,
            );
            println!("{line}");
        }
        Some(PlayableItem::Episode(ep)) => {
            let line = render_track(
                ctx.is_playing,
                &ep.name,
                "(ポッドキャスト)",
                None,
                progress_ms,
                ep.duration.num_milliseconds().max(0) as u128,
                &device,
                vol,
            );
            println!("{line}");
        }
        // Spotify の /me/player はトラックに external_ids を返さず、rspotify の FullTrack 解析が
        // 失敗して Unknown(生JSON) に落ちる。生 JSON から必要な値を取り出してフォールバック表示する。
        Some(PlayableItem::Unknown(v)) => {
            let (title, artists, album, duration_ms) = track_from_json(&v);
            let line = render_track(
                ctx.is_playing,
                &title,
                &join_artists(&artists),
                album.as_deref(),
                progress_ms,
                duration_ms,
                &device,
                vol,
            );
            println!("{line}");
        }
        None => println!("再生中ですが、曲情報を取得できませんでした（広告などの可能性）"),
    }

    Ok(())
}

/// `/me/player` のトラック JSON（rspotify が Unknown に落としたもの）から
/// 表示に必要な (曲名, アーティスト名, アルバム名, 再生時間ms) を取り出す。
/// TUI（`crate::tui`）でも同じフォールバックを使うため crate 内公開にしている。
pub(crate) fn track_from_json(v: &Value) -> (String, Vec<String>, Option<String>, u128) {
    let title = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("(不明なタイトル)")
        .to_string();
    let artists = v
        .get("artists")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let album = v
        .get("album")
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let duration_ms = v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0) as u128;
    (title, artists, album, duration_ms)
}

/// 再生状況の表示ブロックを組み立てる純粋関数。API 応答からの写像は呼び出し側で行う。
// 表示に必要な素をプリミティブで受け取りテスト容易性を優先している。呼び出し元は 1 箇所のみで
// 専用の表示用構造体を挟むほどの重複はないため、引数の多さは許容する（YAGNI）。
#[allow(clippy::too_many_arguments)]
fn render_track(
    playing: bool,
    title: &str,
    artists: &str,
    album: Option<&str>,
    progress_ms: Option<u128>,
    duration_ms: u128,
    device: &str,
    vol: Option<u32>,
) -> String {
    let head = if playing {
        "▶ 再生中"
    } else {
        "⏸ 一時停止"
    };
    let progress = match progress_ms {
        Some(p) => format!("{} / {}", format_ms(p), format_ms(duration_ms)),
        None => format!("- / {}", format_ms(duration_ms)),
    };
    let vol_s = match vol {
        Some(v) => format!("{v}%"),
        None => "-".to_string(),
    };
    let album_line = match album {
        Some(a) => format!("\n  アルバム : {a}"),
        None => String::new(),
    };
    format!(
        "{head}\n  曲       : {title}\n  アーティスト: {artists}{album_line}\n  進捗     : {progress}\n  デバイス : {device} (vol {vol_s})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_track_playing_with_progress() {
        let out = render_track(
            true,
            "Song",
            "Artist",
            Some("Album"),
            Some(83_000),
            187_000,
            "Speaker",
            Some(65),
        );
        assert!(out.starts_with("▶ 再生中"));
        assert!(out.contains("曲       : Song"));
        assert!(out.contains("アーティスト: Artist"));
        assert!(out.contains("アルバム : Album"));
        assert!(out.contains("進捗     : 1:23 / 3:07"));
        assert!(out.contains("デバイス : Speaker (vol 65%)"));
    }

    #[test]
    fn render_track_paused_without_progress_or_vol() {
        let out = render_track(false, "S", "A", None, None, 60_000, "Dev", None);
        assert!(out.starts_with("⏸ 一時停止"));
        assert!(out.contains("進捗     : - / 1:00"));
        assert!(out.contains("(vol -)"));
        // アルバム行は出さない
        assert!(!out.contains("アルバム"));
    }

    #[test]
    fn track_from_json_extracts_fields() {
        let v = serde_json::json!({
            "name": "Get Lucky",
            "artists": [{"name": "Daft Punk"}, {"name": "Pharrell Williams"}],
            "album": {"name": "Random Access Memories"},
            "duration_ms": 248_000
        });
        let (title, artists, album, dur) = track_from_json(&v);
        assert_eq!(title, "Get Lucky");
        assert_eq!(artists, vec!["Daft Punk", "Pharrell Williams"]);
        assert_eq!(album.as_deref(), Some("Random Access Memories"));
        assert_eq!(dur, 248_000);
    }

    #[test]
    fn track_from_json_falls_back_on_missing_fields() {
        let v = serde_json::json!({});
        let (title, artists, album, dur) = track_from_json(&v);
        assert_eq!(title, "(不明なタイトル)");
        assert!(artists.is_empty());
        assert_eq!(album, None);
        assert_eq!(dur, 0);
    }
}
