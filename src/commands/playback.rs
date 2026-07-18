//! 再生コントロール（Phase 4）。アクティブデバイスへ操作を送る。
//! デバイスが未選択（アクティブ無し）の場合は `device use` を促す。

use anyhow::{Context, Result};
use rspotify::model::{PlayableId, SearchResult, SearchType};
use rspotify::prelude::*;

use super::NEED_DEVICE_HINT;
use crate::auth;
use crate::config::Config;
use crate::format::join_artists;

pub async fn pause(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    spotify
        .pause_playback(None)
        .await
        .with_context(|| format!("一時停止に失敗しました{NEED_DEVICE_HINT}"))?;
    println!("⏸ 一時停止しました");
    Ok(())
}

pub async fn next(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    spotify
        .next_track(None)
        .await
        .with_context(|| format!("次の曲への移動に失敗しました{NEED_DEVICE_HINT}"))?;
    println!("⏭ 次の曲へ");
    Ok(())
}

pub async fn prev(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    spotify
        .previous_track(None)
        .await
        .with_context(|| format!("前の曲への移動に失敗しました{NEED_DEVICE_HINT}"))?;
    println!("⏮ 前の曲へ");
    Ok(())
}

pub async fn vol(cfg: &Config, level: u8) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    spotify
        .volume(level, None)
        .await
        .with_context(|| format!("音量設定に失敗しました{NEED_DEVICE_HINT}"))?;
    println!("🔊 音量を {level}% にしました");
    Ok(())
}

pub async fn toggle(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    let ctx = spotify
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("再生状況の取得に失敗しました")?;

    match ctx {
        Some(c) if c.is_playing => {
            spotify
                .pause_playback(None)
                .await
                .context("一時停止に失敗しました")?;
            println!("⏸ 一時停止しました");
        }
        Some(_) => {
            spotify
                .resume_playback(None, None)
                .await
                .context("再生の再開に失敗しました")?;
            println!("▶ 再生を再開しました");
        }
        None => {
            println!(
                "アクティブなデバイスがありません。`spoterm device use <name>` で選択してください"
            );
        }
    }
    Ok(())
}

pub async fn play(cfg: &Config, query: &[String]) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    // 無引数は再開。
    if query.is_empty() {
        spotify
            .resume_playback(None, None)
            .await
            .with_context(|| format!("再生の再開に失敗しました{NEED_DEVICE_HINT}"))?;
        println!("▶ 再生を再開しました");
        return Ok(());
    }

    // クエリ指定は上位トラックを検索して再生。
    let q = query.join(" ");
    let result = spotify
        .search(&q, SearchType::Track, None, None, Some(1), None)
        .await
        .context("検索に失敗しました")?;

    let SearchResult::Tracks(page) = result else {
        anyhow::bail!("検索結果の形式が想定外です");
    };

    let Some(track) = page.items.into_iter().next() else {
        println!("\"{q}\" に一致するトラックがありませんでした");
        return Ok(());
    };

    // フィールドは互いに独立なので個別に move してよい。
    let name = track.name;
    let artists: Vec<String> = track.artists.into_iter().map(|a| a.name).collect();
    let id = track
        .id
        .context("再生できるトラック（ローカル曲などで URI 無し）でした")?;

    spotify
        .start_uris_playback([PlayableId::Track(id)], None, None, None)
        .await
        .with_context(|| format!("再生の開始に失敗しました{NEED_DEVICE_HINT}"))?;

    println!("▶ 再生: {name} — {}", join_artists(&artists));
    Ok(())
}
