//! デバイス選択オーバーレイ（Phase 6.3）。Spotify Connect の利用可能デバイスを一覧し、
//! 選択したデバイスへ再生をトランスファーする。`App` には触れず、データ取得・転送・
//! キー→アクション変換のみ担う（画面状態の更新と描画は `mod.rs` 側）。既存の `devices`/
//! `device use` コマンドと同じ API を再利用する。
//!
//! デバイスは出入りする（起動/終了する）ため、`browse` と違いキャッシュしない。開くたびに
//! 鮮度のある一覧を取り直し、`r` でも再取得する。

use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use rspotify::AuthCodePkceSpotify;
use rspotify::prelude::*;

use crate::auth;

/// 一覧の 1 デバイス。転送には `id` が必要で、`is_restricted` は操作不可を表す。
#[derive(Clone)]
pub struct DeviceEntry {
    pub name: String,
    /// 転送先 ID（Connect が ID を持たないデバイスもあるため Option）。
    pub id: Option<String>,
    pub type_label: String,
    pub volume: Option<u32>,
    pub is_active: bool,
    pub is_restricted: bool,
}

/// デバイス選択オーバーレイの状態。
pub struct DevicePickerState {
    pub items: Vec<DeviceEntry>,
    pub selected: usize,
    pub message: Option<String>,
}

/// キー処理が本体に依頼するアクション。
pub enum DeviceAction {
    None,
    /// オーバーレイを閉じる。
    Close,
    /// 選択デバイスへ再生を転送する。
    Transfer,
    /// 一覧を取り直す。
    Reload,
}

/// 選択位置を同期更新し、必要なアクションを返す（`browse::key_action` と同型）。
pub fn key_action(key: KeyEvent, state: &mut DevicePickerState) -> DeviceAction {
    match key.code {
        KeyCode::Esc => DeviceAction::Close,
        KeyCode::Up => {
            state.selected = state.selected.saturating_sub(1);
            DeviceAction::None
        }
        KeyCode::Down => {
            if state.selected + 1 < state.items.len() {
                state.selected += 1;
            }
            DeviceAction::None
        }
        KeyCode::Enter => DeviceAction::Transfer,
        KeyCode::Char('r') => DeviceAction::Reload,
        _ => DeviceAction::None,
    }
}

/// 利用可能デバイスを取得する（既存 `devices` コマンドと同じ API）。
/// クライアントは呼び出し側が保持し続けるものを借り、必要なときだけトークンを更新する。
pub async fn fetch(spotify: &AuthCodePkceSpotify) -> Result<Vec<DeviceEntry>> {
    auth::ensure_fresh_token(spotify).await?;
    let devices = spotify
        .device()
        .await
        .context("デバイス一覧の取得に失敗しました")?;
    Ok(devices
        .into_iter()
        .map(|d| DeviceEntry {
            name: d.name,
            id: d.id,
            type_label: format!("{:?}", d._type),
            volume: d.volume_percent,
            is_active: d.is_active,
            is_restricted: d.is_restricted,
        })
        .collect())
}

/// 選択デバイスへ再生を転送する（`device use` と同じく `play=Some(true)` で即再生開始）。
pub async fn transfer(spotify: &AuthCodePkceSpotify, id: &str) -> Result<()> {
    auth::ensure_fresh_token(spotify).await?;
    spotify
        .transfer_playback(id, Some(true))
        .await
        .context("デバイスへの再生転送に失敗しました")?;
    Ok(())
}
