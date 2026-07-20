//! Pure helpers that extract Now Playing fields from the raw `/me/player` track JSON.
//!
//! Spotify's `/me/player` does not return `external_ids` for tracks, so rspotify's
//! `FullTrack` parsing fails and drops the item to `PlayableItem::Unknown(raw JSON)`.
//! These helpers read the values we need straight from that raw JSON. They are shared
//! by the TUI (`crate::tui`), which renders Now Playing, saves the current track, and
//! displays cover art on the same Unknown path.

use serde_json::Value;

/// Extract the values needed for display (title, artist names, album name, duration ms)
/// from the `/me/player` track JSON that rspotify dropped to Unknown.
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
pub(crate) fn track_id_from_json(v: &Value) -> Option<String> {
    v.get("uri")
        .and_then(Value::as_str)
        .filter(|uri| uri.starts_with("spotify:track:"))
        .map(str::to_string)
}

/// Extract candidate album cover-art images from the `/me/player` track JSON (dropped to
/// Unknown) as a list of `(url, width, height)`. Missing `width`/`height` default to 0.
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

#[cfg(test)]
mod tests {
    use super::*;

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
