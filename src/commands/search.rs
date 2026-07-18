//! `spoterm search <query>`: track / album / artist を検索して一覧表示する。

use anyhow::{Context, Result};
use rspotify::model::SearchType;
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::format::{join_artists, truncate};

/// 各種別の取得件数（2026-02 以降 API 上限 10・既定 5。控えめに 5）。
const SEARCH_LIMIT: u32 = 5;
/// 名称の表示幅（超過分は省略）。
const NAME_WIDTH: usize = 40;

pub async fn run(cfg: &Config, query: &[String]) -> Result<()> {
    let q = query.join(" ");
    let spotify = auth::authed_client(cfg).await?;

    let result = spotify
        .search_multiple(
            &q,
            [SearchType::Track, SearchType::Album, SearchType::Artist],
            None,
            None,
            Some(SEARCH_LIMIT),
            None,
        )
        .await
        .context("検索に失敗しました")?;

    let mut printed = false;

    if let Some(page) = result.tracks
        && !page.items.is_empty()
    {
        printed = true;
        println!("🎵 Tracks");
        for (i, t) in page.items.into_iter().enumerate() {
            let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            let uri = t.id.as_ref().map(|id| id.uri()).unwrap_or_default();
            println!(
                "{}",
                render_line(
                    i + 1,
                    &truncate(&t.name, NAME_WIDTH),
                    &join_artists(&artists),
                    &uri
                )
            );
        }
    }

    if let Some(page) = result.albums
        && !page.items.is_empty()
    {
        printed = true;
        println!("💿 Albums");
        for (i, a) in page.items.into_iter().enumerate() {
            let artists: Vec<String> = a.artists.into_iter().map(|x| x.name).collect();
            let uri = a.id.as_ref().map(|id| id.uri()).unwrap_or_default();
            println!(
                "{}",
                render_line(
                    i + 1,
                    &truncate(&a.name, NAME_WIDTH),
                    &join_artists(&artists),
                    &uri
                )
            );
        }
    }

    if let Some(page) = result.artists
        && !page.items.is_empty()
    {
        printed = true;
        println!("🎤 Artists");
        for (i, a) in page.items.into_iter().enumerate() {
            let uri = a.id.uri();
            println!(
                "{}",
                render_line(i + 1, &truncate(&a.name, NAME_WIDTH), "", &uri)
            );
        }
    }

    if !printed {
        println!("\"{q}\" に一致する結果はありませんでした");
    }

    Ok(())
}

/// 検索結果 1 行を整形する純粋関数。`subtitle` が空なら省略する。
fn render_line(index: usize, title: &str, subtitle: &str, uri: &str) -> String {
    if subtitle.is_empty() {
        format!("  {index}. {title}    {uri}")
    } else {
        format!("  {index}. {title}  —  {subtitle}    {uri}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_line_with_subtitle() {
        let out = render_line(1, "Song", "Artist", "spotify:track:abc");
        assert_eq!(out, "  1. Song  —  Artist    spotify:track:abc");
    }

    #[test]
    fn render_line_without_subtitle() {
        let out = render_line(2, "Artist", "", "spotify:artist:xyz");
        assert_eq!(out, "  2. Artist    spotify:artist:xyz");
    }
}
