use super::sort::{self, AlbumSort, ArtistSort, TrackSort};
use super::{ApiResult, App, Effect, Screen, View};
use crate::player::PlayerCommand;
use crate::subsonic::{Album, Artist, Song};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

/// Id of the synthetic album that holds the starred songs.
pub const FAVORITES_ID: &str = "deadwax:favorites";
/// Songs requested by the global `R` key.
pub const RANDOM_COUNT: u32 = 200;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Loadable<T> {
    #[default]
    NotLoaded,
    Loading,
    Loaded(T),
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(v) => Some(v),
            _ => None,
        }
    }

    fn is_failed(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ArtistRow {
    Favorites,
    Artist(Artist),
}

impl ArtistRow {
    pub fn id(&self) -> &str {
        match self {
            Self::Favorites => FAVORITES_ID,
            Self::Artist(a) => &a.id,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Column {
    #[default]
    Artists,
    Albums,
    Tracks,
}

impl Column {
    pub fn index(self) -> usize {
        self as usize
    }

    fn next(self) -> Self {
        match self {
            Self::Artists => Self::Albums,
            Self::Albums | Self::Tracks => Self::Tracks,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Artists | Self::Albums => Self::Artists,
            Self::Tracks => Self::Albums,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibraryState {
    pub artists: Loadable<Vec<ArtistRow>>,
    pub albums: Loadable<Vec<Album>>,
    pub tracks: Loadable<Vec<Song>>,
    pub focus: Column,
    pub sel: [usize; 3],
    pub albums_for: Option<String>,
    pub tracks_for: Option<String>,
    /// Album to select once the revealed artist's albums arrive.
    pending_album: Option<String>,
    pub artist_sort: ArtistSort,
    pub album_sort: AlbumSort,
    pub track_sort: TrackSort,
    /// Server order of the loaded artists and tracks, for sorts that fall
    /// back to it.
    artist_ranks: HashMap<String, usize>,
    track_ranks: HashMap<String, usize>,
}

fn favorites_album() -> Album {
    Album {
        id: FAVORITES_ID.to_string(),
        name: "★ Favorites".to_string(),
        artist: String::new(),
        artist_id: None,
        year: None,
        song_count: 0,
        duration: 0,
        cover_art: None,
        music_brainz_id: None,
        ..Default::default()
    }
}

impl LibraryState {
    /// Starts loading the artist list the first time it is needed.
    pub fn start(&mut self) -> Vec<Effect> {
        if self.artists != Loadable::NotLoaded {
            return Vec::new();
        }
        self.artists = Loadable::Loading;
        vec![Effect::LoadArtists]
    }

    /// Points the columns at an artist and optionally one of its albums.
    /// Used by Search. Does nothing until the artist list has loaded.
    pub fn reveal(&mut self, artist_id: &str, album_id: Option<&str>) -> Vec<Effect> {
        let Some(rows) = self.artists.loaded() else {
            return Vec::new();
        };
        let Some(idx) = rows.iter().position(|r| r.id() == artist_id) else {
            return Vec::new();
        };
        self.sel[0] = idx;
        self.focus = if album_id.is_some() {
            Column::Tracks
        } else {
            Column::Albums
        };
        self.pending_album = album_id.map(str::to_string);
        self.select_artist()
    }

    fn len(&self, col: Column) -> usize {
        match col {
            Column::Artists => self.artists.loaded().map_or(0, Vec::len),
            Column::Albums => self.albums.loaded().map_or(0, Vec::len),
            Column::Tracks => self.tracks.loaded().map_or(0, Vec::len),
        }
    }

    pub fn selected_track(&self) -> Option<&Song> {
        self.tracks.loaded()?.get(self.sel[2])
    }

    /// The song `f` acts on: the selected track while the tracks column has focus.
    pub fn focused_song(&self) -> Option<&Song> {
        (self.focus == Column::Tracks)
            .then(|| self.selected_track())
            .flatten()
    }

    fn clear_tracks(&mut self) {
        self.tracks = Loadable::NotLoaded;
        self.tracks_for = None;
        self.sel[2] = 0;
    }

    fn select_artist(&mut self) -> Vec<Effect> {
        self.sel[1] = 0;
        self.albums = Loadable::NotLoaded;
        self.albums_for = None;
        self.clear_tracks();
        let Some(row) = self
            .artists
            .loaded()
            .and_then(|rows| rows.get(self.sel[0]))
            .cloned()
        else {
            return Vec::new();
        };
        match row {
            ArtistRow::Favorites => {
                self.albums_for = Some(FAVORITES_ID.to_string());
                self.albums = Loadable::Loaded(vec![favorites_album()]);
                self.select_album()
            }
            ArtistRow::Artist(a) => {
                self.albums_for = Some(a.id.clone());
                self.albums = Loadable::Loading;
                vec![Effect::LoadArtist(a.id)]
            }
        }
    }

    fn select_album(&mut self) -> Vec<Effect> {
        self.clear_tracks();
        let Some(id) = self
            .albums
            .loaded()
            .and_then(|a| a.get(self.sel[1]))
            .map(|a| a.id.clone())
        else {
            return Vec::new();
        };
        self.tracks_for = Some(id.clone());
        self.tracks = Loadable::Loading;
        vec![if id == FAVORITES_ID {
            Effect::LoadFavorites
        } else {
            Effect::LoadAlbum(id)
        }]
    }

    /// Applies the artist list. `Err` carries a load failure to show.
    pub fn apply_artists(
        &mut self,
        result: Result<Vec<Artist>, String>,
    ) -> Result<Vec<Effect>, String> {
        if self.artists != Loadable::Loading {
            return Ok(Vec::new());
        }
        match result {
            Ok(artists) => {
                let mut rows: Vec<ArtistRow> = std::iter::once(ArtistRow::Favorites)
                    .chain(artists.into_iter().map(ArtistRow::Artist))
                    .collect();
                self.artist_ranks = sort::rank(rows.iter().map(ArtistRow::id));
                sort::sort_artists(&mut rows, self.artist_sort, &self.artist_ranks);
                self.artists = Loadable::Loaded(rows);
                self.sel[0] = 0;
                Ok(self.select_artist())
            }
            Err(msg) => {
                self.artists = Loadable::Failed(msg.clone());
                Err(msg)
            }
        }
    }

    /// Applies an artist's albums if that artist is still selected.
    pub fn apply_albums(
        &mut self,
        id: &str,
        result: Result<Vec<Album>, String>,
    ) -> Result<Vec<Effect>, String> {
        if self.albums != Loadable::Loading || self.albums_for.as_deref() != Some(id) {
            return Ok(Vec::new());
        }
        match result {
            Ok(mut albums) => {
                sort::sort_albums(&mut albums, self.album_sort);
                if let Some(want) = self.pending_album.take()
                    && let Some(i) = albums.iter().position(|a| a.id == want)
                {
                    self.sel[1] = i;
                }
                self.albums = Loadable::Loaded(albums);
                Ok(self.select_album())
            }
            Err(msg) => {
                self.albums = Loadable::Failed(msg.clone());
                Err(msg)
            }
        }
    }

    /// Applies an album's songs if that album is still selected.
    pub fn apply_tracks(
        &mut self,
        id: &str,
        result: Result<Vec<Song>, String>,
    ) -> Result<(), String> {
        if self.tracks != Loadable::Loading || self.tracks_for.as_deref() != Some(id) {
            return Ok(());
        }
        match result {
            Ok(mut songs) => {
                self.track_ranks = sort::rank(songs.iter().map(|s| s.id.as_str()));
                sort::sort_tracks(&mut songs, self.track_sort, &self.track_ranks);
                self.tracks = Loadable::Loaded(songs);
                Ok(())
            }
            Err(msg) => {
                self.tracks = Loadable::Failed(msg.clone());
                Err(msg)
            }
        }
    }

    pub fn set_starred(&mut self, song_id: &str, starred: bool) {
        if let Loadable::Loaded(songs) = &mut self.tracks {
            for s in songs.iter_mut().filter(|s| s.id == song_id) {
                s.starred = starred;
            }
        }
    }

    /// Moves the focused column to its next sort order. The selected row
    /// stays selected, so nothing reloads.
    pub fn cycle_sort(&mut self) {
        let col = self.focus.index();
        match self.focus {
            Column::Artists => {
                self.artist_sort = self.artist_sort.next();
                if let Loadable::Loaded(rows) = &mut self.artists {
                    let keep = rows.get(self.sel[col]).map(|r| r.id().to_string());
                    sort::sort_artists(rows, self.artist_sort, &self.artist_ranks);
                    self.sel[col] = position(rows, keep.as_deref(), ArtistRow::id);
                }
            }
            Column::Albums => {
                self.album_sort = self.album_sort.next();
                if let Loadable::Loaded(albums) = &mut self.albums {
                    let keep = albums.get(self.sel[col]).map(|a| a.id.clone());
                    sort::sort_albums(albums, self.album_sort);
                    self.sel[col] = position(albums, keep.as_deref(), |a| &a.id);
                }
            }
            Column::Tracks => {
                self.track_sort = self.track_sort.next();
                if let Loadable::Loaded(songs) = &mut self.tracks {
                    let keep = songs.get(self.sel[col]).map(|s| s.id.clone());
                    sort::sort_tracks(songs, self.track_sort, &self.track_ranks);
                    self.sel[col] = position(songs, keep.as_deref(), |s| &s.id);
                }
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Vec::new();
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('h') | KeyCode::Left => {
                self.focus = self.focus.prev();
                Vec::new()
            }
            KeyCode::Char('l') | KeyCode::Right => self.drill_in(),
            KeyCode::Enter => self.enter(),
            KeyCode::Char('e') => self.queue(false),
            KeyCode::Char('p') => self.queue(true),
            _ => Vec::new(),
        }
    }

    fn move_selection(&mut self, delta: isize) -> Vec<Effect> {
        let col = self.focus;
        let len = self.len(col);
        if len == 0 {
            return Vec::new();
        }
        let old = self.sel[col.index()];
        let new = old.saturating_add_signed(delta).min(len - 1);
        if new == old {
            return Vec::new();
        }
        self.sel[col.index()] = new;
        match col {
            Column::Artists => self.select_artist(),
            Column::Albums => self.select_album(),
            Column::Tracks => Vec::new(),
        }
    }

    /// Moves focus right, or retries when the focused column failed to load.
    fn drill_in(&mut self) -> Vec<Effect> {
        let failed = match self.focus {
            Column::Artists => self.artists.is_failed(),
            Column::Albums => self.albums.is_failed(),
            Column::Tracks => self.tracks.is_failed(),
        };
        if !failed {
            self.focus = self.focus.next();
            return Vec::new();
        }
        match self.focus {
            Column::Artists => {
                self.artists = Loadable::NotLoaded;
                self.start()
            }
            Column::Albums => self.select_artist(),
            Column::Tracks => self.select_album(),
        }
    }

    fn enter(&mut self) -> Vec<Effect> {
        match self.focus {
            Column::Artists => self.drill_in(),
            Column::Albums => match self.tracks.loaded() {
                Some(songs) if !songs.is_empty() => {
                    vec![Effect::Player(PlayerCommand::PlayQueue {
                        songs: songs.clone(),
                        start: 0,
                    })]
                }
                _ => self.drill_in(),
            },
            Column::Tracks => match self.tracks.loaded() {
                Some(songs) if self.sel[2] < songs.len() => {
                    vec![Effect::Player(PlayerCommand::PlayQueue {
                        songs: songs.clone(),
                        start: self.sel[2],
                    })]
                }
                _ => self.drill_in(),
            },
        }
    }

    /// `e` (append) and `p` (play next) on the focused track or album.
    fn queue(&self, next: bool) -> Vec<Effect> {
        let player = |songs: Vec<Song>| {
            Effect::Player(if next {
                PlayerCommand::PlayNext(songs)
            } else {
                PlayerCommand::Append(songs)
            })
        };
        match self.focus {
            Column::Artists => Vec::new(),
            Column::Tracks => self
                .selected_track()
                .map(|s| vec![player(vec![s.clone()])])
                .unwrap_or_default(),
            Column::Albums => {
                let Some(album) = self.albums.loaded().and_then(|a| a.get(self.sel[1])) else {
                    return Vec::new();
                };
                if album.id == FAVORITES_ID {
                    return self
                        .tracks
                        .loaded()
                        .map(|t| vec![player(t.clone())])
                        .unwrap_or_default();
                }
                vec![if next {
                    Effect::PlayNextAlbum(album.id.clone())
                } else {
                    Effect::AppendAlbum(album.id.clone())
                }]
            }
        }
    }
}

/// Where the row with `id` landed, or the top when it is gone.
pub(super) fn position<T>(items: &[T], id: Option<&str>, id_of: impl Fn(&T) -> &str) -> usize {
    id.and_then(|id| items.iter().position(|i| id_of(i) == id))
        .unwrap_or(0)
}

impl App {
    /// Effects to run once the app is on screen (loads the library).
    pub fn on_start(&mut self) -> Vec<Effect> {
        if self.screen == Screen::Main {
            let mut effects = self.library.start();
            effects.extend(self.sync_cover());
            effects
        } else {
            Vec::new()
        }
    }

    /// The song the global `f` key acts on: the current track in Now
    /// Playing, otherwise the focused song in the active view.
    pub fn selected_song(&self) -> Option<&Song> {
        if self.screen != Screen::Main {
            return None;
        }
        if self.now_playing {
            return self
                .player
                .current
                .and_then(|i| self.player.items.get(i))
                .map(|item| &item.song);
        }
        match self.view {
            View::Library => self.library.focused_song(),
            View::Search => self.search.selected_song(),
            View::Playlists => self.playlists.focused_song(),
            View::Queue => self.selected_queue_song(),
        }
    }

    /// Optimistically flips the star on the selected song. The engine gets
    /// the flip too, so its next snapshot does not undo it.
    pub(super) fn toggle_star(&mut self) -> Vec<Effect> {
        let Some(song) = self.selected_song() else {
            return Vec::new();
        };
        let (song_id, previous) = (song.id.clone(), song.starred);
        if self.pending_stars.contains_key(&song_id) {
            self.set_status("Still saving favorite…");
            return Vec::new();
        }
        let starred = !previous;
        self.pending_stars.insert(song_id.clone(), previous);
        self.set_starred_everywhere(&song_id, starred);
        vec![
            Effect::ToggleStar {
                song_id: song_id.clone(),
                starred,
            },
            Effect::Player(PlayerCommand::SetStarred { song_id, starred }),
        ]
    }

    /// Handles the library's API results. Anything else is left alone.
    pub(super) fn handle_library_result(&mut self, result: ApiResult) -> Vec<Effect> {
        match result {
            ApiResult::Artists(r) => {
                let applied = self.library.apply_artists(r);
                self.finish_load("artists", applied)
            }
            ApiResult::Artist { id, result } => {
                let applied = self.library.apply_albums(&id, result);
                self.finish_load("albums", applied)
            }
            ApiResult::Album { id, result } => {
                let applied = self.library.apply_tracks(&id, result);
                self.finish_load("songs", applied.map(|()| Vec::new()))
            }
            ApiResult::Favorites(r) => {
                let applied = self.library.apply_tracks(FAVORITES_ID, r);
                self.finish_load("favorites", applied.map(|()| Vec::new()))
            }
            ApiResult::Starred {
                song_id,
                starred,
                result,
            } => {
                let previous = self.pending_stars.remove(&song_id).unwrap_or(!starred);
                match result {
                    Ok(()) => {
                        // Re-apply in case the tracks were reloaded meanwhile.
                        self.set_starred_everywhere(&song_id, starred);
                        vec![Effect::Player(PlayerCommand::SetStarred {
                            song_id,
                            starred,
                        })]
                    }
                    Err(msg) => {
                        self.set_starred_everywhere(&song_id, previous);
                        self.set_status(format!("Couldn't update favorite: {msg}"));
                        vec![Effect::Player(PlayerCommand::SetStarred {
                            song_id,
                            starred: previous,
                        })]
                    }
                }
            }
            // The queue changed after R, or a newer R is out.
            ApiResult::Random { seq, .. } if !self.take_random_pending(seq) => Vec::new(),
            ApiResult::Random { result, .. } => match result {
                Ok(songs) if songs.is_empty() => {
                    self.set_status("The server returned no songs");
                    Vec::new()
                }
                Ok(songs) => vec![Effect::Player(PlayerCommand::PlayQueue { songs, start: 0 })],
                Err(msg) => {
                    self.set_status(format!("Couldn't load random songs: {msg}"));
                    Vec::new()
                }
            },
            ApiResult::AlbumQueued { next, result } => match result {
                Ok(songs) => vec![Effect::Player(if next {
                    PlayerCommand::PlayNext(songs)
                } else {
                    PlayerCommand::Append(songs)
                })],
                Err(msg) => {
                    self.set_status(format!("Couldn't load the album: {msg}"));
                    Vec::new()
                }
            },
            _ => Vec::new(),
        }
    }

    fn set_starred_everywhere(&mut self, song_id: &str, starred: bool) {
        self.library.set_starred(song_id, starred);
        self.search.set_starred(song_id, starred);
        self.playlists.set_starred(song_id, starred);
        for item in &mut self.player.items {
            if item.song.id == song_id {
                item.song.starred = starred;
            }
        }
    }

    fn finish_load(&mut self, what: &str, applied: Result<Vec<Effect>, String>) -> Vec<Effect> {
        applied.unwrap_or_else(|msg| {
            self.set_status(format!("Couldn't load {what}: {msg}"));
            Vec::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppEvent;
    use crate::app::sort::Sorts;

    fn press(app: &mut App, c: char) -> Vec<Effect> {
        key(app, KeyCode::Char(c))
    }

    fn key(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn api(app: &mut App, r: ApiResult) -> Vec<Effect> {
        app.handle_event(AppEvent::Api(r))
    }

    fn artist(id: &str) -> Artist {
        Artist {
            id: id.into(),
            name: format!("Artist {id}"),
            album_count: 1,
            cover_art: None,
        }
    }

    fn album(id: &str) -> Album {
        Album {
            name: format!("Album {id}"),
            id: id.into(),
            ..favorites_album()
        }
    }

    fn song(id: &str) -> Song {
        Song {
            id: id.into(),
            title: format!("Song {id}"),
            artist: "A".into(),
            artist_id: None,
            album: "B".into(),
            album_id: None,
            track: None,
            year: None,
            duration: 100,
            cover_art: None,
            suffix: None,
            bit_rate: None,
            starred: false,
            ..Default::default()
        }
    }

    /// Artists a and b loaded, selection on a, a's albums loaded, tracks loaded.
    fn loaded_app() -> App {
        let mut app = App::logged_in("s");
        assert_eq!(app.on_start(), [Effect::LoadArtists]);
        api(
            &mut app,
            ApiResult::Artists(Ok(vec![artist("a"), artist("b")])),
        );
        press(&mut app, 'j'); // Favorites -> a
        api(
            &mut app,
            ApiResult::Artist {
                id: "a".into(),
                result: Ok(vec![album("a1"), album("a2")]),
            },
        );
        api(
            &mut app,
            ApiResult::Album {
                id: "a1".into(),
                result: Ok(vec![song("s0"), song("s1"), song("s2")]),
            },
        );
        app
    }

    #[test]
    fn selecting_artist_loads_albums() {
        let mut app = App::logged_in("s");
        app.on_start();
        api(&mut app, ApiResult::Artists(Ok(vec![artist("a")])));
        assert_eq!(press(&mut app, 'j'), [Effect::LoadArtist("a".into())]);
        assert_eq!(app.library.albums, Loadable::Loading);
    }

    #[test]
    fn arriving_albums_load_the_first_album() {
        let mut app = App::logged_in("s");
        app.on_start();
        api(&mut app, ApiResult::Artists(Ok(vec![artist("a")])));
        press(&mut app, 'j');
        let fx = api(
            &mut app,
            ApiResult::Artist {
                id: "a".into(),
                result: Ok(vec![album("a1")]),
            },
        );
        assert_eq!(fx, [Effect::LoadAlbum("a1".into())]);
    }

    #[test]
    fn stale_album_response_ignored() {
        let mut app = App::logged_in("s");
        app.on_start();
        api(
            &mut app,
            ApiResult::Artists(Ok(vec![artist("a"), artist("b")])),
        );
        press(&mut app, 'j');
        assert_eq!(press(&mut app, 'j'), [Effect::LoadArtist("b".into())]);
        let fx = api(
            &mut app,
            ApiResult::Artist {
                id: "a".into(),
                result: Ok(vec![album("a1")]),
            },
        );
        assert!(fx.is_empty());
        assert_eq!(app.library.albums, Loadable::Loading);
        assert_eq!(app.library.albums_for.as_deref(), Some("b"));
    }

    #[test]
    fn stale_track_response_ignored() {
        let mut app = loaded_app();
        app.library.focus = Column::Albums;
        assert_eq!(press(&mut app, 'j'), [Effect::LoadAlbum("a2".into())]);
        api(
            &mut app,
            ApiResult::Album {
                id: "a1".into(),
                result: Ok(vec![song("x")]),
            },
        );
        assert_eq!(app.library.tracks, Loadable::Loading);
    }

    #[test]
    fn enter_on_track_replaces_queue() {
        let mut app = loaded_app();
        app.library.focus = Column::Tracks;
        press(&mut app, 'j');
        press(&mut app, 'j');
        let fx = key(&mut app, KeyCode::Enter);
        let Effect::Player(PlayerCommand::PlayQueue { songs, start }) = &fx[0] else {
            panic!("{fx:?}");
        };
        assert_eq!((songs.len(), *start), (3, 2));
    }

    #[test]
    fn favorites_row_loads_starred() {
        let mut app = App::logged_in("s");
        app.on_start();
        let fx = api(&mut app, ApiResult::Artists(Ok(vec![artist("a")])));
        assert_eq!(fx, [Effect::LoadFavorites]);
        assert_eq!(app.library.albums.loaded().unwrap()[0].name, "★ Favorites");
        api(&mut app, ApiResult::Favorites(Ok(vec![song("f1")])));
        assert_eq!(app.library.tracks.loaded().unwrap().len(), 1);
    }

    #[test]
    fn favorite_toggle_is_optimistic_and_reverts_on_error() {
        let mut app = loaded_app();
        app.library.focus = Column::Tracks;
        let fx = press(&mut app, 'f');
        assert_eq!(
            fx,
            [
                Effect::ToggleStar {
                    song_id: "s0".into(),
                    starred: true,
                },
                Effect::Player(crate::player::PlayerCommand::SetStarred {
                    song_id: "s0".into(),
                    starred: true,
                }),
            ]
        );
        assert!(app.library.tracks.loaded().unwrap()[0].starred);

        let fx = api(
            &mut app,
            ApiResult::Starred {
                song_id: "s0".into(),
                starred: true,
                result: Ok(()),
            },
        );
        assert_eq!(
            fx,
            [Effect::Player(PlayerCommand::SetStarred {
                song_id: "s0".into(),
                starred: true
            })]
        );

        let fx = api(
            &mut app,
            ApiResult::Starred {
                song_id: "s0".into(),
                starred: true,
                result: Err("timeout".into()),
            },
        );
        // The engine got the flip up front, so it gets the revert too.
        assert_eq!(
            fx,
            [Effect::Player(PlayerCommand::SetStarred {
                song_id: "s0".into(),
                starred: false
            })]
        );
        assert!(!app.library.tracks.loaded().unwrap()[0].starred);
        assert!(app.status_text().unwrap().contains("timeout"));
    }

    fn starred_result(id: &str, starred: bool, ok: bool) -> ApiResult {
        ApiResult::Starred {
            song_id: id.into(),
            starred,
            result: if ok { Ok(()) } else { Err("boom".into()) },
        }
    }

    #[test]
    fn rapid_presses_send_one_toggle() {
        let mut app = loaded_app();
        app.library.focus = Column::Tracks;
        assert_eq!(press(&mut app, 'f').len(), 2);
        assert!(press(&mut app, 'f').is_empty());
        assert!(app.library.tracks.loaded().unwrap()[0].starred);
        api(&mut app, starred_result("s0", true, true));
        assert_eq!(press(&mut app, 'f').len(), 2);
    }

    #[test]
    fn failure_restores_the_previous_value() {
        let mut app = loaded_app();
        app.library.focus = Column::Tracks;
        app.library.set_starred("s0", true);
        press(&mut app, 'f');
        assert!(!app.library.tracks.loaded().unwrap()[0].starred);
        api(&mut app, starred_result("s0", false, false));
        assert!(app.library.tracks.loaded().unwrap()[0].starred);
        assert!(app.pending_stars.is_empty());
    }

    #[test]
    fn success_after_reload_reapplies_the_flag() {
        let mut app = loaded_app();
        app.library.focus = Column::Tracks;
        press(&mut app, 'f');
        app.library.tracks = Loadable::Loaded(vec![song("s0")]);
        api(&mut app, starred_result("s0", true, true));
        assert!(app.library.tracks.loaded().unwrap()[0].starred);
    }

    #[test]
    fn f_needs_a_selected_track() {
        let mut app = loaded_app();
        assert!(press(&mut app, 'f').is_empty());
        app.library.focus = Column::Tracks;
        app.view = View::Queue;
        assert!(press(&mut app, 'f').is_empty());
    }

    #[test]
    fn load_failure_sets_status() {
        let mut app = App::logged_in("s");
        app.on_start();
        api(&mut app, ApiResult::Artists(Err("timeout".into())));
        assert_eq!(app.library.artists, Loadable::Failed("timeout".into()));
        assert!(app.status_text().unwrap().contains("timeout"));
    }

    #[test]
    fn l_retries_a_failed_column() {
        let mut app = App::logged_in("s");
        app.on_start();
        api(&mut app, ApiResult::Artists(Err("x".into())));
        assert_eq!(press(&mut app, 'l'), [Effect::LoadArtists]);
        assert_eq!(app.library.artists, Loadable::Loading);
        assert_eq!(app.library.focus, Column::Artists);
    }

    #[test]
    fn column_keys_move_focus() {
        let mut app = loaded_app();
        press(&mut app, 'l');
        assert_eq!(app.library.focus, Column::Albums);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.library.focus, Column::Tracks);
        press(&mut app, 'l');
        assert_eq!(app.library.focus, Column::Tracks);
        press(&mut app, 'h');
        key(&mut app, KeyCode::Left);
        press(&mut app, 'h');
        assert_eq!(app.library.focus, Column::Artists);
    }

    #[test]
    fn e_and_p_queue_tracks_and_albums() {
        let mut app = loaded_app();
        app.library.focus = Column::Tracks;
        press(&mut app, 'j');
        assert_eq!(
            press(&mut app, 'e'),
            [Effect::Player(PlayerCommand::Append(vec![song("s1")]))]
        );
        assert_eq!(
            press(&mut app, 'p'),
            [Effect::Player(PlayerCommand::PlayNext(vec![song("s1")]))]
        );
        app.library.focus = Column::Albums;
        assert_eq!(press(&mut app, 'e'), [Effect::AppendAlbum("a1".into())]);
        assert_eq!(press(&mut app, 'p'), [Effect::PlayNextAlbum("a1".into())]);
    }

    #[test]
    fn album_queued_result_becomes_player_command() {
        let mut app = App::logged_in("s");
        let fx = api(
            &mut app,
            ApiResult::AlbumQueued {
                next: true,
                result: Ok(vec![song("x")]),
            },
        );
        assert_eq!(
            fx,
            [Effect::Player(PlayerCommand::PlayNext(vec![song("x")]))]
        );
    }

    fn random(seq: u64, songs: Vec<Song>) -> ApiResult {
        ApiResult::Random {
            seq,
            result: Ok(songs),
        }
    }

    #[test]
    fn capital_r_plays_random_and_result_replaces_queue() {
        let mut app = App::logged_in("s");
        assert_eq!(
            press(&mut app, 'R'),
            [Effect::PlayRandom { count: 200, seq: 1 }]
        );
        let fx = api(&mut app, random(1, vec![song("x")]));
        assert_eq!(
            fx,
            [Effect::Player(PlayerCommand::PlayQueue {
                songs: vec![song("x")],
                start: 0
            })]
        );
    }

    #[test]
    fn a_random_result_after_a_queue_change_is_dropped() {
        let mut app = App::logged_in("s");
        press(&mut app, 'R');
        // The user plays something else while the request is out.
        app.handle_event(AppEvent::Api(ApiResult::AlbumQueued {
            next: false,
            result: Ok(vec![song("a")]),
        }));
        assert!(api(&mut app, random(1, vec![song("x")])).is_empty());

        // A result with no request out is dropped too, and a fresh R works.
        assert!(api(&mut app, random(1, vec![song("x")])).is_empty());
        press(&mut app, 'R');
        assert_eq!(api(&mut app, random(2, vec![song("x")])).len(), 1);
    }

    #[test]
    fn only_the_newest_random_request_plays() {
        let mut app = App::logged_in("s");
        press(&mut app, 'R');
        press(&mut app, 'R');
        assert!(api(&mut app, random(1, vec![song("x")])).is_empty());
        assert_eq!(
            api(&mut app, random(2, vec![song("y")])),
            [Effect::Player(PlayerCommand::PlayQueue {
                songs: vec![song("y")],
                start: 0
            })]
        );
    }

    #[test]
    fn library_keys_are_inert_in_other_views_help_and_now_playing() {
        let mut app = loaded_app();
        app.view = View::Queue;
        assert!(press(&mut app, 'j').is_empty());
        app.view = View::Library;
        app.now_playing = true;
        assert!(press(&mut app, 'j').is_empty());
        app.now_playing = false;
        app.help = true;
        assert!(press(&mut app, 'j').is_empty());
        assert!(!app.help);
        assert_eq!(app.library.sel, [1, 0, 0]);
    }

    #[test]
    fn login_loads_artists_and_logout_clears_library() {
        let mut app = loaded_app();
        app.show_login("s", "");
        assert_eq!(app.library, LibraryState::default());
        // A late response from the old session is ignored.
        api(&mut app, ApiResult::Artists(Ok(vec![artist("z")])));
        assert_eq!(app.library.artists, Loadable::NotLoaded);
    }

    #[test]
    fn reveal_selects_artist_and_album() {
        let mut app = loaded_app();
        let fx = app.library.reveal("b", Some("b2"));
        assert_eq!(fx, [Effect::LoadArtist("b".into())]);
        assert_eq!(app.library.sel[0], 2);
        api(
            &mut app,
            ApiResult::Artist {
                id: "b".into(),
                result: Ok(vec![album("b1"), album("b2")]),
            },
        );
        assert_eq!(app.library.sel[1], 1);
        assert_eq!(app.library.tracks_for.as_deref(), Some("b2"));
    }

    fn dated(id: &str, year: u32) -> Album {
        Album {
            year: Some(year),
            ..album(id)
        }
    }

    fn album_ids(app: &App) -> Vec<&str> {
        app.library
            .albums
            .loaded()
            .unwrap()
            .iter()
            .map(|a| a.id.as_str())
            .collect()
    }

    /// One artist with albums from 2000 and 2010, newest selected.
    fn dated_app() -> App {
        let mut app = App::logged_in("s");
        app.on_start();
        api(&mut app, ApiResult::Artists(Ok(vec![artist("a")])));
        press(&mut app, 'j');
        let fx = api(
            &mut app,
            ApiResult::Artist {
                id: "a".into(),
                result: Ok(vec![dated("x", 2000), dated("y", 2010)]),
            },
        );
        assert_eq!(fx, [Effect::LoadAlbum("y".into())]);
        app
    }

    #[test]
    fn albums_arrive_newest_first() {
        let app = dated_app();
        assert_eq!(album_ids(&app), ["y", "x"]);
        assert_eq!(app.library.sel[1], 0);
    }

    #[test]
    fn o_sorts_the_focused_column_and_keeps_the_selection() {
        let mut app = dated_app();
        press(&mut app, 'l');
        press(&mut app, 'j');
        assert_eq!(app.library.tracks_for.as_deref(), Some("x"));
        let fx = press(&mut app, 'o');
        assert_eq!(
            fx,
            [Effect::SaveSorts(Sorts {
                albums: AlbumSort::Oldest,
                ..Sorts::default()
            })]
        );
        assert_eq!(album_ids(&app), ["x", "y"]);
        assert_eq!(app.library.sel[1], 0);
        assert_eq!(app.library.tracks_for.as_deref(), Some("x"));
    }

    #[test]
    fn revealed_album_is_found_after_sorting() {
        let mut app = dated_app();
        app.library.reveal("a", Some("x"));
        api(
            &mut app,
            ApiResult::Artist {
                id: "a".into(),
                result: Ok(vec![dated("x", 2000), dated("y", 2010)]),
            },
        );
        assert_eq!(app.library.sel[1], 1);
        assert_eq!(app.library.tracks_for.as_deref(), Some("x"));
    }

    #[test]
    fn sort_choices_survive_logout_and_login() {
        let mut app = loaded_app();
        press(&mut app, 'o');
        app.show_login("s", "");
        assert_eq!(app.library.artist_sort, ArtistSort::MostAlbums);
        app.view = View::Queue;
        assert_eq!(press(&mut app, 'o'), []);
    }
}
