mod auth;
mod cli;
mod commands;
mod config;
mod format;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command, DeviceAction, PlaylistAction};
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // 全コマンドが設定（client_id 等）を必要とするため、一度だけ読み込む。
    let cfg = Config::load()?;

    match cli.command {
        Command::Login => auth::login(&cfg).await?,
        Command::Status => commands::status::run(&cfg).await?,
        Command::Search { query } => commands::search::run(&cfg, &query).await?,
        Command::Play { query } => commands::playback::play(&cfg, &query).await?,
        Command::Pause => commands::playback::pause(&cfg).await?,
        Command::Next => commands::playback::next(&cfg).await?,
        Command::Prev => commands::playback::prev(&cfg).await?,
        Command::Toggle => commands::playback::toggle(&cfg).await?,
        Command::Vol { level } => commands::playback::vol(&cfg, level).await?,
        Command::Devices => commands::devices::run(&cfg).await?,
        Command::Device { action } => match action {
            DeviceAction::Use { name } => commands::device::run(&cfg, &name).await?,
        },
        Command::Playlist { action } => match action {
            PlaylistAction::Ls => todo_cmd("playlist ls", 5),
            PlaylistAction::Play { name } => {
                todo_cmd(&format!("playlist play '{}'", name.join(" ")), 5)
            }
        },
        Command::Lib => todo_cmd("lib", 5),
    }

    Ok(())
}

/// 未実装コマンドのプレースホルダ。どのフェーズで実装予定かを示す。
fn todo_cmd(name: &str, phase: u8) {
    println!("`{name}` は未実装です（Phase {phase} で実装予定）");
}
