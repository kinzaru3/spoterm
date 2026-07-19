//! `spoterm device use <name>`: transfer playback to the specified device.

use anyhow::{Context, Result};
use rspotify::AuthCodePkceSpotify;
use rspotify::prelude::*;

use crate::auth;
use crate::config::Config;
use crate::match_name::{NameMatch, match_name};

pub async fn run(cfg: &Config, name: &[String]) -> Result<()> {
    let query = name.join(" ");
    let spotify = auth::authed_client(cfg).await?;
    println!("{}", execute(&spotify, &query).await?);
    Ok(())
}

/// Match the requested device by name and transfer playback to it. Returns the text to print so
/// the API glue (device list + transfer request) is testable.
async fn execute(spotify: &AuthCodePkceSpotify, query: &str) -> Result<String> {
    let devices = spotify
        .device()
        .await
        .context("failed to fetch the device list")?;

    if devices.is_empty() {
        return Ok("No playable devices. Please open the Spotify app".to_string());
    }

    // `names` is 1:1 and in the same order as `devices`, so the index returned by
    // match_name can be used directly against `devices`.
    let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();

    let msg = match match_name(&names, query) {
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
            format!("▶ Moved playback to '{}'", target.name)
        }
        NameMatch::None => {
            format!("No device matching '{query}'. Check the list with `spoterm devices`")
        }
        NameMatch::Ambiguous(idxs) => {
            let mut lines = vec![format!(
                "'{query}' matched multiple devices. Please be more specific:"
            )];
            for i in idxs {
                lines.push(format!("  - {}", devices[i].name));
            }
            lines.join("\n")
        }
    };

    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures as fx;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn mount_devices(server: &MockServer, body: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path("/me/player/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn execute_transfers_to_matched_device() {
        let server = MockServer::start().await;
        mount_devices(
            &server,
            fx::devices_envelope(vec![fx::device("d1", "My Mac", false)]),
        )
        .await;
        // The transfer request (PUT /me/player) must be sent exactly once.
        Mock::given(method("PUT"))
            .and(path("/me/player"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = execute(&client, "My Mac").await.unwrap();
        assert_eq!(out, "▶ Moved playback to 'My Mac'");
    }

    #[tokio::test]
    async fn execute_reports_no_match() {
        let server = MockServer::start().await;
        mount_devices(
            &server,
            fx::devices_envelope(vec![fx::device("d1", "My Mac", false)]),
        )
        .await;
        let client = crate::auth::test_client(&server.uri()).await;
        let out = execute(&client, "Nonexistent").await.unwrap();
        assert!(out.contains("No device matching 'Nonexistent'"), "{out}");
    }
}
