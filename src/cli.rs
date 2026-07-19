use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "spotterm", version, about = "A CLI to control Spotify")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Log in to Spotify (PKCE authentication)
    Login,
    /// Show the currently playing track (Now Playing)
    Status,
    /// Search tracks / albums / artists
    Search {
        /// Search keywords (multiple words, space-separated)
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Play (search and play with a query, or resume with no query)
    Play { query: Vec<String> },
    /// Pause
    Pause,
    /// Skip to the next track
    Next,
    /// Skip to the previous track
    Prev,
    /// Toggle play/pause
    Toggle,
    /// Set the volume (0-100)
    Vol {
        #[arg(value_parser = clap::value_parser!(u8).range(0..=100))]
        level: u8,
    },
    /// List the available devices
    Devices,
    /// Select a device and move playback to it
    Device {
        #[command(subcommand)]
        action: DeviceAction,
    },
    /// Playlist operations
    Playlist {
        #[command(subcommand)]
        action: PlaylistAction,
    },
    /// Library (saved tracks / albums)
    Lib,
    /// Interactive TUI (Now Playing dashboard)
    Tui,
}

#[derive(Subcommand, Debug)]
pub enum DeviceAction {
    /// Move playback to the device with the given name
    Use {
        #[arg(required = true)]
        name: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum PlaylistAction {
    /// List playlists
    Ls,
    /// Play a playlist by name
    Play {
        #[arg(required = true)]
        name: Vec<String>,
    },
}
