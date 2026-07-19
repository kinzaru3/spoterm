//! ライブラリ閲覧オーバーレイ（Phase 6.2）。プレイリスト / 保存トラック / 保存アルバムを
//! タブで切り替えて一覧・再生する。`App` には触れず、データ取得・再生・キー→アクション変換のみ担う
//! （画面状態の更新と描画は `mod.rs` 側）。既存の `playlist`/`lib` コマンドと同じ API を再利用する。

use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use rspotify::AuthCodePkceSpotify;
use rspotify::model::{AlbumId, PlayContextId, PlayableId, PlaylistId, TrackId};
use rspotify::prelude::*;

use crate::auth;
use crate::format::join_artists;

/// プレイリストの取得件数（API 上限 50・先頭ページのみ）。
const PLAYLIST_LIMIT: u32 = 50;
/// 保存トラック/アルバムの取得件数（先頭ページのみ）。
const SAVED_LIMIT: u32 = 20;

/// 閲覧タブ。
#[derive(Clone, Copy, PartialEq)]
pub enum BrowseTab {
    Playlists,
    Tracks,
    Albums,
}

impl BrowseTab {
    /// 全タブ（ヘッダ表示・切替の基準）。
    pub const ALL: [BrowseTab; 3] = [BrowseTab::Playlists, BrowseTab::Tracks, BrowseTab::Albums];

    pub fn label(self) -> &'static str {
        match self {
            BrowseTab::Playlists => "プレイリスト",
            BrowseTab::Tracks => "保存トラック",
            BrowseTab::Albums => "保存アルバム",
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

/// 再生方法。トラックは URI 単体再生、プレイリスト/アルバムはコンテキスト再生。
#[derive(Clone)]
pub enum PlayTarget {
    Track(String),
    Playlist(String),
    Album(String),
}

/// 一覧の 1 項目。
#[derive(Clone)]
pub struct BrowseItem {
    pub title: String,
    pub subtitle: String,
    pub target: PlayTarget,
}

/// タブごとの取得結果をセッション内でキャッシュする。タブ切替では再取得せず、
/// `r`（リロード）でのみ破棄して取り直すことで、閲覧中の API 呼び出しを抑える。
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

    /// キャッシュ済みならその一覧を返す（空一覧も有効なキャッシュとして扱う）。
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

    /// 指定タブのキャッシュを破棄する（リロード時に呼び、次回取得を強制する）。
    pub fn clear(&mut self, tab: BrowseTab) {
        *self.slot(tab) = None;
    }
}

/// 閲覧オーバーレイの状態。
pub struct BrowseState {
    pub tab: BrowseTab,
    pub items: Vec<BrowseItem>,
    pub selected: usize,
    pub message: Option<String>,
}

/// キー処理が本体に依頼するアクション。
pub enum BrowseAction {
    None,
    /// オーバーレイを閉じる。
    Close,
    /// タブを切り替える（キャッシュがあれば再取得しない）。
    Switch(BrowseTab),
    /// 選択項目を再生する。
    Play,
    /// 現在タブのキャッシュを捨てて取り直す。
    Reload,
}

/// 選択位置を同期更新し、必要なアクションを返す。
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

/// 指定タブの一覧を取得する（既存コマンドと同じ API・先頭ページのみ）。
/// クライアントは呼び出し側が保持し続けるものを借り、必要なときだけトークンを更新する。
pub async fn fetch(spotify: &AuthCodePkceSpotify, tab: BrowseTab) -> Result<Vec<BrowseItem>> {
    auth::ensure_fresh_token(spotify).await?;
    match tab {
        BrowseTab::Playlists => {
            let page = spotify
                .current_user_playlists_manual(Some(PLAYLIST_LIMIT), None)
                .await
                .context("プレイリスト一覧の取得に失敗しました")?;
            Ok(page
                .items
                .into_iter()
                .map(|pl| BrowseItem {
                    title: pl.name,
                    subtitle: format!("{}曲", pl.items.total),
                    target: PlayTarget::Playlist(pl.id.uri()),
                })
                .collect())
        }
        BrowseTab::Tracks => {
            let page = spotify
                .current_user_saved_tracks_manual(None, Some(SAVED_LIMIT), None)
                .await
                .context("保存済みトラックの取得に失敗しました")?;
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
                .context("保存済みアルバムの取得に失敗しました")?;
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

/// 選択項目を再生する。トラックは URI 単体、プレイリスト/アルバムはコンテキスト再生。
pub async fn play(spotify: &AuthCodePkceSpotify, target: &PlayTarget) -> Result<()> {
    auth::ensure_fresh_token(spotify).await?;
    let result = match target {
        PlayTarget::Track(uri) => {
            let id = TrackId::from_uri(uri).context("トラック URI の解析に失敗しました")?;
            spotify
                .start_uris_playback([PlayableId::Track(id)], None, None, None)
                .await
        }
        PlayTarget::Playlist(uri) => {
            let id = PlaylistId::from_uri(uri).context("プレイリスト URI の解析に失敗しました")?;
            spotify
                .start_context_playback(PlayContextId::Playlist(id), None, None, None)
                .await
        }
        PlayTarget::Album(uri) => {
            let id = AlbumId::from_uri(uri).context("アルバム URI の解析に失敗しました")?;
            spotify
                .start_context_playback(PlayContextId::Album(id), None, None, None)
                .await
        }
    };
    result.context("再生の開始に失敗しました（アクティブなデバイスが必要かもしれません）")?;
    Ok(())
}
