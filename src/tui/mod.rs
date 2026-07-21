//! Interactive TUI. Displays Now Playing live and controls playback via key presses.
//!
//! - Authentication builds a client once at startup with [`crate::auth::authed_client`] and then
//!   keeps and reuses it (without discarding reqwest's connection pool or re-reading the disk on
//!   every operation). The token is refreshed with [`crate::auth::ensure_fresh_token`] only when expired.
//! - `current_playback` is fetched every `POLL_INTERVAL`, and between polls progress is interpolated
//!   locally with [`view::interpolate_progress`] to look smooth.
//! - API errors are shown on the status line and the loop continues (no silent failures).

mod art;
mod browse;
mod detail;
mod devices;
mod view;

use std::collections::HashMap;
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph};
use ratatui_image::StatefulImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{
    CurrentPlaybackContext, FullTrack, LibraryId, Offset, PlayableId, PlayableItem, SearchResult,
    SearchType, TrackId,
};
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::format::join_artists;
use crate::theme;
use view::NowPlaying;

/// Interval for re-fetching the playback status.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// One tick of the input wait (redraw at this interval and apply progress interpolation).
const TICK: Duration = Duration::from_millis(200);
/// Volume step (+/-).
const VOL_STEP: i16 = 5;
/// Seek step (←/→, milliseconds).
const SEEK_STEP_MS: i64 = 5_000;
/// Stop auto-refresh once consecutive poll failures reach this count (avoids infinite retries on an invalid token, etc.).
const MAX_POLL_FAILURES: u32 = 3;
/// Maximum number of results fetched when searching.
const SEARCH_LIMIT: u32 = 10;
/// How long a status line is shown before it is automatically cleared.
const STATUS_TTL: Duration = Duration::from_secs(4);
/// Cap on the per-item detail cache. Past this the cache is cleared wholesale (a simple bound that
/// keeps memory flat over a long session; the current selection is re-fetched on demand anyway).
const DETAIL_CACHE_MAX: usize = 64;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Screen mode. Normally the dashboard (Now Playing + always-visible library); `/` enters search,
/// `d` device selection, `?` help. The library is no longer a modal — it lives in the dashboard.
enum Mode {
    Normal,
    Search(SearchState),
    Devices(devices::DevicePickerState),
    /// Key-list help overlay (display only, no state).
    Help,
}

/// The kind of `Mode` (a data-less discriminant). Used to make key handling an exhaustive `match`,
/// so that adding a new `Mode` surfaces missing branches as compile errors (an if/else chain of
/// `matches!` that holds no borrow cannot catch such omissions).
#[derive(Clone, Copy)]
enum ModeKind {
    Normal,
    Search,
    Devices,
    Help,
}

impl Mode {
    fn kind(&self) -> ModeKind {
        match self {
            Mode::Normal => ModeKind::Normal,
            Mode::Search(_) => ModeKind::Search,
            Mode::Devices(_) => ModeKind::Devices,
            Mode::Help => ModeKind::Help,
        }
    }
}

/// State of the search overlay.
struct SearchState {
    /// The query being typed.
    query: String,
    /// Whether typing input or selecting a result.
    phase: SearchPhase,
    /// Search results (playable tracks only).
    results: Vec<TrackHit>,
    /// Selection position in the result list.
    selected: usize,
    /// Supplementary message (0 results, errors, etc.).
    message: Option<String>,
}

impl SearchState {
    fn new() -> Self {
        Self {
            query: String::new(),
            phase: SearchPhase::Input,
            results: Vec::new(),
            selected: 0,
            message: None,
        }
    }
}

/// Phase of the search overlay.
#[derive(Clone, Copy, PartialEq)]
enum SearchPhase {
    /// Typing the query.
    Input,
    /// Selecting a result.
    Results,
}

/// One track in the search results (holds the URI used to play it).
struct TrackHit {
    name: String,
    artists: String,
    uri: String,
}

/// Async actions the search key handler asks the main body to perform.
enum SearchAction {
    None,
    /// Close the overlay and return to the normal view.
    Close,
    /// Run a search with the query.
    Submit(String),
    /// Play the results as a queue, starting at `selected`, so `next`/`prev` walk the hit list.
    Play {
        uris: Vec<String>,
        selected: usize,
    },
    /// Go back from result selection to input (edit the query).
    BackToInput,
}

/// State of the TUI app.
struct App {
    /// The authenticated client built at startup and reused for the whole session.
    /// Rebuilding it on every operation would discard the connection pool and redo TLS each time
    /// (expensive), so it is kept.
    client: AuthCodePkceSpotify,
    /// The most recently fetched playback status (`None` when nothing is playing).
    now: Option<NowPlaying>,
    /// Status line showing the most recent operation result / error.
    status: String,
    /// The time of the last poll (`None` requests an immediate poll).
    last_poll: Option<Instant>,
    /// Number of consecutive poll failures. Past the threshold, auto-refresh stops and manual retry is prompted.
    poll_failures: u32,
    /// Screen mode (normal / search / library browse / device selection).
    mode: Mode,
    /// Which lower dashboard pane holds keyboard focus (library / detail). `tab` toggles it. Only the
    /// lower panes are navigable; the value clamps to `Library` when the detail pane is hidden.
    focus: view::Focus,
    /// Whether the detail pane was visible in the most recent draw (false on narrow terminals). Read
    /// by the `tab` handler so focus navigation clamps to the panes actually on screen; updated by
    /// `draw_dashboard`. Starts `false` — the first draw sets it before any key can be handled.
    detail_visible: bool,
    /// Per-tab fetch-result cache for the library pane (avoids re-fetching on tab switch).
    browse_cache: browse::BrowseCache,
    /// The always-visible library pane state (current tab, rows, selection, message).
    library: browse::LibraryState,
    /// Whether the initial library fetch has been attempted. Set once so a failed initial load does
    /// not re-fetch every tick (the user can still force a reload by switching tabs).
    library_loaded: bool,
    /// The always-visible detail pane state (tracks of the currently selected library item).
    detail: detail::DetailState,
    /// Per-library-item detail cache keyed by URI, so returning to a previously viewed selection does
    /// not re-fetch. Bounded by the number of library items browsed in a session.
    detail_cache: HashMap<String, detail::DetailData>,
    /// Whether the current track is saved in the library (`None` if undetermined). Re-fetched only on track change.
    saved: Option<bool>,
    /// Whether the current track's saved state has been queried. Query once per track to avoid
    /// hammering every poll on persistent failure (reset to `false` when the track changes).
    saved_checked: bool,
    /// HTTP client for fetching cover art (reuses the connection pool).
    http: reqwest::Client,
    /// The terminal's image-protocol detector (created once at startup).
    picker: Picker,
    /// The cover art currently displayed (rendering protocol). `None` if absent / not yet fetched.
    art: Option<StatefulProtocol>,
    /// The image URL `art` corresponds to. Re-fetched only on track change (URL change).
    art_url: Option<String>,
}

/// `spotterm tui`: launch the Now Playing dashboard.
pub async fn run(cfg: &Config) -> Result<()> {
    // If not logged in, fail clearly here and do not put the terminal into the alt-screen.
    // This client is handed straight into the loop and reused for the session.
    let client = auth::authed_client(cfg)
        .await
        .context("cannot start the TUI")?;

    // Detecting the image protocol involves querying the terminal (stdin/stdout), so do it before
    // entering the alt-screen. On terminals where detection fails, fall back to halfblocks (colored half-blocks).
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

    install_panic_hook();
    let mut terminal = setup_terminal().context("failed to initialize the terminal")?;
    let result = run_loop(&mut terminal, client, picker).await;
    // Always restore the terminal regardless of the render outcome. If both fail, report both.
    let restored = restore_terminal(&mut terminal);
    match (result, restored) {
        (Ok(()), restored) => restored,
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(re)) => {
            Err(e.context(format!("and restoring the terminal also failed: {re}")))
        }
    }
}

/// Main loop. Repeats poll → draw → input handling.
async fn run_loop(terminal: &mut Term, client: AuthCodePkceSpotify, picker: Picker) -> Result<()> {
    let mut app = App {
        client,
        now: None,
        status: "Starting…".to_string(),
        last_poll: None,
        poll_failures: 0,
        mode: Mode::Normal,
        focus: view::Focus::Library,
        detail_visible: false,
        browse_cache: browse::BrowseCache::default(),
        library: browse::LibraryState::default(),
        library_loaded: false,
        detail: detail::DetailState::default(),
        detail_cache: HashMap::new(),
        saved: None,
        saved_checked: false,
        // Impose a timeout (so a hang does not freeze the loop) and disable redirects
        // (SSRF protection: cannot be bounced to a non-allowed host). If building fails, fall back to a plain Client.
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new()),
        picker,
        art: None,
        art_url: None,
    };

    // For auto-clearing the status line (detect changes and time them. Not stored on App; handled within this loop).
    let mut last_status = app.status.clone();
    let mut status_since = Instant::now();

    loop {
        // When `last_poll` is None, force a poll (right after startup, after an operation, or `r`). A
        // timer-driven auto-refresh happens only while consecutive failures are below the threshold
        // (avoids retrying every 2 seconds on an invalid token).
        let forced = app.last_poll.is_none();
        let timer_due = app.last_poll.is_none_or(|t| t.elapsed() >= POLL_INTERVAL);
        if forced || (timer_due && app.poll_failures < MAX_POLL_FAILURES) {
            poll_playback(&mut app).await;
            app.last_poll = Some(Instant::now());
        }

        // When the status changes, reset the timer and auto-clear after a set time.
        // (The auto-refresh-stopped notice is drawn by draw_now from poll_failures, not status, so it does not disappear.)
        if app.status != last_status {
            last_status = app.status.clone();
            status_since = Instant::now();
        }
        // While failures continue (poll_failures > 0), do not clear it. This prevents an accident
        // where the same error repeats, the timer does not restart, and it disappears mid-way
        // (on recovery, poll_playback clears the stale warning).
        if app.poll_failures == 0 && !app.status.is_empty() && status_since.elapsed() >= STATUS_TTL
        {
            app.status.clear();
            last_status.clear();
        }

        terminal.draw(|frame| draw(frame, &mut app))?;

        // Load the library once, *after* the first frame is drawn, so the dashboard (with the
        // "Loading…" library note) appears immediately instead of the whole UI blocking on the
        // multi-call `All` fetch. The fetch is concurrent (see `browse::fetch_all`), so this is one
        // round-trip, comparable to the playback poll above.
        ensure_library_loaded(&mut app).await;

        // Load the detail for the current library selection (only when the selection changed; cached
        // per item). Runs after the library load so there is a selection to describe.
        ensure_detail_loaded(&mut app).await;

        // Wait for a key up to TICK (if none, redraw to advance progress).
        // On Windows it also fires on release, so handle presses only.
        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && handle_key(key, &mut app).await
        {
            break;
        }
    }
    Ok(())
}

/// Handle a key press. Returns `true` on a quit request.
async fn handle_key(key: KeyEvent, app: &mut App) -> bool {
    // Ctrl-C quits from any mode.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }
    match app.mode.kind() {
        ModeKind::Search => {
            handle_search_key(key, app).await;
            false
        }
        ModeKind::Devices => {
            handle_devices_key(key, app).await;
            false
        }
        ModeKind::Help => {
            // Help is display only. Any key closes it and returns to the normal view (Ctrl-C already quit above).
            app.mode = Mode::Normal;
            false
        }
        ModeKind::Normal => handle_normal_key(key, app).await,
    }
}

/// Key handling for normal (Now Playing) mode. Returns `true` on a quit request.
async fn handle_normal_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('/') => app.mode = Mode::Search(SearchState::new()),
        // Cycle keyboard focus between the two lower dashboard panes (library <-> detail). `next`
        // clamps to the library when the detail pane was hidden in the last draw (narrow terminal),
        // so focus never drifts to an off-screen pane.
        KeyCode::Tab => app.focus = app.focus.next(app.detail_visible),
        // Library pane navigation, active only while the library pane holds focus so the same keys
        // stay free for the (future) detail pane. `[`/`]` switch tabs, ↑↓ move the selection, Enter
        // plays. Left/Right remain seek (see below); the library uses the bracket keys for tabs.
        KeyCode::Char('[') if app.focus == view::Focus::Library => {
            load_library(app, app.library.tab.prev()).await;
        }
        KeyCode::Char(']') if app.focus == view::Focus::Library => {
            load_library(app, app.library.tab.next()).await;
        }
        KeyCode::Up if app.focus == view::Focus::Library => app.library.select_prev(),
        KeyCode::Down if app.focus == view::Focus::Library => app.library.select_next(),
        KeyCode::Enter if app.focus == view::Focus::Library => library_play(app).await,
        // Detail pane navigation, active only while the detail pane holds focus. ↑↓ move the
        // selection within the track list, Enter plays it (see `detail_play`).
        KeyCode::Up if app.focus == view::Focus::Detail => app.detail.select_prev(),
        KeyCode::Down if app.focus == view::Focus::Detail => app.detail.select_next(),
        KeyCode::Enter if app.focus == view::Focus::Detail => detail_play(app).await,
        KeyCode::Char('d') => open_devices(app).await,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char(' ') => control_toggle(app).await,
        KeyCode::Char('n') => control_next(app).await,
        KeyCode::Char('p') => control_prev(app).await,
        KeyCode::Char('+') | KeyCode::Char('=') => control_volume(app, VOL_STEP).await,
        KeyCode::Char('-') | KeyCode::Char('_') => control_volume(app, -VOL_STEP).await,
        KeyCode::Left => control_seek(app, -SEEK_STEP_MS).await,
        KeyCode::Right => control_seek(app, SEEK_STEP_MS).await,
        KeyCode::Char('s') => control_save(app).await,
        // Manual refresh: reset the failure counter and resume auto-refresh. Also clear art_url so
        // the cover art can be re-fetched (even a track whose art failed can be retried with `r` — no dead end).
        // While the library pane holds focus, also discard its current tab's cache and re-fetch, so
        // library changes (new saves, follows) are picked up without restarting the app.
        KeyCode::Char('r') => {
            app.poll_failures = 0;
            app.last_poll = None;
            app.art_url = None;
            if app.focus == view::Focus::Library {
                app.browse_cache.clear(app.library.tab);
                load_library(app, app.library.tab).await;
            } else if app.focus == view::Focus::Detail {
                // Force the detail to re-fetch: drop its cache entry and clear the key so the next
                // `ensure_detail_loaded` reloads the current selection (recovers from a failed load
                // even when the selection cannot change, e.g. a single-item library).
                if let Some(key) = app.detail.key.take() {
                    app.detail_cache.remove(&key);
                }
            }
        }
        _ => {}
    }
    false
}

/// Key handling for the search overlay. Updates the query/selection synchronously and runs the required async action.
async fn handle_search_key(key: KeyEvent, app: &mut App) {
    // First drop the borrow, then run the action (the async work re-borrows app).
    let action = {
        let Mode::Search(state) = &mut app.mode else {
            return;
        };
        search_key_action(key, state)
    };
    match action {
        SearchAction::None => {}
        SearchAction::Close => app.mode = Mode::Normal,
        SearchAction::Submit(q) => run_search(app, &q).await,
        SearchAction::Play { uris, selected } => play_selection(app, &uris, selected).await,
        SearchAction::BackToInput => {
            if let Mode::Search(state) = &mut app.mode {
                // Going back to input means rebuilding the query. Discard the old results and selection.
                state.phase = SearchPhase::Input;
                state.results.clear();
                state.selected = 0;
                state.message = None;
            }
        }
    }
}

/// Update the query/selection synchronously and return the required async action.
fn search_key_action(key: KeyEvent, state: &mut SearchState) -> SearchAction {
    match state.phase {
        SearchPhase::Input => match key.code {
            KeyCode::Esc => SearchAction::Close,
            KeyCode::Enter => {
                if state.query.trim().is_empty() {
                    SearchAction::None
                } else {
                    SearchAction::Submit(state.query.clone())
                }
            }
            KeyCode::Backspace => {
                state.query.pop();
                SearchAction::None
            }
            KeyCode::Char(c) => {
                state.query.push(c);
                SearchAction::None
            }
            _ => SearchAction::None,
        },
        SearchPhase::Results => match key.code {
            KeyCode::Esc => SearchAction::BackToInput,
            KeyCode::Up => {
                state.selected = state.selected.saturating_sub(1);
                SearchAction::None
            }
            KeyCode::Down => {
                if state.selected + 1 < state.results.len() {
                    state.selected += 1;
                }
                SearchAction::None
            }
            KeyCode::Enter => {
                if state.results.is_empty() {
                    SearchAction::None
                } else {
                    // Queue every hit so `next`/`prev` walk the result list, starting at the selection.
                    let uris = state.results.iter().map(|h| h.uri.clone()).collect();
                    SearchAction::Play {
                        uris,
                        selected: state.selected,
                    }
                }
            }
            _ => SearchAction::None,
        },
    }
}

/// Search tracks by query and transition to the results phase. On failure, stay in the input phase and inform the user.
async fn run_search(app: &mut App, q: &str) {
    match search_tracks(app, q).await {
        Ok(hits) => {
            let message = hits
                .is_empty()
                .then(|| format!("No track found matching \"{q}\""));
            app.mode = Mode::Search(SearchState {
                query: q.to_string(),
                phase: SearchPhase::Results,
                results: hits,
                selected: 0,
                message,
            });
        }
        Err(e) => {
            app.status = format!("{} search failed: {e}", theme::WARN);
            if let Mode::Search(state) = &mut app.mode {
                state.phase = SearchPhase::Input;
                state.message = Some(format!("search failed: {e}"));
            }
        }
    }
}

async fn search_tracks(app: &App, q: &str) -> Result<Vec<TrackHit>> {
    auth::ensure_fresh_token(&app.client).await?;
    let result = app
        .client
        .search(q, SearchType::Track, None, None, Some(SEARCH_LIMIT), None)
        .await
        .context("search failed")?;
    let SearchResult::Tracks(page) = result else {
        anyhow::bail!("unexpected search result format");
    };
    Ok(page.items.into_iter().filter_map(track_to_hit).collect())
}

/// Map only playable tracks (those with a URI) into `TrackHit`. Local songs, etc. are excluded.
fn track_to_hit(t: FullTrack) -> Option<TrackHit> {
    let uri = t.id.as_ref()?.uri();
    let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
    Some(TrackHit {
        name: t.name,
        artists: join_artists(&artists),
        uri,
    })
}

/// Play the search results as a queue, starting at `selected`. On success, return to the normal
/// view; on failure, stay on the overlay and inform via a message (the search screen does not draw
/// `app.status`, so use `state.message` here).
async fn play_selection(app: &mut App, uris: &[String], selected: usize) {
    match start_playback_queue(app, uris, selected).await {
        Ok(()) => {
            app.status = format!("{} Playback started", theme::PLAY);
            app.last_poll = None; // Reflect playback start on screen quickly
            app.mode = Mode::Normal;
        }
        Err(e) => {
            if let Mode::Search(state) = &mut app.mode {
                state.message = Some(format!("playback failed: {e}"));
            } else {
                app.status = format!("{} playback failed: {e}", theme::WARN);
            }
        }
    }
}

async fn start_playback_queue(app: &App, uris: &[String], selected: usize) -> Result<()> {
    let (ids, offset) = queue_from_uris(uris, selected)?;
    auth::ensure_fresh_token(&app.client).await?;
    app.client
        .start_uris_playback(ids.into_iter().map(PlayableId::Track), None, offset, None)
        .await
        .context("failed to start playback (an active device may be required)")?;
    Ok(())
}

/// Parse result URIs into track ids and compute the play offset for the selected index.
/// Queueing every hit (not just the selected one) is what gives `next`/`prev` somewhere to go.
/// A URI that fails to parse aborts the whole play rather than silently dropping a track.
fn queue_from_uris(uris: &[String], selected: usize) -> Result<(Vec<TrackId<'_>>, Option<Offset>)> {
    let ids = uris
        .iter()
        .map(|u| TrackId::from_uri(u))
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse a track URI")?;
    let offset = uris.get(selected).map(|u| Offset::Uri(u.clone()));
    Ok((ids, offset))
}

// ---- API integration --------------------------------------------------------

/// Fetch the playback status and update `app.now`. Failures are shown on the status line.
async fn poll_playback(app: &mut App) {
    // Detect recovery (failures were ongoing until just now) to clear a lingering warning.
    let was_failing = app.poll_failures > 0;
    match fetch_playback(app).await {
        Ok(Some(np)) => {
            // On track change, discard the saved state and re-fetch it next (only on change, not every poll).
            let prev_uri = app.now.as_ref().and_then(|n| n.track_uri.clone());
            if np.track_uri != prev_uri {
                app.saved = None;
                app.saved_checked = false;
            }
            app.now = Some(np);
            app.poll_failures = 0;
            // On recovery, clear only if what remains is a stale ⚠ warning (do not clear a
            // legitimate message from the user's last operation, i.e. Ok/Info).
            if was_failing && view::status_kind(&app.status) == view::StatusKind::Warn {
                app.status.clear();
            }
            refresh_saved(app).await;
            refresh_art(app).await;
        }
        Ok(None) => {
            app.now = None;
            app.saved = None;
            app.saved_checked = false;
            app.art = None;
            app.art_url = None;
            app.poll_failures = 0;
            if was_failing && view::status_kind(&app.status) == view::StatusKind::Warn {
                app.status.clear();
            }
        }
        Err(e) => {
            app.poll_failures = app.poll_failures.saturating_add(1);
            app.status = if app.poll_failures >= MAX_POLL_FAILURES {
                format!(
                    "{} auto-refresh stopped ({e}). Press r to retry / q to quit",
                    theme::WARN
                )
            } else {
                format!("{} refresh failed: {e}", theme::WARN)
            };
        }
    }
}

async fn fetch_playback(app: &App) -> Result<Option<NowPlaying>> {
    auth::ensure_fresh_token(&app.client).await?;
    let ctx = app
        .client
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("failed to fetch playback status")?;
    Ok(ctx.map(snapshot_from_context))
}

/// Map rspotify's playback context into a display snapshot.
fn snapshot_from_context(ctx: CurrentPlaybackContext) -> NowPlaying {
    let device = ctx.device.name;
    // By Spotify's contract this is 0-100, but as an external boundary, cap at 100 before casting to u8 (avoids a silent wraparound).
    let volume = ctx.device.volume_percent.map(|v| v.min(100) as u8);
    let progress_ms = ctx
        .progress
        .map(|d| d.num_milliseconds().max(0) as u128)
        .unwrap_or(0);
    let is_playing = ctx.is_playing;

    // track_uri is used for the save action and track-change detection; album_image_url for cover-art fetching.
    // Track uses the typed model; Unknown is extracted from raw JSON.
    let (title, artists, album, duration_ms, track_uri, album_image_url) = match ctx.item {
        Some(PlayableItem::Track(t)) => {
            let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            let dur = t.duration.num_milliseconds().max(0) as u128;
            let uri = t.id.as_ref().map(|id| id.uri());
            let images: Vec<(String, u32, u32)> = t
                .album
                .images
                .into_iter()
                .map(|im| (im.url, im.width.unwrap_or(0), im.height.unwrap_or(0)))
                .collect();
            let art_url = art::pick_image_url(&images);
            (t.name, artists, Some(t.album.name), dur, uri, art_url)
        }
        Some(PlayableItem::Episode(e)) => {
            let dur = e.duration.num_milliseconds().max(0) as u128;
            (e.name, vec!["(podcast)".to_string()], None, dur, None, None)
        }
        // Like the status command, extract a fallback from the raw JSON that fell to Unknown.
        Some(PlayableItem::Unknown(v)) => {
            let (title, artists, album, dur) = crate::np_json::track_from_json(&v);
            let images = crate::np_json::album_images_from_json(&v);
            (
                title,
                artists,
                album,
                dur,
                crate::np_json::track_id_from_json(&v),
                art::pick_image_url(&images),
            )
        }
        None => (
            "(no track info while playing)".to_string(),
            Vec::new(),
            None,
            0,
            None,
            None,
        ),
    };

    NowPlaying {
        is_playing,
        title,
        artists: join_artists(&artists),
        album,
        progress_ms,
        duration_ms,
        device,
        volume,
        track_uri,
        album_image_url,
        fetched_at: Instant::now(),
    }
}

/// Refresh the retained client's token if needed. On failure, show it on the status line and return `false`.
async fn ensure_ready(app: &mut App) -> bool {
    match auth::ensure_fresh_token(&app.client).await {
        Ok(()) => true,
        Err(e) => {
            app.status = format!("{} {e}", theme::WARN);
            false
        }
    }
}

/// Reflect the operation result on the status line, and on success schedule an immediate poll.
fn finish<E: std::fmt::Display>(app: &mut App, res: Result<(), E>, ok: &str) {
    match res {
        Ok(()) => {
            app.status = ok.to_string();
            app.last_poll = None; // Reflect the change on screen quickly
        }
        Err(e) => {
            app.status = format!(
                "{} operation failed: {e} (press d to select and activate a device)",
                theme::WARN
            );
        }
    }
}

async fn control_toggle(app: &mut App) {
    let playing = app.now.as_ref().is_some_and(|n| n.is_playing);
    if !ensure_ready(app).await {
        return;
    }
    // To avoid a borrow conflict, settle the result first, then pass it to finish (&mut app).
    if playing {
        let res = app.client.pause_playback(None).await;
        finish(app, res, &format!("{} Paused", theme::PAUSE));
    } else {
        let res = app.client.resume_playback(None, None).await;
        finish(app, res, &format!("{} Playing", theme::PLAY));
    }
}

async fn control_next(app: &mut App) {
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.next_track(None).await;
    finish(app, res, &format!("{} Next track", theme::NEXT));
}

async fn control_prev(app: &mut App) {
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.previous_track(None).await;
    finish(app, res, &format!("{} Previous track", theme::PREV));
}

async fn control_volume(app: &mut App, delta: i16) {
    let Some(cur) = app.now.as_ref().and_then(|n| n.volume) else {
        app.status = format!(
            "{} device volume is unavailable (press d to select a device)",
            theme::WARN
        );
        return;
    };
    let next = (cur as i16 + delta).clamp(0, 100) as u8;
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.volume(next, None).await;
    finish(app, res, &format!("{} Volume {next}%", theme::VOLUME));
}

/// Fetch the current track's saved state and update `app.saved`. Best-effort: query only when
/// `saved` is undetermined and a URI exists, and do not surface a status on failure (the main poll
/// reports network/token errors, so do not overwrite the status and confuse the user here). The
/// marker simply does not appear.
async fn refresh_saved(app: &mut App) {
    // Query only once per track (`saved_checked`). Do not hammer every poll even on persistent failure.
    if app.saved_checked {
        return;
    }
    let Some(uri) = app.now.as_ref().and_then(|n| n.track_uri.clone()) else {
        return; // Unknown track (episodes, etc.) is not queried. No API call either.
    };
    let Ok(id) = TrackId::from_uri(&uri) else {
        app.saved_checked = true;
        return;
    };
    // A token-refresh failure is not a hard stop (the main poll reports the failure, and past the threshold auto-refresh itself stops).
    if auth::ensure_fresh_token(&app.client).await.is_err() {
        return;
    }
    // Regardless of success, stop re-querying for this track (best-effort).
    app.saved_checked = true;
    if let Ok(mut flags) = app.client.library_contains([LibraryId::Track(id)]).await {
        app.saved = flags.pop();
    }
}

/// Refresh the cover art. Re-fetch only when the current track's art URL differs from `art_url`
/// (once per track, no retry on failure = best-effort). A fetch failure keeps the metadata display
/// and is shown on the status line (no silent failures).
async fn refresh_art(app: &mut App) {
    let url = app.now.as_ref().and_then(|n| n.album_image_url.clone());
    if url == app.art_url {
        return; // No change (same track) → do not re-fetch
    }
    // Regardless of success, stop re-fetching for this URL (prevents hammering every poll).
    app.art_url = url.clone();
    let Some(url) = url else {
        app.art = None; // No art (episodes, etc.)
        return;
    };
    match art::fetch_decode(&app.http, &url).await {
        Ok(img) => app.art = Some(app.picker.new_resize_protocol(img)),
        Err(e) => {
            app.art = None;
            app.status = format!("{} failed to fetch cover art: {e}", theme::WARN);
        }
    }
}

/// Seek the current track by ±`delta_ms`. The target is computed from local progress (with
/// interpolation), and on success progress is updated immediately and reflected on screen (no
/// forced poll, to avoid appearing to rewind due to Connect's propagation delay). Repeated presses
/// accumulate from the locally updated progress.
async fn control_seek(app: &mut App, delta_ms: i64) {
    let Some(n) = app.now.as_ref() else {
        app.status = format!("{} nothing is playing", theme::WARN);
        return;
    };
    let elapsed = n.fetched_at.elapsed().as_millis();
    let current = view::interpolate_progress(n.progress_ms, elapsed, n.duration_ms, n.is_playing);
    let target = view::seek_target(current, n.duration_ms, delta_ms);
    if !ensure_ready(app).await {
        return;
    }
    // target as i64: target is already clamped by duration_ms (and even when length is unknown, within a
    // realistic number of presses), so it will not reach i64::MAX (~290 million years) — safe.
    let res = app
        .client
        .seek_track(chrono::Duration::milliseconds(target as i64), None)
        .await;
    match res {
        Ok(()) => {
            // Reflect local progress immediately (no forced poll).
            if let Some(n) = app.now.as_mut() {
                n.progress_ms = target;
                n.fetched_at = Instant::now();
            }
            app.status = format!("{} Seek {}", theme::SEEK, crate::format::format_ms(target));
        }
        Err(e) => {
            app.status = format!(
                "{} seek failed: {e} (press d to select and activate a device)",
                theme::WARN
            );
        }
    }
}

/// Save/unsave the current track in the library (`s`). Toggles to the opposite of the current saved state, updating it on success.
async fn control_save(app: &mut App) {
    let Some(uri) = app.now.as_ref().and_then(|n| n.track_uri.clone()) else {
        app.status = format!(
            "{} cannot save the current track (track info is unknown)",
            theme::WARN
        );
        return;
    };
    let id = match TrackId::from_uri(&uri) {
        Ok(id) => id,
        Err(e) => {
            app.status = format!("{} failed to parse the track URI: {e}", theme::WARN);
            return;
        }
    };
    if !ensure_ready(app).await {
        return;
    }
    // If undetermined, interpret as "save".
    let want_save = !app.saved.unwrap_or(false);
    let res = if want_save {
        app.client.library_add([LibraryId::Track(id)]).await
    } else {
        app.client.library_remove([LibraryId::Track(id)]).await
    };
    match res {
        Ok(()) => {
            app.saved = Some(want_save);
            app.saved_checked = true;
            app.status = if want_save {
                format!("{} Saved to your library", theme::HEART)
            } else {
                format!("{} Removed from your library", theme::HEART_O)
            };
        }
        Err(e) => {
            app.status = format!("{} save operation failed: {e}", theme::WARN);
        }
    }
}

// ---- Library pane -----------------------------------------------------------

/// Load the library once, after the first frame is drawn. Set the "attempted" flag before awaiting so
/// a slow or failing initial fetch does not re-trigger every loop tick; switching tabs (or `r`) still
/// forces a reload of an un-cached tab, so a failed startup is recoverable.
async fn ensure_library_loaded(app: &mut App) {
    if app.library_loaded {
        return;
    }
    app.library_loaded = true;
    let tab = app.library.tab;
    load_library(app, tab).await;
}

/// Switch to `tab` and populate the library pane. Uses the per-tab cache when present (no network);
/// otherwise fetches and caches. Fetch failure is never silent: it shows on the pane and the status
/// line, and leaves the pane empty (switching tabs re-fetches, so it is not a dead end).
async fn load_library(app: &mut App, tab: browse::BrowseTab) {
    app.library.tab = tab;
    // On a cache hit, reuse the stored note too — a persistent partial failure (e.g. Artists needs a
    // re-login) keeps being reported every time the tab is shown, not just on the first load.
    if let Some(loaded) = app.browse_cache.get(tab).cloned() {
        let message = library_message(&loaded.rows, loaded.note, tab);
        app.library.set_rows(loaded.rows, message);
        return;
    }
    match browse::fetch(&app.client, tab).await {
        Ok(loaded) => {
            // Cache the whole load (rows + note); the clone is cheap for a few dozen small structs.
            app.browse_cache.set(tab, loaded.clone());
            let message = library_message(&loaded.rows, loaded.note, tab);
            app.library.set_rows(loaded.rows, message);
        }
        Err(e) => {
            // `{e:#}` shows anyhow's full cause chain (e.g. the 403 behind "failed to fetch followed
            // artists"), so the message names the real cause — a missing scope, a timeout, etc. —
            // instead of only the outermost context.
            app.library
                .set_rows(Vec::new(), Some(format!("failed to fetch: {e:#}")));
            app.status = format!("{} failed to fetch the library: {e:#}", theme::WARN);
        }
    }
}

/// The pane message for a freshly loaded tab: the partial-failure note if any, else an "empty" notice
/// when no playable rows loaded (so a 0-item tab is never silent), else `None` (the hint is shown).
fn library_message(
    rows: &[browse::LibraryRow],
    note: Option<String>,
    tab: browse::BrowseTab,
) -> Option<String> {
    if let Some(note) = note {
        return Some(note);
    }
    if !rows.iter().any(browse::LibraryRow::is_selectable) {
        return Some(format!("{} is empty", tab.label()));
    }
    None
}

/// Play the currently selected library item. Both outcomes report on the always-visible status line
/// (the library pane is not a modal, so its own message is left as the load-derived note/hint and is
/// never overwritten by a transient play result that would then go stale). A header selection plays
/// nothing (headers are never selectable, so this only happens on an empty list, already messaged).
async fn library_play(app: &mut App) {
    let Some(target) = app.library.selected_item().map(|it| it.target.clone()) else {
        return;
    };
    match browse::play(&app.client, &target).await {
        Ok(()) => {
            app.status = format!("{} Playback started", theme::PLAY);
            app.last_poll = None;
        }
        Err(e) => {
            // `{e:#}` surfaces anyhow's full cause chain, so playback failures name the real reason
            // (no active device, parse error, etc.), not just the outermost context.
            app.status = format!("{} playback failed: {e:#}", theme::WARN);
        }
    }
}

// ---- Detail pane ------------------------------------------------------------

/// Load the detail for the current library selection, when it changed. Cached per library-item URI so
/// scrolling back to a previously viewed item is free. Fetch failure and an empty track list are both
/// surfaced (never silent). Runs each loop tick but returns early when the selection is unchanged.
async fn ensure_detail_loaded(app: &mut App) {
    // Clone the bits we need from the selected item, dropping the `app.library` borrow before the
    // async fetch reaches for `app.client`.
    let selected = app
        .library
        .selected_item()
        .map(|it| (it.target.clone(), it.title.clone(), it.subtitle.clone()));
    let Some((target, title, subtitle)) = selected else {
        // Empty library or the selection is on a header: there is nothing to show a detail for.
        if app.detail.key.is_some() || app.detail.message.is_none() {
            app.detail.clear(Some("Nothing selected".to_string()));
        }
        return;
    };
    let key = target.uri().to_string();
    if app.detail.key.as_deref() == Some(key.as_str()) {
        return; // selection unchanged — keep the current detail (and its own selection)
    }
    if let Some(data) = app.detail_cache.get(&key).cloned() {
        app.detail.set(key, data);
        return;
    }
    let fallback = if subtitle.is_empty() {
        title
    } else {
        format!("{title} — {subtitle}")
    };
    match detail::fetch(&app.client, &target, &fallback).await {
        Ok(data) => {
            // Bound the cache: clear it wholesale once it grows past the cap (keeps memory flat over
            // a long session; re-fetches happen on demand).
            if app.detail_cache.len() >= DETAIL_CACHE_MAX {
                app.detail_cache.clear();
            }
            app.detail_cache.insert(key.clone(), data.clone());
            app.detail.set(key, data);
        }
        Err(e) => {
            // Surface the full cause chain (`{e:#}`): `detail::fetch_err` leads with a concise,
            // user-facing message and keeps the underlying error for diagnosis; non-mapped failures
            // (token refresh, URI parse) retain their own context. Non-silent in pane and status line.
            let msg = format!("{e:#}");
            app.detail.set_error(key, msg.clone());
            app.status = format!("{} {msg}", theme::WARN);
        }
    }
}

/// Play the detail track list as a queue, starting at the selected row, so `next`/`prev` walk the
/// list (the same all-URIs-queued invariant as search). Reports on the always-visible status line.
async fn detail_play(app: &mut App) {
    let uris: Vec<String> = app.detail.rows.iter().map(|r| r.uri.clone()).collect();
    if uris.is_empty() {
        return;
    }
    let selected = app.detail.selected;
    match start_playback_queue(app, &uris, selected).await {
        Ok(()) => {
            app.status = format!("{} Playback started", theme::PLAY);
            app.last_poll = None;
        }
        Err(e) => {
            app.status = format!("{} playback failed: {e:#}", theme::WARN);
        }
    }
}

// ---- Device picker ----------------------------------------------------------

/// Key handling for the device picker overlay. Updates the selection synchronously and runs the required async action.
async fn handle_devices_key(key: KeyEvent, app: &mut App) {
    let action = {
        let Mode::Devices(state) = &mut app.mode else {
            return;
        };
        devices::key_action(key, state)
    };
    match action {
        devices::DeviceAction::None => {}
        devices::DeviceAction::Close => app.mode = Mode::Normal,
        devices::DeviceAction::Transfer => devices_transfer(app).await,
        devices::DeviceAction::Reload => open_devices(app).await,
    }
}

/// Fetch the device list and enter selection mode. Empty list / fetch failure are reported (no silent failures).
/// Devices come and go, so they are not cached and are re-fetched every time it opens.
async fn open_devices(app: &mut App) {
    let items = match devices::fetch(&app.client).await {
        Ok(items) => items,
        Err(e) => {
            // If selecting, stay on screen and show a message; if in the normal view, put it on the status line.
            if let Mode::Devices(state) = &mut app.mode {
                state.message = Some(format!("failed to fetch: {e}"));
            } else {
                app.status = format!("{} failed to fetch the device list: {e}", theme::WARN);
            }
            return;
        }
    };
    let message = items
        .is_empty()
        .then(|| "No playable devices. Please open the Spotify app".to_string());
    // On re-fetch, snap to the active position (or the first) so the selection does not fall out of range.
    let selected = items.iter().position(|d| d.is_active).unwrap_or(0);
    app.mode = Mode::Devices(devices::DevicePickerState {
        items,
        selected,
        message,
    });
}

/// Transfer playback to the selected device. On success return to the normal view and poll immediately; on failure keep it on the overlay.
/// Non-transferable devices (no ID / restricted) are rejected up front and reported via a message.
async fn devices_transfer(app: &mut App) {
    let target = match &app.mode {
        Mode::Devices(state) => state.items.get(state.selected).cloned(),
        _ => None,
    };
    let Some(target) = target else {
        return;
    };
    if target.is_restricted {
        if let Mode::Devices(state) = &mut app.mode {
            state.message = Some(format!(
                "'{}' is restricted and cannot be transferred to",
                target.name
            ));
        }
        return;
    }
    let Some(id) = target.id.as_deref() else {
        if let Mode::Devices(state) = &mut app.mode {
            state.message = Some(format!(
                "'{}' has no ID and cannot be transferred to",
                target.name
            ));
        }
        return;
    };
    match devices::transfer(&app.client, id).await {
        Ok(()) => {
            app.status = format!("{} Moved playback to '{}'", theme::PLAY, target.name);
            app.last_poll = None; // Reflect the transfer into Now Playing quickly
            app.mode = Mode::Normal;
        }
        Err(e) => {
            if let Mode::Devices(state) = &mut app.mode {
                state.message = Some(format!("transfer failed: {e}"));
            } else {
                app.status = format!("{} transfer failed: {e}", theme::WARN);
            }
        }
    }
}

// ---- Terminal control -------------------------------------------------------

fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // If it fails partway, restore the terminal state changed so far before returning the error
    // (the caller cannot receive `terminal` to call restore_terminal, so clean up here).
    if let Err(e) = execute!(stdout, EnterAlternateScreen) {
        let _ = disable_raw_mode();
        return Err(e.into());
    }
    match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(terminal) => Ok(terminal),
        Err(e) => {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            Err(e.into())
        }
    }
}

fn restore_terminal(terminal: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Restore the terminal even on panic (undo raw mode / alt-screen).
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

// ---- Rendering --------------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, app: &mut App) {
    // Branch on ModeKind (Copy) to release the borrow immediately. Normal needs `&mut app` for image rendering.
    match app.mode.kind() {
        ModeKind::Normal => draw_dashboard(frame, app),
        ModeKind::Search => {
            if let Mode::Search(state) = &app.mode {
                draw_search(frame, state);
            }
        }
        ModeKind::Devices => {
            if let Mode::Devices(state) = &app.mode {
                draw_devices(frame, state);
            }
        }
        ModeKind::Help => draw_help(frame),
    }
}

/// Help view (all key bindings). Built from the single source of truth `view::help_entries()`.
fn draw_help(frame: &mut ratatui::Frame) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::GREEN))
        .title(" spotterm — Help ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // key list
            Constraint::Length(1), // footer
        ])
        .split(inner);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);

    // Right-align the `key` column to a common width for readability (computed by display width).
    let key_width = view::help_entries()
        .iter()
        .map(|(k, _)| crate::format::display_width(k))
        .max()
        .unwrap_or(0);
    let items: Vec<ListItem> = view::help_entries()
        .iter()
        .map(|(key, desc)| {
            let pad = key_width.saturating_sub(crate::format::display_width(key));
            ListItem::new(format!("  {key}{}   {desc}", " ".repeat(pad)))
        })
        .collect();
    frame.render_widget(List::new(items).highlight_style(bold), rows[0]);

    frame.render_widget(
        Paragraph::new("Press any key to go back (Esc / ? / q)")
            .alignment(Alignment::Center)
            .style(dim),
        rows[1],
    );
}

/// A bordered placeholder pane (for regions not yet implemented in this phase). It is a visible
/// label rather than an empty area, so the region never silently reads as broken. When `focused` is
/// true the border is drawn in solid GREEN (the same accent used for selection elsewhere); otherwise
/// it is dimmed. Display-only panes (e.g. Visualizer) always pass `focused = false`.
fn placeholder_pane(title: &str, focused: bool) -> Paragraph<'_> {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let (text_style, border_style) = if focused {
        (
            Style::default(),
            Style::default()
                .fg(theme::GREEN)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (dim, dim)
    };
    Paragraph::new(title)
        .alignment(Alignment::Center)
        .style(text_style)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style),
        )
}

/// Normal (dashboard) view. A thin orchestrator: it draws the outer frame, asks the pure
/// `view::dashboard_areas` to carve the inner area into regions, and hands each region to a focused
/// sub-function. Regions returned as `None` (too small a terminal) are simply skipped. In this phase
/// only Now Playing is real; the other panes are placeholders. The Now Playing pane is drawn last so
/// its `&mut app.art` borrow comes after every immutable read of `app`.
fn draw_dashboard(frame: &mut ratatui::Frame, app: &mut App) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::GREEN))
        .title(" spotterm — tui ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Phase 1 draws the dashboard with search inactive (search still uses its own overlay view).
    let areas = view::dashboard_areas(inner, false);

    // Reserve the cover-art column first (pure), then build the display lines against the *text*
    // width that actually remains. Passing the full pane width would make `truncate` think no
    // truncation is needed and let the non-wrapping Paragraph clip the text with no ellipsis. `v`
    // owns its strings, so it holds no borrow of `app` and is reused by both the pane and the playbar.
    let want_art = app.art.is_some() || app.now.is_some();
    let art_cols = view::art_col_width(areas.now_playing.width, areas.now_playing.height, want_art);
    let text_width = areas.now_playing.width.saturating_sub(art_cols);
    let elapsed = app
        .now
        .as_ref()
        .map(|n| n.fetched_at.elapsed().as_millis())
        .unwrap_or(0);
    let v = view::render_lines(app.now.as_ref(), elapsed, text_width as usize, app.saved);

    // Placeholder panes for later phases (visible labels, never silently blank). The focused lower
    // pane is highlighted; focus clamps to the library when the detail pane is hidden (narrow term),
    // so the highlight never lands on a pane the user cannot see. Visualizer is display-only.
    let detail_visible = areas.detail.is_some();
    // Record it so the `tab` key handler (which has no access to the frame size) can clamp focus
    // navigation to the panes actually on screen.
    app.detail_visible = detail_visible;
    let focus = app.focus.effective(detail_visible);
    if let Some(vis) = areas.visualizer {
        frame.render_widget(placeholder_pane("Visualizer", false), vis);
    }
    draw_library_pane(frame, app, areas.library, focus == view::Focus::Library);
    if let Some(detail_area) = areas.detail {
        draw_detail_pane(frame, app, detail_area, focus == view::Focus::Detail);
    }

    draw_status_line(frame, app, areas.status);
    draw_playbar(frame, v.ratio, &v.progress_label, areas.playbar);
    if let Some(footer) = areas.footer {
        frame.render_widget(
            Paragraph::new("tab focus   ? help   q quit")
                .alignment(Alignment::Center)
                .style(Style::default().add_modifier(Modifier::DIM)),
            footer,
        );
    }

    // Draw the Now Playing pane last: it is the only region that borrows `&mut app` (for the cover
    // art), so every immutable read above is already done. `art_cols` is the width already reserved
    // above (0 = no column), reused here so the split matches the width `render_lines` was given.
    draw_now_playing_pane(frame, app, areas.now_playing, art_cols, &v);
}

/// Draw the Now Playing pane: an optional cover-art column of `art_cols` columns on the left (0 =
/// none) and the text lines on the right. The text rows are placed by `view::stack_rows` in priority
/// order (state / title / artist / album / device), so a short pane drops the lower rows first
/// instead of letting the layout solver crush an arbitrary one to height 0. Progress is shown by the
/// bottom playbar, not here. The cover art is rendered last so `&mut app.art` is the final borrow.
fn draw_now_playing_pane(
    frame: &mut ratatui::Frame,
    app: &mut App,
    area: ratatui::layout::Rect,
    art_cols: u16,
    v: &view::RenderLines,
) {
    // A placeholder is shown even when art is absent/not yet fetched, to make the empty state
    // explicit. `art_cols` (from `view::art_col_width`) is 0 when no column should be shown.
    let (art_area, text_area) = if art_cols > 0 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(art_cols), Constraint::Min(1)])
            .split(area);
        (Some(cols[0]), cols[1])
    } else {
        (None, area)
    };

    // Priority-ordered rows: highest first, so `stack_rows` drops device/album before title.
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let accent = Style::default()
        .fg(theme::GREEN)
        .add_modifier(Modifier::BOLD);
    let plain = Style::default();
    let lines = [
        (v.state.as_str(), accent),
        (v.title.as_str(), bold),
        (v.artist.as_str(), plain),
        (v.album.as_str(), plain),
        (v.device.as_str(), plain),
    ];
    for ((text, style), rect) in lines.iter().zip(view::stack_rows(text_area, lines.len())) {
        frame.render_widget(Paragraph::new(*text).style(*style), rect);
    }

    // Cover art last (first `&mut app.art` borrow). Placeholder makes the empty state explicit.
    if let Some(art_rect) = art_area {
        if let Some(art) = app.art.as_mut() {
            frame.render_stateful_widget(StatefulImage::default(), art_rect, art);
        } else {
            let art_placeholder = Paragraph::new(format!("{}\n\n(no art)", theme::MUSIC))
                .alignment(Alignment::Center)
                .style(Style::default().add_modifier(Modifier::DIM))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(theme::GREEN)),
                );
            frame.render_widget(art_placeholder, art_rect);
        }
    }
}

/// Draw the always-present status line. If auto-refresh has stopped, always show the notice (drawn
/// from `poll_failures` so it does not vanish with the status auto-clear); otherwise color by kind.
fn draw_status_line(frame: &mut ratatui::Frame, app: &App, area: ratatui::layout::Rect) {
    let (text, style) = if app.poll_failures >= MAX_POLL_FAILURES {
        (
            format!(
                "{} auto-refresh is stopped. Press r to retry / q to quit",
                theme::WARN
            ),
            Style::default().fg(Color::Red),
        )
    } else {
        let style = match view::status_kind(&app.status) {
            view::StatusKind::Warn => Style::default().fg(Color::Red),
            view::StatusKind::Ok => Style::default().fg(theme::GREEN),
            view::StatusKind::Info => Style::default().add_modifier(Modifier::DIM),
        };
        (app.status.clone(), style)
    };
    frame.render_widget(Paragraph::new(text).style(style), area);
}

/// Draw the bottom playback bar (single source of progress). A graphical redesign is a later phase.
fn draw_playbar(frame: &mut ratatui::Frame, ratio: f64, label: &str, area: ratatui::layout::Rect) {
    frame.render_widget(
        Gauge::default()
            .ratio(ratio)
            .label(label.to_owned())
            .gauge_style(Style::default().fg(theme::GREEN))
            .use_unicode(true),
        area,
    );
}

/// Draw the always-visible library pane (lower-left dashboard region): a bordered block with a tab
/// header, a hint/message line, and the selectable row list. `Header` rows are dimmed and skipped by
/// selection; `Item` rows reuse the same `search_row` formatter. The border is highlighted (GREEN
/// bold) while the pane holds focus, dimmed otherwise, matching the other lower panes.
fn draw_library_pane(
    frame: &mut ratatui::Frame,
    app: &App,
    area: ratatui::layout::Rect,
    focused: bool,
) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let border_style = if focused {
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD)
    } else {
        dim
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" Library ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab header
            Constraint::Length(1), // hint / message
            Constraint::Min(1),    // list
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(view::library_tab_header(app.library.tab)).style(bold),
        rows[0],
    );

    // Playable item count (headers excluded) drives the default hint; a message (loading / empty /
    // error / partial-failure note) takes precedence so the pane is never silent.
    let item_count = app
        .library
        .rows
        .iter()
        .filter(|r| r.is_selectable())
        .count();
    let hint = app
        .library
        .message
        .clone()
        .unwrap_or_else(|| view::library_hint(item_count));
    frame.render_widget(Paragraph::new(hint).style(dim), rows[1]);

    let width = inner.width as usize;
    let items: Vec<ListItem> = app
        .library
        .rows
        .iter()
        .map(|row| match row {
            browse::LibraryRow::Header(text) => {
                ListItem::new(text.clone()).style(bold.add_modifier(Modifier::DIM))
            }
            browse::LibraryRow::Item(it) => {
                ListItem::new(view::search_row(&it.title, &it.subtitle, width))
            }
        })
        .collect();
    let mut list_state = ListState::default();
    // Only highlight when the selection is on a playable row (never on a header or an empty list).
    if app.library.selected_item().is_some() {
        list_state.select(Some(app.library.selected));
    }
    let list = List::new(items).highlight_symbol("▶ ").highlight_style(
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, rows[2], &mut list_state);
}

/// Draw the always-visible detail pane (lower-right dashboard region): a bordered block with the
/// context title, a hint/message line, and the track list for the currently selected library item.
/// The currently-playing track (matched by URI against Now Playing) is prefixed with the play glyph;
/// the list selection is the `▶ ` marker. Border highlighted (GREEN bold) while focused.
fn draw_detail_pane(
    frame: &mut ratatui::Frame,
    app: &App,
    area: ratatui::layout::Rect,
    focused: bool,
) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let border_style = if focused {
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD)
    } else {
        dim
    };
    let title = if app.detail.title.is_empty() {
        " Details ".to_string()
    } else {
        format!(" {} ", app.detail.title)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)]) // hint / list
        .split(inner);

    let hint = app.detail.message.clone().unwrap_or_else(|| {
        if app.detail.key.is_none() {
            // Nothing has resolved yet (before the first selection loads): show a loading note
            // instead of a misleading "0 tracks".
            "Loading…".to_string()
        } else {
            view::detail_hint(app.detail.rows.len())
        }
    });
    frame.render_widget(Paragraph::new(hint).style(dim), rows[0]);

    // The URI of the track playing now, so the detail list can mark it with the play glyph.
    let current = app.now.as_ref().and_then(|n| n.track_uri.as_deref());
    let width = inner.width as usize;
    let items: Vec<ListItem> = app
        .detail
        .rows
        .iter()
        .map(|r| {
            let is_current = current == Some(r.uri.as_str());
            let text = view::detail_row(
                r.track_no,
                &r.title,
                &r.artists,
                r.duration_ms,
                is_current,
                width,
            );
            let item = ListItem::new(text);
            // Bold the currently-playing row so the glyph is not the only cue.
            if is_current { item.style(bold) } else { item }
        })
        .collect();
    let mut list_state = ListState::default();
    if !app.detail.rows.is_empty() {
        list_state.select(Some(app.detail.selected));
    }
    let list = List::new(items).highlight_symbol("▶ ").highlight_style(
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, rows[1], &mut list_state);
}

/// Device picker view (list + selection highlight).
fn draw_devices(frame: &mut ratatui::Frame, state: &devices::DevicePickerState) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::GREEN))
        .title(" spotterm — Devices ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // hint
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(inner);

    let dim = Style::default().add_modifier(Modifier::DIM);

    let hint = state.message.clone().unwrap_or_else(|| {
        format!(
            "{} devices — ↑↓ select / Enter transfer / r refresh / Esc back",
            state.items.len()
        )
    });
    frame.render_widget(Paragraph::new(hint).style(dim), rows[0]);

    // List (device-row formatting is delegated to the pure function `view::device_row`).
    let width = inner.width as usize;
    let items: Vec<ListItem> = state
        .items
        .iter()
        .map(|d| {
            ListItem::new(view::device_row(
                &d.name,
                &d.type_label,
                d.volume,
                d.is_active,
                d.is_restricted,
                width,
            ))
        })
        .collect();
    let mut list_state = ListState::default();
    if !state.items.is_empty() {
        list_state.select(Some(state.selected));
    }
    let list = List::new(items).highlight_symbol("▶ ").highlight_style(
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, rows[1], &mut list_state);

    frame.render_widget(
        Paragraph::new("↑↓ select   Enter transfer   r refresh   Esc back   Ctrl-C quit")
            .alignment(Alignment::Center)
            .style(dim),
        rows[2],
    );
}

/// Search overlay view (input field + result list).
fn draw_search(frame: &mut ratatui::Frame, state: &SearchState) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::GREEN))
        .title(" spotterm — Search ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // input field
            Constraint::Length(1), // hint
            Constraint::Min(1),    // result list
            Constraint::Length(1), // footer
        ])
        .split(inner);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);

    // Input field (show the cursor only in the input phase).
    let cursor = if state.phase == SearchPhase::Input {
        "▌"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(format!("Search: {}{}", state.query, cursor)).style(bold),
        rows[0],
    );

    // Hint line (message takes priority; otherwise a per-phase hint).
    let hint = state.message.clone().unwrap_or_else(|| {
        view::search_hint(state.phase == SearchPhase::Input, state.results.len())
    });
    frame.render_widget(Paragraph::new(hint).style(dim), rows[1]);

    // Result list (highlight the selection position).
    let width = inner.width as usize;
    let items: Vec<ListItem> = state
        .results
        .iter()
        .map(|h| ListItem::new(view::search_row(&h.name, &h.artists, width)))
        .collect();
    let mut list_state = ListState::default();
    if !state.results.is_empty() {
        list_state.select(Some(state.selected));
    }
    let list = List::new(items).highlight_symbol("▶ ").highlight_style(
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, rows[2], &mut list_state);

    frame.render_widget(
        Paragraph::new("type → Enter search   ↑↓ select   Enter play   Esc back   Ctrl-C quit")
            .alignment(Alignment::Center)
            .style(dim),
        rows[3],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(id: &str) -> String {
        format!("spotify:track:{id}")
    }

    #[test]
    fn queue_from_uris_queues_all_hits_and_offsets_to_selection() {
        let uris = vec![uri("4iV5W9uYEdYUVa79Axb7Rh"), uri("1301WleyT98MSxVHPZCA6M")];

        let (ids, offset) = queue_from_uris(&uris, 1).unwrap();

        // Every hit is queued so `next`/`prev` have somewhere to go...
        assert_eq!(ids.len(), 2);
        // ...and playback starts at the selected track, not the first one.
        assert_eq!(offset, Some(Offset::Uri(uris[1].clone())));
    }

    #[test]
    fn queue_from_uris_rejects_an_unparseable_uri() {
        let uris = vec![uri("4iV5W9uYEdYUVa79Axb7Rh"), "not-a-uri".to_string()];

        assert!(queue_from_uris(&uris, 0).is_err());
    }

    #[test]
    fn queue_from_uris_without_a_matching_selection_omits_the_offset() {
        let uris = vec![uri("4iV5W9uYEdYUVa79Axb7Rh")];

        // `selected` past the end yields no offset (Spotify then starts at the queue head).
        let (ids, offset) = queue_from_uris(&uris, 9).unwrap();

        assert_eq!(ids.len(), 1);
        assert_eq!(offset, None);
    }
}
