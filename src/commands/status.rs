//! `spoterm status`: display the current playback (Now Playing).

use anyhow::{Context, Result};
use rspotify::model::PlayableItem;
use rspotify::prelude::*;
use serde_json::Value;

use crate::auth;
use crate::config::Config;
use crate::format::{format_ms, join_artists};

pub async fn run(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    let ctx = spotify
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("failed to fetch playback status")?;

    let Some(ctx) = ctx else {
        println!("Nothing is playing (start playback with `spoterm play`)");
        return Ok(());
    };

    let device = ctx.device.name;
    let vol = ctx.device.volume_percent;
    // Convert rspotify's Duration (chrono) into non-negative milliseconds via a method,
    // without surfacing the type name.
    let progress_ms = ctx.progress.map(|d| d.num_milliseconds().max(0) as u128);

    match ctx.item {
        Some(PlayableItem::Track(track)) => {
            let artists: Vec<String> = track.artists.into_iter().map(|a| a.name).collect();
            let line = render_track(
                ctx.is_playing,
                &track.name,
                &join_artists(&artists),
                Some(&track.album.name),
                progress_ms,
                track.duration.num_milliseconds().max(0) as u128,
                &device,
                vol,
            );
            println!("{line}");
        }
        Some(PlayableItem::Episode(ep)) => {
            let line = render_track(
                ctx.is_playing,
                &ep.name,
                "(podcast)",
                None,
                progress_ms,
                ep.duration.num_milliseconds().max(0) as u128,
                &device,
                vol,
            );
            println!("{line}");
        }
        // Spotify's /me/player does not return external_ids for tracks, so rspotify's FullTrack
        // parsing fails and falls back to Unknown(raw JSON). Extract the values we need from the
        // raw JSON and display them as a fallback.
        Some(PlayableItem::Unknown(v)) => {
            let (title, artists, album, duration_ms) = track_from_json(&v);
            let line = render_track(
                ctx.is_playing,
                &title,
                &join_artists(&artists),
                album.as_deref(),
                progress_ms,
                duration_ms,
                &device,
                vol,
            );
            println!("{line}");
        }
        None => println!("Playing, but track info is unavailable (possibly an ad, etc.)"),
    }

    Ok(())
}

/// Extract the values needed for display (title, artist names, album name, duration ms)
/// from the `/me/player` track JSON that rspotify dropped to Unknown.
/// Exposed within the crate because the TUI (`crate::tui`) uses the same fallback.
pub(crate) fn track_from_json(v: &Value) -> (String, Vec<String>, Option<String>, u128) {
    let title = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("(unknown title)")
        .to_string();
    let artists = v
        .get("artists")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let album = v
        .get("album")
        .and_then(|a| a.get("name"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let duration_ms = v.get("duration_ms").and_then(Value::as_u64).unwrap_or(0) as u128;
    (title, artists, album, duration_ms)
}

/// Extract the track URI (`spotify:track:…`) used for library operations from the
/// `/me/player` track JSON (the one dropped to Unknown). Returns `None` for
/// non-tracks (episodes, etc.) or when the URI is missing.
/// Exposed within the crate so the TUI save action (`s`) also works on the Unknown path.
pub(crate) fn track_id_from_json(v: &Value) -> Option<String> {
    v.get("uri")
        .and_then(Value::as_str)
        .filter(|uri| uri.starts_with("spotify:track:"))
        .map(str::to_string)
}

/// Extract candidate album cover-art images from the `/me/player` track JSON (dropped to
/// Unknown) as a list of `(url, width, height)`. Missing `width`/`height` default to 0.
/// Exposed within the crate so the TUI cover-art display works on the Unknown path too.
pub(crate) fn album_images_from_json(v: &Value) -> Vec<(String, u32, u32)> {
    v.get("album")
        .and_then(|a| a.get("images"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|img| {
                    let url = img.get("url").and_then(Value::as_str)?.to_string();
                    let width = img.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
                    let height = img.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
                    Some((url, width, height))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Pure function that builds the playback status display block. Mapping from the API
/// response happens on the caller side.
// Takes the display inputs as primitives to favor testability. There is only one caller and
// not enough duplication to justify a dedicated display struct, so the large argument count
// is acceptable (YAGNI).
#[allow(clippy::too_many_arguments)]
fn render_track(
    playing: bool,
    title: &str,
    artists: &str,
    album: Option<&str>,
    progress_ms: Option<u128>,
    duration_ms: u128,
    device: &str,
    vol: Option<u32>,
) -> String {
    let head = if playing { "▶ Playing" } else { "⏸ Paused" };
    let progress = match progress_ms {
        Some(p) => format!("{} / {}", format_ms(p), format_ms(duration_ms)),
        None => format!("- / {}", format_ms(duration_ms)),
    };
    let vol_s = match vol {
        Some(v) => format!("{v}%"),
        None => "-".to_string(),
    };
    let album_line = match album {
        Some(a) => format!("\n  Album    : {a}"),
        None => String::new(),
    };
    format!(
        "{head}\n  Track    : {title}\n  Artist   : {artists}{album_line}\n  Progress : {progress}\n  Device   : {device} (vol {vol_s})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_track_playing_with_progress() {
        let out = render_track(
            true,
            "Song",
            "Artist",
            Some("Album"),
            Some(83_000),
            187_000,
            "Speaker",
            Some(65),
        );
        assert!(out.starts_with("▶ Playing"));
        assert!(out.contains("Track    : Song"));
        assert!(out.contains("Artist   : Artist"));
        assert!(out.contains("Album    : Album"));
        assert!(out.contains("Progress : 1:23 / 3:07"));
        assert!(out.contains("Device   : Speaker (vol 65%)"));
    }

    #[test]
    fn render_track_paused_without_progress_or_vol() {
        let out = render_track(false, "S", "A", None, None, 60_000, "Dev", None);
        assert!(out.starts_with("⏸ Paused"));
        assert!(out.contains("Progress : - / 1:00"));
        assert!(out.contains("(vol -)"));
        // The album line is omitted
        assert!(!out.contains("Album"));
    }

    #[test]
    fn track_from_json_extracts_fields() {
        let v = serde_json::json!({
            "name": "Get Lucky",
            "artists": [{"name": "Daft Punk"}, {"name": "Pharrell Williams"}],
            "album": {"name": "Random Access Memories"},
            "duration_ms": 248_000
        });
        let (title, artists, album, dur) = track_from_json(&v);
        assert_eq!(title, "Get Lucky");
        assert_eq!(artists, vec!["Daft Punk", "Pharrell Williams"]);
        assert_eq!(album.as_deref(), Some("Random Access Memories"));
        assert_eq!(dur, 248_000);
    }

    #[test]
    fn track_from_json_falls_back_on_missing_fields() {
        let v = serde_json::json!({});
        let (title, artists, album, dur) = track_from_json(&v);
        assert_eq!(title, "(unknown title)");
        assert!(artists.is_empty());
        assert_eq!(album, None);
        assert_eq!(dur, 0);
    }

    #[test]
    fn track_id_from_json_extracts_track_uri() {
        let v = serde_json::json!({ "uri": "spotify:track:4iV5W9uYEdYUVa79Axb7Rh" });
        assert_eq!(
            track_id_from_json(&v).as_deref(),
            Some("spotify:track:4iV5W9uYEdYUVa79Axb7Rh")
        );
    }

    #[test]
    fn track_id_from_json_ignores_non_track_and_missing() {
        // An episode URI is not treated as a track
        let ep = serde_json::json!({ "uri": "spotify:episode:512ojhOuo1ktJprKbVcKyQ" });
        assert_eq!(track_id_from_json(&ep), None);
        // URI missing
        assert_eq!(track_id_from_json(&serde_json::json!({})), None);
    }

    #[test]
    fn album_images_from_json_extracts_list() {
        let v = serde_json::json!({
            "album": { "images": [
                { "url": "u640", "width": 640, "height": 640 },
                { "url": "u300", "width": 300, "height": 300 },
                { "url": "u_noWidth" }
            ]}
        });
        let imgs = album_images_from_json(&v);
        assert_eq!(imgs.len(), 3);
        assert_eq!(imgs[0], ("u640".to_string(), 640, 640));
        assert_eq!(imgs[2], ("u_noWidth".to_string(), 0, 0)); // missing width is 0
    }

    #[test]
    fn album_images_from_json_empty_when_missing() {
        assert!(album_images_from_json(&serde_json::json!({})).is_empty());
    }
}
