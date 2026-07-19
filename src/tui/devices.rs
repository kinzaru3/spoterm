//! Device picker overlay. Lists the available Spotify Connect devices and transfers playback
//! to the selected one. It does not touch `App`; it only handles data fetching, transfer, and
//! key→action conversion (screen-state updates and rendering are on the `mod.rs` side). It reuses
//! the same API as the existing `devices`/`device use` commands.
//!
//! Devices come and go (start up / shut down), so unlike `browse` they are not cached. A fresh
//! list is fetched every time the overlay opens, and `r` re-fetches it too.

use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use rspotify::AuthCodePkceSpotify;
use rspotify::prelude::*;

use crate::auth;

/// One device in the list. Transfer requires an `id`, and `is_restricted` means it cannot be controlled.
#[derive(Clone)]
pub struct DeviceEntry {
    pub name: String,
    /// Transfer target ID (an Option because some Connect devices have no ID).
    pub id: Option<String>,
    pub type_label: String,
    pub volume: Option<u32>,
    pub is_active: bool,
    pub is_restricted: bool,
}

/// State of the device picker overlay.
pub struct DevicePickerState {
    pub items: Vec<DeviceEntry>,
    pub selected: usize,
    pub message: Option<String>,
}

/// Actions the key handler asks the main body to perform.
pub enum DeviceAction {
    None,
    /// Close the overlay.
    Close,
    /// Transfer playback to the selected device.
    Transfer,
    /// Re-fetch the list.
    Reload,
}

/// Update the selection position in place and return the required action (same shape as `browse::key_action`).
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

/// Fetch the available devices (same API as the existing `devices` command).
/// Borrows the client the caller keeps alive and refreshes the token only when needed.
pub async fn fetch(spotify: &AuthCodePkceSpotify) -> Result<Vec<DeviceEntry>> {
    auth::ensure_fresh_token(spotify).await?;
    let devices = spotify
        .device()
        .await
        .context("failed to fetch the device list")?;
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

/// Transfer playback to the selected device (like `device use`, starts playing immediately with `play=Some(true)`).
pub async fn transfer(spotify: &AuthCodePkceSpotify, id: &str) -> Result<()> {
    auth::ensure_fresh_token(spotify).await?;
    spotify
        .transfer_playback(id, Some(true))
        .await
        .context("failed to transfer playback to the device")?;
    Ok(())
}
