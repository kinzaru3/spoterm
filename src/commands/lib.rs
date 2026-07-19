//! `spoterm lib`. List saved tracks and albums (read-only).

use anyhow::{Context, Result};
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::format::{join_artists, render_entry};

/// Number of items fetched per section (only the first page is shown. KISS).
const PAGE_LIMIT: u32 = 20;

pub async fn run(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    let tracks = spotify
        .current_user_saved_tracks_manual(None, Some(PAGE_LIMIT), None)
        .await
        .context("failed to fetch saved tracks")?;
    let albums = spotify
        .current_user_saved_albums_manual(None, Some(PAGE_LIMIT), None)
        .await
        .context("failed to fetch saved albums")?;

    let mut printed = false;

    let track_total = tracks.total;
    let track_items = tracks.items;
    if !track_items.is_empty() {
        printed = true;
        println!(
            "{}",
            section_header("🎵 Saved tracks", track_items.len(), track_total)
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
            section_header("💿 Saved albums", album_items.len(), album_total)
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
        println!("No saved tracks or albums in your library");
    }

    Ok(())
}

/// Pure function that formats a section header. Adds a breakdown only when the total exceeds the number shown.
fn section_header(label: &str, shown: usize, total: u32) -> String {
    if (total as usize) > shown {
        format!("{label} (first {shown} / {total} total)")
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_header_plain_when_all_shown() {
        assert_eq!(section_header("🎵 Tracks", 5, 5), "🎵 Tracks");
        // If total is <= shown (not expected), don't add a breakdown
        assert_eq!(section_header("🎵 Tracks", 5, 3), "🎵 Tracks");
    }

    #[test]
    fn section_header_annotates_when_truncated() {
        assert_eq!(
            section_header("💿 Albums", 20, 57),
            "💿 Albums (first 20 / 57 total)"
        );
    }
}
