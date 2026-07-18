//! `spoterm devices`: Spotify Connect の利用可能デバイス一覧を表示する。

use anyhow::{Context, Result};
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;

pub async fn run(cfg: &Config) -> Result<()> {
    let spotify = auth::authed_client(cfg).await?;

    let devices = spotify
        .device()
        .await
        .context("デバイス一覧の取得に失敗しました")?;

    if devices.is_empty() {
        println!(
            "再生可能なデバイスがありません。Spotify アプリまたは spotifyd を起動してください"
        );
        return Ok(());
    }

    println!("利用可能なデバイス:");
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

/// デバイス 1 行を整形する純粋関数。`DeviceType` → ラベルの写像は呼び出し側で行う。
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
        line.push_str(" (操作不可)");
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_device_active_with_volume() {
        let out = render_device("MacBook-spotifyd", "Computer", Some(65), true, false);
        assert_eq!(out, "  ● MacBook-spotifyd [Computer]  vol 65%   (active)");
    }

    #[test]
    fn render_device_inactive_without_volume() {
        let out = render_device("Speaker", "Speaker", None, false, false);
        assert_eq!(out, "  ○ Speaker [Speaker]  vol -");
    }

    #[test]
    fn render_device_restricted() {
        let out = render_device("TV", "Tv", Some(40), false, true);
        assert_eq!(out, "  ○ TV [Tv]  vol 40% (操作不可)");
    }
}
