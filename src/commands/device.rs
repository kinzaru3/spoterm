//! `spoterm device use <name>`: transfer playback to the specified device.

use anyhow::{Context, Result};
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::match_name::{NameMatch, match_name};

pub async fn run(cfg: &Config, name: &[String]) -> Result<()> {
    let query = name.join(" ");
    let spotify = auth::authed_client(cfg).await?;

    let devices = spotify
        .device()
        .await
        .context("failed to fetch the device list")?;

    if devices.is_empty() {
        println!("No playable devices. Please open the Spotify app");
        return Ok(());
    }

    // `names` is 1:1 and in the same order as `devices`, so the index returned by
    // match_name can be used directly against `devices`.
    let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();

    match match_name(&names, &query) {
        NameMatch::Found(i) => {
            let target = &devices[i];
            let id = target
                .id
                .as_deref()
                .context("the selected device has no ID and cannot be transferred to")?;
            spotify
                .transfer_playback(id, Some(true))
                .await
                .context("failed to transfer playback to the device")?;
            println!("▶ Moved playback to '{}'", target.name);
        }
        NameMatch::None => {
            println!("No device matching '{query}'. Check the list with `spoterm devices`");
        }
        NameMatch::Ambiguous(idxs) => {
            println!("'{query}' matched multiple devices. Please be more specific:");
            for i in idxs {
                println!("  - {}", devices[i].name);
            }
        }
    }

    Ok(())
}
