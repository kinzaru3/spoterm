use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "spotterm",
    version,
    about = "A Spotify TUI (official Web API, PKCE auth)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Log in to Spotify (PKCE authentication)
    Login,
    /// Interactive TUI (Now Playing dashboard)
    Tui,
}
