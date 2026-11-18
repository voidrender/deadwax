<p align="center">
  <img src="assets/icon/deadwax.svg" alt="Deadwax logo" width="160">
</p>

<h1 align="center">Deadwax</h1>

Deadwax is a terminal music player for Navidrome and other Subsonic servers. It streams through mpv, shows album art in the terminal, scrobbles to your server, posts track-change notifications, and can publish a "Listening to" status on Discord. It runs on macOS and Linux.

<!-- Screenshot: add assets/screenshot.png and uncomment.
![Deadwax playing an album, with the library on the left and album art on the right](assets/screenshot.png)
-->

## Requirements

- mpv on your `PATH`. Homebrew and the Arch package install it for you. Otherwise:
  - macOS: `brew install mpv`
  - Arch: `sudo pacman -S mpv`
- A Navidrome server, or another server that speaks Subsonic API 1.16.1 or newer

Deadwax exits with an install hint if it cannot find mpv.

## Install

### Homebrew (macOS and Linux)

```sh
brew install voidrender/tap/deadwax
```

On macOS the formula also installs the notification helper, so notifications work without extra setup.

### Arch Linux

The AUR package isn't up yet. Until it is, every release comes with a tested `PKGBUILD` attached. Build and install it with:

```sh
mkdir deadwax && cd deadwax
curl -LO https://github.com/voidrender/deadwax/releases/latest/download/PKGBUILD
makepkg -si
```

Homebrew on Linux works too.

### From source

You need Rust 1.90 or newer.

```sh
cargo install --git https://github.com/voidrender/deadwax
```

On macOS you also need the notification helper. Build and install it from a clone of the repository:

```sh
git clone https://github.com/voidrender/deadwax
cd deadwax
scripts/install-macos-notifier.sh
```

The script compiles the helper with `swiftc`, ad-hoc signs it, and copies it to `~/Applications/DeadwaxNotifier.app`. It then launches the app once, so macOS asks for notification permission. Allow it in the prompt.

On Linux, notifications go through the desktop notification service (D-Bus), so no setup is needed.

## Getting started

Run `deadwax` and fill in the login screen with your server URL, username, and password. Tab and Shift+Tab move between fields, and Enter logs in. Deadwax remembers the login, so later starts skip this screen.

Press `?` to see the keys and `S` to open the settings.

## Credentials

After a successful login Deadwax stores your password in the system keychain under the service name `deadwax`, with the account `username@server_url`. The keychain is the only place the password is kept.

If no keychain is available (for example Linux without a Secret Service), Deadwax does not store the password at all. It writes a salt and the token derived from it to `credentials.toml` next to `config.toml`, with mode `0600`, and shows a status message about the fallback. That token is enough to sign in to your server, so treat the file like a password.

Logging out (`X`) deletes both the keychain entry and `credentials.toml`.

Requests use Subsonic token auth only. Stream URLs carry credentials, so they are never logged or shown.

## Configuration

The config file is `$XDG_CONFIG_HOME/deadwax/config.toml`, or `~/.config/deadwax/config.toml` when `XDG_CONFIG_HOME` is not set. This applies on macOS too. The login screen creates it. The settings menu (`S`) changes Discord presence, the Discord status, notifications, and the album art protocol, and you can edit the file by hand for the rest.

| Key | Default | Meaning |
|---|---|---|
| `server_url` | none (required) | Base URL of your server |
| `username` | none (required) | Login name |
| `discord` | `true` | Set to `false` to turn off Discord presence |
| `discord_status` | `artist` | What your status in the Discord member list shows: `artist`, `album`, or `song` |
| `notifications` | `true` | Set to `false` to turn off track-change notifications |
| `discord_app_id` | built-in | Discord application ID. Set it to show presence under your own Discord application |
| `discord_default_image` | `deadwax` | Name of the Discord art asset used when no public cover is found |
| `image_protocol` | unset (auto) | Album art protocol: `auto`, `kitty`, `iterm2`, `sixel`, or `halfblocks` |

Example:

```toml
server_url = "https://music.example.com"
username = "alice"
image_protocol = "kitty"
```

## Discord presence

Discord presence works out of the box. While a track plays, your status in the member list shows the artist, and your profile shows "Listening to Deadwax Terminal" with the track, artist, album, cover, and a button that links to this repository. To show the album or song title in the member list instead, change Discord status in the settings menu, or set `discord_status` to `album` or `song`. With `album`, the album and artist trade places on your profile. The Discord desktop app must be running on the same machine. Turn it off in the settings menu (`S`), or set `discord = false` in `config.toml`.

To show presence under a different name or default image, use your own Discord application:

1. Open the [Discord developer portal](https://discord.com/developers/applications) and create a new application. Discord shows its name after "Listening to".
2. Copy the Application ID from the General Information page into `discord_app_id` in `config.toml`.
3. Under Rich Presence, upload an art asset named `deadwax` (or whatever you set in `discord_default_image`). Deadwax shows it when it cannot find a public cover for the album. `assets/icon/deadwax-discord-1024.png` is the Deadwax icon sized for this.
4. Restart Deadwax.

Discord only ever receives public cover art URLs, found by looking up the album on the Cover Art Archive, iTunes, and Deezer. Your Navidrome server's address, username, and credentials are never sent to Discord or to those lookup services. Lookups use only the artist and album names (or the MusicBrainz release ID when your library has one). Results are cached in `public_art.json`.

## Album art

Deadwax picks the best image protocol your terminal supports, in this order: kitty graphics, iTerm2 inline images, sixel, and a half-block character fallback that works anywhere.

To choose one yourself, change Album art in the settings menu (`S`). The change applies on the next start. For a single run, set the environment variable instead. It wins over the setting:

```sh
DEADWAX_IMAGE_PROTOCOL=halfblocks deadwax
```

Accepted values are `auto`, `kitty`, `iterm2`, `sixel`, and `halfblocks`, case-insensitive. An unknown value is logged and treated as `auto`.

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
| `i` | Show the cover and details of the selection (or the current track in Now Playing) |
| `S` | Settings |
| `a` | Add the selected song to a playlist (picker popup) |
| `o` | Change the sort order of the focused list (see below) |
| `X` | Log out (asks to confirm) |
| `q` or `Ctrl+C` | Quit |
| `Esc` | Close a popup or leave a text input |

The info popup (`i`) works on songs, albums, artists, and playlists. `i`, `Esc`, or `Enter` closes it. In the settings popup, `j` and `k` move, `Enter` or `space` changes the selected setting, and `S` or `Esc` closes it.

### Sorting

`o` steps through the sort orders of the focused list. The column title shows the current order, and Deadwax remembers your choice between runs.

| List | Orders |
|---|---|
| Artists | Name (the server's order), most albums. Favorites stays at the top |
| Albums | Newest release year first (the default), oldest first, name |
| Tracks | Album order, title, artist |
| Playlists | Server order, name |

The queue, search results, and the tracks inside a playlist keep their own order. Playlist tracks follow the order that `J` and `K` set on the server.

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
| `n` `E` `D` | New / rename / delete playlist (Playlists, either column; rename and delete act on the highlighted playlist) |
| `d` | Remove the selected track (Playlists track column, Queue) |
| `J` `K` | Move the selected track down / up (Playlists track column, Queue) |
| `c` | Clear the queue (asks to confirm) |

### Search

In the input, `Enter` or `Esc` leaves the input and `↓` jumps to the results. From the results, `/` focuses the input again. `Enter` on an artist or album opens it in the Library.

## Files and logs

| What | macOS | Linux |
|---|---|---|
| Config, `credentials.toml` | `~/.config/deadwax/` | `~/.config/deadwax/` |
| Logs (`deadwax.log`), album art, `public_art.json` | `~/Library/Caches/deadwax/` | `~/.cache/deadwax/` |
| `state.json` (saved queue), `outbox.json` (scrobbles waiting to send), `sorts.json` (sort orders) | `~/Library/Application Support/deadwax/` | `~/.local/share/deadwax/` |

On Linux the config path follows `XDG_CONFIG_HOME`, and the cache and data paths follow `XDG_CACHE_HOME` and `XDG_DATA_HOME`.

The log is a single file, `deadwax.log`, that is appended to and never rotated. Log output never contains stream URLs or credentials.

## Behavior notes

- A track scrobbles after you have listened to half of it or 4 minutes, whichever comes first. Seeking forward does not count as listening. If the server is unreachable, scrobbles wait in `outbox.json` and are sent later.
- The queue is saved on exit and restored on the next start.
- If mpv dies mid-track, Deadwax restarts it and resumes.
- Playback is gapless where mpv can do it.
- Only one Deadwax runs at a time. A second one prints a message and exits.

## Troubleshooting

Start with `deadwax.log` (see [Files and logs](#files-and-logs)), which records most failures with their cause.

### No notifications

Check that Notifications is On in the settings menu (`S`) and that Do Not Disturb or a Focus mode is off. A notification only appears once a track has played for a second.

- macOS: if the status line says macOS isn't allowing them, turn on notifications for Deadwax in System Settings > Notifications. After a source install, make sure the helper is installed (see [From source](#from-source)).
- Linux: run `notify-send test`. If nothing appears, no notification daemon is running. Window managers such as i3 and Hyprland need one, for example dunst or mako.

### Album art is garbled or missing

Some terminals report a protocol they do not render well. Set Album art to Half-blocks in the settings menu, or run `DEADWAX_IMAGE_PROTOCOL=halfblocks deadwax`.

### Nothing shows on Discord

The Discord desktop app must be running on the same machine, and Discord presence must be On in the settings menu. Discord also hides it when activity sharing is off in its Activity Privacy settings. Presence clears while playback is paused.

### macOS asks for keychain access after an update

Keychain access belongs to the exact build that was granted it, so a new install or upgrade asks once. Choose Always Allow.

### Reporting a bug

Open an issue at [github.com/voidrender/deadwax/issues](https://github.com/voidrender/deadwax/issues) and include the relevant lines from `deadwax.log`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for building, testing, and the macOS signing setup.

## License

Deadwax is licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in Deadwax, as defined in the Apache-2.0 license, is dual licensed as above, without any additional terms or conditions.
