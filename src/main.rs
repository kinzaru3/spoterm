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

    match cli.command {
        Command::Login => {
            let cfg = Config::load()?;
            auth::login(&cfg).await?;
        }
        Command::Status => {
            let cfg = Config::load()?;
            commands::status::run(&cfg).await?;
        }
        Command::Search { query } => {
            let cfg = Config::load()?;
            commands::search::run(&cfg, &query).await?;
        }
        Command::Play { query } => {
            let what = if query.is_empty() {
                "play (再開)".to_string()
            } else {
                format!("play '{}'", query.join(" "))
            };
            todo_cmd(&what, 4);
        }
        Command::Pause => todo_cmd("pause", 4),
        Command::Next => todo_cmd("next", 4),
        Command::Prev => todo_cmd("prev", 4),
        Command::Toggle => todo_cmd("toggle", 4),
        Command::Vol { level } => todo_cmd(&format!("vol {level}"), 4),
        Command::Devices => {
            let cfg = Config::load()?;
            commands::devices::run(&cfg).await?;
        }
        Command::Device { action } => match action {
            DeviceAction::Use { name } => todo_cmd(&format!("device use '{}'", name.join(" ")), 4),
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
