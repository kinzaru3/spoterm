//! Playback controls. Send operations to the active device.
//! When no device is selected (none active), prompt the user to run `device use`.

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{Offset, PlayableId, SearchResult, SearchType};
use rspotify::prelude::*;

use super::NEED_DEVICE_HINT;
use crate::auth;
use crate::config::Config;
use crate::format::join_artists;

/// How many search matches `play <query>` queues so that `next`/`prev` have somewhere to go.
/// The Search API `limit` maxes out at 10 (see `docs/design/search.md`), so stay within that.
const PLAY_QUEUE_LIMIT: u32 = 10;

pub async fn pause(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", exec_pause(&spotify).await?);
    Ok(())
}

async fn exec_pause(spotify: &AuthCodePkceSpotify) -> Result<String> {
    spotify
        .pause_playback(None)
        .await
        .with_context(|| format!("failed to pause{NEED_DEVICE_HINT}"))?;
    Ok("⏸ Paused".to_string())
}

pub async fn next(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", exec_next(&spotify).await?);
    Ok(())
}

async fn exec_next(spotify: &AuthCodePkceSpotify) -> Result<String> {
    spotify
        .next_track(None)
        .await
        .with_context(|| format!("failed to skip to the next track{NEED_DEVICE_HINT}"))?;
    Ok("⏭ Next track".to_string())
}

pub async fn prev(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", exec_prev(&spotify).await?);
    Ok(())
}

async fn exec_prev(spotify: &AuthCodePkceSpotify) -> Result<String> {
    spotify
        .previous_track(None)
        .await
        .with_context(|| format!("failed to skip to the previous track{NEED_DEVICE_HINT}"))?;
    Ok("⏮ Previous track".to_string())
}

pub async fn vol(cfg: &Config, level: u8) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", exec_vol(&spotify, level).await?);
    Ok(())
}

async fn exec_vol(spotify: &AuthCodePkceSpotify, level: u8) -> Result<String> {
    spotify
        .volume(level, None)
        .await
        .with_context(|| format!("failed to set volume{NEED_DEVICE_HINT}"))?;
    Ok(format!("🔊 Volume set to {level}%"))
}

pub async fn toggle(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", exec_toggle(&spotify).await?);
    Ok(())
}

async fn exec_toggle(spotify: &AuthCodePkceSpotify) -> Result<String> {
    let ctx = spotify
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("failed to fetch playback status")?;

    let msg = match ctx {
        Some(c) if c.is_playing => {
            spotify
                .pause_playback(None)
                .await
                .context("failed to pause")?;
            "⏸ Paused".to_string()
        }
        Some(_) => {
            spotify
                .resume_playback(None, None)
                .await
                .context("failed to resume playback")?;
            "▶ Resumed playback".to_string()
        }
        None => "No active device. Select one with `spotterm device use <name>`".to_string(),
    };
    Ok(msg)
}

pub async fn play(cfg: &Config, query: &[String]) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", exec_play(&spotify, query).await?);
    Ok(())
}

async fn exec_play(spotify: &AuthCodePkceSpotify, query: &[String]) -> Result<String> {
    // No arguments means resume.
    if query.is_empty() {
        spotify
            .resume_playback(None, None)
            .await
            .with_context(|| format!("failed to resume playback{NEED_DEVICE_HINT}"))?;
        return Ok("▶ Resumed playback".to_string());
    }

    // With a query, search for the top matches and play them as a queue so `next`/`prev` work.
    let q = query.join(" ");
    let result = spotify
        .search(
            &q,
            SearchType::Track,
            None,
            None,
            Some(PLAY_QUEUE_LIMIT),
            None,
        )
        .await
        .context("search failed")?;

    let SearchResult::Tracks(page) = result else {
        anyhow::bail!("unexpected search result format");
    };

    // Keep only playable tracks (those with a URI); local songs have no id and are skipped.
    // Remember the top hit's name/artists for the confirmation message.
    let mut ids: Vec<_> = Vec::new();
    let mut top: Option<(String, Vec<String>)> = None;
    for track in page.items {
        let Some(id) = track.id else { continue };
        if top.is_none() {
            let artists = track.artists.into_iter().map(|a| a.name).collect();
            top = Some((track.name, artists));
        }
        ids.push(id);
    }

    let Some((name, artists)) = top else {
        return Ok(format!("No track found matching \"{q}\""));
    };

    // Start at the top hit; the rest stay queued behind it.
    let offset = Offset::Uri(ids[0].uri());
    spotify
        .start_uris_playback(
            ids.into_iter().map(PlayableId::Track),
            None,
            Some(offset),
            None,
        )
        .await
        .with_context(|| format!("failed to start playback{NEED_DEVICE_HINT}"))?;

    Ok(format!("▶ Playing: {name} — {}", join_artists(&artists)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures as fx;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Mount a single endpoint returning 204 (the empty success shape the control APIs use).
    async fn mount_ok(server: &MockServer, http_method: &str, http_path: &str) {
        Mock::given(method(http_method))
            .and(path(http_path))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn exec_pause_sends_request_and_reports() {
        let server = MockServer::start().await;
        mount_ok(&server, "PUT", "/me/player/pause").await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert_eq!(exec_pause(&client).await.unwrap(), "⏸ Paused");
    }

    #[tokio::test]
    async fn exec_next_sends_request_and_reports() {
        let server = MockServer::start().await;
        mount_ok(&server, "POST", "/me/player/next").await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert_eq!(exec_next(&client).await.unwrap(), "⏭ Next track");
    }

    #[tokio::test]
    async fn exec_prev_sends_request_and_reports() {
        let server = MockServer::start().await;
        mount_ok(&server, "POST", "/me/player/previous").await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert_eq!(exec_prev(&client).await.unwrap(), "⏮ Previous track");
    }

    #[tokio::test]
    async fn exec_vol_sends_request_and_reports() {
        let server = MockServer::start().await;
        mount_ok(&server, "PUT", "/me/player/volume").await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert_eq!(exec_vol(&client, 30).await.unwrap(), "🔊 Volume set to 30%");
    }

    #[tokio::test]
    async fn exec_play_resumes_when_no_query() {
        let server = MockServer::start().await;
        mount_ok(&server, "PUT", "/me/player/play").await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert_eq!(exec_play(&client, &[]).await.unwrap(), "▶ Resumed playback");
    }

    #[tokio::test]
    async fn exec_play_searches_then_starts_playback() {
        let server = MockServer::start().await;
        let tracks = fx::page(
            vec![fx::full_track(
                "4iV5W9uYEdYUVa79Axb7Rh",
                "Cool Song",
                "The Artist",
            )],
            1,
        );
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "tracks": tracks })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/me/player/play"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = exec_play(&client, &["cool".to_string()]).await.unwrap();
        assert_eq!(out, "▶ Playing: Cool Song — The Artist");
    }

    #[tokio::test]
    async fn exec_toggle_reports_no_active_device() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/player"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = exec_toggle(&client).await.unwrap();
        assert!(out.contains("No active device"), "{out}");
    }

    #[tokio::test]
    async fn exec_toggle_pauses_when_playing() {
        let server = MockServer::start().await;
        // The fixture reports is_playing: true, so toggle takes the pause branch.
        Mock::given(method("GET"))
            .and(path("/me/player"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(fx::playback_unknown_track("Song", "Artist")),
            )
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/me/player/pause"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        assert_eq!(exec_toggle(&client).await.unwrap(), "⏸ Paused");
    }

    #[tokio::test]
    async fn exec_play_queues_all_matches_and_offsets_to_top() {
        let server = MockServer::start().await;
        let tracks = fx::page(
            vec![
                fx::full_track("4iV5W9uYEdYUVa79Axb7Rh", "Song One", "Artist A"),
                fx::full_track("1301WleyT98MSxVHPZCA6M", "Song Two", "Artist B"),
            ],
            2,
        );
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "tracks": tracks })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/me/player/play"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;

        let out = exec_play(&client, &["song".to_string()]).await.unwrap();
        assert_eq!(out, "▶ Playing: Song One — Artist A");

        // The play request must queue every match (so `next`/`prev` work) and start at the top hit.
        let reqs = server.received_requests().await.unwrap();
        let play = reqs
            .iter()
            .find(|r| r.url.path() == "/me/player/play")
            .expect("a play request was sent");
        let body: serde_json::Value = play.body_json().unwrap();
        let uris = body["uris"].as_array().expect("uris array");
        assert_eq!(uris.len(), 2, "all matches are queued");
        assert_eq!(uris[0], "spotify:track:4iV5W9uYEdYUVa79Axb7Rh");
        assert_eq!(
            body["offset"]["uri"],
            "spotify:track:4iV5W9uYEdYUVa79Axb7Rh"
        );
    }

    #[tokio::test]
    async fn exec_play_reports_no_track_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "tracks": fx::empty_page() })),
            )
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = exec_play(&client, &["nope".to_string()]).await.unwrap();
        assert!(out.contains("No track found matching"), "{out}");
    }
}
