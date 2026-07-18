//! `spoterm device use <name>`: 指定デバイスへ再生をトランスファーする（Phase 4）。

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
        .context("デバイス一覧の取得に失敗しました")?;

    if devices.is_empty() {
        println!(
            "再生可能なデバイスがありません。Spotify アプリまたは spotifyd を起動してください"
        );
        return Ok(());
    }

    // names は devices と同順 1:1。match_name が返す index を devices にそのまま使える。
    let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();

    match match_name(&names, &query) {
        NameMatch::Found(i) => {
            let target = &devices[i];
            let id = target
                .id
                .as_deref()
                .context("選択したデバイスに ID がなく、転送できません")?;
            spotify
                .transfer_playback(id, Some(true))
                .await
                .context("デバイスへの再生転送に失敗しました")?;
            println!("▶ '{}' へ再生を移しました", target.name);
        }
        NameMatch::None => {
            println!(
                "'{query}' に一致するデバイスがありません。spoterm devices で一覧を確認してください"
            );
        }
        NameMatch::Ambiguous(idxs) => {
            println!("'{query}' が複数のデバイスに一致しました。より具体的に指定してください:");
            for i in idxs {
                println!("  - {}", devices[i].name);
            }
        }
    }

    Ok(())
}
