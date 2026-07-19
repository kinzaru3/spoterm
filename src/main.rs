mod auth;
mod cli;
mod commands;
mod config;
mod format;
mod match_name;
#[cfg(test)]
mod test_fixtures;
mod tui;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command, DeviceAction, PlaylistAction};
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Every command needs the config (client_id, etc.), so load it once.
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
            PlaylistAction::Ls => commands::playlist::ls(&cfg).await?,
            PlaylistAction::Play { name } => commands::playlist::play(&cfg, &name).await?,
        },
        Command::Lib => commands::lib::run(&cfg).await?,
        Command::Tui => tui::run(&cfg).await?,
    }

    Ok(())
}
