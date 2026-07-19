//! `spoterm search <query>`: search tracks / albums / artists and list the results.

use anyhow::{Context, Result};
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
        .context("search failed")?;

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
                render_entry(
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
                render_entry(
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
                render_entry(i + 1, &truncate(&a.name, NAME_WIDTH), "", &uri)
            );
        }
    }

    if !printed {
        println!("No results found for \"{q}\"");
    }

    Ok(())
}
