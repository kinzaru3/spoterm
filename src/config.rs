use anyhow::{Context, Result};
use std::path::PathBuf;

/// Runtime configuration. Currently read from environment variables.
/// Consolidated in `load()` so config files can be layered on later.
#[derive(Debug, Clone)]
pub struct Config {
    pub client_id: String,
    pub redirect_uri: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let client_id = std::env::var("SPOTERM_CLIENT_ID")
            .context("SPOTERM_CLIENT_ID is not set (check your environment)")?;
        let redirect_uri = std::env::var("SPOTERM_REDIRECT_URI")
            .unwrap_or_else(|_| "http://127.0.0.1:8888/callback".to_string());
        Ok(Self {
            client_id,
            redirect_uri,
        })
    }
}

/// Config directory for the token cache and similar files (XDG compliant).
pub fn config_dir() -> Result<PathBuf> {
    let proj = directories::ProjectDirs::from("", "", "spoterm")
        .context("could not determine the config directory")?;
    Ok(proj.config_dir().to_path_buf())
}
