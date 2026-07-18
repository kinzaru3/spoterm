//! `spoterm playlist ls|play`（Phase 5）。プレイリストの一覧表示と名前指定での再生。
//! 詳細設計: docs/design/playlist.md

use anyhow::{Context, Result};
use rspotify::model::PlayContextId;
use rspotify::prelude::*;

use super::NEED_DEVICE_HINT;
use crate::auth;
use crate::config::Config;
use crate::format::render_entry;
use crate::match_name::{NameMatch, match_name};

/// 1 回の取得件数（API 上限 50）。先頭ページのみ扱う（KISS）。
const PAGE_LIMIT: u32 = 50;

pub async fn ls(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    let page = spotify
        .current_user_playlists_manual(Some(PAGE_LIMIT), None)
        .await
        .context("プレイリスト一覧の取得に失敗しました")?;

    if page.items.is_empty() {
        println!("プレイリストがありません");
        return Ok(());
    }

    println!("プレイリスト:");
    for (i, pl) in page.items.iter().enumerate() {
        let count = format!("{}曲", pl.items.total);
        println!("{}", render_entry(i + 1, &pl.name, &count, &pl.id.uri()));
    }

    if (page.total as usize) > page.items.len() {
        println!(
            "  … 先頭 {} 件を表示（全 {} 件）",
            page.items.len(),
            page.total
        );
    }

    Ok(())
}

pub async fn play(cfg: &Config, name: &[String]) -> Result<()> {
    let query = name.join(" ");
    let spotify = auth::authed_client(cfg).await?;

    let page = spotify
        .current_user_playlists_manual(Some(PAGE_LIMIT), None)
        .await
        .context("プレイリスト一覧の取得に失敗しました")?;

    let playlists = &page.items;
    if playlists.is_empty() {
        println!("プレイリストがありません");
        return Ok(());
    }

    // 先頭ページしか見ていない場合、該当なし時にその旨を添える。
    let truncated = (page.total as usize) > playlists.len();
    let names: Vec<&str> = playlists.iter().map(|p| p.name.as_str()).collect();

    match match_name(&names, &query) {
        NameMatch::Found(i) => {
            let pl = &playlists[i];
            let ctx = PlayContextId::Playlist(pl.id.as_ref());
            spotify
                .start_context_playback(ctx, None, None, None)
                .await
                .with_context(|| {
                    format!("プレイリストの再生開始に失敗しました{NEED_DEVICE_HINT}")
                })?;
            println!("▶ 再生: {}", pl.name);
        }
        NameMatch::None => {
            println!("{}", no_match_message(&query, truncated, playlists.len()));
        }
        NameMatch::Ambiguous(idxs) => {
            println!("'{query}' が複数のプレイリストに一致しました。より具体的に指定してください:");
            for i in idxs {
                println!("  - {}", playlists[i].name);
            }
        }
    }

    Ok(())
}

/// 「該当プレイリストなし」の案内文を組み立てる純粋関数。先頭ページのみ照合した場合は
/// その旨（`shown` 件）を添える。
fn no_match_message(query: &str, truncated: bool, shown: usize) -> String {
    let base = format!(
        "'{query}' に一致するプレイリストがありません。spoterm playlist ls で確認してください"
    );
    if truncated {
        format!("{base}（先頭 {shown} 件のみ照合しました）")
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_match_message_plain_when_not_truncated() {
        let out = no_match_message("mix", false, 12);
        assert_eq!(
            out,
            "'mix' に一致するプレイリストがありません。spoterm playlist ls で確認してください"
        );
    }

    #[test]
    fn no_match_message_notes_truncation() {
        let out = no_match_message("mix", true, 50);
        assert!(out.ends_with("（先頭 50 件のみ照合しました）"));
    }
}
