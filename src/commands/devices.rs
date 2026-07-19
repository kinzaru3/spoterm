//! `spoterm devices`: list the available Spotify Connect devices.

use anyhow::{Context, Result};
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;

pub async fn run(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    let devices = spotify
        .device()
        .await
        .context("failed to fetch the device list")?;

    if devices.is_empty() {
        println!("No playable devices. Please open the Spotify app");
        return Ok(());
    }

    println!("Available devices:");
    for d in &devices {
        let type_label = format!("{:?}", d._type);
        println!(
            "{}",
            render_device(
                &d.name,
                &type_label,
                d.volume_percent,
                d.is_active,
                d.is_restricted
            )
        );
    }

    Ok(())
}

/// Pure function that formats a single device line. Mapping `DeviceType` to a label is done on the caller side.
fn render_device(
    name: &str,
    type_label: &str,
    vol: Option<u32>,
    is_active: bool,
    is_restricted: bool,
) -> String {
    let mark = if is_active { "●" } else { "○" };
    let vol_s = match vol {
        Some(v) => format!("vol {v}%"),
        None => "vol -".to_string(),
    };
    let mut line = format!("  {mark} {name} [{type_label}]  {vol_s}");
    if is_active {
        line.push_str("   (active)");
    }
    if is_restricted {
        line.push_str(" (restricted)");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_device_active_with_volume() {
        let out = render_device("MacBook Pro", "Computer", Some(65), true, false);
        assert_eq!(out, "  ● MacBook Pro [Computer]  vol 65%   (active)");
    }

    #[test]
    fn render_device_inactive_without_volume() {
        let out = render_device("Speaker", "Speaker", None, false, false);
        assert_eq!(out, "  ○ Speaker [Speaker]  vol -");
    }

    #[test]
    fn render_device_restricted() {
        let out = render_device("TV", "Tv", Some(40), false, true);
        assert_eq!(out, "  ○ TV [Tv]  vol 40% (restricted)");
    }
}
