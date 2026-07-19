//! `spotterm lib`. List saved tracks and albums (read-only).

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::format::{join_artists, render_entry};

/// Number of items fetched per section (only the first page is shown. KISS).
const PAGE_LIMIT: u32 = 20;

pub async fn run(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", execute(&spotify).await?);
    Ok(())
}

/// Fetch saved tracks and albums and build the listing. Returns the text to print so the API
/// glue (requests + response mapping) is testable.
async fn execute(spotify: &AuthCodePkceSpotify) -> Result<String> {
    let tracks = spotify
        .current_user_saved_tracks_manual(None, Some(PAGE_LIMIT), None)
        .await
        .context("failed to fetch saved tracks")?;
    let albums = spotify
        .current_user_saved_albums_manual(None, Some(PAGE_LIMIT), None)
        .await
        .context("failed to fetch saved albums")?;

    let mut lines: Vec<String> = Vec::new();

    let track_total = tracks.total;
    let track_items = tracks.items;
    if !track_items.is_empty() {
        lines.push(section_header(
            "🎵 Saved tracks",
            track_items.len(),
            track_total,
        ));
        for (i, saved) in track_items.into_iter().enumerate() {
            let t = saved.track;
            let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            let uri = t.id.as_ref().map(|id| id.uri()).unwrap_or_default();
            lines.push(render_entry(i + 1, &t.name, &join_artists(&artists), &uri));
        }
    }

    let album_total = albums.total;
    let album_items = albums.items;
    if !album_items.is_empty() {
        lines.push(section_header(
            "💿 Saved albums",
            album_items.len(),
            album_total,
        ));
        for (i, saved) in album_items.into_iter().enumerate() {
            let a = saved.album;
            let artists: Vec<String> = a.artists.into_iter().map(|x| x.name).collect();
            let uri = a.id.uri();
            lines.push(render_entry(i + 1, &a.name, &join_artists(&artists), &uri));
        }
    }

    if lines.is_empty() {
        return Ok("No saved tracks or albums in your library".to_string());
    }

    Ok(lines.join("\n"))
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
    use crate::test_fixtures as fx;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mount_get(server: &MockServer, http_path: &str, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(http_path))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn execute_reports_empty_library() {
        let server = MockServer::start().await;
        mount_get(&server, "/me/tracks", fx::empty_page()).await;
        mount_get(&server, "/me/albums", fx::empty_page()).await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = execute(&client).await.unwrap();
        assert_eq!(out, "No saved tracks or albums in your library");
    }

    #[tokio::test]
    async fn execute_lists_saved_tracks() {
        let server = MockServer::start().await;
        mount_get(
            &server,
            "/me/tracks",
            fx::page(
                vec![fx::saved_track(
                    "4iV5W9uYEdYUVa79Axb7Rh",
                    "Saved Song",
                    "Saved Artist",
                )],
                1,
            ),
        )
        .await;
        mount_get(&server, "/me/albums", fx::empty_page()).await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = execute(&client).await.unwrap();
        assert!(out.contains("🎵 Saved tracks"), "{out}");
        assert!(out.contains("Saved Song"), "{out}");
        assert!(out.contains("Saved Artist"), "{out}");
    }

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
