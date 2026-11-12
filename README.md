# Senritsu

Senritsu is a terminal music player for Navidrome and other Subsonic servers. It streams through mpv, shows album art in the terminal, scrobbles to your server, posts track-change notifications, and can publish a "Listening to" status on Discord. It runs on macOS and Linux.

## Requirements

- Rust (edition 2024, so a recent stable toolchain)
- mpv on your `PATH`
  - macOS: `brew install mpv`
  - Arch: `sudo pacman -S mpv`
- A Navidrome or Subsonic-compatible server (API 1.16.1)

Senritsu exits with an install hint if it cannot find mpv.

## Install

```sh
cargo install --path .
```

This installs the `senritsu` binary. Run `senritsu` and fill in the login screen (server URL, username, password). Use Tab and Shift+Tab to move between fields and Enter to log in.

### macOS notifications

macOS needs a small signed helper app to show notifications. Build and install it with:

```sh
scripts/install-macos-notifier.sh
```

The script compiles the helper with `swiftc`, ad-hoc signs it, and copies it to `~/Applications/SenritsuNotifier.app`. It then launches the app once, so macOS asks for notification permission the first time. Allow it in the prompt.

To build without installing, pass `--build-only`. The app is left in `target/notifier/SenritsuNotifier.app`. Set `SENRITSU_NOTIFIER_APP` to a different app path if you keep the helper elsewhere.

On Linux, notifications go through the desktop notification service (D-Bus), so no setup is needed.

## Credentials

After a successful login Senritsu stores your password in the system keychain under the service name `senritsu`, with the account `username@server_url`. The keychain is the only place the password is kept.

If no keychain is available (for example Linux without a Secret Service), Senritsu does not store the password at all. It writes a salt and the token derived from it to `credentials.toml` next to `config.toml`, with mode `0600`, and shows a status message about the fallback. That token is enough to sign in to your server, so treat the file like a password.

Logging out (`X`) deletes both the keychain entry and `credentials.toml`.

Requests use Subsonic token auth only. Stream URLs carry credentials, so they are never logged or shown.

## Configuration

The config file is `$XDG_CONFIG_HOME/senritsu/config.toml`, or `~/.config/senritsu/config.toml` when `XDG_CONFIG_HOME` is not set. This applies on macOS too. The login screen creates it, and you can edit it by hand.

| Key | Default | Meaning |
|---|---|---|
| `server_url` | none (required) | Base URL of your server |
| `username` | none (required) | Login name |
| `discord` | `true` | Set to `false` to turn off Discord presence |
| `discord_app_id` | built-in | Discord application ID. Set it to show presence under your own Discord application |
| `discord_default_image` | `senritsu` | Name of the Discord art asset used when no public cover is found |
| `image_protocol` | unset (auto) | Album art protocol: `auto`, `kitty`, `iterm2`, `sixel`, or `halfblocks` |

Example:

```toml
server_url = "https://music.example.com"
username = "alice"
image_protocol = "kitty"
```

## Discord presence

Discord presence works out of the box. While a track plays, Discord shows "Listening to Senritsu" with the track, artist, album, and cover. The Discord desktop app must be running on the same machine. Set `discord = false` in `config.toml` to turn it off.

To show presence under a different name or default image, use your own Discord application:

1. Open the [Discord developer portal](https://discord.com/developers/applications) and create a new application. Discord shows its name after "Listening to".
2. Copy the Application ID from the General Information page into `discord_app_id` in `config.toml`.
3. Under Rich Presence, upload an art asset named `senritsu` (or whatever you set in `discord_default_image`). Senritsu shows it when it cannot find a public cover for the album. `assets/icon/senritsu-discord-1024.png` is the Senritsu icon sized for this.
4. Restart Senritsu.

Discord only ever receives public cover art URLs, found by looking up the album on the Cover Art Archive, iTunes, and Deezer. Your Navidrome server's address, username, and credentials are never sent to Discord or to those lookup services. Lookups use only the artist and album names (or the MusicBrainz release ID when your library has one). Results are cached in `public_art.json`.

## Album art

Senritsu picks the best image protocol your terminal supports, in this order: kitty graphics, iTerm2 inline images, sixel, and a half-block character fallback that works anywhere.

To force a protocol, set the environment variable (it wins over `image_protocol` in the config):

```sh
SENRITSU_IMAGE_PROTOCOL=halfblocks senritsu
```

Accepted values are `auto`, `kitty`, `iterm2`, `sixel`, and `halfblocks`, case-insensitive. An unknown value is logged and treated as `auto`. Some terminals report a protocol they do not render well. If art looks garbled or missing, try `SENRITSU_IMAGE_PROTOCOL=halfblocks`.

## Keys

Press `?` in the app for the same list. When a text input has focus (search, login, playlist names), printable keys go to the input.

### Global

| Key | Action |
|---|---|
| `1` `2` `3` `4` | Library, Playlists, Search, Queue |
| `/` | Search, with the input focused |
| `z` | Toggle full-screen Now Playing |
| `?` | Toggle help |
| `space` | Play / pause |
| `.` `,` | Next / previous track |
| `]` `[` | Seek +10 s / -10 s |
| `=` `-` | Volume +5 / -5 |
| `s` | Toggle shuffle |
| `r` | Cycle repeat: off, all, one |
| `R` | Play 200 random songs |
| `f` | Toggle favorite on the selected song (or the current track in Now Playing) |
| `a` | Add the selected song to a playlist (picker popup) |
| `X` | Log out (asks to confirm) |
| `q` or `Ctrl+C` | Quit |
| `Esc` | Close a popup or leave a text input |

### Lists

| Key | Action |
|---|---|
| `j` `k` or `↓` `↑` | Move selection |
| `h` `l` or `←` `→` | Move between columns (Library, Playlists) |
| `Enter` | Play from the selection, replacing the queue. On an artist or playlist it opens the next column |
| `e` | Append to the queue |
| `p` | Play next |

### Playlists and Queue

| Key | Action |
|---|---|
| `n` `i` `D` | New / rename / delete playlist (Playlists, either column; rename and delete act on the highlighted playlist) |
| `d` | Remove the selected track (Playlists track column, Queue) |
| `J` `K` | Move the selected track down / up (Playlists track column, Queue) |
| `c` | Clear the queue (asks to confirm) |

### Search

In the input, `Enter` or `Esc` leaves the input and `↓` jumps to the results. From the results, `/` focuses the input again. `Enter` on an artist or album opens it in the Library.

## Files and logs

| What | macOS | Linux |
|---|---|---|
| Config, `credentials.toml` | `~/.config/senritsu/` | `~/.config/senritsu/` |
| Logs (`senritsu.log`), album art, `public_art.json` | `~/Library/Caches/senritsu/` | `~/.cache/senritsu/` |
| `state.json` (saved queue), `outbox.json` (scrobbles waiting to send) | `~/Library/Application Support/senritsu/` | `~/.local/share/senritsu/` |

On Linux the config path follows `XDG_CONFIG_HOME`, and the cache and data paths follow `XDG_CACHE_HOME` and `XDG_DATA_HOME`.

The log is a single file, `senritsu.log`, that is appended to and never rotated. Log output never contains stream URLs or credentials.

## Behavior notes

- A track scrobbles after you have listened to half of it or 4 minutes, whichever comes first. Seeking forward does not count as listening. If the server is unreachable, scrobbles wait in `outbox.json` and are sent later.
- The queue is saved on exit and restored on the next start.
- If mpv dies mid-track, Senritsu restarts it and resumes.
- Playback is gapless where mpv can do it.
