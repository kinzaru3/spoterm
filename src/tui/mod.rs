//! Interactive TUI. Displays Now Playing live and controls playback via key presses.
//!
//! - Authentication builds a client once at startup with [`crate::auth::authed_client`] and then
//!   keeps and reuses it (without discarding reqwest's connection pool or re-reading the disk on
//!   every operation). The token is refreshed with [`crate::auth::ensure_fresh_token`] only when expired.
//! - `current_playback` is fetched every `POLL_INTERVAL`, and between polls progress is interpolated
//!   locally with [`view::interpolate_progress`] to look smooth.
//! - API errors are shown on the status line and the loop continues (no silent failures).

use crate::art;
mod browse;
mod devices;
mod view;

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

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Screen mode. Normally Now Playing; `/` enters search, `2` library browse, `d` device selection.
enum Mode {
    Normal,
    Search(SearchState),
    Browse(browse::BrowseState),
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
    Browse,
    Devices,
    Help,
}

impl Mode {
    fn kind(&self) -> ModeKind {
        match self {
            Mode::Normal => ModeKind::Normal,
            Mode::Search(_) => ModeKind::Search,
            Mode::Browse(_) => ModeKind::Browse,
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
    /// Per-tab fetch-result cache for library browse (avoids re-fetching on tab switch).
    browse_cache: browse::BrowseCache,
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
        browse_cache: browse::BrowseCache::default(),
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
        ModeKind::Browse => {
            handle_browse_key(key, app).await;
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
        KeyCode::Char('2') => load_browse(app, browse::BrowseTab::Playlists).await,
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
        KeyCode::Char('r') => {
            app.poll_failures = 0;
            app.last_poll = None;
            app.art_url = None;
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
            app.status = format!("⚠ search failed: {e}");
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
            app.status = "▶ Playback started".to_string();
            app.last_poll = None; // Reflect playback start on screen quickly
            app.mode = Mode::Normal;
        }
        Err(e) => {
            if let Mode::Search(state) = &mut app.mode {
                state.message = Some(format!("playback failed: {e}"));
            } else {
                app.status = format!("⚠ playback failed: {e}");
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
                format!("⚠ auto-refresh stopped ({e}). Press r to retry / q to quit")
            } else {
                format!("⚠ refresh failed: {e}")
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
            let (title, artists, album, dur) = crate::commands::status::track_from_json(&v);
            let images = crate::commands::status::album_images_from_json(&v);
            (
                title,
                artists,
                album,
                dur,
                crate::commands::status::track_id_from_json(&v),
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
            app.status = format!("⚠ {e}");
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
            app.status =
                format!("⚠ operation failed: {e} (press d to select and activate a device)");
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
        finish(app, res, "⏸ Paused");
    } else {
        let res = app.client.resume_playback(None, None).await;
        finish(app, res, "▶ Playing");
    }
}

async fn control_next(app: &mut App) {
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.next_track(None).await;
    finish(app, res, "⏭ Next track");
}

async fn control_prev(app: &mut App) {
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.previous_track(None).await;
    finish(app, res, "⏮ Previous track");
}

async fn control_volume(app: &mut App, delta: i16) {
    let Some(cur) = app.now.as_ref().and_then(|n| n.volume) else {
        app.status = "⚠ device volume is unavailable (press d to select a device)".to_string();
        return;
    };
    let next = (cur as i16 + delta).clamp(0, 100) as u8;
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.volume(next, None).await;
    finish(app, res, &format!("🔊 Volume {next}%"));
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
            app.status = format!("⚠ failed to fetch cover art: {e}");
        }
    }
}

/// Seek the current track by ±`delta_ms`. The target is computed from local progress (with
/// interpolation), and on success progress is updated immediately and reflected on screen (no
/// forced poll, to avoid appearing to rewind due to Connect's propagation delay). Repeated presses
/// accumulate from the locally updated progress.
async fn control_seek(app: &mut App, delta_ms: i64) {
    let Some(n) = app.now.as_ref() else {
        app.status = "⚠ nothing is playing".to_string();
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
            app.status = format!("⏩ Seek {}", crate::format::format_ms(target));
        }
        Err(e) => {
            app.status = format!("⚠ seek failed: {e} (press d to select and activate a device)");
        }
    }
}

/// Save/unsave the current track in the library (`s`). Toggles to the opposite of the current saved state, updating it on success.
async fn control_save(app: &mut App) {
    let Some(uri) = app.now.as_ref().and_then(|n| n.track_uri.clone()) else {
        app.status = "⚠ cannot save the current track (track info is unknown)".to_string();
        return;
    };
    let id = match TrackId::from_uri(&uri) {
        Ok(id) => id,
        Err(e) => {
            app.status = format!("⚠ failed to parse the track URI: {e}");
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
                "♥ Saved to your library".to_string()
            } else {
                "♡ Removed from your library".to_string()
            };
        }
        Err(e) => {
            app.status = format!("⚠ save operation failed: {e}");
        }
    }
}

// ---- Library browse ---------------------------------------------------------

/// Key handling for the browse overlay. Updates the selection synchronously and runs the required async action.
async fn handle_browse_key(key: KeyEvent, app: &mut App) {
    let action = {
        let Mode::Browse(state) = &mut app.mode else {
            return;
        };
        browse::key_action(key, state)
    };
    match action {
        browse::BrowseAction::None => {}
        browse::BrowseAction::Close => app.mode = Mode::Normal,
        browse::BrowseAction::Switch(tab) => load_browse(app, tab).await,
        browse::BrowseAction::Play => browse_play(app).await,
        browse::BrowseAction::Reload => {
            // Discard the current tab's cache and re-fetch (= user-driven reload).
            let Mode::Browse(state) = &app.mode else {
                return;
            };
            let tab = state.tab;
            app.browse_cache.clear(tab);
            load_browse(app, tab).await;
        }
    }
}

/// Show the given tab's list and enter browse mode (switch tabs if already browsing).
/// If cached, do not hit the network; fetch and cache only when not cached. Failures are reported.
async fn load_browse(app: &mut App, tab: browse::BrowseTab) {
    // If cached, clone and show immediately (clone is cheap for a few dozen small structs).
    let items = match app.browse_cache.get(tab).cloned() {
        Some(items) => items,
        None => {
            match browse::fetch(&app.client, tab).await {
                Ok(items) => {
                    app.browse_cache.set(tab, items.clone());
                    items
                }
                Err(e) => {
                    // If browsing, stay on screen and show a message; if in the normal view, put it on the status line.
                    if let Mode::Browse(state) = &mut app.mode {
                        state.message = Some(format!("failed to fetch: {e}"));
                    } else {
                        app.status = format!("⚠ failed to fetch the library: {e}");
                    }
                    return;
                }
            }
        }
    };
    let message = items
        .is_empty()
        .then(|| format!("{} is empty", tab.label()));
    app.mode = Mode::Browse(browse::BrowseState {
        tab,
        items,
        selected: 0,
        message,
    });
}

/// Play the selected item. On success return to the normal view; on failure keep a message on the overlay.
async fn browse_play(app: &mut App) {
    let target = match &app.mode {
        Mode::Browse(state) => state.items.get(state.selected).map(|it| it.target.clone()),
        _ => None,
    };
    let Some(target) = target else {
        return;
    };
    match browse::play(&app.client, &target).await {
        Ok(()) => {
            app.status = "▶ Playback started".to_string();
            app.last_poll = None;
            app.mode = Mode::Normal;
        }
        Err(e) => {
            if let Mode::Browse(state) = &mut app.mode {
                state.message = Some(format!("playback failed: {e}"));
            } else {
                app.status = format!("⚠ playback failed: {e}");
            }
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
                app.status = format!("⚠ failed to fetch the device list: {e}");
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
            app.status = format!("▶ Moved playback to '{}'", target.name);
            app.last_poll = None; // Reflect the transfer into Now Playing quickly
            app.mode = Mode::Normal;
        }
        Err(e) => {
            if let Mode::Devices(state) = &mut app.mode {
                state.message = Some(format!("transfer failed: {e}"));
            } else {
                app.status = format!("⚠ transfer failed: {e}");
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
        ModeKind::Normal => draw_now(frame, app),
        ModeKind::Search => {
            if let Mode::Search(state) = &app.mode {
                draw_search(frame, state);
            }
        }
        ModeKind::Browse => {
            if let Mode::Browse(state) = &app.mode {
                draw_browse(frame, state);
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

/// Normal (Now Playing) view. If cover art exists, place the image column on the left and text on the right.
fn draw_now(frame: &mut ratatui::Frame, app: &mut App) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" spotterm — Now Playing ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // While playing, give the left an image column (show a placeholder even if art is absent/not yet
    // fetched to make the empty state explicit). The image is square; given a cell aspect ratio of
    // about 1:2, aim for width ≈ height*2, capped at half the inner width and 24 columns. If too
    // narrow, do not show a column (full-width text). Also no column when nothing is playing.
    let want_art_col = app.art.is_some() || app.now.is_some();
    let art_cols: u16 = if want_art_col {
        inner.height.saturating_mul(2).min(inner.width / 2).min(24)
    } else {
        0
    };
    let (art_area, text_area) = if art_cols >= 4 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(art_cols), Constraint::Min(1)])
            .split(inner);
        (Some(cols[0]), cols[1])
    } else {
        (None, inner)
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // state
            Constraint::Length(1), // title
            Constraint::Length(1), // artist
            Constraint::Length(1), // album
            Constraint::Length(1), // progress gauge
            Constraint::Length(1), // device
            Constraint::Min(1),    // spacer
            Constraint::Length(1), // status
            Constraint::Length(1), // footer (keys)
        ])
        .split(text_area);

    // Building the display lines is delegated to the pure function `view::render_lines` (tested). Here it is only turned into widgets.
    let elapsed = app
        .now
        .as_ref()
        .map(|n| n.fetched_at.elapsed().as_millis())
        .unwrap_or(0);
    let v = view::render_lines(
        app.now.as_ref(),
        elapsed,
        text_area.width as usize,
        app.saved,
    );

    let bold = Style::default().add_modifier(Modifier::BOLD);
    frame.render_widget(Paragraph::new(v.state).style(bold), rows[0]);
    frame.render_widget(Paragraph::new(v.title).style(bold), rows[1]);
    frame.render_widget(Paragraph::new(v.artist), rows[2]);
    frame.render_widget(Paragraph::new(v.album), rows[3]);
    frame.render_widget(
        Gauge::default()
            .ratio(v.ratio)
            .label(v.progress_label)
            .use_unicode(true),
        rows[4],
    );
    frame.render_widget(Paragraph::new(v.device), rows[5]);

    // Status line: if auto-refresh has stopped, always show the notice (drawn from poll_failures so
    // it does not vanish with the status auto-clear). Otherwise, color by kind.
    let (status_text, status_style) = if app.poll_failures >= MAX_POLL_FAILURES {
        (
            "⚠ auto-refresh is stopped. Press r to retry / q to quit".to_string(),
            Style::default().fg(Color::Red),
        )
    } else {
        let style = match view::status_kind(&app.status) {
            view::StatusKind::Warn => Style::default().fg(Color::Red),
            view::StatusKind::Ok => Style::default().fg(Color::Green),
            view::StatusKind::Info => Style::default().add_modifier(Modifier::DIM),
        };
        (app.status.clone(), style)
    };
    frame.render_widget(Paragraph::new(status_text).style(status_style), rows[7]);
    frame.render_widget(
        Paragraph::new("? help   q quit")
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::DIM)),
        rows[8],
    );

    // Draw the cover art last (this is the first time `&mut app.art` is borrowed = all the immutable
    // borrows of the text drawing above are done). The protocol shows a real image / half-blocks
    // depending on the terminal. When art is absent (episode / no image / before fetch or on
    // failure), a placeholder makes the empty state explicit.
    if let Some(area) = art_area {
        if let Some(art) = app.art.as_mut() {
            frame.render_stateful_widget(StatefulImage::default(), area, art);
        } else {
            let placeholder = Paragraph::new("♪\n\n(no art)")
                .alignment(Alignment::Center)
                .style(Style::default().add_modifier(Modifier::DIM))
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(placeholder, area);
        }
    }
}

/// Library browse view (tabs + list).
fn draw_browse(frame: &mut ratatui::Frame, state: &browse::BrowseState) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" spotterm — Library ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab header
            Constraint::Length(1), // hint
            Constraint::Min(1),    // list
            Constraint::Length(1), // footer
        ])
        .split(inner);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);

    // Tab header (wrap the current tab in [ ]).
    let header = browse::BrowseTab::ALL
        .iter()
        .map(|t| {
            if *t == state.tab {
                format!("[{}]", t.label())
            } else {
                format!(" {} ", t.label())
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    frame.render_widget(Paragraph::new(header).style(bold), rows[0]);

    let hint = state.message.clone().unwrap_or_else(|| {
        format!(
            "{} items — ↑↓ select / ←→ tab / Enter play / r refresh / Esc back",
            state.items.len()
        )
    });
    frame.render_widget(Paragraph::new(hint).style(dim), rows[1]);

    // List (title — subtitle. Row formatting reuses the same pure function as search).
    let width = inner.width as usize;
    let items: Vec<ListItem> = state
        .items
        .iter()
        .map(|it| ListItem::new(view::search_row(&it.title, &it.subtitle, width)))
        .collect();
    let mut list_state = ListState::default();
    if !state.items.is_empty() {
        list_state.select(Some(state.selected));
    }
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(bold);
    frame.render_stateful_widget(list, rows[2], &mut list_state);

    frame.render_widget(
        Paragraph::new("↑↓ select   ←→ tab   Enter play   r refresh   Esc back   Ctrl-C quit")
            .alignment(Alignment::Center)
            .style(dim),
        rows[3],
    );
}

/// Device picker view (list + selection highlight).
fn draw_devices(frame: &mut ratatui::Frame, state: &devices::DevicePickerState) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
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

    let bold = Style::default().add_modifier(Modifier::BOLD);
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
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(bold);
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
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(bold);
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
