//! `spoterm device use <name>`: 指定デバイスへ再生をトランスファーする（Phase 4）。

use anyhow::{Context, Result};
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;

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

    // names は devices と同順 1:1。match_device が返す index を devices にそのまま使える。
    let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();

    match match_device(&names, &query) {
        DeviceMatch::Found(i) => {
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
        DeviceMatch::None => {
            println!(
                "'{query}' に一致するデバイスがありません。spoterm devices で一覧を確認してください"
            );
        }
        DeviceMatch::Ambiguous(idxs) => {
            println!("'{query}' が複数のデバイスに一致しました。より具体的に指定してください:");
            for i in idxs {
                println!("  - {}", devices[i].name);
            }
        }
    }

    Ok(())
}

/// デバイス名照合の結果。
#[derive(Debug, PartialEq)]
enum DeviceMatch {
    Found(usize),
    None,
    Ambiguous(Vec<usize>),
}

/// デバイス名を照合する。大文字小文字を無視し、完全一致を部分一致より優先する。
fn match_device(names: &[&str], query: &str) -> DeviceMatch {
    let q = query.trim().to_lowercase();
    // 空クエリは全件に部分一致してしまうため、明示的に「該当なし」とする。
    if q.is_empty() {
        return DeviceMatch::None;
    }

    let exact: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_lowercase() == q)
        .map(|(i, _)| i)
        .collect();
    match exact.len() {
        1 => return DeviceMatch::Found(exact[0]),
        n if n > 1 => return DeviceMatch::Ambiguous(exact),
        _ => {}
    }

    let partial: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect();
    match partial.len() {
        0 => DeviceMatch::None,
        1 => DeviceMatch::Found(partial[0]),
        _ => DeviceMatch::Ambiguous(partial),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_case_insensitive() {
        let n = ["MacBook-spotifyd", "iPhone"];
        assert_eq!(match_device(&n, "macbook-spotifyd"), DeviceMatch::Found(0));
    }

    #[test]
    fn partial_match_single() {
        let n = ["MacBook-spotifyd", "iPhone"];
        assert_eq!(match_device(&n, "spotifyd"), DeviceMatch::Found(0));
    }

    #[test]
    fn no_match() {
        let n = ["MacBook-spotifyd", "iPhone"];
        assert_eq!(match_device(&n, "speaker"), DeviceMatch::None);
    }

    #[test]
    fn partial_match_ambiguous() {
        let n = ["Living Room TV", "Living Room Speaker"];
        assert_eq!(
            match_device(&n, "living room"),
            DeviceMatch::Ambiguous(vec![0, 1])
        );
    }

    #[test]
    fn exact_wins_over_partial() {
        // "Living Room" は 0 番と完全一致し、1 番とは部分一致。完全一致を優先する。
        let n = ["Living Room", "Living Room TV"];
        assert_eq!(match_device(&n, "living room"), DeviceMatch::Found(0));
    }

    #[test]
    fn empty_query_matches_nothing() {
        let n = ["MacBook-spotifyd", "iPhone"];
        assert_eq!(match_device(&n, "   "), DeviceMatch::None);
    }
}
