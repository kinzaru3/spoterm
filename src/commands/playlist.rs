//! `spoterm playlist ls|play`. List playlists and play one by name.

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
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
    println!("{}", exec_ls(&spotify).await?);
    Ok(())
}

/// Fetch the playlists and build the listing. Returns the text to print so the API glue
/// (request + response mapping) is testable.
async fn exec_ls(spotify: &AuthCodePkceSpotify) -> Result<String> {
    let page = spotify
        .current_user_playlists_manual(Some(PAGE_LIMIT), None)
        .await
        .context("failed to fetch the playlist list")?;

    if page.items.is_empty() {
        return Ok("No playlists".to_string());
    }

    let mut lines = vec!["Playlists:".to_string()];
    for (i, pl) in page.items.iter().enumerate() {
        let count = format!("{} tracks", pl.items.total);
        lines.push(render_entry(i + 1, &pl.name, &count, &pl.id.uri()));
    }

    if (page.total as usize) > page.items.len() {
        lines.push(format!(
            "  … showing the first {} (of {} total)",
            page.items.len(),
            page.total
        ));
    }

    Ok(lines.join("\n"))
}

pub async fn play(cfg: &Config, name: &[String]) -> Result<()> {
    let query = name.join(" ");
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", exec_play(&spotify, &query).await?);
    Ok(())
}

/// Match a playlist by name and start context playback. Returns the text to print so the API
/// glue (playlist list + play request) is testable.
async fn exec_play(spotify: &AuthCodePkceSpotify, query: &str) -> Result<String> {
    let page = spotify
        .current_user_playlists_manual(Some(PAGE_LIMIT), None)
        .await
        .context("failed to fetch the playlist list")?;

    let playlists = &page.items;
    if playlists.is_empty() {
        return Ok("No playlists".to_string());
    }

    // If we only looked at the first page, note that on a no-match.
    let truncated = (page.total as usize) > playlists.len();
    let names: Vec<&str> = playlists.iter().map(|p| p.name.as_str()).collect();

    let msg = match match_name(&names, query) {
        NameMatch::Found(i) => {
            let pl = &playlists[i];
            let ctx = PlayContextId::Playlist(pl.id.as_ref());
            spotify
                .start_context_playback(ctx, None, None, None)
                .await
                .with_context(|| format!("failed to start playlist playback{NEED_DEVICE_HINT}"))?;
            format!("▶ Playing: {}", pl.name)
        }
        NameMatch::None => no_match_message(query, truncated, playlists.len()),
        NameMatch::Ambiguous(idxs) => {
            let mut lines = vec![format!(
                "'{query}' matched multiple playlists. Please be more specific:"
            )];
            for i in idxs {
                lines.push(format!("  - {}", playlists[i].name));
            }
            lines.join("\n")
        }
    };

    Ok(msg)
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
    use crate::test_fixtures as fx;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mount_playlists(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/me/playlists"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn exec_ls_reports_empty() {
        let server = MockServer::start().await;
        mount_playlists(&server, fx::empty_page()).await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert_eq!(exec_ls(&client).await.unwrap(), "No playlists");
    }

    #[tokio::test]
    async fn exec_ls_lists_playlists() {
        let server = MockServer::start().await;
        mount_playlists(
            &server,
            fx::page(
                vec![fx::simplified_playlist(
                    "37i9dQZF1DXcBWIGoYBM5M",
                    "My Mix",
                    12,
                )],
                1,
            ),
        )
        .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = exec_ls(&client).await.unwrap();
        assert!(out.contains("Playlists:"), "{out}");
        assert!(out.contains("My Mix"), "{out}");
        assert!(out.contains("12 tracks"), "{out}");
    }

    #[tokio::test]
    async fn exec_play_reports_no_match() {
        let server = MockServer::start().await;
        mount_playlists(
            &server,
            fx::page(
                vec![fx::simplified_playlist(
                    "37i9dQZF1DXcBWIGoYBM5M",
                    "My Mix",
                    12,
                )],
                1,
            ),
        )
        .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = exec_play(&client, "Nonexistent").await.unwrap();
        assert!(out.contains("No playlist matching 'Nonexistent'"), "{out}");
    }

    #[tokio::test]
    async fn exec_play_starts_matched_playlist() {
        let server = MockServer::start().await;
        mount_playlists(
            &server,
            fx::page(
                vec![fx::simplified_playlist(
                    "37i9dQZF1DXcBWIGoYBM5M",
                    "My Mix",
                    12,
                )],
                1,
            ),
        )
        .await;
        // start_context_playback issues PUT /me/player/play; assert it fires exactly once.
        Mock::given(method("PUT"))
            .and(path("/me/player/play"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = exec_play(&client, "My Mix").await.unwrap();
        assert_eq!(out, "▶ Playing: My Mix");
    }

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
