//! Library browse overlay. Switch between playlists / saved tracks / saved albums with tabs to
//! list and play them. It does not touch `App`; it only handles data fetching, playback, and
//! key→action conversion (screen-state updates and rendering are on the `mod.rs` side). It reuses
//! the same API as the existing `playlist`/`lib` commands.

use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{AlbumId, PlayContextId, PlayableId, PlaylistId, TrackId};
use rspotify::prelude::*;

use crate::auth;
use crate::format::join_artists;

/// Number of playlists fetched (API max 50, first page only).
const PLAYLIST_LIMIT: u32 = 50;
/// Number of saved tracks/albums fetched (first page only).
const SAVED_LIMIT: u32 = 20;

/// Browse tab.
#[derive(Clone, Copy, PartialEq)]
pub enum BrowseTab {
    Playlists,
    Tracks,
    Albums,
}

impl BrowseTab {
    /// All tabs (the basis for header display and switching).
    pub const ALL: [BrowseTab; 3] = [BrowseTab::Playlists, BrowseTab::Tracks, BrowseTab::Albums];

    pub fn label(self) -> &'static str {
        match self {
            BrowseTab::Playlists => "Playlists",
            BrowseTab::Tracks => "Saved Tracks",
            BrowseTab::Albums => "Saved Albums",
        }
    }

    pub fn next(self) -> Self {
        match self {
            BrowseTab::Playlists => BrowseTab::Tracks,
            BrowseTab::Tracks => BrowseTab::Albums,
            BrowseTab::Albums => BrowseTab::Playlists,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            BrowseTab::Playlists => BrowseTab::Albums,
            BrowseTab::Tracks => BrowseTab::Playlists,
            BrowseTab::Albums => BrowseTab::Tracks,
        }
    }
}

/// How to play. Tracks play a single URI; playlists/albums use context playback.
#[derive(Clone)]
pub enum PlayTarget {
    Track(String),
    Playlist(String),
    Album(String),
}

/// One item in the list.
#[derive(Clone)]
pub struct BrowseItem {
    pub title: String,
    pub subtitle: String,
    pub target: PlayTarget,
}

/// Cache the per-tab fetch results within the session. Switching tabs does not re-fetch;
/// only `r` (reload) discards and re-fetches, keeping API calls low while browsing.
#[derive(Default)]
pub struct BrowseCache {
    playlists: Option<Vec<BrowseItem>>,
    tracks: Option<Vec<BrowseItem>>,
    albums: Option<Vec<BrowseItem>>,
}

impl BrowseCache {
    fn slot(&mut self, tab: BrowseTab) -> &mut Option<Vec<BrowseItem>> {
        match tab {
            BrowseTab::Playlists => &mut self.playlists,
            BrowseTab::Tracks => &mut self.tracks,
            BrowseTab::Albums => &mut self.albums,
        }
    }

    /// Return the cached list if present (an empty list is treated as a valid cache).
    pub fn get(&self, tab: BrowseTab) -> Option<&Vec<BrowseItem>> {
        match tab {
            BrowseTab::Playlists => self.playlists.as_ref(),
            BrowseTab::Tracks => self.tracks.as_ref(),
            BrowseTab::Albums => self.albums.as_ref(),
        }
    }

    pub fn set(&mut self, tab: BrowseTab, items: Vec<BrowseItem>) {
        *self.slot(tab) = Some(items);
    }

    /// Discard the cache for the given tab (called on reload to force the next fetch).
    pub fn clear(&mut self, tab: BrowseTab) {
        *self.slot(tab) = None;
    }
}

/// State of the browse overlay.
pub struct BrowseState {
    pub tab: BrowseTab,
    pub items: Vec<BrowseItem>,
    pub selected: usize,
    pub message: Option<String>,
}

/// Actions the key handler asks the main body to perform.
pub enum BrowseAction {
    None,
    /// Close the overlay.
    Close,
    /// Switch tabs (does not re-fetch if cached).
    Switch(BrowseTab),
    /// Play the selected item.
    Play,
    /// Discard the current tab's cache and re-fetch.
    Reload,
}

/// Update the selection position in place and return the required action.
pub fn key_action(key: KeyEvent, state: &mut BrowseState) -> BrowseAction {
    match key.code {
        KeyCode::Esc => BrowseAction::Close,
        KeyCode::Left => BrowseAction::Switch(state.tab.prev()),
        KeyCode::Right => BrowseAction::Switch(state.tab.next()),
        KeyCode::Up => {
            state.selected = state.selected.saturating_sub(1);
            BrowseAction::None
        }
        KeyCode::Down => {
            if state.selected + 1 < state.items.len() {
                state.selected += 1;
            }
            BrowseAction::None
        }
        KeyCode::Enter => BrowseAction::Play,
        KeyCode::Char('r') => BrowseAction::Reload,
        _ => BrowseAction::None,
    }
}

/// Fetch the list for the given tab (same API as the existing commands, first page only).
/// Borrows the client the caller keeps alive and refreshes the token only when needed.
pub async fn fetch(spotify: &AuthCodePkceSpotify, tab: BrowseTab) -> Result<Vec<BrowseItem>> {
    auth::ensure_fresh_token(spotify).await?;
    match tab {
        BrowseTab::Playlists => {
            let page = spotify
                .current_user_playlists_manual(Some(PLAYLIST_LIMIT), None)
                .await
                .context("failed to fetch the playlist list")?;
            Ok(page
                .items
                .into_iter()
                .map(|pl| BrowseItem {
                    title: pl.name,
                    subtitle: format!("{} tracks", pl.items.total),
                    target: PlayTarget::Playlist(pl.id.uri()),
                })
                .collect())
        }
        BrowseTab::Tracks => {
            let page = spotify
                .current_user_saved_tracks_manual(None, Some(SAVED_LIMIT), None)
                .await
                .context("failed to fetch saved tracks")?;
            Ok(page
                .items
                .into_iter()
                .filter_map(|saved| {
                    let t = saved.track;
                    let uri = t.id.as_ref()?.uri();
                    let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
                    Some(BrowseItem {
                        title: t.name,
                        subtitle: join_artists(&artists),
                        target: PlayTarget::Track(uri),
                    })
                })
                .collect())
        }
        BrowseTab::Albums => {
            let page = spotify
                .current_user_saved_albums_manual(None, Some(SAVED_LIMIT), None)
                .await
                .context("failed to fetch saved albums")?;
            Ok(page
                .items
                .into_iter()
                .map(|saved| {
                    let a = saved.album;
                    let artists: Vec<String> = a.artists.into_iter().map(|x| x.name).collect();
                    BrowseItem {
                        title: a.name,
                        subtitle: join_artists(&artists),
                        target: PlayTarget::Album(a.id.uri()),
                    }
                })
                .collect())
        }
    }
}

/// Play the selected item. Tracks use a single URI; playlists/albums use context playback.
pub async fn play(spotify: &AuthCodePkceSpotify, target: &PlayTarget) -> Result<()> {
    auth::ensure_fresh_token(spotify).await?;
    let result = match target {
        PlayTarget::Track(uri) => {
            let id = TrackId::from_uri(uri).context("failed to parse the track URI")?;
            spotify
                .start_uris_playback([PlayableId::Track(id)], None, None, None)
                .await
        }
        PlayTarget::Playlist(uri) => {
            let id = PlaylistId::from_uri(uri).context("failed to parse the playlist URI")?;
            spotify
                .start_context_playback(PlayContextId::Playlist(id), None, None, None)
                .await
        }
        PlayTarget::Album(uri) => {
            let id = AlbumId::from_uri(uri).context("failed to parse the album URI")?;
            spotify
                .start_context_playback(PlayContextId::Album(id), None, None, None)
                .await
        }
    };
    result.context("failed to start playback (an active device may be required)")?;
    Ok(())
}
