//! Library panel data model (issue #26 dashboard redesign). Drives the always-visible lower-left
//! library pane: per-tab data fetching, playback, and selection logic. It does not touch `App`;
//! rendering and key wiring live in `mod.rs`, row formatting in `view.rs`. Replaces the former
//! browse *overlay* — the library is now a first-class dashboard pane rather than a modal.

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{AlbumId, ArtistId, PlayContextId, PlayableId, PlaylistId, TrackId};
use rspotify::prelude::*;

use crate::auth;
use crate::format::join_artists;

/// Number of playlists fetched (API max 50, first page only).
const PLAYLIST_LIMIT: u32 = 50;
/// Number of saved tracks/albums fetched (first page only).
const SAVED_LIMIT: u32 = 20;
/// Number of followed artists fetched (first page only).
const FOLLOWED_LIMIT: u32 = 20;
/// Per-category cap when building the combined `All` tab, so the merged list stays scannable and the
/// startup fetch stays cheap (the dedicated tabs show more of each category).
const ALL_SECTION_LIMIT: u32 = 8;

/// Library tab. Order matches the mock header `[All][Artists][Albums][Playlists][Tracks]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseTab {
    All,
    Artists,
    Albums,
    Playlists,
    Tracks,
}

impl BrowseTab {
    /// All tabs in header order (the basis for display and `[`/`]` switching).
    pub const ALL: [BrowseTab; 5] = [
        BrowseTab::All,
        BrowseTab::Artists,
        BrowseTab::Albums,
        BrowseTab::Playlists,
        BrowseTab::Tracks,
    ];

    /// Short label shown in the tab header.
    pub fn label(self) -> &'static str {
        match self {
            BrowseTab::All => "All",
            BrowseTab::Artists => "Artists",
            BrowseTab::Albums => "Albums",
            BrowseTab::Playlists => "Playlists",
            BrowseTab::Tracks => "Tracks",
        }
    }

    /// The next tab (wraps around), used by `]`.
    pub fn next(self) -> Self {
        let i = Self::index(self);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    /// The previous tab (wraps around), used by `[`.
    pub fn prev(self) -> Self {
        let i = Self::index(self);
        Self::ALL[(i + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    fn index(self) -> usize {
        // ALL is exhaustive, so `self` is always present.
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// How to play an item. Tracks play a single URI; the rest use context playback.
#[derive(Clone)]
pub enum PlayTarget {
    Track(String),
    Playlist(String),
    Album(String),
    Artist(String),
}

impl PlayTarget {
    /// The Spotify URI backing this target. Used as the detail pane's cache key so switching the
    /// library selection back to a previously viewed item reuses its fetched detail.
    pub fn uri(&self) -> &str {
        match self {
            PlayTarget::Track(u)
            | PlayTarget::Playlist(u)
            | PlayTarget::Album(u)
            | PlayTarget::Artist(u) => u,
        }
    }
}

/// One playable entry in the library list.
#[derive(Clone)]
pub struct BrowseItem {
    pub title: String,
    pub subtitle: String,
    pub target: PlayTarget,
}

/// A row in the library list. Only `Item` rows are selectable/playable; `Header` rows label a
/// section (used by the combined `All` tab) and are skipped during selection so the highlight never
/// lands on a non-playable line.
#[derive(Clone)]
pub enum LibraryRow {
    Header(String),
    Item(BrowseItem),
}

impl LibraryRow {
    pub fn is_selectable(&self) -> bool {
        matches!(self, LibraryRow::Item(_))
    }
}

/// The result of loading a tab: the rows plus an optional non-fatal note (e.g. the `All` tab loaded
/// but one category failed). The note is surfaced to the user so a partial failure is never silent,
/// and is cached alongside the rows so it survives tab switches (a persistent partial failure keeps
/// being reported, not just on the first load).
#[derive(Clone)]
pub struct Loaded {
    pub rows: Vec<LibraryRow>,
    pub note: Option<String>,
}

/// Cache the per-tab fetch results within the session. Switching tabs does not re-fetch; only an
/// explicit reload discards and re-fetches, keeping API calls low while browsing. An empty list is a
/// valid cache (it still means "already fetched").
#[derive(Default)]
pub struct BrowseCache {
    all: Option<Loaded>,
    artists: Option<Loaded>,
    albums: Option<Loaded>,
    playlists: Option<Loaded>,
    tracks: Option<Loaded>,
}

impl BrowseCache {
    fn slot(&mut self, tab: BrowseTab) -> &mut Option<Loaded> {
        match tab {
            BrowseTab::All => &mut self.all,
            BrowseTab::Artists => &mut self.artists,
            BrowseTab::Albums => &mut self.albums,
            BrowseTab::Playlists => &mut self.playlists,
            BrowseTab::Tracks => &mut self.tracks,
        }
    }

    /// Return the cached load (rows + note) if present (an empty list is a valid cache).
    pub fn get(&self, tab: BrowseTab) -> Option<&Loaded> {
        match tab {
            BrowseTab::All => self.all.as_ref(),
            BrowseTab::Artists => self.artists.as_ref(),
            BrowseTab::Albums => self.albums.as_ref(),
            BrowseTab::Playlists => self.playlists.as_ref(),
            BrowseTab::Tracks => self.tracks.as_ref(),
        }
    }

    pub fn set(&mut self, tab: BrowseTab, loaded: Loaded) {
        *self.slot(tab) = Some(loaded);
    }

    /// Discard the cache for the given tab (forces the next fetch).
    pub fn clear(&mut self, tab: BrowseTab) {
        *self.slot(tab) = None;
    }
}

/// State of the always-visible library pane.
pub struct LibraryState {
    pub tab: BrowseTab,
    pub rows: Vec<LibraryRow>,
    pub selected: usize,
    pub message: Option<String>,
}

impl Default for LibraryState {
    fn default() -> Self {
        // Starts empty with a loading note so the first frame is never silently blank before the
        // initial fetch completes.
        Self {
            tab: BrowseTab::All,
            rows: Vec::new(),
            selected: 0,
            message: Some("Loading…".to_string()),
        }
    }
}

impl LibraryState {
    /// Replace the rows, snap the selection to the first selectable row, and set the message.
    pub fn set_rows(&mut self, rows: Vec<LibraryRow>, message: Option<String>) {
        self.selected = first_selectable(&rows).unwrap_or(0);
        self.rows = rows;
        self.message = message;
    }

    /// The currently selected item (`None` when the selection is on a header or the list is empty).
    pub fn selected_item(&self) -> Option<&BrowseItem> {
        match self.rows.get(self.selected) {
            Some(LibraryRow::Item(item)) => Some(item),
            _ => None,
        }
    }

    /// Move the selection to the next selectable row (stays put if there is none below).
    pub fn select_next(&mut self) {
        if let Some(i) = next_selectable(&self.rows, self.selected) {
            self.selected = i;
        }
    }

    /// Move the selection to the previous selectable row (stays put if there is none above).
    pub fn select_prev(&mut self) {
        if let Some(i) = prev_selectable(&self.rows, self.selected) {
            self.selected = i;
        }
    }
}

/// Index of the first selectable row, if any. Shared with the search pane (same `LibraryRow` model).
pub(crate) fn first_selectable(rows: &[LibraryRow]) -> Option<usize> {
    rows.iter().position(LibraryRow::is_selectable)
}

/// Index of the first selectable row strictly after `from`, if any.
pub(crate) fn next_selectable(rows: &[LibraryRow], from: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .skip(from + 1)
        .find(|(_, r)| r.is_selectable())
        .map(|(i, _)| i)
}

/// Index of the last selectable row strictly before `from`, if any.
pub(crate) fn prev_selectable(rows: &[LibraryRow], from: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .take(from)
        .rev()
        .find(|(_, r)| r.is_selectable())
        .map(|(i, _)| i)
}

/// Fetch the rows for the given tab (first page only). Borrows the client the caller keeps alive and
/// refreshes the token only when needed. A dedicated tab hard-fails on error (the caller shows the
/// message); `All` degrades to a partial list with a note instead.
pub async fn fetch(spotify: &AuthCodePkceSpotify, tab: BrowseTab) -> Result<Loaded> {
    auth::ensure_fresh_token(spotify).await?;
    match tab {
        BrowseTab::All => fetch_all(spotify).await,
        BrowseTab::Artists => Ok(items_only(fetch_artists(spotify, FOLLOWED_LIMIT).await?)),
        BrowseTab::Albums => Ok(items_only(fetch_albums(spotify, SAVED_LIMIT).await?)),
        BrowseTab::Playlists => Ok(items_only(fetch_playlists(spotify, PLAYLIST_LIMIT).await?)),
        BrowseTab::Tracks => Ok(items_only(fetch_tracks(spotify, SAVED_LIMIT).await?)),
    }
}

/// Wrap a flat item list as selectable rows with no header and no note.
fn items_only(items: Vec<BrowseItem>) -> Loaded {
    Loaded {
        rows: items.into_iter().map(LibraryRow::Item).collect(),
        note: None,
    }
}

/// One section of the combined `All` tab: the uppercase row header, the human label used in a
/// failure note, and the fetch result for that category.
type Section = (&'static str, &'static str, Result<Vec<BrowseItem>>);

/// Build the combined `All` tab. Fetches the four categories concurrently (so startup waits on one
/// round-trip, not four in series), then folds them with the pure [`build_all`] so the aggregation —
/// including the partial-failure note and the all-failed hard error — is unit-testable.
async fn fetch_all(spotify: &AuthCodePkceSpotify) -> Result<Loaded> {
    let (artists, albums, playlists, tracks) = tokio::join!(
        fetch_artists(spotify, ALL_SECTION_LIMIT),
        fetch_albums(spotify, ALL_SECTION_LIMIT),
        fetch_playlists(spotify, ALL_SECTION_LIMIT),
        fetch_tracks(spotify, ALL_SECTION_LIMIT),
    );
    build_all(vec![
        ("ARTISTS", "Artists", artists),
        ("ALBUMS", "Albums", albums),
        ("PLAYLISTS", "Playlists", playlists),
        ("TRACKS", "Tracks", tracks),
    ])
}

/// Fold fetched sections into the `All` rows: each non-empty category under its header, failures
/// collected into a note. Pure (no I/O) so the branch logic is tested directly. If *every* category
/// errored (nothing loaded and at least one failure) it is a hard error, so the caller shows a clear
/// "failed to fetch" status rather than a bare "empty" note.
fn build_all(sections: Vec<Section>) -> Result<Loaded> {
    let mut rows: Vec<LibraryRow> = Vec::new();
    let mut failed: Vec<&str> = Vec::new();
    for (header, label, result) in sections {
        match result {
            Ok(items) if !items.is_empty() => {
                rows.push(LibraryRow::Header(header.to_string()));
                rows.extend(items.into_iter().map(LibraryRow::Item));
            }
            Ok(_) => {}
            Err(_) => failed.push(label),
        }
    }

    if rows.is_empty() && !failed.is_empty() {
        anyhow::bail!("failed to load the library ({})", failed.join(", "));
    }

    let note = (!failed.is_empty()).then(|| format!("could not load: {}", failed.join(", ")));
    Ok(Loaded { rows, note })
}

async fn fetch_playlists(spotify: &AuthCodePkceSpotify, limit: u32) -> Result<Vec<BrowseItem>> {
    let page = spotify
        .current_user_playlists_manual(Some(limit), None)
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

async fn fetch_tracks(spotify: &AuthCodePkceSpotify, limit: u32) -> Result<Vec<BrowseItem>> {
    let page = spotify
        .current_user_saved_tracks_manual(None, Some(limit), None)
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

async fn fetch_albums(spotify: &AuthCodePkceSpotify, limit: u32) -> Result<Vec<BrowseItem>> {
    let page = spotify
        .current_user_saved_albums_manual(None, Some(limit), None)
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

/// Followed artists. Needs the `user-follow-read` scope; without it Spotify returns 403 and this
/// errors, surfaced to the user (Artists tab) or noted (All tab) — never silent.
async fn fetch_artists(spotify: &AuthCodePkceSpotify, limit: u32) -> Result<Vec<BrowseItem>> {
    let page = spotify
        .current_user_followed_artists(None, Some(limit))
        .await
        .context("failed to fetch followed artists")?;
    Ok(page
        .items
        .into_iter()
        .map(|a| BrowseItem {
            title: a.name,
            subtitle: "Artist".to_string(),
            target: PlayTarget::Artist(a.id.uri()),
        })
        .collect())
}

/// Play the given target. Tracks use a single URI; the rest use context playback.
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
        PlayTarget::Artist(uri) => {
            let id = ArtistId::from_uri(uri).context("failed to parse the artist URI")?;
            spotify
                .start_context_playback(PlayContextId::Artist(id), None, None, None)
                .await
        }
    };
    result.context("failed to start playback (an active device may be required)")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str) -> LibraryRow {
        LibraryRow::Item(BrowseItem {
            title: title.to_string(),
            subtitle: String::new(),
            target: PlayTarget::Track(format!("spotify:track:{title}")),
        })
    }

    fn header(text: &str) -> LibraryRow {
        LibraryRow::Header(text.to_string())
    }

    fn bi(title: &str) -> BrowseItem {
        BrowseItem {
            title: title.to_string(),
            subtitle: String::new(),
            target: PlayTarget::Track(format!("spotify:track:{title}")),
        }
    }

    fn row_titles(rows: &[LibraryRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                LibraryRow::Header(h) => format!("#{h}"),
                LibraryRow::Item(it) => it.title.clone(),
            })
            .collect()
    }

    #[test]
    fn tab_next_wraps_in_header_order() {
        assert_eq!(BrowseTab::All.next(), BrowseTab::Artists);
        assert_eq!(BrowseTab::Tracks.next(), BrowseTab::All);
    }

    #[test]
    fn tab_prev_wraps_in_header_order() {
        assert_eq!(BrowseTab::All.prev(), BrowseTab::Tracks);
        assert_eq!(BrowseTab::Artists.prev(), BrowseTab::All);
    }

    #[test]
    fn first_selectable_skips_leading_header() {
        let rows = vec![header("ARTISTS"), item("a"), item("b")];
        assert_eq!(first_selectable(&rows), Some(1));
    }

    #[test]
    fn first_selectable_none_when_all_headers() {
        let rows = vec![header("ARTISTS"), header("ALBUMS")];
        assert_eq!(first_selectable(&rows), None);
    }

    #[test]
    fn next_and_prev_selectable_skip_headers() {
        // idx: 0 header, 1 item, 2 header, 3 item
        let rows = vec![header("A"), item("a"), header("B"), item("b")];
        assert_eq!(next_selectable(&rows, 1), Some(3));
        assert_eq!(next_selectable(&rows, 3), None);
        assert_eq!(prev_selectable(&rows, 3), Some(1));
        assert_eq!(prev_selectable(&rows, 1), None);
    }

    #[test]
    fn set_rows_snaps_selection_to_first_item() {
        let mut state = LibraryState::default();
        state.set_rows(vec![header("A"), item("a"), item("b")], None);
        assert_eq!(state.selected, 1);
        assert_eq!(state.selected_item().map(|i| i.title.as_str()), Some("a"));
    }

    #[test]
    fn select_next_and_prev_skip_headers_and_clamp() {
        let mut state = LibraryState::default();
        state.set_rows(vec![header("A"), item("a"), header("B"), item("b")], None);
        assert_eq!(state.selected, 1);
        state.select_next();
        assert_eq!(state.selected, 3); // skipped the header at index 2
        state.select_next();
        assert_eq!(state.selected, 3); // clamps at the last item
        state.select_prev();
        assert_eq!(state.selected, 1);
        state.select_prev();
        assert_eq!(state.selected, 1); // clamps at the first item
    }

    #[test]
    fn selected_item_none_on_header() {
        // Force the selection onto a header to prove headers are never "played".
        let state = LibraryState {
            rows: vec![header("A"), item("a")],
            selected: 0,
            ..LibraryState::default()
        };
        assert!(state.selected_item().is_none());
    }

    #[test]
    fn build_all_groups_nonempty_sections_under_headers() {
        let loaded = build_all(vec![
            ("ARTISTS", "Artists", Ok(vec![bi("Radiohead")])),
            ("ALBUMS", "Albums", Ok(vec![bi("In Rainbows")])),
            ("PLAYLISTS", "Playlists", Ok(vec![])),
            ("TRACKS", "Tracks", Ok(vec![bi("15 Step")])),
        ])
        .unwrap();
        assert_eq!(
            row_titles(&loaded.rows),
            vec![
                "#ARTISTS",
                "Radiohead",
                "#ALBUMS",
                "In Rainbows",
                "#TRACKS",
                "15 Step"
            ],
        );
        // Empty Playlists section contributes no header, and no failure note.
        assert!(loaded.note.is_none());
    }

    #[test]
    fn build_all_notes_partial_failure_but_keeps_rows() {
        let loaded = build_all(vec![
            ("ARTISTS", "Artists", Err(anyhow::anyhow!("403"))),
            ("ALBUMS", "Albums", Ok(vec![bi("In Rainbows")])),
            ("PLAYLISTS", "Playlists", Ok(vec![])),
            ("TRACKS", "Tracks", Ok(vec![bi("15 Step")])),
        ])
        .unwrap();
        assert_eq!(loaded.note.as_deref(), Some("could not load: Artists"));
        assert!(
            loaded
                .rows
                .iter()
                .any(|r| matches!(r, LibraryRow::Header(h) if h == "ALBUMS"))
        );
    }

    #[test]
    fn build_all_hard_errors_only_when_everything_fails() {
        let err = build_all(vec![
            ("ARTISTS", "Artists", Err(anyhow::anyhow!("x"))),
            ("ALBUMS", "Albums", Err(anyhow::anyhow!("x"))),
            ("PLAYLISTS", "Playlists", Err(anyhow::anyhow!("x"))),
            ("TRACKS", "Tracks", Err(anyhow::anyhow!("x"))),
        ]);
        assert!(err.is_err());
    }

    #[test]
    fn build_all_empty_with_no_errors_is_ok_and_noteless() {
        // All categories legitimately empty (a brand-new account): not an error, no note.
        let loaded = build_all(vec![
            ("ARTISTS", "Artists", Ok(vec![])),
            ("ALBUMS", "Albums", Ok(vec![])),
            ("PLAYLISTS", "Playlists", Ok(vec![])),
            ("TRACKS", "Tracks", Ok(vec![])),
        ])
        .unwrap();
        assert!(loaded.rows.is_empty());
        assert!(loaded.note.is_none());
    }
}
