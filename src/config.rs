use anyhow::{Context, Result};
use std::path::PathBuf;

/// 実行時設定。今は環境変数（`.env` → docker compose の env_file 経由）から読む。
/// 将来的に設定ファイルを重ねられるよう `load()` に集約しておく。
#[derive(Debug, Clone)]
pub struct Config {
    pub client_id: String,
    pub redirect_uri: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let client_id = std::env::var("SPOTERM_CLIENT_ID")
            .context("SPOTERM_CLIENT_ID が未設定です（.env を確認してください）")?;
        let redirect_uri = std::env::var("SPOTERM_REDIRECT_URI")
            .unwrap_or_else(|_| "http://127.0.0.1:8888/callback".to_string());
        Ok(Self {
            client_id,
            redirect_uri,
        })
    }
}

/// トークンキャッシュ等を置く設定ディレクトリ（XDG 準拠）。
pub fn config_dir() -> Result<PathBuf> {
    let proj = directories::ProjectDirs::from("", "", "spoterm")
        .context("設定ディレクトリを特定できませんでした")?;
    Ok(proj.config_dir().to_path_buf())
}
