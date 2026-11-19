pub mod info;
pub mod keys;
pub mod library;
pub mod login;
pub mod playlists;
pub mod queue_view;
pub mod search;
pub mod settings;
pub mod sort;

use crate::credentials::SavedTo;
use crate::player::{EngineEvent, PlayerCommand, PlayerSnapshot};
use crate::subsonic::{Album, Artist, Playlist, SearchResults, Song, SubsonicClient};
use crate::ui::art::{ArtState, ResizeDone};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use image::DynamicImage;
use keys::{Action, SEEK_STEP, VOLUME_STEP};
use library::{LibraryState, RANDOM_COUNT};
use login::{LoginField, LoginState};
use playlists::{PickerState, PlaylistsState, TextPurpose};
use queue_view::QueueViewState;
use search::SearchState;
use sort::Sorts;
use std::time::{Duration, Instant};

/// Shown after a login that fell back to the credentials file.
pub const FILE_CREDENTIALS_NOTICE: &str =
    "No system keychain found. Login saved to credentials.toml (owner-only).";

/// How long a status message replaces the view tabs.
pub const STATUS_TTL: Duration = Duration::from_secs(4);
/// How long notices the user must not miss stay up.
pub const LONG_STATUS_TTL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Library,
    Playlists,
    Search,
    Queue,
}

impl View {
    pub const ALL: [View; 4] = [View::Library, View::Playlists, View::Search, View::Queue];

    pub fn title(self) -> &'static str {
        match self {
            View::Library => "Library",
            View::Playlists => "Playlists",
            View::Search => "Search",
            View::Queue => "Queue",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Screen {
    #[default]
    Main,
    Login,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Popup {
    Confirm {
        message: String,
        on_yes: Box<Effect>,
    },
    /// A one-line text prompt. All keys type except Esc, Enter, Backspace
    /// and Ctrl+C.
    TextInput {
        title: String,
        value: String,
        on_submit: TextPurpose,
    },
    PlaylistPicker(PickerState),
    Info(Box<info::Info>),
    /// The settings menu, with the selected row.
    Settings {
        sel: usize,
    },
}

/// Results of async work spawned by the runtime. The task that issues a
/// request adds its variant here.
pub enum ApiResult {
    LoggedIn {
        client: SubsonicClient,
        saved_to: SavedTo,
    },
    LoginFailed(String),
    /// The saved login was removed in the background. `Err` carries a
    /// user-facing message when something could not be cleaned up.
    LoggedOut(Result<(), String>),
    Artists(Result<Vec<Artist>, String>),
    Artist {
        id: String,
        result: Result<Vec<Album>, String>,
    },
    Album {
        id: String,
        result: Result<Vec<Song>, String>,
    },
    Favorites(Result<Vec<Song>, String>),
    Starred {
        song_id: String,
        starred: bool,
        result: Result<(), String>,
    },
    /// Random songs for the `PlayRandom` request with this `seq`.
    Random {
        seq: u64,
        result: Result<Vec<Song>, String>,
    },
    /// An album fetched to be appended (`next` false) or played next.
    AlbumQueued {
        next: bool,
        result: Result<Vec<Song>, String>,
    },
    Search {
        seq: u64,
        result: Result<SearchResults, String>,
    },
    Playlists(Result<Vec<Playlist>, String>),
    Playlist {
        id: String,
        result: Result<Vec<Song>, String>,
    },
    /// The server's copy of a playlist after the mutation worker changed it.
    PlaylistReloaded {
        id: String,
        result: Result<Vec<Song>, String>,
    },
    /// A playlist mutation finished. `Ok` carries the status to show (empty
    /// for none).
    PlaylistMutated {
        result: Result<String, String>,
        /// The playlist the mutation targeted, if any. A failure marks it
        /// as refreshing until the reload lands.
        id: Option<String>,
    },
    /// A server result tagged with the session it belongs to. Results
    /// from before a logout or login are dropped.
    Session {
        session: u64,
        inner: Box<ApiResult>,
    },
}

impl std::fmt::Debug for ApiResult {
    // The client carries credentials, so it is never printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoggedIn { saved_to, .. } => f
                .debug_struct("LoggedIn")
                .field("saved_to", saved_to)
                .finish_non_exhaustive(),
            Self::LoginFailed(msg) => f.debug_tuple("LoginFailed").field(msg).finish(),
            Self::LoggedOut(r) => f.debug_tuple("LoggedOut").field(r).finish(),
            Self::Artists(r) => f.debug_tuple("Artists").field(r).finish(),
            Self::Artist { id, result } => f
                .debug_struct("Artist")
                .field("id", id)
                .field("result", result)
                .finish(),
            Self::Album { id, result } => f
                .debug_struct("Album")
                .field("id", id)
                .field("result", result)
                .finish(),
            Self::Favorites(r) => f.debug_tuple("Favorites").field(r).finish(),
            Self::Starred {
                song_id,
                starred,
                result,
            } => f
                .debug_struct("Starred")
                .field("song_id", song_id)
                .field("starred", starred)
                .field("result", result)
                .finish(),
            Self::Random { seq, result } => f
                .debug_struct("Random")
                .field("seq", seq)
                .field("result", result)
                .finish(),
            Self::AlbumQueued { next, result } => f
                .debug_struct("AlbumQueued")
                .field("next", next)
                .field("result", result)
                .finish(),
            Self::Search { seq, result } => f
                .debug_struct("Search")
                .field("seq", seq)
                .field("result", result)
                .finish(),
            Self::Playlists(r) => f.debug_tuple("Playlists").field(r).finish(),
            Self::Playlist { id, result } => f
                .debug_struct("Playlist")
                .field("id", id)
                .field("result", result)
                .finish(),
            Self::PlaylistReloaded { id, result } => f
                .debug_struct("PlaylistReloaded")
                .field("id", id)
                .field("result", result)
                .finish(),
            Self::PlaylistMutated { result, id } => f
                .debug_struct("PlaylistMutated")
                .field("result", result)
                .field("id", id)
                .finish(),
            Self::Session { session, inner } => f
                .debug_struct("Session")
                .field("session", session)
                .field("inner", inner)
                .finish(),
        }
    }
}

#[derive(Debug)]
pub enum AppEvent {
    Key(KeyEvent),
    Resize,
    Tick,
    Snapshot(PlayerSnapshot),
    Engine(EngineEvent),
    Api(ApiResult),
    CoverReady {
        cover_id: String,
        image: DynamicImage,
    },
    /// The cover could not be loaded.
    CoverFailed {
        cover_id: String,
    },
    /// Cover art resized and encoded off the UI thread.
    ArtResized(ResizeDone),
    /// A message from a background service, shown for a while.
    Status(String),
}

/// Side effects the runtime carries out for the app. Player commands go
/// straight to the engine; anything async is spawned and reports back as an
/// `AppEvent`.
#[derive(Clone, PartialEq)]
pub enum Effect {
    Player(PlayerCommand),
    Login {
        server: String,
        username: String,
        password: String,
    },
    Logout,
    Quit,
    LoadArtists,
    LoadArtist(String),
    LoadAlbum(String),
    LoadFavorites,
    LoadAllAlbums,
    PlayRandom {
        count: u32,
        seq: u64,
    },
    ToggleStar {
        song_id: String,
        starred: bool,
    },
    AppendAlbum(String),
    PlayNextAlbum(String),
    /// The runtime waits out the debounce, then drops it if `seq` is stale.
    Search {
        query: String,
        seq: u64,
    },
    LoadPlaylists,
    LoadPlaylist(String),
    /// Read of a playlist whose last edit failed. The worker refuses edits
    /// to it until this runs, then the read replaces the local copy.
    ResyncPlaylist(String),
    CreatePlaylist {
        name: String,
        song_ids: Vec<String>,
    },
    RenamePlaylist {
        id: String,
        name: String,
    },
    DeletePlaylist(String),
    AddToPlaylist {
        id: String,
        song_ids: Vec<String>,
    },
    RemoveFromPlaylist {
        id: String,
        index: usize,
    },
    ReplacePlaylistSongs {
        id: String,
        song_ids: Vec<String>,
    },
    /// Fetch (or read from cache) and decode a cover for the current track.
    LoadCover(String),
    /// Save the sort orders so the next run starts with them.
    SaveSorts(Sorts),
    /// Write the settings to `config.toml` and apply what can change live.
    SaveSettings(settings::Settings),
}

impl Effect {
    /// Whether the runtime hands this to the playlist mutation worker.
    pub fn is_playlist_mutation(&self) -> bool {
        matches!(
            self,
            Self::CreatePlaylist { .. }
                | Self::RenamePlaylist { .. }
                | Self::DeletePlaylist(_)
                | Self::AddToPlaylist { .. }
                | Self::RemoveFromPlaylist { .. }
                | Self::ReplacePlaylistSongs { .. }
        )
    }
}

impl std::fmt::Debug for Effect {
    // Hand-written so the login password can never reach a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Player(cmd) => f.debug_tuple("Player").field(cmd).finish(),
            Self::Login {
                server, username, ..
            } => f
                .debug_struct("Login")
                .field("server", server)
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::Logout => f.write_str("Logout"),
            Self::Quit => f.write_str("Quit"),
            Self::LoadArtists => f.write_str("LoadArtists"),
            Self::LoadArtist(id) => f.debug_tuple("LoadArtist").field(id).finish(),
            Self::LoadAlbum(id) => f.debug_tuple("LoadAlbum").field(id).finish(),
            Self::LoadFavorites => f.write_str("LoadFavorites"),
            Self::LoadAllAlbums => f.write_str("LoadAllAlbums"),
            Self::PlayRandom { count, seq } => f
                .debug_struct("PlayRandom")
                .field("count", count)
                .field("seq", seq)
                .finish(),
            Self::ToggleStar { song_id, starred } => f
                .debug_struct("ToggleStar")
                .field("song_id", song_id)
                .field("starred", starred)
                .finish(),
            Self::AppendAlbum(id) => f.debug_tuple("AppendAlbum").field(id).finish(),
            Self::PlayNextAlbum(id) => f.debug_tuple("PlayNextAlbum").field(id).finish(),
            Self::Search { query, seq } => f
                .debug_struct("Search")
                .field("query", query)
                .field("seq", seq)
                .finish(),
            Self::LoadPlaylists => f.write_str("LoadPlaylists"),
            Self::LoadPlaylist(id) => f.debug_tuple("LoadPlaylist").field(id).finish(),
            Self::CreatePlaylist { name, song_ids } => f
                .debug_struct("CreatePlaylist")
                .field("name", name)
                .field("song_ids", song_ids)
                .finish(),
            Self::RenamePlaylist { id, name } => f
                .debug_struct("RenamePlaylist")
                .field("id", id)
                .field("name", name)
                .finish(),
            Self::ResyncPlaylist(id) => f.debug_tuple("ResyncPlaylist").field(id).finish(),
            Self::DeletePlaylist(id) => f.debug_tuple("DeletePlaylist").field(id).finish(),
            Self::AddToPlaylist { id, song_ids } => f
                .debug_struct("AddToPlaylist")
                .field("id", id)
                .field("song_ids", song_ids)
                .finish(),
            Self::RemoveFromPlaylist { id, index } => f
                .debug_struct("RemoveFromPlaylist")
                .field("id", id)
                .field("index", index)
                .finish(),
            Self::ReplacePlaylistSongs { id, song_ids } => f
                .debug_struct("ReplacePlaylistSongs")
                .field("id", id)
                .field("song_ids", song_ids)
                .finish(),
            Self::LoadCover(id) => f.debug_tuple("LoadCover").field(id).finish(),
            Self::SaveSorts(s) => f.debug_tuple("SaveSorts").field(s).finish(),
            Self::SaveSettings(s) => f.debug_tuple("SaveSettings").field(s).finish(),
        }
    }
}

#[derive(Debug, Default)]
pub struct App {
    pub screen: Screen,
    pub login: LoginState,
    /// The server the session is logged in to, for the logout prompt.
    pub server: String,
    pub view: View,
    pub library: LibraryState,
    pub search: SearchState,
    pub playlists: PlaylistsState,
    pub queue: QueueViewState,
    /// Bumped on every login and logout. Server requests and their results
    /// carry it so work from an earlier session is dropped.
    pub session: u64,
    /// Songs with a star toggle in flight, mapped to their value before it.
    pub pending_stars: std::collections::HashMap<String, bool>,
    pub now_playing: bool,
    /// Cover art for the current track.
    pub art: ArtState,
    /// The queue item whose cover failed to load, so progress snapshots for
    /// that same track do not refetch it. The next track retries.
    cover_failed_on: Option<u64>,
    /// The newest random-songs request, while nothing has changed the
    /// queue since. Only its result replaces the queue.
    random_pending: Option<u64>,
    /// The sequence number of the last random-songs request.
    random_seq: u64,
    pub help: bool,
    pub popup: Option<Popup>,
    /// Options from the config that the settings menu changes. The runtime
    /// fills them in at startup and after a login.
    pub settings: settings::Settings,
    /// The album art protocol this run started with.
    pub image_protocol_at_start: crate::ui::art::ProtocolChoice,
    pub status: Option<(String, Instant)>,
    pub player: PlayerSnapshot,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// An app that starts on the main screen for an existing session.
    pub fn logged_in(server: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            ..Self::default()
        }
    }

    /// The sort order of every sortable list.
    pub fn sorts(&self) -> Sorts {
        Sorts {
            artists: self.library.artist_sort,
            albums: self.library.album_sort,
            all_albums: self.library.all_albums_sort,
            tracks: self.library.track_sort,
            playlists: self.playlists.list_sort,
        }
    }

    /// Applies saved sort orders. Lists sort as they load, so call this
    /// before the first load.
    pub fn set_sorts(&mut self, sorts: Sorts) {
        self.library.artist_sort = sorts.artists;
        self.library.album_sort = sorts.albums;
        self.library.all_albums_sort = sorts.all_albums;
        self.library.track_sort = sorts.tracks;
        self.playlists.list_sort = sorts.playlists;
    }

    /// `o`: sorts the focused list in the Library or the playlist list.
    fn cycle_sort(&mut self) -> Vec<Effect> {
        if self.now_playing {
            return Vec::new();
        }
        match self.view {
            View::Library => self.library.cycle_sort(),
            View::Playlists if self.playlists.focus == playlists::PlColumn::Lists => {
                self.playlists.cycle_sort();
            }
            View::Playlists => {
                self.set_status("Playlist tracks keep their playlist order");
                return Vec::new();
            }
            View::Search | View::Queue => return Vec::new(),
        }
        vec![Effect::SaveSorts(self.sorts())]
    }

    /// An app that starts on the login screen, with known fields filled in.
    pub fn login_screen(server: &str, username: &str) -> Self {
        let mut app = Self::default();
        app.show_login(server, username);
        app
    }

    /// Switches to the login screen. The password is always blank.
    pub fn show_login(&mut self, server: &str, username: &str) {
        let focus = if server.is_empty() {
            LoginField::Server
        } else if username.is_empty() {
            LoginField::Username
        } else {
            LoginField::Password
        };
        self.screen = Screen::Login;
        self.server.clear();
        self.now_playing = false;
        self.help = false;
        self.popup = None;
        let sorts = self.sorts();
        self.library = LibraryState::default();
        self.search = SearchState::default();
        self.playlists = PlaylistsState::default();
        self.set_sorts(sorts);
        self.session += 1;
        self.pending_stars.clear();
        self.login = LoginState {
            server: server.to_string(),
            username: username.to_string(),
            focus,
            ..LoginState::default()
        };
    }

    /// Shows the login form after a logout. It stays busy until the
    /// cleanup reports back, so a new login cannot race the cleanup.
    pub fn show_login_after_logout(&mut self, server: &str) {
        self.show_login(server, "");
        self.login.busy = true;
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// A status that stays up for `LONG_STATUS_TTL` instead of `STATUS_TTL`.
    /// The timestamp is pushed into the future, so the normal expiry check
    /// (which saturates at zero) keeps it alive for the extra time.
    pub fn set_long_status(&mut self, msg: impl Into<String>) {
        let at = Instant::now() + LONG_STATUS_TTL.checked_sub(STATUS_TTL).unwrap();
        self.status = Some((msg.into(), at));
    }

    /// The status message, if it hasn't expired yet.
    pub fn status_text(&self) -> Option<&str> {
        self.status.as_ref().map(|(m, _)| m.as_str())
    }

    fn expire_status(&mut self, now: Instant) {
        if self
            .status
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= STATUS_TTL)
        {
            self.status = None;
        }
    }

    pub fn handle_event(&mut self, ev: AppEvent) -> Vec<Effect> {
        let effects = self.dispatch(ev);
        self.note_playlist_effects(&effects);
        self.note_random(&effects);
        effects
    }

    /// Tracks whether a random-songs result may still replace the queue. A
    /// slow reply must not undo something the user queued after pressing R.
    fn note_random(&mut self, effects: &[Effect]) {
        for effect in effects {
            match effect {
                Effect::PlayRandom { seq, .. } => self.random_pending = Some(*seq),
                Effect::Logout
                | Effect::AppendAlbum(_)
                | Effect::PlayNextAlbum(_)
                | Effect::Player(
                    PlayerCommand::PlayQueue { .. }
                    | PlayerCommand::Append(_)
                    | PlayerCommand::PlayNext(_)
                    | PlayerCommand::JumpTo(_)
                    | PlayerCommand::Remove(_)
                    | PlayerCommand::Move { .. }
                    | PlayerCommand::Clear
                    | PlayerCommand::Restore(_),
                ) => self.random_pending = None,
                _ => {}
            }
        }
    }

    /// Takes the pending random request when it is `seq`. Its result is
    /// dropped when this returns false.
    pub(super) fn take_random_pending(&mut self, seq: u64) -> bool {
        if self.random_pending != Some(seq) {
            return false;
        }
        self.random_pending = None;
        true
    }

    fn dispatch(&mut self, ev: AppEvent) -> Vec<Effect> {
        match ev {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Resize => Vec::new(),
            AppEvent::Tick => {
                self.expire_status(Instant::now());
                Vec::new()
            }
            AppEvent::Snapshot(snap) => {
                self.player = snap;
                self.queue.sync(&self.player.items);
                self.sync_cover()
            }
            AppEvent::Engine(ev) => {
                if let EngineEvent::Error(msg) = ev {
                    self.set_status(msg);
                }
                Vec::new()
            }
            AppEvent::Api(result) => self.handle_api(result),
            AppEvent::CoverReady { cover_id, image } => {
                self.art.set_image(&cover_id, image);
                Vec::new()
            }
            AppEvent::CoverFailed { cover_id } => {
                if self.art.forget(&cover_id) {
                    self.cover_failed_on = self.current_item().map(|item| item.uid);
                }
                Vec::new()
            }
            AppEvent::ArtResized(done) => {
                self.art.finish_resize(done);
                Vec::new()
            }
            AppEvent::Status(msg) => {
                self.set_long_status(msg);
                Vec::new()
            }
        }
    }

    /// Points the art at the current track's cover, loading it when it
    /// changed. A track without a cover clears the art.
    pub(crate) fn sync_cover(&mut self) -> Vec<Effect> {
        let uid = self.current_item().map(|item| item.uid);
        if self.cover_failed_on.is_some() && self.cover_failed_on == uid {
            return Vec::new();
        }
        self.cover_failed_on = None;
        let cover = self
            .current_item()
            .and_then(|item| item.song.cover_art.clone());
        match cover {
            Some(id) if self.art.expect(id.clone()) => vec![Effect::LoadCover(id)],
            Some(_) => Vec::new(),
            None => {
                self.art.clear();
                Vec::new()
            }
        }
    }

    fn current_item(&self) -> Option<&crate::player::queue::QueueItem> {
        self.player.current.and_then(|i| self.player.items.get(i))
    }

    fn handle_api(&mut self, result: ApiResult) -> Vec<Effect> {
        match result {
            ApiResult::LoggedIn { client, saved_to } => {
                self.server = client.server_url().to_string();
                self.screen = Screen::Main;
                self.login = LoginState::default();
                if saved_to == SavedTo::File {
                    self.set_long_status(FILE_CREDENTIALS_NOTICE);
                }
                let sorts = self.sorts();
                self.library = LibraryState::default();
                self.search = SearchState::default();
                self.playlists = PlaylistsState::default();
                self.set_sorts(sorts);
                self.session += 1;
                // A cover load from before the login had no client to use.
                self.art.clear();
                let mut effects = self.library.start();
                effects.extend(self.sync_cover());
                return effects;
            }
            ApiResult::LoginFailed(msg) => {
                self.login.busy = false;
                self.login.error = Some(msg);
            }
            ApiResult::LoggedOut(result) => {
                if self.screen == Screen::Login {
                    self.login.busy = false;
                    if let Err(msg) = result {
                        self.login.warning = Some(msg);
                    }
                }
            }
            r @ ApiResult::Search { .. } => self.handle_search_result(r),
            r @ (ApiResult::Playlists(_)
            | ApiResult::Playlist { .. }
            | ApiResult::PlaylistReloaded { .. }
            | ApiResult::PlaylistMutated { .. }) => return self.handle_playlist_result(r),
            ApiResult::Session { session, inner } => {
                if session != self.session {
                    return Vec::new();
                }
                return self.handle_api(*inner);
            }
            other => return self.handle_library_result(other),
        }
        Vec::new()
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        if self.popup.is_some() {
            return self.handle_popup_key(key);
        }
        if self.screen == Screen::Login {
            return self.login.handle_key(key);
        }
        if self.screen == Screen::Main && self.view == View::Search && self.search.editing {
            return self.search.handle_edit_key(key);
        }
        match keys::global_action(&key) {
            Some(action) => self.run_action(action),
            // Global keys act through the help overlay. Any other key,
            // Esc included, only dismisses it.
            None if self.help => {
                self.help = false;
                Vec::new()
            }
            None if self.screen == Screen::Main
                && self.view == View::Library
                && !self.now_playing =>
            {
                self.library.handle_key(key)
            }
            None if self.screen == Screen::Main
                && self.view == View::Search
                && !self.now_playing =>
            {
                self.handle_search_key(key)
            }
            None if self.screen == Screen::Main
                && self.view == View::Playlists
                && !self.now_playing =>
            {
                self.handle_playlists_key(key)
            }
            None if self.screen == Screen::Main
                && self.view == View::Queue
                && !self.now_playing =>
            {
                self.handle_queue_key(key)
            }
            None => Vec::new(),
        }
    }

    fn handle_popup_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        let Some(popup) = self.popup.take() else {
            return Vec::new();
        };
        // Each arm returns the popup to keep open (if any) and the effects.
        let (keep, effects) = match popup {
            Popup::Confirm { message, on_yes } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => (None, vec![*on_yes]),
                KeyCode::Char('n') | KeyCode::Esc => (None, Vec::new()),
                _ => (Some(Popup::Confirm { message, on_yes }), Vec::new()),
            },
            Popup::TextInput {
                title,
                value,
                on_submit,
            } => self.text_input_key(title, value, on_submit, key),
            Popup::PlaylistPicker(picker) => self.picker_key(picker, key),
            Popup::Info(info) => (self.info_key(info, key), Vec::new()),
            Popup::Settings { sel } => self.settings_key(sel, key),
        };
        self.popup = keep;
        effects
    }

    fn run_action(&mut self, action: Action) -> Vec<Effect> {
        let cmd = match action {
            Action::ShowView(view) => {
                self.view = view;
                self.now_playing = false;
                if view == View::Playlists {
                    return self.playlists.start();
                }
                return Vec::new();
            }
            Action::FocusSearch => {
                self.view = View::Search;
                self.now_playing = false;
                self.help = false;
                self.search.editing = true;
                return Vec::new();
            }
            Action::ToggleNowPlaying => {
                self.now_playing = !self.now_playing;
                return Vec::new();
            }
            Action::ToggleHelp => {
                self.help = !self.help;
                return Vec::new();
            }
            Action::Quit => return vec![Effect::Quit],
            Action::Logout => {
                let message = if self.server.is_empty() {
                    "Log out?".to_string()
                } else {
                    format!("Log out of {}?", self.server)
                };
                self.popup = Some(Popup::Confirm {
                    message,
                    on_yes: Box::new(Effect::Logout),
                });
                return Vec::new();
            }
            Action::ToggleStar => return self.toggle_star(),
            Action::AddToPlaylist => return self.open_picker(),
            Action::PlayRandom => {
                self.random_seq += 1;
                return vec![Effect::PlayRandom {
                    count: RANDOM_COUNT,
                    seq: self.random_seq,
                }];
            }
            Action::CycleSort => return self.cycle_sort(),
            Action::ShowInfo => return self.open_info(),
            Action::ShowSettings => return self.open_settings(),
            Action::TogglePause => PlayerCommand::TogglePause,
            Action::Next => PlayerCommand::Next,
            Action::Prev => PlayerCommand::Prev,
            Action::SeekForward => PlayerCommand::SeekRelative(SEEK_STEP),
            Action::SeekBack => PlayerCommand::SeekRelative(-SEEK_STEP),
            Action::VolumeUp => PlayerCommand::ChangeVolume(i16::from(VOLUME_STEP)),
            Action::VolumeDown => PlayerCommand::ChangeVolume(-i16::from(VOLUME_STEP)),
            Action::ToggleShuffle => PlayerCommand::ToggleShuffle,
            Action::CycleRepeat => PlayerCommand::CycleRepeat,
        };
        vec![Effect::Player(cmd)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn press(app: &mut App, c: char) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )))
    }

    fn press_code(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn number_keys_switch_views() {
        let mut app = App::new();
        assert_eq!(press(&mut app, '2'), [Effect::LoadPlaylists]);
        assert_eq!(app.view, View::Playlists);
        press(&mut app, '4');
        assert_eq!(app.view, View::Queue);
        press(&mut app, '3');
        assert_eq!(app.view, View::Search);
        press(&mut app, '1');
        assert_eq!(app.view, View::Library);
    }

    #[test]
    fn space_toggles_pause() {
        let mut app = App::new();
        let effects = press(&mut app, ' ');
        assert_eq!(effects, [Effect::Player(PlayerCommand::TogglePause)]);
    }

    #[test]
    fn question_mark_toggles_help() {
        let mut app = App::new();
        press(&mut app, '?');
        assert!(app.help);
        press(&mut app, '?');
        assert!(!app.help);
        press(&mut app, '?');
        press_code(&mut app, KeyCode::Esc);
        assert!(!app.help);
    }

    #[test]
    fn help_closes_on_non_global_keys_but_globals_still_work() {
        let mut app = App::new();
        app.help = true;
        assert_eq!(
            press(&mut app, ' '),
            [Effect::Player(PlayerCommand::TogglePause)]
        );
        assert!(app.help);
        press(&mut app, '2');
        assert_eq!(app.view, View::Playlists);
        assert!(app.help);

        assert!(press(&mut app, 'j').is_empty());
        assert!(!app.help);
    }

    #[test]
    fn logout_requires_confirm() {
        let mut app = App::logged_in("https://music.example");
        assert!(press(&mut app, 'X').is_empty());
        assert_eq!(
            app.popup,
            Some(Popup::Confirm {
                message: "Log out of https://music.example?".into(),
                on_yes: Box::new(Effect::Logout),
            })
        );
        assert_eq!(press(&mut app, 'y'), [Effect::Logout]);
        assert!(app.popup.is_none());
    }

    #[test]
    fn logout_key_acts_while_help_is_open() {
        let mut app = App::logged_in("s");
        app.help = true;
        press(&mut app, 'X');
        assert!(app.popup.is_some());
    }

    #[test]
    fn login_screen_ignores_the_global_keymap() {
        let mut app = App::login_screen("", "");
        assert!(press(&mut app, 'q').is_empty());
        assert!(press(&mut app, 'X').is_empty());
        assert!(press(&mut app, '?').is_empty());
        assert!(app.popup.is_none() && !app.help);
        assert_eq!(app.login.server, "qX?");
    }

    #[test]
    fn login_screen_prefills_and_focuses_first_empty_field() {
        let app = App::login_screen("https://m.example", "alice");
        assert_eq!(app.screen, Screen::Login);
        assert_eq!(app.login.server, "https://m.example");
        assert_eq!(app.login.focus, LoginField::Password);
        let app = App::login_screen("https://m.example", "");
        assert_eq!(app.login.focus, LoginField::Username);
        let app = App::login_screen("", "");
        assert_eq!(app.login.focus, LoginField::Server);
    }

    #[test]
    fn login_failed_clears_busy_and_shows_error() {
        let mut app = App::login_screen("a", "b");
        app.login.busy = true;
        app.handle_event(AppEvent::Api(ApiResult::LoginFailed("nope".into())));
        assert!(!app.login.busy);
        assert_eq!(app.login.error.as_deref(), Some("nope"));
        assert_eq!(app.screen, Screen::Login);
    }

    fn logged_in_event(saved_to: SavedTo) -> AppEvent {
        let client = SubsonicClient::new(
            "https://music.example",
            "alice",
            crate::subsonic::Auth::Password("pw".into()),
        )
        .unwrap();
        AppEvent::Api(ApiResult::LoggedIn { client, saved_to })
    }

    #[test]
    fn logged_in_goes_to_main_and_wipes_the_password() {
        let mut app = App::login_screen("a", "b");
        app.login.password = "secret".into();
        app.handle_event(logged_in_event(SavedTo::Keychain));
        assert_eq!(app.screen, Screen::Main);
        assert_eq!(app.server, "https://music.example");
        assert!(app.login.password.is_empty());
        assert_eq!(app.status_text(), None);
    }

    #[test]
    fn artists_from_an_earlier_login_are_dropped() {
        let mut app = App::logged_in("s");
        app.on_start();
        let old = app.session;
        app.show_login("s", "u");
        app.handle_event(logged_in_event(SavedTo::Keychain));
        assert_eq!(app.library.artists, library::Loadable::Loading);
        app.handle_event(AppEvent::Api(ApiResult::Session {
            session: old,
            inner: Box::new(ApiResult::Artists(Ok(Vec::new()))),
        }));
        assert_eq!(app.library.artists, library::Loadable::Loading);
    }

    #[test]
    fn file_fallback_shows_a_status_notice() {
        let mut app = App::login_screen("a", "b");
        app.handle_event(logged_in_event(SavedTo::File));
        assert_eq!(
            app.status_text(),
            Some("No system keychain found. Login saved to credentials.toml (owner-only).")
        );
    }

    #[test]
    fn effect_debug_redacts_the_password() {
        let e = Effect::Login {
            server: "https://m.example".into(),
            username: "alice".into(),
            password: "sesame".into(),
        };
        let text = format!("{e:?}");
        assert!(!text.contains("sesame"), "{text}");
        assert!(text.contains("alice"), "{text}");
    }

    #[test]
    fn logged_out_error_lands_on_the_login_screen() {
        let mut app = App::login_screen("s", "");
        app.handle_event(AppEvent::Api(ApiResult::LoggedOut(Err("stuck".into()))));
        assert_eq!(app.login.warning.as_deref(), Some("stuck"));
        let mut app = App::login_screen("s", "");
        app.handle_event(AppEvent::Api(ApiResult::LoggedOut(Ok(()))));
        assert_eq!(app.login.warning, None);
    }

    #[test]
    fn login_waits_for_the_logout_cleanup() {
        let mut app = App::logged_in("s");
        app.show_login_after_logout("https://m.example");
        for c in "alice".chars() {
            press(&mut app, c);
        }
        press_code(&mut app, KeyCode::Tab);
        press(&mut app, 'p');
        assert!(press_code(&mut app, KeyCode::Enter).is_empty());
        app.handle_event(AppEvent::Api(ApiResult::LoggedOut(Ok(()))));
        assert!(matches!(
            press_code(&mut app, KeyCode::Enter).as_slice(),
            [Effect::Login { .. }]
        ));
    }

    #[test]
    fn long_status_outlives_the_normal_ttl() {
        let mut app = App::new();
        app.set_long_status("keep");
        let later = Instant::now() + STATUS_TTL + Duration::from_secs(1);
        app.expire_status(later);
        assert_eq!(app.status_text(), Some("keep"));
        app.expire_status(Instant::now() + LONG_STATUS_TTL + Duration::from_secs(1));
        assert_eq!(app.status_text(), None);
    }

    #[test]
    fn q_quits() {
        let mut app = App::new();
        assert_eq!(press(&mut app, 'q'), [Effect::Quit]);
    }

    #[test]
    fn transport_keys_send_player_commands() {
        let mut app = App::new();
        let cases = [
            ('.', PlayerCommand::Next),
            (',', PlayerCommand::Prev),
            (']', PlayerCommand::SeekRelative(10.0)),
            ('[', PlayerCommand::SeekRelative(-10.0)),
            ('s', PlayerCommand::ToggleShuffle),
            ('r', PlayerCommand::CycleRepeat),
        ];
        for (c, cmd) in cases {
            assert_eq!(press(&mut app, c), [Effect::Player(cmd)], "key {c:?}");
        }
    }

    #[test]
    fn volume_keys_send_steps() {
        let mut app = App::new();
        assert_eq!(
            press(&mut app, '='),
            [Effect::Player(PlayerCommand::ChangeVolume(5))]
        );
        assert_eq!(
            press(&mut app, '-'),
            [Effect::Player(PlayerCommand::ChangeVolume(-5))]
        );
    }

    #[test]
    fn z_toggles_now_playing() {
        let mut app = App::new();
        press(&mut app, 'z');
        assert!(app.now_playing);
        press(&mut app, 'z');
        assert!(!app.now_playing);
    }

    fn track(id: &str, cover: Option<&str>) -> crate::player::queue::QueueItem {
        crate::player::queue::QueueItem {
            uid: id.len() as u64,
            song: Song {
                id: id.into(),
                title: id.into(),
                artist: String::new(),
                artist_id: None,
                album: String::new(),
                album_id: None,
                track: None,
                year: None,
                duration: 100,
                cover_art: cover.map(Into::into),
                suffix: None,
                bit_rate: None,
                starred: false,
                ..Default::default()
            },
        }
    }

    fn playing(item: crate::player::queue::QueueItem) -> AppEvent {
        AppEvent::Snapshot(PlayerSnapshot {
            items: vec![item],
            current: Some(0),
            ..Default::default()
        })
    }

    fn cover() -> DynamicImage {
        image::RgbImage::new(8, 8).into()
    }

    #[test]
    fn track_change_loads_its_cover_once() {
        let mut app = App::new();
        assert_eq!(
            app.handle_event(playing(track("a", Some("al-1")))),
            [Effect::LoadCover("al-1".into())]
        );
        // Same album art, next track: nothing to reload.
        assert!(
            app.handle_event(playing(track("b", Some("al-1"))))
                .is_empty()
        );
        assert_eq!(app.art.cover_id.as_deref(), Some("al-1"));
    }

    #[test]
    fn track_without_cover_clears_the_art() {
        let mut app = App::new();
        app.handle_event(playing(track("a", Some("al-1"))));
        app.handle_event(AppEvent::CoverReady {
            cover_id: "al-1".into(),
            image: cover(),
        });
        assert!(app.art.has_image());
        assert!(app.handle_event(playing(track("b", None))).is_empty());
        assert!(!app.art.has_image());
        assert_eq!(app.art.cover_id, None);
    }

    #[test]
    fn failed_cover_is_retried_on_the_next_track_not_every_snapshot() {
        let mut app = App::new();
        app.handle_event(playing(track("a", Some("al-1"))));
        assert!(
            app.handle_event(AppEvent::CoverFailed {
                cover_id: "al-1".into()
            })
            .is_empty()
        );
        assert_eq!(app.art.cover_id, None);
        // Same track again (a progress snapshot): no refetch loop.
        assert!(
            app.handle_event(playing(track("a", Some("al-1"))))
                .is_empty()
        );
        // Next track on the same album retries.
        assert_eq!(
            app.handle_event(playing(track("bb", Some("al-1")))),
            [Effect::LoadCover("al-1".into())]
        );
    }

    #[test]
    fn stale_cover_failure_is_ignored() {
        let mut app = App::new();
        app.handle_event(playing(track("a", Some("al-1"))));
        app.handle_event(playing(track("bb", Some("al-2"))));
        app.handle_event(AppEvent::CoverFailed {
            cover_id: "al-1".into(),
        });
        assert_eq!(app.art.cover_id.as_deref(), Some("al-2"));
    }

    #[test]
    fn stale_cover_is_dropped() {
        let mut app = App::new();
        app.handle_event(playing(track("a", Some("al-1"))));
        app.handle_event(playing(track("b", Some("al-2"))));
        app.handle_event(AppEvent::CoverReady {
            cover_id: "al-1".into(),
            image: cover(),
        });
        assert!(!app.art.has_image());
        assert_eq!(app.art.cover_id.as_deref(), Some("al-2"));
        app.handle_event(AppEvent::CoverReady {
            cover_id: "al-2".into(),
            image: cover(),
        });
        assert!(app.art.has_image());
    }

    #[test]
    fn f_in_now_playing_stars_the_current_track() {
        let mut app = App::new();
        app.handle_event(playing(track("cur", None)));
        app.now_playing = true;
        assert_eq!(
            press(&mut app, 'f'),
            [
                Effect::ToggleStar {
                    song_id: "cur".into(),
                    starred: true,
                },
                Effect::Player(crate::player::PlayerCommand::SetStarred {
                    song_id: "cur".into(),
                    starred: true,
                }),
            ]
        );
        assert!(app.player.items[0].song.starred);
    }

    #[test]
    fn confirm_popup_takes_keys_and_runs_on_yes() {
        let mut app = App::new();
        let confirm = || Popup::Confirm {
            message: "Sure?".into(),
            on_yes: Box::new(Effect::Quit),
        };

        app.popup = Some(confirm());
        assert!(press(&mut app, '2').is_empty());
        assert_eq!(app.view, View::Library);
        assert!(app.popup.is_some());
        assert_eq!(press(&mut app, 'y'), [Effect::Quit]);
        assert!(app.popup.is_none());

        app.popup = Some(confirm());
        assert_eq!(press_code(&mut app, KeyCode::Enter), [Effect::Quit]);

        app.popup = Some(confirm());
        assert!(press(&mut app, 'n').is_empty());
        assert!(app.popup.is_none());

        app.popup = Some(confirm());
        assert!(press_code(&mut app, KeyCode::Esc).is_empty());
        assert!(app.popup.is_none());
    }

    #[test]
    fn snapshot_event_replaces_player_state() {
        let mut app = App::new();
        let snap = PlayerSnapshot {
            volume: 42,
            position: 3.0,
            ..Default::default()
        };
        app.handle_event(AppEvent::Snapshot(snap.clone()));
        assert_eq!(app.player, snap);
    }

    #[test]
    fn engine_errors_become_status_messages() {
        let mut app = App::new();
        app.handle_event(AppEvent::Engine(EngineEvent::Error(
            "Couldn't play x".into(),
        )));
        assert_eq!(app.status_text(), Some("Couldn't play x"));
    }

    #[test]
    fn status_expires_on_tick_after_four_seconds() {
        let mut app = App::new();
        app.set_status("hello");
        app.handle_event(AppEvent::Tick);
        assert_eq!(app.status_text(), Some("hello"));

        let old = Instant::now()
            .checked_sub(STATUS_TTL + Duration::from_millis(1))
            .unwrap();
        app.status = Some(("hello".into(), old));
        app.handle_event(AppEvent::Tick);
        assert_eq!(app.status_text(), None);
    }
}
