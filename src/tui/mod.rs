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
mod playback;
mod queue;
mod rate_limit;
mod search;
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
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use rspotify::AuthCodePkceSpotify;
use rspotify::ClientError;

use crate::auth;
use crate::config::Config;
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
    // Boxed: `SearchState` (query + classified results + highlight detail) is far larger than the
    // other variants, so boxing keeps `Mode` small (clippy::large_enum_variant).
    Search(Box<search::SearchState>),
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
    /// Whether the queue pane was visible in the most recent draw (false on narrow/short terminals,
    /// where `dashboard_areas` drops it). Gates `poll_queue` so a terminal too small to show the
    /// queue does not spend a `current_user_queue()` call every poll (#38). Reset to `false` at the
    /// start of every `draw` and set back to its real value only by `draw_dashboard`, so full-screen
    /// overlays (Devices/Help) that never draw the pane also stop its poll. Starts `false` — the
    /// first draw sets it before any poll gating reads it.
    queue_visible: bool,
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
    /// The playback queue shown in the display-only upper-right pane. Refreshed on the playback poll.
    queue: queue::QueueState,
    /// When `Some`, a 429 cooldown is active until this instant: the playback poll, background loads,
    /// and user operations are all gated until it passes so the client stops hammering Spotify. Armed
    /// from a detected 429's `Retry-After` (or a local exponential backoff) and cleared on the next
    /// successful poll. See [`rate_limit`].
    rate_limited_until: Option<Instant>,
    /// Consecutive 429 count, driving the local exponential backoff when the server sends no
    /// `Retry-After`. Reset to 0 on any successful poll. Kept separate from `poll_failures` so rate
    /// limiting (a transient, self-healing condition) never trips the auto-refresh-stopped path.
    rate_limit_hits: u32,
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
        queue_visible: false,
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
        queue: queue::QueueState::default(),
        rate_limited_until: None,
        rate_limit_hits: 0,
    };

    // For auto-clearing the status line (detect changes and time them. Not stored on App; handled within this loop).
    let mut last_status = app.status.clone();
    let mut status_since = Instant::now();
    // Queue-pane visibility as of the previous draw. When it flips hidden→visible we force an
    // immediate poll so the reappearing pane is not stuck on "Loading…" for a poll interval (#38).
    // Loop-local like `last_status`; starts `false` to match `App.queue_visible`'s initial value.
    let mut prev_queue_visible = false;

    loop {
        // When `last_poll` is None, force a poll (right after startup, after an operation, or `r`). A
        // timer-driven auto-refresh happens only while consecutive failures are below the threshold
        // (avoids retrying every 2 seconds on an invalid token).
        let forced = app.last_poll.is_none();
        let timer_due = app.last_poll.is_none_or(|t| t.elapsed() >= POLL_INTERVAL);
        // A 429 cooldown gates every API call this iteration. While blocked we neither poll nor
        // advance `last_poll`, so `timer_due` stays satisfied and the very next iteration after the
        // cooldown lifts polls immediately (明け即 poll). `rate_limit_blocked` also refreshes the
        // countdown on the status line so it ticks down (no silent wait). Rate limiting is tracked
        // apart from `poll_failures`, so a cooldown never counts toward the auto-refresh-stopped cap.
        let blocked = rate_limit_blocked(&mut app);
        if !blocked && (forced || (timer_due && app.poll_failures < MAX_POLL_FAILURES)) {
            playback::poll_playback(&mut app).await;
            // Poll the queue only while its pane is on screen (#38), reading the *previous* frame's
            // visibility (this poll runs before this iteration's `draw`). A hidden→visible flip is
            // caught after the draw below and forces a re-poll, so the one-frame lag is bounded by
            // `TICK`, not `POLL_INTERVAL`. `poll_playback` stays unconditional (Now Playing / playbar
            // are always shown). Sequential, not `join!`ed: `ensure_fresh_token` assumes single-task
            // ordering, so we only thin the cadence.
            if app.queue_visible {
                queue::poll_queue(&mut app).await;
            }
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

        // `draw` refreshed `app.queue_visible`. If the queue pane just reappeared (terminal widened
        // or grew tall enough, or an overlay closed), force an immediate poll so it does not sit on
        // "Loading…" for up to a poll interval (#38). Compared against the previous draw's value;
        // both start `false`. The pane this frame shows the last-known `QueueState` (like the
        // library/detail caches do); the forced re-poll refreshes it on the next iteration (≤ TICK),
        // so the momentary stale row is bounded and self-heals rather than flashing a spinner.
        if view::queue_became_visible(prev_queue_visible, app.queue_visible) {
            app.last_poll = None;
        }
        prev_queue_visible = app.queue_visible;

        // Background loads are also gated by an active 429 cooldown: they issue their own API calls,
        // so running them while rate limited would keep the burst alive. They are one-shot/cached, so
        // pausing them for a short cooldown only defers the fetch by a few seconds.
        if !blocked {
            // Load the library once, *after* the first frame is drawn, so the dashboard (with the
            // "Loading…" library note) appears immediately instead of the whole UI blocking on the
            // multi-call `All` fetch. The fetch is concurrent (see `browse::fetch_all`), so this is one
            // round-trip, comparable to the playback poll above.
            browse::ensure_library_loaded(&mut app).await;

            // Load the detail for the current library selection (only when the selection changed; cached
            // per item). Runs after the library load so there is a selection to describe.
            detail::ensure_detail_loaded(&mut app).await;

            // In search mode, load the highlight detail for the selected result (same per-URI cache).
            search::ensure_search_detail_loaded(&mut app).await;
        }

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
            search::handle_search_key(key, app).await;
            false
        }
        ModeKind::Devices => {
            devices::handle_devices_key(key, app).await;
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
        KeyCode::Char('/') => app.mode = Mode::Search(Box::default()),
        // Cycle keyboard focus between the two lower dashboard panes (library <-> detail). `next`
        // clamps to the library when the detail pane was hidden in the last draw (narrow terminal),
        // so focus never drifts to an off-screen pane.
        KeyCode::Tab => app.focus = app.focus.next(app.detail_visible),
        // Library pane navigation, active only while the library pane holds focus so the same keys
        // stay free for the (future) detail pane. `[`/`]` switch tabs, ↑↓ move the selection, Enter
        // plays. Left/Right remain seek (see below); the library uses the bracket keys for tabs.
        KeyCode::Char('[') if app.focus == view::Focus::Library => {
            browse::load_library(app, app.library.tab.prev()).await;
        }
        KeyCode::Char(']') if app.focus == view::Focus::Library => {
            browse::load_library(app, app.library.tab.next()).await;
        }
        KeyCode::Up if app.focus == view::Focus::Library => app.library.select_prev(),
        KeyCode::Down if app.focus == view::Focus::Library => app.library.select_next(),
        KeyCode::Enter if app.focus == view::Focus::Library => browse::library_play(app).await,
        // Detail pane navigation, active only while the detail pane holds focus. ↑↓ move the
        // selection within the track list, Enter plays it (see `detail_play`).
        KeyCode::Up if app.focus == view::Focus::Detail => app.detail.select_prev(),
        KeyCode::Down if app.focus == view::Focus::Detail => app.detail.select_next(),
        KeyCode::Enter if app.focus == view::Focus::Detail => detail::detail_play(app).await,
        KeyCode::Char('d') => devices::open_devices(app).await,
        KeyCode::Char('?') => app.mode = Mode::Help,
        KeyCode::Char(' ') => playback::control_toggle(app).await,
        KeyCode::Char('n') => playback::control_next(app).await,
        KeyCode::Char('p') => playback::control_prev(app).await,
        KeyCode::Char('+') | KeyCode::Char('=') => playback::control_volume(app, VOL_STEP).await,
        KeyCode::Char('-') | KeyCode::Char('_') => playback::control_volume(app, -VOL_STEP).await,
        KeyCode::Left => playback::control_seek(app, -SEEK_STEP_MS).await,
        KeyCode::Right => playback::control_seek(app, SEEK_STEP_MS).await,
        KeyCode::Char('s') => playback::control_save(app).await,
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
                browse::load_library(app, app.library.tab).await;
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

/// Refresh the retained client's token if needed. On failure, show it on the status line and return `false`.
/// A 429 cooldown is checked first: while blocked, the operation is refused (the countdown is shown)
/// so user actions do not add to the burst.
async fn ensure_ready(app: &mut App) -> bool {
    if rate_limit_blocked(app) {
        return false;
    }
    match auth::ensure_fresh_token(&app.client).await {
        Ok(()) => true,
        Err(e) => {
            // A 429 from the token refresh itself must also arm the cooldown; otherwise every keypress
            // would keep re-refreshing without backing off and feed the burst.
            if !note_if_rate_limited(app, &e) {
                app.status = format!("{} {e}", theme::WARN);
            }
            false
        }
    }
}

/// Reflect the operation result on the status line, and on success schedule an immediate poll. A
/// `429` in the error is intercepted and armed as a cooldown rather than shown as a generic failure.
fn finish(app: &mut App, res: Result<(), ClientError>, ok: &str) {
    match res {
        Ok(()) => {
            clear_rate_limit(app); // a call the server accepted proves the budget has recovered
            app.status = ok.to_string();
            app.last_poll = None; // Reflect the change on screen quickly
        }
        Err(e) => {
            if !note_if_rate_limited_client(app, &e) {
                app.status = format!(
                    "{} operation failed: {e} (press d to select and activate a device)",
                    theme::WARN
                );
            }
        }
    }
}

/// Record a detected 429: bump the consecutive-hit count, arm the cooldown from the server
/// `Retry-After` (or the local exponential backoff), and show the countdown. Deliberately does not
/// touch `poll_failures` — rate limiting is a distinct, self-healing condition from the
/// auth/network failures that stop auto-refresh.
fn note_rate_limit(app: &mut App, hit: rate_limit::RateLimitHit) {
    app.rate_limit_hits = app.rate_limit_hits.saturating_add(1);
    let secs = rate_limit::wait_secs(hit.retry_after, app.rate_limit_hits);
    app.rate_limited_until = Some(Instant::now() + Duration::from_secs(secs));
    app.status = rate_limit::rate_limit_status(secs);
}

/// Fold rate-limit handling into an existing `anyhow` error arm: if `err` carries a 429, arm the
/// cooldown and return `true` (the caller then returns without showing its generic error). Returns
/// `false` for any non-429 error, which the caller reports as before. Used by every API call site so
/// the *first* 429 from any path — not just the poll — starts the backoff.
fn note_if_rate_limited(app: &mut App, err: &anyhow::Error) -> bool {
    match rate_limit::detect(err) {
        Some(hit) => {
            note_rate_limit(app, hit);
            true
        }
        None => false,
    }
}

/// [`note_if_rate_limited`] for a concrete `ClientError` (the control-op paths that keep the typed
/// error rather than wrapping it in `anyhow`).
fn note_if_rate_limited_client(app: &mut App, err: &ClientError) -> bool {
    match rate_limit::detect_client_error(err) {
        Some(hit) => {
            note_rate_limit(app, hit);
            true
        }
        None => false,
    }
}

/// If a 429 cooldown is active, refresh the countdown status and return `true` (the caller aborts
/// its API call). Returns `false` when not blocked. Shown seconds are floored but never below 1
/// while still blocked, so the countdown never displays a misleading `0s`.
fn rate_limit_blocked(app: &mut App) -> bool {
    match rate_limit::remaining(app.rate_limited_until, Instant::now()) {
        Some(d) => {
            app.status = rate_limit::rate_limit_status(d.as_secs().max(1));
            true
        }
        None => false,
    }
}

/// End any 429 cooldown and reset the consecutive-hit count. Called on a successful poll — proof the
/// budget has recovered.
fn clear_rate_limit(app: &mut App) {
    app.rate_limited_until = None;
    app.rate_limit_hits = 0;
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
    // Default the queue pane to hidden each frame; only `draw_dashboard` (Normal/Search) sets it back
    // to its real visibility. This keeps `queue_visible` meaning "the queue pane was drawn in the
    // last frame", so full-screen overlays (Devices `d` / Help `?`) correctly stop the queue poll —
    // the pane is not on screen there, so we must not spend a `current_user_queue()` call for it (#38).
    app.queue_visible = false;
    // Branch on ModeKind (Copy) to release the borrow immediately. Normal needs `&mut app` for image rendering.
    match app.mode.kind() {
        // Search shares the dashboard shell (Now Playing / playbar / footer); it only swaps the lower
        // panes for the results/highlight and reveals the search bar. So both draw the dashboard.
        ModeKind::Normal | ModeKind::Search => draw_dashboard(frame, app),
        ModeKind::Devices => {
            if let Mode::Devices(state) = &app.mode {
                devices::draw_devices(frame, state);
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

    // Search mode reveals the search bar and swaps the lower panes; Normal hides the bar and shows
    // the library. `search_focus` (when in search) selects the focused lower pane, replacing `app.focus`.
    let search_focus = match &app.mode {
        Mode::Search(state) => Some(state.focus),
        _ => None,
    };
    let search_active = search_focus.is_some();
    let areas = view::dashboard_areas(inner, search_active);

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
    // so the highlight never lands on a pane the user cannot see. The queue pane is display-only.
    let detail_visible = areas.detail.is_some();
    // Record it so the `tab` key handler (which has no access to the frame size) can clamp focus
    // navigation to the panes actually on screen.
    app.detail_visible = detail_visible;
    // Record queue-pane visibility on the same cadence (#38): the poll loop reads it to skip the
    // `current_user_queue()` call when the pane is off-screen. Mode-independent — the queue pane is
    // drawn in both Normal and Search — so `areas.queue.is_some()` is the sole source of truth.
    app.queue_visible = areas.queue.is_some();
    // In search mode the focused pane comes from the search state; otherwise from `app.focus`.
    let focus = search_focus.unwrap_or(app.focus).effective(detail_visible);
    if let Some(queue_area) = areas.queue {
        queue::draw_queue_pane(frame, app, queue_area);
    }
    // The search bar row is present only in search mode (pure splitter returns `Some` then).
    if let Some(bar) = areas.search_bar {
        search::draw_search_bar(frame, app, bar);
    }
    // Lower panes: search swaps in the results/highlight; Normal shows the library/detail.
    if search_active {
        search::draw_search_results_pane(frame, app, areas.library, focus == view::Focus::Library);
        if let Some(detail_area) = areas.detail {
            search::draw_search_detail_pane(frame, app, detail_area, focus == view::Focus::Detail);
        }
    } else {
        browse::draw_library_pane(frame, app, areas.library, focus == view::Focus::Library);
        if let Some(detail_area) = areas.detail {
            detail::draw_detail_pane(frame, app, detail_area, focus == view::Focus::Detail);
        }
    }

    draw_status_line(frame, app, areas.status);
    draw_playbar(frame, &v, areas.playbar);
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
    playback::draw_now_playing_pane(frame, app, areas.now_playing, art_cols, &v);
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

/// Draw the bottom playback bar (the single source of progress): a play/pause glyph, the elapsed
/// time, a graphical `▬▬●───` slider, the total time, and a `▮▮▯▯▯ 40%` volume bar. All layout math
/// (including the narrow-terminal degrade that keeps the volume visible) lives in the pure
/// `view::playbar_segments`; this only maps each segment to a color (accent = green, knob = bold
/// green, track/empty = dim).
fn draw_playbar(frame: &mut ratatui::Frame, v: &view::RenderLines, area: ratatui::layout::Rect) {
    let green = Style::default().fg(theme::GREEN);
    let knob_style = green.add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);

    let segs = view::playbar_segments(
        area.width as usize,
        v.is_playing,
        v.ratio,
        &v.elapsed_label,
        &v.total_label,
        v.volume,
    );
    let spans: Vec<Span> = segs
        .into_iter()
        .map(|s| match s {
            view::PlaybarSeg::Accent(t) => Span::styled(t, green),
            view::PlaybarSeg::Knob(t) => Span::styled(t, knob_style),
            view::PlaybarSeg::Track(t) => Span::styled(t, dim),
            view::PlaybarSeg::Plain(t) => Span::raw(t),
        })
        .collect();

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Shared renderer for the lower-left tabbed list pane (library and search results): a bordered block
/// with a tab header, a hint/message line, and the selectable row list. `Header` rows are dimmed and
/// skipped by selection; `Item` rows reuse the same `search_row` formatter. The border is highlighted
/// (GREEN bold) while focused, dimmed otherwise, matching the other lower panes. `has_selection` gates
/// the highlight so it never lands on a header or an empty list.
#[allow(clippy::too_many_arguments)]
fn draw_tabbed_list_pane(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    focused: bool,
    title: &str,
    tab_header: String,
    hint: String,
    rows_data: &[browse::LibraryRow],
    selected: usize,
    has_selection: bool,
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
        .title(title.to_string());
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

    frame.render_widget(Paragraph::new(tab_header).style(bold), rows[0]);
    frame.render_widget(Paragraph::new(hint).style(dim), rows[1]);

    let width = inner.width as usize;
    let items: Vec<ListItem> = rows_data
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
    if has_selection {
        list_state.select(Some(selected));
    }
    let list = List::new(items).highlight_symbol("▶ ").highlight_style(
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_stateful_widget(list, rows[2], &mut list_state);
}
