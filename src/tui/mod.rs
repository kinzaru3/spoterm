//! 対話型 TUI（Phase 6）。Now Playing をライブ表示し、キー操作で再生を制御する。
//!
//! - 認証・トークン更新は既存の [`crate::auth::authed_client`] を再利用する。
//! - `POLL_INTERVAL` ごとに `current_playback` を取得し、ポーリング間は
//!   [`view::interpolate_progress`] で進捗をローカル補間して滑らかに見せる。
//! - API エラーはステータス行に出してループは継続する（silent failure 禁止）。

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
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{
    CurrentPlaybackContext, FullTrack, PlayableId, PlayableItem, SearchResult, SearchType, TrackId,
};
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::format::join_artists;
use view::NowPlaying;

/// 再生状況を再取得する間隔。
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// 入力待ちの 1 ティック（この間隔で再描画し、進捗補間を反映する）。
const TICK: Duration = Duration::from_millis(200);
/// 音量ステップ（+/-）。
const VOL_STEP: i16 = 5;
/// 連続ポーリング失敗がこの回数に達したら自動更新を止める（無効トークン等での無限リトライ回避）。
const MAX_POLL_FAILURES: u32 = 3;
/// 検索時に取得する上限件数。
const SEARCH_LIMIT: u32 = 10;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// 画面モード。通常は Now Playing、`/` で検索オーバーレイに入る。
enum Mode {
    Normal,
    Search(SearchState),
}

/// 検索オーバーレイの状態。
struct SearchState {
    /// 入力中のクエリ。
    query: String,
    /// 入力中か結果選択中か。
    phase: SearchPhase,
    /// 検索結果（再生可能なトラックのみ）。
    results: Vec<TrackHit>,
    /// 結果リストの選択位置。
    selected: usize,
    /// 補足メッセージ（0 件・エラーなど）。
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

/// 検索オーバーレイのフェーズ。
#[derive(Clone, Copy, PartialEq)]
enum SearchPhase {
    /// クエリ入力中。
    Input,
    /// 結果を選択中。
    Results,
}

/// 検索結果の 1 トラック（再生に使う URI を保持）。
struct TrackHit {
    name: String,
    artists: String,
    uri: String,
}

/// 検索キー処理が本体に依頼する非同期アクション。
enum SearchAction {
    None,
    /// オーバーレイを閉じて通常表示へ戻る。
    Close,
    /// クエリで検索を実行する。
    Submit(String),
    /// 選択トラック（URI）を再生する。
    Play(String),
    /// 結果選択から入力へ戻る（クエリ修正）。
    BackToInput,
}

/// TUI アプリの状態。
struct App {
    /// 直近に取得した再生状況（無再生なら `None`）。
    now: Option<NowPlaying>,
    /// 直近の操作結果・エラーを表示するステータス行。
    status: String,
    /// 最後にポーリングした時刻（`None` は即時ポーリングを要求）。
    last_poll: Option<Instant>,
    /// 連続ポーリング失敗回数。閾値を超えたら自動更新を止め、手動再試行を促す。
    poll_failures: u32,
    /// 画面モード（通常 / 検索オーバーレイ）。
    mode: Mode,
}

/// `spoterm tui`: Now Playing ダッシュボードを起動する。
pub async fn run(cfg: &Config) -> Result<()> {
    // 未ログインならここで分かりやすく失敗させ、端末を alt-screen にしない。
    auth::authed_client(cfg)
        .await
        .context("TUI を起動できません")?;

    install_panic_hook();
    let mut terminal = setup_terminal().context("端末の初期化に失敗しました")?;
    let result = run_loop(&mut terminal, cfg).await;
    // 描画結果に関わらず端末は必ず元に戻す。両方失敗したら両方を伝える。
    let restored = restore_terminal(&mut terminal);
    match (result, restored) {
        (Ok(()), restored) => restored,
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(re)) => Err(e.context(format!("さらに端末の復元にも失敗しました: {re}"))),
    }
}

/// メインループ。ポーリング → 描画 → 入力処理を繰り返す。
async fn run_loop(terminal: &mut Term, cfg: &Config) -> Result<()> {
    let mut app = App {
        now: None,
        status: "起動中…".to_string(),
        last_poll: None,
        poll_failures: 0,
        mode: Mode::Normal,
    };

    loop {
        // `last_poll` が None のときは強制ポーリング（起動直後・操作直後・`r`）。タイマー起因の
        // 自動更新は連続失敗が閾値未満のときだけ行う（無効トークンでの毎 2 秒リトライを避ける）。
        let forced = app.last_poll.is_none();
        let timer_due = app.last_poll.is_none_or(|t| t.elapsed() >= POLL_INTERVAL);
        if forced || (timer_due && app.poll_failures < MAX_POLL_FAILURES) {
            poll_playback(cfg, &mut app).await;
            app.last_poll = Some(Instant::now());
        }

        terminal.draw(|frame| draw(frame, &app))?;

        // TICK までキー入力を待つ（無ければ再描画して進捗を進める）。
        // Windows ではリリースでも発火するため押下のみ処理する。
        if event::poll(TICK)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && handle_key(key, cfg, &mut app).await
        {
            break;
        }
    }
    Ok(())
}

/// キー入力を処理する。終了要求なら `true` を返す。
async fn handle_key(key: KeyEvent, cfg: &Config, app: &mut App) -> bool {
    // Ctrl-C はどのモードでも終了。
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }
    if matches!(app.mode, Mode::Search(_)) {
        handle_search_key(key, cfg, app).await;
        false
    } else {
        handle_normal_key(key, cfg, app).await
    }
}

/// 通常（Now Playing）モードのキー処理。終了要求なら `true`。
async fn handle_normal_key(key: KeyEvent, cfg: &Config, app: &mut App) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('/') => app.mode = Mode::Search(SearchState::new()),
        KeyCode::Char(' ') => control_toggle(cfg, app).await,
        KeyCode::Char('n') => control_next(cfg, app).await,
        KeyCode::Char('p') => control_prev(cfg, app).await,
        KeyCode::Char('+') | KeyCode::Char('=') => control_volume(cfg, app, VOL_STEP).await,
        KeyCode::Char('-') | KeyCode::Char('_') => control_volume(cfg, app, -VOL_STEP).await,
        // 手動更新: 失敗カウンタもリセットして自動更新を再開する。
        KeyCode::Char('r') => {
            app.poll_failures = 0;
            app.last_poll = None;
        }
        _ => {}
    }
    false
}

/// 検索オーバーレイのキー処理。同期でクエリ/選択を更新し、必要な非同期アクションを実行する。
async fn handle_search_key(key: KeyEvent, cfg: &Config, app: &mut App) {
    // まず借用を閉じてからアクションを実行する（非同期処理は app を再借用するため）。
    let action = {
        let Mode::Search(state) = &mut app.mode else {
            return;
        };
        search_key_action(key, state)
    };
    match action {
        SearchAction::None => {}
        SearchAction::Close => app.mode = Mode::Normal,
        SearchAction::Submit(q) => run_search(cfg, app, &q).await,
        SearchAction::Play(uri) => play_uri(cfg, app, uri).await,
        SearchAction::BackToInput => {
            if let Mode::Search(state) = &mut app.mode {
                // 入力に戻る＝クエリを組み直す想定。古い結果と選択は破棄する。
                state.phase = SearchPhase::Input;
                state.results.clear();
                state.selected = 0;
                state.message = None;
            }
        }
    }
}

/// クエリ/選択を同期更新し、必要な非同期アクションを返す。
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
            KeyCode::Enter => match state.results.get(state.selected) {
                Some(hit) => SearchAction::Play(hit.uri.clone()),
                None => SearchAction::None,
            },
            _ => SearchAction::None,
        },
    }
}

/// クエリでトラック検索し、結果フェーズへ遷移する。失敗時は入力フェーズに留めて案内する。
async fn run_search(cfg: &Config, app: &mut App, q: &str) {
    match search_tracks(cfg, q).await {
        Ok(hits) => {
            let message = hits
                .is_empty()
                .then(|| format!("\"{q}\" に一致するトラックはありませんでした"));
            app.mode = Mode::Search(SearchState {
                query: q.to_string(),
                phase: SearchPhase::Results,
                results: hits,
                selected: 0,
                message,
            });
        }
        Err(e) => {
            app.status = format!("⚠ 検索に失敗: {e}");
            if let Mode::Search(state) = &mut app.mode {
                state.phase = SearchPhase::Input;
                state.message = Some(format!("検索に失敗しました: {e}"));
            }
        }
    }
}

async fn search_tracks(cfg: &Config, q: &str) -> Result<Vec<TrackHit>> {
    let spotify = auth::authed_client(cfg).await?;
    let result = spotify
        .search(q, SearchType::Track, None, None, Some(SEARCH_LIMIT), None)
        .await
        .context("検索に失敗しました")?;
    let SearchResult::Tracks(page) = result else {
        anyhow::bail!("検索結果の形式が想定外です");
    };
    Ok(page.items.into_iter().filter_map(track_to_hit).collect())
}

/// 再生可能な（URI を持つ）トラックだけ `TrackHit` に写す。ローカル曲等は除外する。
fn track_to_hit(t: FullTrack) -> Option<TrackHit> {
    let uri = t.id.as_ref()?.uri();
    let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
    Some(TrackHit {
        name: t.name,
        artists: join_artists(&artists),
        uri,
    })
}

/// 選択トラックの URI を再生する。成功したら通常表示へ戻り、失敗はオーバーレイに残して
/// メッセージで伝える（検索画面は `app.status` を描画しないため、ここでは `state.message` に出す）。
async fn play_uri(cfg: &Config, app: &mut App, uri: String) {
    match play_track(cfg, &uri).await {
        Ok(()) => {
            app.status = "▶ 再生を開始しました".to_string();
            app.last_poll = None; // 再生開始を素早く画面へ反映
            app.mode = Mode::Normal;
        }
        Err(e) => {
            if let Mode::Search(state) = &mut app.mode {
                state.message = Some(format!("再生に失敗しました: {e}"));
            } else {
                app.status = format!("⚠ 再生に失敗: {e}");
            }
        }
    }
}

async fn play_track(cfg: &Config, uri: &str) -> Result<()> {
    let id = TrackId::from_uri(uri).context("トラック URI の解析に失敗しました")?;
    let spotify = auth::authed_client(cfg).await?;
    spotify
        .start_uris_playback([PlayableId::Track(id)], None, None, None)
        .await
        .context("再生の開始に失敗しました（アクティブなデバイスが必要かもしれません）")?;
    Ok(())
}

// ---- API 連携 ---------------------------------------------------------------

/// 再生状況を取得して `app.now` を更新する。失敗はステータス行に出す。
async fn poll_playback(cfg: &Config, app: &mut App) {
    match fetch_playback(cfg).await {
        Ok(Some(np)) => {
            app.now = Some(np);
            app.poll_failures = 0;
        }
        Ok(None) => {
            app.now = None;
            app.poll_failures = 0;
        }
        Err(e) => {
            app.poll_failures = app.poll_failures.saturating_add(1);
            app.status = if app.poll_failures >= MAX_POLL_FAILURES {
                format!("⚠ 自動更新を停止しました（{e}）。r で再試行 / q で終了")
            } else {
                format!("⚠ 更新失敗: {e}")
            };
        }
    }
}

async fn fetch_playback(cfg: &Config) -> Result<Option<NowPlaying>> {
    let spotify = auth::authed_client(cfg).await?;
    let ctx = spotify
        .current_playback(None, None::<Vec<_>>)
        .await
        .context("再生状況の取得に失敗しました")?;
    Ok(ctx.map(snapshot_from_context))
}

/// rspotify の再生コンテキストを表示用スナップショットへ写像する。
fn snapshot_from_context(ctx: CurrentPlaybackContext) -> NowPlaying {
    let device = ctx.device.name;
    // Spotify の契約上 0-100 だが、外部境界のため 100 で頭打ちしてから u8 化する（silent な wraparound 回避）。
    let volume = ctx.device.volume_percent.map(|v| v.min(100) as u8);
    let progress_ms = ctx
        .progress
        .map(|d| d.num_milliseconds().max(0) as u128)
        .unwrap_or(0);
    let is_playing = ctx.is_playing;

    let (title, artists, album, duration_ms) = match ctx.item {
        Some(PlayableItem::Track(t)) => {
            let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            let dur = t.duration.num_milliseconds().max(0) as u128;
            (t.name, artists, Some(t.album.name), dur)
        }
        Some(PlayableItem::Episode(e)) => {
            let dur = e.duration.num_milliseconds().max(0) as u128;
            (e.name, vec!["(ポッドキャスト)".to_string()], None, dur)
        }
        // status コマンドと同じく、Unknown に落ちた生 JSON からフォールバック抽出する。
        Some(PlayableItem::Unknown(v)) => crate::commands::status::track_from_json(&v),
        None => ("(再生中の曲情報なし)".to_string(), Vec::new(), None, 0),
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
        fetched_at: Instant::now(),
    }
}

/// 認証済みクライアントを取得する。失敗時はステータス行に出して `None` を返す。
async fn acquire_client(cfg: &Config, app: &mut App) -> Option<AuthCodePkceSpotify> {
    match auth::authed_client(cfg).await {
        Ok(c) => Some(c),
        Err(e) => {
            app.status = format!("⚠ {e}");
            None
        }
    }
}

/// 操作結果をステータス行へ反映し、成功時は即時ポーリングを予約する。
fn finish<E: std::fmt::Display>(app: &mut App, res: Result<(), E>, ok: &str) {
    match res {
        Ok(()) => {
            app.status = ok.to_string();
            app.last_poll = None; // 変更を素早く画面へ反映
        }
        Err(e) => {
            app.status = format!("⚠ 操作に失敗: {e}（アクティブなデバイスが必要かもしれません）");
        }
    }
}

async fn control_toggle(cfg: &Config, app: &mut App) {
    let playing = app.now.as_ref().is_some_and(|n| n.is_playing);
    let Some(c) = acquire_client(cfg, app).await else {
        return;
    };
    if playing {
        finish(app, c.pause_playback(None).await, "⏸ 一時停止");
    } else {
        finish(app, c.resume_playback(None, None).await, "▶ 再生");
    }
}

async fn control_next(cfg: &Config, app: &mut App) {
    let Some(c) = acquire_client(cfg, app).await else {
        return;
    };
    finish(app, c.next_track(None).await, "⏭ 次の曲へ");
}

async fn control_prev(cfg: &Config, app: &mut App) {
    let Some(c) = acquire_client(cfg, app).await else {
        return;
    };
    finish(app, c.previous_track(None).await, "⏮ 前の曲へ");
}

async fn control_volume(cfg: &Config, app: &mut App, delta: i16) {
    let Some(cur) = app.now.as_ref().and_then(|n| n.volume) else {
        app.status = "⚠ デバイス音量が取得できません".to_string();
        return;
    };
    let next = (cur as i16 + delta).clamp(0, 100) as u8;
    let Some(c) = acquire_client(cfg, app).await else {
        return;
    };
    finish(app, c.volume(next, None).await, &format!("🔊 音量 {next}%"));
}

// ---- 端末制御 ---------------------------------------------------------------

fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // 途中で失敗したら、それまでに変えた端末状態を戻してからエラーを返す（呼び出し側は
    // terminal を受け取れず restore_terminal を呼べないため、ここで後始末する）。
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

/// パニックしても端末を元に戻す（raw mode / alt-screen を解除）。
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original(info);
    }));
}

// ---- 描画 -------------------------------------------------------------------

fn draw(frame: &mut ratatui::Frame, app: &App) {
    match &app.mode {
        Mode::Normal => draw_now(frame, app),
        Mode::Search(state) => draw_search(frame, state),
    }
}

/// 通常（Now Playing）表示。
fn draw_now(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" spoterm — Now Playing ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 状態
            Constraint::Length(1), // 曲名
            Constraint::Length(1), // アーティスト
            Constraint::Length(1), // アルバム
            Constraint::Length(1), // 進捗ゲージ
            Constraint::Length(1), // デバイス
            Constraint::Min(1),    // 余白
            Constraint::Length(1), // ステータス
            Constraint::Length(1), // フッター（キー）
        ])
        .split(inner);

    // 表示行の組み立ては純粋関数 `view::render_lines` に委譲（テスト済み）。ここは widget 化のみ。
    let elapsed = app
        .now
        .as_ref()
        .map(|n| n.fetched_at.elapsed().as_millis())
        .unwrap_or(0);
    let v = view::render_lines(app.now.as_ref(), elapsed, inner.width as usize);

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
    frame.render_widget(Paragraph::new(app.status.clone()), rows[7]);
    frame.render_widget(
        Paragraph::new("space ⏯   n ⏭   p ⏮   +/- 音量   / 検索   r 更新   q 終了")
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::DIM)),
        rows[8],
    );
}

/// 検索オーバーレイ表示（入力欄 + 結果リスト）。
fn draw_search(frame: &mut ratatui::Frame, state: &SearchState) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" spoterm — 検索 ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 入力欄
            Constraint::Length(1), // 補足
            Constraint::Min(1),    // 結果リスト
            Constraint::Length(1), // フッター
        ])
        .split(inner);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);

    // 入力欄（入力フェーズのみカーソルを出す）。
    let cursor = if state.phase == SearchPhase::Input {
        "▌"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(format!("検索: {}{}", state.query, cursor)).style(bold),
        rows[0],
    );

    // 補足行（メッセージ優先、無ければフェーズ別の案内）。
    let hint = state.message.clone().unwrap_or_else(|| {
        view::search_hint(state.phase == SearchPhase::Input, state.results.len())
    });
    frame.render_widget(Paragraph::new(hint).style(dim), rows[1]);

    // 結果リスト（選択位置をハイライト）。
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
        Paragraph::new("入力 → Enter 検索   ↑↓ 選択   Enter 再生   Esc 戻る   Ctrl-C 終了")
            .alignment(Alignment::Center)
            .style(dim),
        rows[3],
    );
}
