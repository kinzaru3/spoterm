//! Subcommand implementations. Each command starts from `auth::authed_client`
//! to obtain an authenticated client, then calls the Web API to display or
//! control results (read operations, playback, and playlist/library).

pub mod device;
pub mod devices;
pub mod lib;
pub mod playback;
pub mod playlist;
pub mod search;
pub mod status;

/// Hint shown when an operation that requires an active device fails (shared by playback / playlist).
pub(crate) const NEED_DEVICE_HINT: &str =
    "(An active device is required. Select one with `spoterm device use <name>`.)";
