//! Playback controls. Send operations to the active device.
//! When no device is selected (none active), prompt the user to run `device use`.

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
        .with_context(|| format!("failed to pause{NEED_DEVICE_HINT}"))?;
    println!("⏸ Paused");
    Ok(())
}

pub async fn next(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    spotify
        .next_track(None)
        .await
        .with_context(|| format!("failed to skip to the next track{NEED_DEVICE_HINT}"))?;
    println!("⏭ Next track");
    Ok(())
}

pub async fn prev(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    spotify
        .previous_track(None)
        .await
        .with_context(|| format!("failed to skip to the previous track{NEED_DEVICE_HINT}"))?;
    println!("⏮ Previous track");
    Ok(())
}

pub async fn vol(cfg: &Config, level: u8) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    spotify
        .volume(level, None)
        .await
        .with_context(|| format!("failed to set volume{NEED_DEVICE_HINT}"))?;
    println!("🔊 Volume set to {level}%");
    Ok(())
}

pub async fn toggle(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    let ctx = spotify
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("failed to fetch playback status")?;

    match ctx {
        Some(c) if c.is_playing => {
            spotify
                .pause_playback(None)
                .await
                .context("failed to pause")?;
            println!("⏸ Paused");
        }
        Some(_) => {
            spotify
                .resume_playback(None, None)
                .await
                .context("failed to resume playback")?;
            println!("▶ Resumed playback");
        }
        None => {
            println!("No active device. Select one with `spoterm device use <name>`");
        }
    }
    Ok(())
}

pub async fn play(cfg: &Config, query: &[String]) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    // No arguments means resume.
    if query.is_empty() {
        spotify
            .resume_playback(None, None)
            .await
            .with_context(|| format!("failed to resume playback{NEED_DEVICE_HINT}"))?;
        println!("▶ Resumed playback");
        return Ok(());
    }

    // With a query, search for the top track and play it.
    let q = query.join(" ");
    let result = spotify
        .search(&q, SearchType::Track, None, None, Some(1), None)
        .await
        .context("search failed")?;

    let SearchResult::Tracks(page) = result else {
        anyhow::bail!("unexpected search result format");
    };

    let Some(track) = page.items.into_iter().next() else {
        println!("No track found matching \"{q}\"");
        return Ok(());
    };

    // The fields are independent, so they can be moved out individually.
    let name = track.name;
    let artists: Vec<String> = track.artists.into_iter().map(|a| a.name).collect();
    let id = track
        .id
        .context("not a playable track (e.g. a local song with no URI)")?;

    spotify
        .start_uris_playback([PlayableId::Track(id)], None, None, None)
        .await
        .with_context(|| format!("failed to start playback{NEED_DEVICE_HINT}"))?;

    println!("▶ Playing: {name} — {}", join_artists(&artists));
    Ok(())
}
