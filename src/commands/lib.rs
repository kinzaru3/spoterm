//! `spoterm lib`（Phase 5）。保存済みトラック・アルバムを一覧表示する（読み取り専用）。
//! 詳細設計: docs/design/lib.md

use anyhow::{Context, Result};
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::format::{join_artists, render_entry};

/// 各セクションの取得件数（先頭のみ表示。KISS）。
const PAGE_LIMIT: u32 = 20;

pub async fn run(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    let tracks = spotify
        .current_user_saved_tracks_manual(None, Some(PAGE_LIMIT), None)
        .await
        .context("保存済みトラックの取得に失敗しました")?;
    let albums = spotify
        .current_user_saved_albums_manual(None, Some(PAGE_LIMIT), None)
        .await
        .context("保存済みアルバムの取得に失敗しました")?;

    let mut printed = false;

    let track_total = tracks.total;
    let track_items = tracks.items;
    if !track_items.is_empty() {
        printed = true;
        println!(
            "{}",
            section_header("🎵 保存済みトラック", track_items.len(), track_total)
        );
        for (i, saved) in track_items.into_iter().enumerate() {
            let t = saved.track;
            let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            let uri = t.id.as_ref().map(|id| id.uri()).unwrap_or_default();
            println!(
                "{}",
                render_entry(i + 1, &t.name, &join_artists(&artists), &uri)
            );
        }
    }

    let album_total = albums.total;
    let album_items = albums.items;
    if !album_items.is_empty() {
        printed = true;
        println!(
            "{}",
            section_header("💿 保存済みアルバム", album_items.len(), album_total)
        );
        for (i, saved) in album_items.into_iter().enumerate() {
            let a = saved.album;
            let artists: Vec<String> = a.artists.into_iter().map(|x| x.name).collect();
            let uri = a.id.uri();
            println!(
                "{}",
                render_entry(i + 1, &a.name, &join_artists(&artists), &uri)
            );
        }
    }

    if !printed {
        println!("ライブラリに保存済みのトラック/アルバムはありません");
    }

    Ok(())
}

/// セクション見出しを整形する純粋関数。全件数が表示件数を超える場合のみ内訳を添える。
fn section_header(label: &str, shown: usize, total: u32) -> String {
    if (total as usize) > shown {
        format!("{label}（先頭 {shown} 件 / 全 {total} 件）")
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_header_plain_when_all_shown() {
        assert_eq!(section_header("🎵 曲", 5, 5), "🎵 曲");
        // total が shown 以下（想定外だが）なら内訳を付けない
        assert_eq!(section_header("🎵 曲", 5, 3), "🎵 曲");
    }

    #[test]
    fn section_header_annotates_when_truncated() {
        assert_eq!(
            section_header("💿 アルバム", 20, 57),
            "💿 アルバム（先頭 20 件 / 全 57 件）"
        );
    }
}
