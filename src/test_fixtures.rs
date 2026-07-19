//! Test-only JSON fixtures shaped to deserialize into the rspotify models the commands consume.
//! Centralized here so the per-command `execute` tests (`commands::*`) share one source of truth
//! for response bodies. Values are synthetic.

use serde_json::{Value, json};

/// A `FullTrack` payload as returned by `/search` and `/me/tracks` (includes `external_ids`, so
/// rspotify parses it as a real track rather than dropping to `Unknown`).
pub(crate) fn full_track(id: &str, name: &str, artist: &str) -> Value {
    json!({
        "album": {
            "artists": [],
            "external_urls": {},
            "images": [],
            "name": "Some Album"
        },
        "artists": [{ "external_urls": {}, "name": artist }],
        "disc_number": 1,
        "duration_ms": 200_000,
        "explicit": false,
        "external_ids": {},
        "external_urls": {},
        "href": null,
        "id": id,
        "is_local": false,
        "name": name,
        "popularity": 0,
        "preview_url": null,
        "track_number": 1,
        "type": "track"
    })
}

/// A paging object wrapping `items`.
pub(crate) fn page(items: Vec<Value>, total: u32) -> Value {
    json!({
        "href": "",
        "items": items,
        "limit": 20,
        "offset": 0,
        "next": null,
        "previous": null,
        "total": total
    })
}

/// An empty paging object.
pub(crate) fn empty_page() -> Value {
    page(vec![], 0)
}

/// A `Device` payload.
pub(crate) fn device(id: &str, name: &str, is_active: bool) -> Value {
    json!({
        "id": id,
        "is_active": is_active,
        "is_private_session": false,
        "is_restricted": false,
        "name": name,
        "type": "Computer",
        "volume_percent": 40
    })
}

/// The `{ "devices": [...] }` envelope returned by `/me/player/devices`.
pub(crate) fn devices_envelope(devices: Vec<Value>) -> Value {
    json!({ "devices": devices })
}

/// A `SimplifiedPlaylist` payload as returned by `/me/playlists`.
pub(crate) fn simplified_playlist(id: &str, name: &str, total: u32) -> Value {
    json!({
        "collaborative": false,
        "external_urls": {},
        "href": "",
        "id": id,
        "images": [],
        "name": name,
        "owner": {
            "display_name": "Owner",
            "external_urls": {},
            "followers": null,
            "href": "",
            "id": "owner1",
            "images": []
        },
        "public": true,
        "snapshot_id": "snap",
        "tracks": { "href": "", "total": total },
        "items": { "href": "", "total": total }
    })
}

/// A `SavedTrack` payload (added_at + track) as returned by `/me/tracks`.
pub(crate) fn saved_track(id: &str, name: &str, artist: &str) -> Value {
    json!({
        "added_at": "2020-01-01T00:00:00Z",
        "track": full_track(id, name, artist)
    })
}

/// A `CurrentPlaybackContext` whose `item` lacks `external_ids`, so rspotify drops it to
/// `PlayableItem::Unknown(raw JSON)` — the fallback path `status`/`toggle` handle.
pub(crate) fn playback_unknown_track(name: &str, artist: &str) -> Value {
    json!({
        "device": device("d1", "Test Speaker", true),
        "repeat_state": "off",
        "shuffle_state": false,
        "context": null,
        "timestamp": 1_700_000_000_000_i64,
        "progress_ms": 83_000,
        "is_playing": true,
        "item": {
            "name": name,
            "artists": [{ "name": artist }],
            "album": { "name": "Fallback Album" },
            "duration_ms": 187_000,
            "uri": "spotify:track:4iV5W9uYEdYUVa79Axb7Rh"
        },
        "currently_playing_type": "track",
        "actions": { "disallows": {} }
    })
}
