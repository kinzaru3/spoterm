//! Device picker overlay. Lists the available Spotify Connect devices and transfers playback
//! to the selected one. This module owns the device picker end to end: data fetching, transfer,
//! key→action conversion, the `App`-facing handlers (`handle_devices_key` / `open_devices` /
//! `devices_transfer`), and rendering (`draw_devices`). The dashboard shell in `mod.rs` only routes
//! into it. It reuses the same API as the existing `devices`/`device use` commands.
//!
//! Devices come and go (start up / shut down), so unlike `browse` they are not cached. A fresh
//! list is fetched every time the overlay opens, and `r` re-fetches it too.

use anyhow::{Context, Result};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use rspotify::AuthCodePkceSpotify;
use rspotify::prelude::*;

use crate::auth;
use crate::theme;
use crate::tui::view;

use super::App;

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

/// Update the selection position in place and return the required action (same 2-step borrow-avoiding
/// shape the search/library key handlers use: sync state update here, async work in the caller).
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

// ---- Device picker (App-facing key handling, action, rendering) -------------

/// Key handling for the device picker overlay. Updates the selection synchronously and runs the required async action.
pub(super) async fn handle_devices_key(key: KeyEvent, app: &mut App) {
    let action = {
        let super::Mode::Devices(state) = &mut app.mode else {
            return;
        };
        key_action(key, state)
    };
    match action {
        DeviceAction::None => {}
        DeviceAction::Close => app.mode = super::Mode::Normal,
        DeviceAction::Transfer => devices_transfer(app).await,
        DeviceAction::Reload => open_devices(app).await,
    }
}

/// Fetch the device list and enter selection mode. Empty list / fetch failure are reported (no silent failures).
/// Devices come and go, so they are not cached and are re-fetched every time it opens.
pub(super) async fn open_devices(app: &mut App) {
    let items = match fetch(&app.client).await {
        Ok(items) => items,
        Err(e) => {
            // If selecting, stay on screen and show a message; if in the normal view, put it on the status line.
            if let super::Mode::Devices(state) = &mut app.mode {
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
    app.mode = super::Mode::Devices(DevicePickerState {
        items,
        selected,
        message,
    });
}

/// Transfer playback to the selected device. On success return to the normal view and poll immediately; on failure keep it on the overlay.
/// Non-transferable devices (no ID / restricted) are rejected up front and reported via a message.
async fn devices_transfer(app: &mut App) {
    let target = match &app.mode {
        super::Mode::Devices(state) => state.items.get(state.selected).cloned(),
        _ => None,
    };
    let Some(target) = target else {
        return;
    };
    if target.is_restricted {
        if let super::Mode::Devices(state) = &mut app.mode {
            state.message = Some(format!(
                "'{}' is restricted and cannot be transferred to",
                target.name
            ));
        }
        return;
    }
    let Some(id) = target.id.as_deref() else {
        if let super::Mode::Devices(state) = &mut app.mode {
            state.message = Some(format!(
                "'{}' has no ID and cannot be transferred to",
                target.name
            ));
        }
        return;
    };
    match transfer(&app.client, id).await {
        Ok(()) => {
            app.status = format!("{} Moved playback to '{}'", theme::PLAY, target.name);
            app.last_poll = None; // Reflect the transfer into Now Playing quickly
            app.mode = super::Mode::Normal;
        }
        Err(e) => {
            if let super::Mode::Devices(state) = &mut app.mode {
                state.message = Some(format!("transfer failed: {e}"));
            } else {
                app.status = format!("{} transfer failed: {e}", theme::WARN);
            }
        }
    }
}

/// Device picker view (list + selection highlight).
pub(super) fn draw_devices(frame: &mut ratatui::Frame, state: &DevicePickerState) {
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
