//! 対話型 TUI（Phase 6）。Now Playing をライブ表示し、キー操作で再生を制御する。
//!
//! - 認証は起動時に一度だけ [`crate::auth::authed_client`] でクライアントを組み立て、以降は
//!   それを保持して使い回す（reqwest の接続プールを捨てず、毎操作のディスク読みも避ける）。
//!   トークンは [`crate::auth::ensure_fresh_token`] で期限切れ時のみ更新する。
//! - `POLL_INTERVAL` ごとに `current_playback` を取得し、ポーリング間は
//!   [`view::interpolate_progress`] で進捗をローカル補間して滑らかに見せる。
//! - API エラーはステータス行に出してループは継続する（silent failure 禁止）。

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
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{
    CurrentPlaybackContext, FullTrack, LibraryId, PlayableId, PlayableItem, SearchResult,
    SearchType, TrackId,
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
/// シークステップ（←/→、ミリ秒）。
const SEEK_STEP_MS: i64 = 5_000;
/// 連続ポーリング失敗がこの回数に達したら自動更新を止める（無効トークン等での無限リトライ回避）。
const MAX_POLL_FAILURES: u32 = 3;
/// 検索時に取得する上限件数。
const SEARCH_LIMIT: u32 = 10;

type Term = Terminal<CrosstermBackend<Stdout>>;

/// 画面モード。通常は Now Playing、`/` で検索、`2` でライブラリ閲覧、`d` でデバイス選択に入る。
enum Mode {
    Normal,
    Search(SearchState),
    Browse(browse::BrowseState),
    Devices(devices::DevicePickerState),
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
    /// 起動時に組み立て、セッション中ずっと使い回す認証済みクライアント。
    /// 毎操作で作り直すと接続プールを捨てて都度 TLS からやり直す（＝重い）ため保持する。
    client: AuthCodePkceSpotify,
    /// 直近に取得した再生状況（無再生なら `None`）。
    now: Option<NowPlaying>,
    /// 直近の操作結果・エラーを表示するステータス行。
    status: String,
    /// 最後にポーリングした時刻（`None` は即時ポーリングを要求）。
    last_poll: Option<Instant>,
    /// 連続ポーリング失敗回数。閾値を超えたら自動更新を止め、手動再試行を促す。
    poll_failures: u32,
    /// 画面モード（通常 / 検索 / ライブラリ閲覧 / デバイス選択）。
    mode: Mode,
    /// ライブラリ閲覧タブごとの取得結果キャッシュ（タブ切替での再取得を避ける）。
    browse_cache: browse::BrowseCache,
    /// 現在曲がライブラリに保存済みか（`None` は未確定）。曲変化時のみ再取得する。
    saved: Option<bool>,
    /// 現在曲の保存状態を問い合わせ済みか。曲ごとに 1 回だけ問い合わせ、永続失敗での
    /// 毎ポーリング連打を防ぐ（曲が変わると `false` に戻す）。
    saved_checked: bool,
}

/// `spoterm tui`: Now Playing ダッシュボードを起動する。
pub async fn run(cfg: &Config) -> Result<()> {
    // 未ログインならここで分かりやすく失敗させ、端末を alt-screen にしない。
    // このクライアントをそのままループへ渡し、セッション中は使い回す。
    let client = auth::authed_client(cfg)
        .await
        .context("TUI を起動できません")?;

    install_panic_hook();
    let mut terminal = setup_terminal().context("端末の初期化に失敗しました")?;
    let result = run_loop(&mut terminal, client).await;
    // 描画結果に関わらず端末は必ず元に戻す。両方失敗したら両方を伝える。
    let restored = restore_terminal(&mut terminal);
    match (result, restored) {
        (Ok(()), restored) => restored,
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(re)) => Err(e.context(format!("さらに端末の復元にも失敗しました: {re}"))),
    }
}

/// メインループ。ポーリング → 描画 → 入力処理を繰り返す。
async fn run_loop(terminal: &mut Term, client: AuthCodePkceSpotify) -> Result<()> {
    let mut app = App {
        client,
        now: None,
        status: "起動中…".to_string(),
        last_poll: None,
        poll_failures: 0,
        mode: Mode::Normal,
        browse_cache: browse::BrowseCache::default(),
        saved: None,
        saved_checked: false,
    };

    loop {
        // `last_poll` が None のときは強制ポーリング（起動直後・操作直後・`r`）。タイマー起因の
        // 自動更新は連続失敗が閾値未満のときだけ行う（無効トークンでの毎 2 秒リトライを避ける）。
        let forced = app.last_poll.is_none();
        let timer_due = app.last_poll.is_none_or(|t| t.elapsed() >= POLL_INTERVAL);
        if forced || (timer_due && app.poll_failures < MAX_POLL_FAILURES) {
            poll_playback(&mut app).await;
            app.last_poll = Some(Instant::now());
        }

        terminal.draw(|frame| draw(frame, &app))?;

        // TICK までキー入力を待つ（無ければ再描画して進捗を進める）。
        // Windows ではリリースでも発火するため押下のみ処理する。
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

/// キー入力を処理する。終了要求なら `true` を返す。
async fn handle_key(key: KeyEvent, app: &mut App) -> bool {
    // Ctrl-C はどのモードでも終了。
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }
    if matches!(app.mode, Mode::Search(_)) {
        handle_search_key(key, app).await;
        false
    } else if matches!(app.mode, Mode::Browse(_)) {
        handle_browse_key(key, app).await;
        false
    } else if matches!(app.mode, Mode::Devices(_)) {
        handle_devices_key(key, app).await;
        false
    } else {
        handle_normal_key(key, app).await
    }
}

/// 通常（Now Playing）モードのキー処理。終了要求なら `true`。
async fn handle_normal_key(key: KeyEvent, app: &mut App) -> bool {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Char('/') => app.mode = Mode::Search(SearchState::new()),
        KeyCode::Char('2') => load_browse(app, browse::BrowseTab::Playlists).await,
        KeyCode::Char('d') => open_devices(app).await,
        KeyCode::Char(' ') => control_toggle(app).await,
        KeyCode::Char('n') => control_next(app).await,
        KeyCode::Char('p') => control_prev(app).await,
        KeyCode::Char('+') | KeyCode::Char('=') => control_volume(app, VOL_STEP).await,
        KeyCode::Char('-') | KeyCode::Char('_') => control_volume(app, -VOL_STEP).await,
        KeyCode::Left => control_seek(app, -SEEK_STEP_MS).await,
        KeyCode::Right => control_seek(app, SEEK_STEP_MS).await,
        KeyCode::Char('s') => control_save(app).await,
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
async fn handle_search_key(key: KeyEvent, app: &mut App) {
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
        SearchAction::Submit(q) => run_search(app, &q).await,
        SearchAction::Play(uri) => play_uri(app, uri).await,
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
async fn run_search(app: &mut App, q: &str) {
    match search_tracks(app, q).await {
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

async fn search_tracks(app: &App, q: &str) -> Result<Vec<TrackHit>> {
    auth::ensure_fresh_token(&app.client).await?;
    let result = app
        .client
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
async fn play_uri(app: &mut App, uri: String) {
    match play_track(app, &uri).await {
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

async fn play_track(app: &App, uri: &str) -> Result<()> {
    let id = TrackId::from_uri(uri).context("トラック URI の解析に失敗しました")?;
    auth::ensure_fresh_token(&app.client).await?;
    app.client
        .start_uris_playback([PlayableId::Track(id)], None, None, None)
        .await
        .context("再生の開始に失敗しました（アクティブなデバイスが必要かもしれません）")?;
    Ok(())
}

// ---- API 連携 ---------------------------------------------------------------

/// 再生状況を取得して `app.now` を更新する。失敗はステータス行に出す。
async fn poll_playback(app: &mut App) {
    match fetch_playback(app).await {
        Ok(Some(np)) => {
            // 曲が変わったら保存状態を破棄し、次で取り直す（毎ポーリングではなく変化時のみ）。
            let prev_uri = app.now.as_ref().and_then(|n| n.track_uri.clone());
            if np.track_uri != prev_uri {
                app.saved = None;
                app.saved_checked = false;
            }
            app.now = Some(np);
            app.poll_failures = 0;
            refresh_saved(app).await;
        }
        Ok(None) => {
            app.now = None;
            app.saved = None;
            app.saved_checked = false;
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

async fn fetch_playback(app: &App) -> Result<Option<NowPlaying>> {
    auth::ensure_fresh_token(&app.client).await?;
    let ctx = app
        .client
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

    // track_uri は保存操作・曲変化検知に使う。Track は型付き ID、Unknown は生 JSON から取り出す。
    let (title, artists, album, duration_ms, track_uri) = match ctx.item {
        Some(PlayableItem::Track(t)) => {
            let artists: Vec<String> = t.artists.into_iter().map(|a| a.name).collect();
            let dur = t.duration.num_milliseconds().max(0) as u128;
            let uri = t.id.as_ref().map(|id| id.uri());
            (t.name, artists, Some(t.album.name), dur, uri)
        }
        Some(PlayableItem::Episode(e)) => {
            let dur = e.duration.num_milliseconds().max(0) as u128;
            (
                e.name,
                vec!["(ポッドキャスト)".to_string()],
                None,
                dur,
                None,
            )
        }
        // status コマンドと同じく、Unknown に落ちた生 JSON からフォールバック抽出する。
        Some(PlayableItem::Unknown(v)) => {
            let (title, artists, album, dur) = crate::commands::status::track_from_json(&v);
            (
                title,
                artists,
                album,
                dur,
                crate::commands::status::track_id_from_json(&v),
            )
        }
        None => (
            "(再生中の曲情報なし)".to_string(),
            Vec::new(),
            None,
            0,
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
        fetched_at: Instant::now(),
    }
}

/// 保持中クライアントのトークンを必要なら更新する。失敗時はステータス行に出して `false` を返す。
async fn ensure_ready(app: &mut App) -> bool {
    match auth::ensure_fresh_token(&app.client).await {
        Ok(()) => true,
        Err(e) => {
            app.status = format!("⚠ {e}");
            false
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
            app.status = format!("⚠ 操作に失敗: {e}（d でデバイスを選択して有効化してください）");
        }
    }
}

async fn control_toggle(app: &mut App) {
    let playing = app.now.as_ref().is_some_and(|n| n.is_playing);
    if !ensure_ready(app).await {
        return;
    }
    // 借用衝突を避けるため結果を先に確定してから finish（&mut app）へ渡す。
    if playing {
        let res = app.client.pause_playback(None).await;
        finish(app, res, "⏸ 一時停止");
    } else {
        let res = app.client.resume_playback(None, None).await;
        finish(app, res, "▶ 再生");
    }
}

async fn control_next(app: &mut App) {
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.next_track(None).await;
    finish(app, res, "⏭ 次の曲へ");
}

async fn control_prev(app: &mut App) {
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.previous_track(None).await;
    finish(app, res, "⏮ 前の曲へ");
}

async fn control_volume(app: &mut App, delta: i16) {
    let Some(cur) = app.now.as_ref().and_then(|n| n.volume) else {
        app.status = "⚠ デバイス音量が取得できません（d でデバイスを選択してください）".to_string();
        return;
    };
    let next = (cur as i16 + delta).clamp(0, 100) as u8;
    if !ensure_ready(app).await {
        return;
    }
    let res = app.client.volume(next, None).await;
    finish(app, res, &format!("🔊 音量 {next}%"));
}

/// 現在曲の保存状態を取得して `app.saved` を更新する。best-effort：`saved` 未確定かつ URI が
/// あるときだけ問い合わせ、失敗しても状態を出さない（本流のポーリングがネットワーク/トークン
/// エラーを報告するため、ここで status を上書きして混乱させない）。マーカーが出ないだけ。
async fn refresh_saved(app: &mut App) {
    // 曲ごとに 1 回だけ問い合わせる（`saved_checked`）。永続失敗でも毎ポーリング連打しない。
    if app.saved_checked {
        return;
    }
    let Some(uri) = app.now.as_ref().and_then(|n| n.track_uri.clone()) else {
        return; // トラック不明（エピソード等）は問い合わせない。API も呼ばない。
    };
    let Ok(id) = TrackId::from_uri(&uri) else {
        app.saved_checked = true;
        return;
    };
    // トークン更新失敗は打ち止めにしない（本流ポーリングが失敗を報告し、閾値超で自動更新自体が止まる）。
    if auth::ensure_fresh_token(&app.client).await.is_err() {
        return;
    }
    // 成否に関わらずこの曲での再問い合わせは打ち止め（best-effort）。
    app.saved_checked = true;
    if let Ok(mut flags) = app.client.library_contains([LibraryId::Track(id)]).await {
        app.saved = flags.pop();
    }
}

/// 現在曲を ±`delta_ms` シークする。目標はローカル進捗（補間込み）から算出し、成功したら
/// 進捗を即時更新して画面へ反映する（Connect の伝播遅延で巻き戻って見えるのを避けるため
/// 強制ポーリングはしない）。連打は都度更新するローカル進捗から積算される。
async fn control_seek(app: &mut App, delta_ms: i64) {
    let Some(n) = app.now.as_ref() else {
        app.status = "⚠ 再生中の曲がありません".to_string();
        return;
    };
    let elapsed = n.fetched_at.elapsed().as_millis();
    let current = view::interpolate_progress(n.progress_ms, elapsed, n.duration_ms, n.is_playing);
    let target = view::seek_target(current, n.duration_ms, delta_ms);
    if !ensure_ready(app).await {
        return;
    }
    // target as i64: target は duration_ms でクランプ済み（尺不明時も現実的な連打回数の範囲）で
    // i64::MAX（≒2.9 億年）に達しないため安全。
    let res = app
        .client
        .seek_track(chrono::Duration::milliseconds(target as i64), None)
        .await;
    match res {
        Ok(()) => {
            // ローカル進捗を即時反映（強制ポーリングはしない）。
            if let Some(n) = app.now.as_mut() {
                n.progress_ms = target;
                n.fetched_at = Instant::now();
            }
            app.status = format!("⏩ シーク {}", crate::format::format_ms(target));
        }
        Err(e) => {
            app.status = format!("⚠ シークに失敗: {e}（d でデバイスを選択して有効化してください）");
        }
    }
}

/// 現在曲をライブラリに保存/解除する（`s`）。現在の保存状態の反対にし、成功で状態を更新する。
async fn control_save(app: &mut App) {
    let Some(uri) = app.now.as_ref().and_then(|n| n.track_uri.clone()) else {
        app.status = "⚠ 現在の曲を保存できません（トラック情報が不明です）".to_string();
        return;
    };
    let id = match TrackId::from_uri(&uri) {
        Ok(id) => id,
        Err(e) => {
            app.status = format!("⚠ トラック URI の解析に失敗: {e}");
            return;
        }
    };
    if !ensure_ready(app).await {
        return;
    }
    // 未確定なら「保存する」と解釈する。
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
                "♥ ライブラリに保存しました".to_string()
            } else {
                "♡ 保存を解除しました".to_string()
            };
        }
        Err(e) => {
            app.status = format!("⚠ 保存操作に失敗: {e}");
        }
    }
}

// ---- ライブラリ閲覧（browse） -----------------------------------------------

/// 閲覧オーバーレイのキー処理。同期で選択を更新し、必要な非同期アクションを実行する。
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
            // 現在タブのキャッシュを捨てて取り直す（＝ユーザー主導のリロード）。
            let Mode::Browse(state) = &app.mode else {
                return;
            };
            let tab = state.tab;
            app.browse_cache.clear(tab);
            load_browse(app, tab).await;
        }
    }
}

/// 指定タブの一覧を表示して閲覧モードに入る（既に閲覧中ならタブ切替）。
/// キャッシュがあればネットワークへ行かず、無いときだけ取得してキャッシュする。失敗は案内する。
async fn load_browse(app: &mut App, tab: browse::BrowseTab) {
    // キャッシュ済みなら複製して即表示（clone は数十件の小さな構造体で安価）。
    let items = match app.browse_cache.get(tab).cloned() {
        Some(items) => items,
        None => {
            match browse::fetch(&app.client, tab).await {
                Ok(items) => {
                    app.browse_cache.set(tab, items.clone());
                    items
                }
                Err(e) => {
                    // 閲覧中なら画面に留めてメッセージ表示、通常表示中ならステータス行へ。
                    if let Mode::Browse(state) = &mut app.mode {
                        state.message = Some(format!("取得に失敗しました: {e}"));
                    } else {
                        app.status = format!("⚠ ライブラリの取得に失敗: {e}");
                    }
                    return;
                }
            }
        }
    };
    let message = items
        .is_empty()
        .then(|| format!("{} は空です", tab.label()));
    app.mode = Mode::Browse(browse::BrowseState {
        tab,
        items,
        selected: 0,
        message,
    });
}

/// 選択項目を再生する。成功で通常表示へ戻り、失敗はオーバーレイにメッセージを残す。
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
            app.status = "▶ 再生を開始しました".to_string();
            app.last_poll = None;
            app.mode = Mode::Normal;
        }
        Err(e) => {
            if let Mode::Browse(state) = &mut app.mode {
                state.message = Some(format!("再生に失敗しました: {e}"));
            } else {
                app.status = format!("⚠ 再生に失敗: {e}");
            }
        }
    }
}

// ---- デバイス選択（device picker） ------------------------------------------

/// デバイス選択オーバーレイのキー処理。同期で選択を更新し、必要な非同期アクションを実行する。
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

/// デバイス一覧を取得して選択モードに入る。空一覧・取得失敗は案内する（silent failure 禁止）。
/// デバイスは出入りするためキャッシュせず、開くたびに取り直す。
async fn open_devices(app: &mut App) {
    let items = match devices::fetch(&app.client).await {
        Ok(items) => items,
        Err(e) => {
            // 選択中なら画面に留めてメッセージ表示、通常表示中ならステータス行へ。
            if let Mode::Devices(state) = &mut app.mode {
                state.message = Some(format!("取得に失敗しました: {e}"));
            } else {
                app.status = format!("⚠ デバイス一覧の取得に失敗: {e}");
            }
            return;
        }
    };
    let message = items.is_empty().then(|| {
        "再生可能なデバイスがありません。Spotify アプリまたは spotifyd を起動してください"
            .to_string()
    });
    // 再取得時に選択が範囲外へずれないよう、アクティブ位置（無ければ先頭）に寄せる。
    let selected = items.iter().position(|d| d.is_active).unwrap_or(0);
    app.mode = Mode::Devices(devices::DevicePickerState {
        items,
        selected,
        message,
    });
}

/// 選択デバイスへ再生を転送する。成功で通常表示へ戻り即ポーリング、失敗はオーバーレイに残す。
/// 転送不可（ID なし / 操作不可）は事前に弾いてメッセージで伝える。
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
            state.message = Some(format!("'{}' は操作不可のため転送できません", target.name));
        }
        return;
    }
    let Some(id) = target.id.as_deref() else {
        if let Mode::Devices(state) = &mut app.mode {
            state.message = Some(format!("'{}' は ID がなく転送できません", target.name));
        }
        return;
    };
    match devices::transfer(&app.client, id).await {
        Ok(()) => {
            app.status = format!("▶ '{}' へ再生を移しました", target.name);
            app.last_poll = None; // 転送を素早く Now Playing へ反映
            app.mode = Mode::Normal;
        }
        Err(e) => {
            if let Mode::Devices(state) = &mut app.mode {
                state.message = Some(format!("転送に失敗しました: {e}"));
            } else {
                app.status = format!("⚠ 転送に失敗: {e}");
            }
        }
    }
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
        Mode::Browse(state) => draw_browse(frame, state),
        Mode::Devices(state) => draw_devices(frame, state),
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
    let v = view::render_lines(app.now.as_ref(), elapsed, inner.width as usize, app.saved);

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
        Paragraph::new(
            "space ⏯  n ⏭  p ⏮  ←→ シーク  +/- 音量  s ♥  / 検索  2 ライブラリ  d デバイス  r 更新  q 終了",
        )
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::DIM)),
        rows[8],
    );
}

/// ライブラリ閲覧表示（タブ + 一覧）。
fn draw_browse(frame: &mut ratatui::Frame, state: &browse::BrowseState) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" spoterm — ライブラリ ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // タブ見出し
            Constraint::Length(1), // 補足
            Constraint::Min(1),    // 一覧
            Constraint::Length(1), // フッター
        ])
        .split(inner);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);

    // タブ見出し（現在タブを [ ] で囲う）。
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
            "{} 件 — ↑↓ 選択 / ←→ タブ / Enter 再生 / r 更新 / Esc 戻る",
            state.items.len()
        )
    });
    frame.render_widget(Paragraph::new(hint).style(dim), rows[1]);

    // 一覧（title — subtitle。行整形は検索と共通の純粋関数を再利用）。
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
        Paragraph::new("↑↓ 選択   ←→ タブ   Enter 再生   r 更新   Esc 戻る   Ctrl-C 終了")
            .alignment(Alignment::Center)
            .style(dim),
        rows[3],
    );
}

/// デバイス選択表示（一覧 + 選択強調）。
fn draw_devices(frame: &mut ratatui::Frame, state: &devices::DevicePickerState) {
    let area = frame.area();
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" spoterm — デバイス ");
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 補足
            Constraint::Min(1),    // 一覧
            Constraint::Length(1), // フッター
        ])
        .split(inner);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let dim = Style::default().add_modifier(Modifier::DIM);

    let hint = state.message.clone().unwrap_or_else(|| {
        format!(
            "{} 台 — ↑↓ 選択 / Enter 転送 / r 更新 / Esc 戻る",
            state.items.len()
        )
    });
    frame.render_widget(Paragraph::new(hint).style(dim), rows[0]);

    // 一覧（デバイス行整形は純粋関数 `view::device_row` に委譲）。
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
        Paragraph::new("↑↓ 選択   Enter 転送   r 更新   Esc 戻る   Ctrl-C 終了")
            .alignment(Alignment::Center)
            .style(dim),
        rows[2],
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
