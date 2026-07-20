mod auth;
mod cli;
mod config;
mod format;
mod np_json;
mod theme;
mod tui;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};
use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Both commands need the config (client_id, etc.), so load it once.
    let cfg = Config::load()?;

    match cli.command {
        Command::Login => auth::login(&cfg).await?,
        Command::Tui => tui::run(&cfg).await?,
    }

    Ok(())
}
