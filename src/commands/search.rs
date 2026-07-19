//! `spoterm search <query>`: search tracks / albums / artists and list the results.

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::SearchType;
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::format::{join_artists, render_entry, truncate};

/// Number of results fetched per type (API max is 10; kept modest at 5).
const SEARCH_LIMIT: u32 = 5;
/// Display width for names (anything longer is truncated).
const NAME_WIDTH: usize = 40;

pub async fn run(cfg: &Config, query: &[String]) -> Result<()> {
    let q = query.join(" ");
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", execute(&spotify, &q).await?);
    Ok(())
}

/// Run the multi-type search and build the result listing. Returns the text to print so the
/// API glue (request + response mapping) is testable.
async fn execute(spotify: &AuthCodePkceSpotify, q: &str) -> Result<String> {
    let result = spotify
        .search_multiple(
            q,
            [SearchType::Track, SearchType::Album, SearchType::Artist],
            None,
            None,
            Some(SEARCH_LIMIT),
            None,
        )
        .await
        .context("search failed")?;

    let mut lines: Vec<String> = Vec::new();

    if let Some(page) = result.tracks
        && !page.items.is_empty()
    {
        lines.push("🎵 Tracks".to_string());
        for (i, t) in page.items.into_iter().enumerate() {
            let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            let uri = t.id.as_ref().map(|id| id.uri()).unwrap_or_default();
            lines.push(render_entry(
                i + 1,
                &truncate(&t.name, NAME_WIDTH),
                &join_artists(&artists),
                &uri,
            ));
        }
    }

    if let Some(page) = result.albums
        && !page.items.is_empty()
    {
        lines.push("💿 Albums".to_string());
        for (i, a) in page.items.into_iter().enumerate() {
            let artists: Vec<String> = a.artists.into_iter().map(|x| x.name).collect();
            let uri = a.id.as_ref().map(|id| id.uri()).unwrap_or_default();
            lines.push(render_entry(
                i + 1,
                &truncate(&a.name, NAME_WIDTH),
                &join_artists(&artists),
                &uri,
            ));
        }
    }

    if let Some(page) = result.artists
        && !page.items.is_empty()
    {
        lines.push("🎤 Artists".to_string());
        for (i, a) in page.items.into_iter().enumerate() {
            let uri = a.id.uri();
            lines.push(render_entry(
                i + 1,
                &truncate(&a.name, NAME_WIDTH),
                "",
                &uri,
            ));
        }
    }

    if lines.is_empty() {
        return Ok(format!("No results found for \"{q}\""));
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures as fx;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mount_search(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn execute_reports_no_results() {
        let server = MockServer::start().await;
        mount_search(
            &server,
            json!({ "tracks": fx::empty_page(), "albums": fx::empty_page(), "artists": fx::empty_page() }),
        )
        .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = execute(&client, "nothing").await.unwrap();
        assert_eq!(out, "No results found for \"nothing\"");
    }

    #[tokio::test]
    async fn execute_lists_track_hits() {
        let server = MockServer::start().await;
        let tracks = fx::page(
            vec![fx::full_track(
                "4iV5W9uYEdYUVa79Axb7Rh",
                "Cool Song",
                "The Artist",
            )],
            1,
        );
        mount_search(
            &server,
            json!({ "tracks": tracks, "albums": fx::empty_page(), "artists": fx::empty_page() }),
        )
        .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = execute(&client, "cool").await.unwrap();
        assert!(out.contains("🎵 Tracks"), "{out}");
        assert!(out.contains("Cool Song"), "{out}");
        assert!(out.contains("The Artist"), "{out}");
    }
}
