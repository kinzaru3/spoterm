//! Search dashboard (issue #26 Phase 5). Runs a multi-type Spotify search and classifies the hits
//! into the mock's `TOP / SONGS / ARTISTS / ALBUMS` categories. It reuses the library pane's row
//! model (`browse::LibraryRow` / `BrowseItem` / `PlayTarget`) so the results render, select, and play
//! through the same machinery as the always-visible library, and the right-hand highlight detail
//! reuses `detail::fetch`. Loaders stay thin — map API models to primitives and hand off to the pure
//! formatters, like `browse.rs`. It does not touch `App`; rendering lives in `mod.rs`.

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{SearchResult, SearchType};
use rspotify::prelude::*;

use crate::auth;
use crate::format::join_artists;
use crate::tui::browse::{
    BrowseItem, LibraryRow, PlayTarget, first_selectable, next_selectable, prev_selectable,
};
use crate::tui::detail::DetailState;
use crate::tui::view;

/// Per-category cap fetched per query (one page each). Keeps the three searches cheap and the lists
/// scannable; the dedicated category tabs still show the full page while `TOP` shows only a preview.
const SEARCH_LIMIT: u32 = 10;
/// Items shown per category section under the combined `TOP` tab (a preview, like browse's `All`).
const TOP_SECTION_LIMIT: usize = 3;

/// Search result category tab. Order matches the mock header `[TOP][SONGS][ARTISTS][ALBUMS]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchTab {
    Top,
    Songs,
    Artists,
    Albums,
}

impl SearchTab {
    /// All tabs in header order (the basis for display and `[`/`]` switching).
    pub const ALL: [SearchTab; 4] = [
        SearchTab::Top,
        SearchTab::Songs,
        SearchTab::Artists,
        SearchTab::Albums,
    ];

    /// Short label shown in the tab header.
    pub fn label(self) -> &'static str {
        match self {
            SearchTab::Top => "Top",
            SearchTab::Songs => "Songs",
            SearchTab::Artists => "Artists",
            SearchTab::Albums => "Albums",
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

/// Categorized search hits, each already mapped to a playable `BrowseItem` (so the results share the
/// library pane's render/select/play path). `Default` is the empty pre-search state.
#[derive(Default)]
pub struct SearchResults {
    pub songs: Vec<BrowseItem>,
    pub artists: Vec<BrowseItem>,
    pub albums: Vec<BrowseItem>,
}

impl SearchResults {
    /// True when no category returned any hit (drives the "no results" message).
    pub fn is_empty(&self) -> bool {
        self.songs.is_empty() && self.artists.is_empty() && self.albums.is_empty()
    }

    /// The rows to display for `tab`. `Top` interleaves the three categories under section headers
    /// (only non-empty sections, each capped to a short preview), mirroring browse's `All` tab; each
    /// dedicated tab is a flat, header-less item list.
    pub fn rows(&self, tab: SearchTab) -> Vec<LibraryRow> {
        match tab {
            SearchTab::Top => self.top_rows(),
            SearchTab::Songs => items_rows(&self.songs),
            SearchTab::Artists => items_rows(&self.artists),
            SearchTab::Albums => items_rows(&self.albums),
        }
    }

    /// The combined `TOP` view: each non-empty category, capped to `TOP_SECTION_LIMIT`, under its
    /// uppercase header. Headers are non-selectable, so navigation skips them (like the `All` tab).
    fn top_rows(&self) -> Vec<LibraryRow> {
        let sections = [
            ("SONGS", &self.songs),
            ("ARTISTS", &self.artists),
            ("ALBUMS", &self.albums),
        ];
        let mut rows: Vec<LibraryRow> = Vec::new();
        for (header, items) in sections {
            if items.is_empty() {
                continue;
            }
            rows.push(LibraryRow::Header(header.to_string()));
            rows.extend(
                items
                    .iter()
                    .take(TOP_SECTION_LIMIT)
                    .cloned()
                    .map(LibraryRow::Item),
            );
        }
        rows
    }

    /// URIs of every song hit, in order — the queue used so `next`/`prev` walk the full song list
    /// after starting from the selected track (the "queue every hit" invariant).
    pub fn song_uris(&self) -> Vec<String> {
        self.songs
            .iter()
            .map(|it| it.target.uri().to_string())
            .collect()
    }

    /// The position of `uri` within the song list, if present (the play offset for track playback).
    pub fn song_index(&self, uri: &str) -> Option<usize> {
        self.songs.iter().position(|it| it.target.uri() == uri)
    }
}

/// Wrap a flat item slice as selectable rows (no header), cloning each item.
fn items_rows(items: &[BrowseItem]) -> Vec<LibraryRow> {
    items.iter().cloned().map(LibraryRow::Item).collect()
}

/// Phase of the search: typing the query, or navigating the classified results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPhase {
    Input,
    Results,
}

/// State of the search dashboard. The results and detail panes occupy the same lower-left/right
/// regions the library normally uses; `focus` selects between them (`Library` = the results list,
/// `Detail` = the highlight pane), reusing `view::Focus` rather than a parallel enum.
pub struct SearchState {
    pub query: String,
    pub phase: SearchPhase,
    pub tab: SearchTab,
    pub results: SearchResults,
    /// Rows for the current `tab`, recomputed on search and on tab switch (a small derived cache so
    /// rendering and selection do not rebuild it every frame).
    pub rows: Vec<LibraryRow>,
    pub selected: usize,
    pub focus: view::Focus,
    pub message: Option<String>,
    /// The right-hand highlight detail for the selected result (reuses the library detail model).
    pub detail: DetailState,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            phase: SearchPhase::Input,
            tab: SearchTab::Top,
            results: SearchResults::default(),
            rows: Vec::new(),
            selected: 0,
            focus: view::Focus::Library,
            message: None,
            detail: DetailState::default(),
        }
    }

    /// Populate from a completed search: keep the query, switch to the results phase, reset the tab to
    /// `Top` and focus to the results list, and snap the selection to the first selectable row. An
    /// empty result set is messaged so the pane is never silently blank.
    pub fn set_results(&mut self, query: String, results: SearchResults) {
        self.message = results
            .is_empty()
            .then(|| format!("No results for \"{query}\""));
        self.query = query;
        self.results = results;
        self.tab = SearchTab::Top;
        self.focus = view::Focus::Library;
        self.phase = SearchPhase::Results;
        self.rows = self.results.rows(self.tab);
        self.selected = first_selectable(&self.rows).unwrap_or(0);
        self.detail.clear(None);
    }

    /// Switch the category tab, rebuild the rows, and snap the selection to the first selectable row.
    pub fn set_tab(&mut self, tab: SearchTab) {
        self.tab = tab;
        self.rows = self.results.rows(tab);
        self.selected = first_selectable(&self.rows).unwrap_or(0);
    }

    /// Return to the input phase to edit the query, discarding the old results/detail so a stale list
    /// is never shown against a new query.
    pub fn back_to_input(&mut self) {
        self.phase = SearchPhase::Input;
        self.results = SearchResults::default();
        self.rows = Vec::new();
        self.selected = 0;
        self.focus = view::Focus::Library;
        self.message = None;
        self.detail.clear(None);
    }

    /// The currently selected result item (`None` on a header row or an empty list).
    pub fn selected_item(&self) -> Option<&BrowseItem> {
        match self.rows.get(self.selected) {
            Some(LibraryRow::Item(it)) => Some(it),
            _ => None,
        }
    }

    pub fn select_next(&mut self) {
        if let Some(i) = next_selectable(&self.rows, self.selected) {
            self.selected = i;
        }
    }

    pub fn select_prev(&mut self) {
        if let Some(i) = prev_selectable(&self.rows, self.selected) {
            self.selected = i;
        }
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the multi-type search and classify the hits. Fetches Track / Artist / Album concurrently (one
/// round-trip instead of three in series). Any type erroring fails the whole search (surfaced to the
/// user, never silent) — the three types share one access tier, so a failure normally means the
/// query, network, or token is bad for all of them, not one category.
pub async fn fetch(spotify: &AuthCodePkceSpotify, query: &str) -> Result<SearchResults> {
    auth::ensure_fresh_token(spotify).await?;
    let (tracks, artists, albums) = tokio::try_join!(
        search_one(spotify, query, SearchType::Track),
        search_one(spotify, query, SearchType::Artist),
        search_one(spotify, query, SearchType::Album),
    )?;
    Ok(SearchResults {
        songs: track_items(tracks)?,
        artists: artist_items(artists)?,
        albums: album_items(albums)?,
    })
}

/// One typed search call, returning the raw `SearchResult` for the caller to map by kind.
async fn search_one(
    spotify: &AuthCodePkceSpotify,
    query: &str,
    type_: SearchType,
) -> Result<SearchResult> {
    spotify
        .search(query, type_, None, None, Some(SEARCH_LIMIT), None)
        .await
        .with_context(|| format!("search failed ({type_:?})"))
}

/// Map track hits to playable items (tracks without a URI — local songs — are skipped). A result of
/// the wrong kind is a hard error, not an empty list: silently returning `Vec::new()` would disguise
/// a real failure (API/version mismatch) as "no songs found", so the whole search fails instead and
/// the caller surfaces it (never silent).
fn track_items(result: SearchResult) -> Result<Vec<BrowseItem>> {
    let SearchResult::Tracks(page) = result else {
        anyhow::bail!("unexpected search result (expected tracks)");
    };
    Ok(page
        .items
        .into_iter()
        .filter_map(|t| {
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

/// Map artist hits to playable items (context playback of the artist). A wrong-kind result is a hard
/// error (see [`track_items`]).
fn artist_items(result: SearchResult) -> Result<Vec<BrowseItem>> {
    let SearchResult::Artists(page) = result else {
        anyhow::bail!("unexpected search result (expected artists)");
    };
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

/// Map album hits to playable items (albums without an id — should not happen for search hits — are
/// skipped rather than shown as unplayable). A wrong-kind result is a hard error (see [`track_items`]).
fn album_items(result: SearchResult) -> Result<Vec<BrowseItem>> {
    let SearchResult::Albums(page) = result else {
        anyhow::bail!("unexpected search result (expected albums)");
    };
    Ok(page
        .items
        .into_iter()
        .filter_map(|a| {
            let uri = a.id.as_ref()?.uri();
            let artists: Vec<String> = a.artists.into_iter().map(|x| x.name).collect();
            Some(BrowseItem {
                title: a.name,
                subtitle: join_artists(&artists),
                target: PlayTarget::Album(uri),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, target: PlayTarget) -> BrowseItem {
        BrowseItem {
            title: title.to_string(),
            subtitle: String::new(),
            target,
        }
    }

    fn track(title: &str) -> BrowseItem {
        item(title, PlayTarget::Track(format!("spotify:track:{title}")))
    }

    fn artist(title: &str) -> BrowseItem {
        item(title, PlayTarget::Artist(format!("spotify:artist:{title}")))
    }

    fn album(title: &str) -> BrowseItem {
        item(title, PlayTarget::Album(format!("spotify:album:{title}")))
    }

    fn row_labels(rows: &[LibraryRow]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                LibraryRow::Header(h) => format!("#{h}"),
                LibraryRow::Item(it) => it.title.clone(),
            })
            .collect()
    }

    fn sample() -> SearchResults {
        SearchResults {
            songs: vec![track("s1"), track("s2"), track("s3"), track("s4")],
            artists: vec![artist("a1")],
            albums: vec![album("al1"), album("al2")],
        }
    }

    #[test]
    fn tab_next_and_prev_wrap_in_header_order() {
        assert_eq!(SearchTab::Top.next(), SearchTab::Songs);
        assert_eq!(SearchTab::Albums.next(), SearchTab::Top);
        assert_eq!(SearchTab::Top.prev(), SearchTab::Albums);
        assert_eq!(SearchTab::Songs.prev(), SearchTab::Top);
    }

    #[test]
    fn top_rows_preview_each_nonempty_category_under_headers() {
        let rows = sample().rows(SearchTab::Top);
        // Songs capped to TOP_SECTION_LIMIT (3 of 4); every category header present.
        assert_eq!(
            row_labels(&rows),
            vec![
                "#SONGS", "s1", "s2", "s3", // capped preview
                "#ARTISTS", "a1", //
                "#ALBUMS", "al1", "al2",
            ]
        );
    }

    #[test]
    fn top_rows_skip_empty_categories() {
        let results = SearchResults {
            songs: vec![track("s1")],
            artists: vec![],
            albums: vec![],
        };
        assert_eq!(
            row_labels(&results.rows(SearchTab::Top)),
            vec!["#SONGS", "s1"]
        );
    }

    #[test]
    fn dedicated_tab_is_a_flat_item_list() {
        let rows = sample().rows(SearchTab::Songs);
        assert_eq!(row_labels(&rows), vec!["s1", "s2", "s3", "s4"]);
        assert!(rows.iter().all(LibraryRow::is_selectable));
    }

    #[test]
    fn set_results_snaps_selection_past_the_top_header() {
        let mut s = SearchState::new();
        s.set_results("q".to_string(), sample());
        assert_eq!(s.phase, SearchPhase::Results);
        assert_eq!(s.tab, SearchTab::Top);
        // Row 0 is the SONGS header (not selectable); selection lands on the first item.
        assert_eq!(s.selected, 1);
        assert_eq!(s.selected_item().map(|i| i.title.as_str()), Some("s1"));
    }

    #[test]
    fn empty_results_are_messaged_not_silent() {
        let mut s = SearchState::new();
        s.set_results("zzz".to_string(), SearchResults::default());
        assert_eq!(s.message.as_deref(), Some("No results for \"zzz\""));
        assert!(s.selected_item().is_none());
    }

    #[test]
    fn set_tab_rebuilds_rows_and_snaps_selection() {
        let mut s = SearchState::new();
        s.set_results("q".to_string(), sample());
        s.set_tab(SearchTab::Artists);
        assert_eq!(s.selected, 0);
        assert_eq!(s.selected_item().map(|i| i.title.as_str()), Some("a1"));
    }

    #[test]
    fn selection_skips_headers_when_navigating_top() {
        let mut s = SearchState::new();
        s.set_results("q".to_string(), sample());
        // From s1 (idx 1) down through the songs preview, then across the ARTISTS header to a1.
        s.select_next(); // s2
        s.select_next(); // s3
        s.select_next(); // a1 (skips the #ARTISTS header)
        assert_eq!(s.selected_item().map(|i| i.title.as_str()), Some("a1"));
    }

    #[test]
    fn song_index_locates_the_selected_track_for_the_play_queue() {
        let r = sample();
        assert_eq!(r.song_uris().len(), 4);
        assert_eq!(r.song_index("spotify:track:s3"), Some(2));
        assert_eq!(r.song_index("spotify:track:missing"), None);
    }

    #[test]
    fn back_to_input_discards_results() {
        let mut s = SearchState::new();
        s.set_results("q".to_string(), sample());
        s.back_to_input();
        assert_eq!(s.phase, SearchPhase::Input);
        assert!(s.rows.is_empty());
        assert!(s.results.is_empty());
    }
}
