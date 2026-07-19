//! `spoterm playlist ls|play`. List playlists and play one by name.

use anyhow::{Context, Result};
use rspotify::model::PlayContextId;
use rspotify::prelude::*;

use super::NEED_DEVICE_HINT;
use crate::auth;
use crate::config::Config;
use crate::format::render_entry;
use crate::match_name::{NameMatch, match_name};

/// Number of items fetched per request (API max is 50). Only the first page is handled (KISS).
const PAGE_LIMIT: u32 = 50;

pub async fn ls(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    let page = spotify
        .current_user_playlists_manual(Some(PAGE_LIMIT), None)
        .await
        .context("failed to fetch the playlist list")?;

    if page.items.is_empty() {
        println!("No playlists");
        return Ok(());
    }

    println!("Playlists:");
    for (i, pl) in page.items.iter().enumerate() {
        let count = format!("{} tracks", pl.items.total);
        println!("{}", render_entry(i + 1, &pl.name, &count, &pl.id.uri()));
    }

    if (page.total as usize) > page.items.len() {
        println!(
            "  … showing the first {} (of {} total)",
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
        .context("failed to fetch the playlist list")?;

    let playlists = &page.items;
    if playlists.is_empty() {
        println!("No playlists");
        return Ok(());
    }

    // If we only looked at the first page, note that on a no-match.
    let truncated = (page.total as usize) > playlists.len();
    let names: Vec<&str> = playlists.iter().map(|p| p.name.as_str()).collect();

    match match_name(&names, &query) {
        NameMatch::Found(i) => {
            let pl = &playlists[i];
            let ctx = PlayContextId::Playlist(pl.id.as_ref());
            spotify
                .start_context_playback(ctx, None, None, None)
                .await
                .with_context(|| format!("failed to start playlist playback{NEED_DEVICE_HINT}"))?;
            println!("▶ Playing: {}", pl.name);
        }
        NameMatch::None => {
            println!("{}", no_match_message(&query, truncated, playlists.len()));
        }
        NameMatch::Ambiguous(idxs) => {
            println!("'{query}' matched multiple playlists. Please be more specific:");
            for i in idxs {
                println!("  - {}", playlists[i].name);
            }
        }
    }

    Ok(())
}

/// Pure function that builds the "no matching playlist" message. When only the first page
/// was matched, note that (`shown` items).
fn no_match_message(query: &str, truncated: bool, shown: usize) -> String {
    let base = format!("No playlist matching '{query}'. Check with `spoterm playlist ls`");
    if truncated {
        format!("{base} (only the first {shown} were matched)")
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
            "No playlist matching 'mix'. Check with `spoterm playlist ls`"
        );
    }

    #[test]
    fn no_match_message_notes_truncation() {
        let out = no_match_message("mix", true, 50);
        assert!(out.ends_with("(only the first 50 were matched)"));
    }
}
