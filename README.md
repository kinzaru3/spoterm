# spotterm

**English** | [日本語](README.ja.md)

A fast Spotify CLI & TUI for your terminal, built on the official Spotify Web API.

spotterm controls playback, searches, browses your library, and shows a live "Now Playing"
dashboard (with **album cover art** in supporting terminals). It uses **Authorization Code + PKCE**
(no client secret on your machine) and talks only to the official Web API — it never downloads
audio or bundles any Spotify SDK.

Playback happens on any **Spotify Connect device** you already run (the official Spotify desktop
or mobile app); spotterm just tells that device what to play.

## Features

- **Now Playing TUI** (`spotterm tui`): live track/artist/album, progress bar, volume, and **cover art**.
- **Playback control**: play / pause / next / prev / seek / volume, from the CLI or the TUI.
- **Search & play** tracks.
- **Browse & play** your playlists, saved tracks, and saved albums.
- **Device picker**: list Connect devices and transfer playback.
- **Save / unsave** the current track to your library.

## Requirements

- **Spotify Premium** (required by the Web API for playback control).
- A **Spotify Connect device** to play on — the official Spotify app (desktop/mobile) running and
  logged in on the same account.
- **Your own Spotify app Client ID** (free; see Setup). Each user registers their own app.
- Rust toolchain (to build from source) — Rust 1.85+ (edition 2024).
- For real album art: a terminal that supports an image protocol (iTerm2, kitty, WezTerm, Ghostty).
  Other terminals fall back to colored half-blocks automatically.

## Install

### Prebuilt binaries

Download a tarball for your platform from the [Releases](https://github.com/kinzaru3/spotterm/releases)
page (Linux `x86_64`, macOS `aarch64`/`x86_64`), then extract and put `spotterm` on your `PATH`:

```sh
tar xzf spotterm-*.tar.gz
sudo mv spotterm-*/spotterm /usr/local/bin/
```

### Build from source

```sh
cargo install --git https://github.com/kinzaru3/spotterm
# or
git clone https://github.com/kinzaru3/spotterm && cd spotterm && cargo install --path .
```

## Setup

1. **Create a Spotify app** at the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard)
   and copy its **Client ID**.
2. In the app settings, add this **Redirect URI**:
   ```
   http://127.0.0.1:8888/callback
   ```
3. **Provide your Client ID** via environment variable (or a `.env` file — see `.env.example`):
   ```sh
   export SPOTTERM_CLIENT_ID=your_client_id_here
   # optional, defaults to http://127.0.0.1:8888/callback
   # export SPOTTERM_REDIRECT_URI=http://127.0.0.1:8888/callback
   ```
4. **Log in** (opens your browser for consent; the token is cached locally):
   ```sh
   spotterm login
   ```

> **Why your own Client ID?** Spotify's development mode limits an app to a small number of
> users, so each user runs spotterm with their own registered app. No client secret is needed
> (PKCE), and your token is stored in your OS config directory with `0600` permissions.

## Usage

### One-shot commands

```sh
spotterm status                 # Now Playing (track / artist / progress / device)
spotterm search <query>         # search tracks/albums/artists
spotterm play [query]           # resume, or search and play
spotterm pause | next | prev | toggle
spotterm vol <0-100>            # set volume
spotterm devices                # list available Connect devices
spotterm device use <name>      # transfer playback to a device
spotterm playlist ls            # list your playlists
spotterm playlist play <name>   # play a playlist by name
spotterm lib                    # list saved tracks / albums
```

### Interactive TUI

```sh
spotterm tui
```

| Key | Action |
|---|---|
| `space` | play / pause |
| `n` / `p` | next / previous track |
| `←` / `→` | seek 5s back / forward |
| `+` / `-` | volume ±5 |
| `s` | save / unsave the current track |
| `/` | search and play |
| `2` | browse library (playlists / saved tracks / albums) |
| `d` | device picker (transfer playback) |
| `r` | refresh |
| `?` | help |
| `q` / `Esc` / `Ctrl-C` | quit |

## Album cover art

The TUI renders album art using the best protocol your terminal supports (kitty, iTerm2, Sixel),
and falls back to colored half-blocks elsewhere — so you always see *something*.

**Inside tmux**, image protocols are stripped unless passthrough is enabled. Add this to your
tmux config to see real images:

```tmux
set -g allow-passthrough on
```

## Notes

- **Personal, non-commercial use.** spotterm is a client of the public Spotify Web API; it does not
  redistribute any Spotify SDK, content, or client secret.
- Cover art and track metadata are shown together, per Spotify's developer guidelines.

## License

[MIT](LICENSE) © kinzaru3

This project is not affiliated with or endorsed by Spotify. "Spotify" is a trademark of Spotify AB.
