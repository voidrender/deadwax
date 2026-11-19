use super::library::Loadable;
use super::{ApiResult, App, Effect, View};
use crate::player::PlayerCommand;
use crate::subsonic::{SearchResults, Song};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Results requested per group (artists, albums, songs).
pub const SEARCH_LIMIT: u32 = 20;

/// One line of the flattened results list. Indexes point into `SearchResults`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchRow {
    Header(&'static str),
    Artist(usize),
    Album(usize),
    Song(usize),
}

impl SearchRow {
    fn selectable(self) -> bool {
        !matches!(self, Self::Header(_))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchState {
    pub input: String,
    pub editing: bool,
    /// Bumped on every edit. Results for an older value are dropped.
    pub seq: u64,
    pub results: Loadable<SearchResults>,
    /// Index into `rows()`. Always on a selectable row when there is one.
    pub sel: usize,
}

impl SearchState {
    /// The results flattened into Artists, Albums and Songs sections.
    /// Empty sections get no header.
    pub fn rows(&self) -> Vec<SearchRow> {
        let Some(r) = self.results.loaded() else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        if !r.artists.is_empty() {
            rows.push(SearchRow::Header("Artists"));
            rows.extend((0..r.artists.len()).map(SearchRow::Artist));
        }
        if !r.albums.is_empty() {
            rows.push(SearchRow::Header("Albums"));
            rows.extend((0..r.albums.len()).map(SearchRow::Album));
        }
        if !r.songs.is_empty() {
            rows.push(SearchRow::Header("Songs"));
            rows.extend((0..r.songs.len()).map(SearchRow::Song));
        }
        rows
    }

    pub fn selected_row(&self) -> Option<SearchRow> {
        self.rows()
            .get(self.sel)
            .copied()
            .filter(|r| r.selectable())
    }

    /// The song `f`, `e` and `p` act on. `None` while typing.
    pub fn selected_song(&self) -> Option<&Song> {
        if self.editing {
            return None;
        }
        match self.selected_row()? {
            SearchRow::Song(i) => self.results.loaded()?.songs.get(i),
            _ => None,
        }
    }

    pub fn set_starred(&mut self, song_id: &str, starred: bool) {
        if let Loadable::Loaded(r) = &mut self.results {
            for s in r.songs.iter_mut().filter(|s| s.id == song_id) {
                s.starred = starred;
            }
        }
    }

    fn snap_selection(&mut self) {
        let rows = self.rows();
        if rows.get(self.sel).is_some_and(|r| r.selectable()) {
            return;
        }
        self.sel = rows.iter().position(|r| r.selectable()).unwrap_or(0);
    }

    /// Applies a result if it answers the latest query.
    pub fn apply(&mut self, seq: u64, result: Result<SearchResults, String>) {
        if seq != self.seq || self.input.trim().is_empty() {
            return;
        }
        match result {
            Ok(r) => {
                self.results = Loadable::Loaded(r);
                self.sel = 0;
                self.snap_selection();
            }
            Err(msg) => self.results = Loadable::Failed(msg),
        }
    }

    /// Bumps the sequence after an edit and returns the search to run.
    fn changed(&mut self) -> Vec<Effect> {
        self.seq += 1;
        let query = self.input.trim();
        if query.is_empty() {
            self.results = Loadable::NotLoaded;
            self.sel = 0;
            return Vec::new();
        }
        if !matches!(self.results, Loadable::Loaded(_)) {
            self.results = Loadable::Loading;
        }
        vec![Effect::Search {
            query: query.to_string(),
            seq: self.seq,
        }]
    }

    /// Keys while the input has focus. Everything but the few exit keys types.
    pub(super) fn handle_edit_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                vec![Effect::Quit]
            } else {
                Vec::new()
            };
        }
        match key.code {
            KeyCode::Esc | KeyCode::Enter => {
                self.editing = false;
                Vec::new()
            }
            KeyCode::Down => {
                self.editing = false;
                self.snap_selection();
                Vec::new()
            }
            KeyCode::Backspace => {
                if self.input.pop().is_some() {
                    self.changed()
                } else {
                    Vec::new()
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.input.push(c);
                self.changed()
            }
            _ => Vec::new(),
        }
    }

    /// A click on the results. Stops typing, and selects `row` when it is
    /// a result rather than a header. Returns whether it did.
    pub(super) fn click(&mut self, row: Option<usize>) -> bool {
        self.editing = false;
        match row {
            Some(row) if self.rows().get(row).is_some_and(|r| r.selectable()) => {
                self.sel = row;
                true
            }
            _ => {
                self.snap_selection();
                false
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let rows = self.rows();
        let mut i = self.sel;
        loop {
            let Some(next) = i.checked_add_signed(delta).filter(|n| *n < rows.len()) else {
                return;
            };
            i = next;
            if rows[i].selectable() {
                self.sel = i;
                return;
            }
        }
    }
}

impl App {
    pub(super) fn handle_search_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if self.search.editing {
            return self.search.handle_edit_key(key);
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Vec::new();
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.search.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.search.move_selection(-1),
            KeyCode::Enter => return self.search_enter(),
            KeyCode::Char('e') => return self.search_queue(false),
            KeyCode::Char('p') => return self.search_queue(true),
            _ => {}
        }
        Vec::new()
    }

    fn search_enter(&mut self) -> Vec<Effect> {
        let Some(row) = self.search.selected_row() else {
            return Vec::new();
        };
        let Some(results) = self.search.results.loaded() else {
            return Vec::new();
        };
        match row {
            SearchRow::Header(_) => Vec::new(),
            SearchRow::Artist(i) => {
                let Some(id) = results.artists.get(i).map(|a| a.id.clone()) else {
                    return Vec::new();
                };
                self.view = View::Library;
                self.library.reveal(&id, None)
            }
            SearchRow::Album(i) => {
                let Some(album) = results.albums.get(i) else {
                    return Vec::new();
                };
                let Some(artist_id) = album.artist_id.clone() else {
                    self.set_status("Can't open this album");
                    return Vec::new();
                };
                let album_id = album.id.clone();
                self.view = View::Library;
                self.library.reveal(&artist_id, Some(&album_id))
            }
            SearchRow::Song(i) => {
                if i >= results.songs.len() {
                    return Vec::new();
                }
                vec![Effect::Player(PlayerCommand::PlayQueue {
                    songs: results.songs.clone(),
                    start: i,
                })]
            }
        }
    }

    fn search_queue(&self, next: bool) -> Vec<Effect> {
        let Some(song) = self.search.selected_song() else {
            return Vec::new();
        };
        let songs = vec![song.clone()];
        vec![Effect::Player(if next {
            PlayerCommand::PlayNext(songs)
        } else {
            PlayerCommand::Append(songs)
        })]
    }

    pub(super) fn handle_search_result(&mut self, result: ApiResult) {
        if let ApiResult::Search { seq, result } = result {
            self.search.apply(seq, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppEvent;
    use crate::app::library::ArtistRow;
    use crate::subsonic::{Album, Artist};

    fn key(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn press(app: &mut App, c: char) -> Vec<Effect> {
        key(app, KeyCode::Char(c))
    }

    fn api(app: &mut App, r: ApiResult) -> Vec<Effect> {
        app.handle_event(AppEvent::Api(r))
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

    fn artist(id: &str) -> Artist {
        Artist {
            id: id.into(),
            name: format!("Artist {id}"),
            album_count: 1,
            cover_art: None,
        }
    }

    fn album(id: &str, artist_id: Option<&str>) -> Album {
        Album {
            id: id.into(),
            name: format!("Album {id}"),
            artist: "A".into(),
            artist_id: artist_id.map(str::to_string),
            year: None,
            song_count: 0,
            duration: 0,
            cover_art: None,
            music_brainz_id: None,
            ..Default::default()
        }
    }

    fn results() -> SearchResults {
        SearchResults {
            artists: vec![artist("a1")],
            albums: vec![album("b1", Some("a1")), album("b2", None)],
            songs: vec![song("s1"), song("s2")],
        }
    }

    /// An app on the Search view with results loaded and the input unfocused.
    fn app_with_results() -> App {
        let mut app = App::new();
        app.view = View::Search;
        app.search.input = "x".into();
        app.search.seq = 1;
        api(
            &mut app,
            ApiResult::Search {
                seq: 1,
                result: Ok(results()),
            },
        );
        app
    }

    #[test]
    fn slash_focuses_the_input_even_with_help_open() {
        let mut app = App::new();
        app.help = true;
        app.now_playing = true;
        press(&mut app, '/');
        assert_eq!(app.view, View::Search);
        assert!(app.search.editing && !app.help && !app.now_playing);
    }

    #[test]
    fn typing_bumps_seq_and_searches() {
        let mut app = App::new();
        press(&mut app, '/');
        assert_eq!(
            press(&mut app, 'a'),
            [Effect::Search {
                query: "a".into(),
                seq: 1
            }]
        );
        assert_eq!(
            press(&mut app, 'b'),
            [Effect::Search {
                query: "ab".into(),
                seq: 2
            }]
        );
        assert_eq!(
            key(&mut app, KeyCode::Backspace),
            [Effect::Search {
                query: "a".into(),
                seq: 3
            }]
        );
        assert_eq!(app.search.seq, 3);
    }

    #[test]
    fn empty_query_clears_without_a_request() {
        let mut app = app_with_results();
        app.search.editing = true;
        assert!(key(&mut app, KeyCode::Backspace).is_empty());
        assert_eq!(app.search.results, Loadable::NotLoaded);
        assert_eq!(app.search.seq, 2);
        // The cleared query's late answer must not resurrect results.
        api(
            &mut app,
            ApiResult::Search {
                seq: 1,
                result: Ok(results()),
            },
        );
        assert_eq!(app.search.results, Loadable::NotLoaded);
    }

    #[test]
    fn stale_search_result_ignored() {
        let mut app = App::new();
        press(&mut app, '/');
        press(&mut app, 'a');
        press(&mut app, 'b');
        api(
            &mut app,
            ApiResult::Search {
                seq: 1,
                result: Ok(results()),
            },
        );
        assert_eq!(app.search.results, Loadable::Loading);
        api(
            &mut app,
            ApiResult::Search {
                seq: 2,
                result: Ok(results()),
            },
        );
        assert_eq!(app.search.results, Loadable::Loaded(results()));
    }

    #[test]
    fn keys_go_to_input_while_editing() {
        let mut app = App::new();
        press(&mut app, '/');
        for c in "q1 zX".chars() {
            let fx = press(&mut app, c);
            assert!(!fx.contains(&Effect::Quit), "{c:?}");
        }
        assert_eq!(app.search.input, "q1 zX");
        assert_eq!(app.view, View::Search);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.handle_event(AppEvent::Key(ctrl_c)), [Effect::Quit]);
    }

    #[test]
    fn esc_enter_and_down_leave_the_input() {
        for code in [KeyCode::Esc, KeyCode::Enter, KeyCode::Down] {
            let mut app = App::new();
            press(&mut app, '/');
            assert!(key(&mut app, code).is_empty());
            assert!(!app.search.editing, "{code:?}");
        }
    }

    #[test]
    fn selection_skips_headers() {
        let mut app = app_with_results();
        // rows: H, Artist, H, Album, Album, H, Song, Song
        assert_eq!(app.search.sel, 1);
        press(&mut app, 'j');
        assert_eq!(app.search.sel, 3);
        press(&mut app, 'j');
        press(&mut app, 'j');
        assert_eq!(app.search.sel, 6);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.search.sel, 7);
        press(&mut app, 'k');
        press(&mut app, 'k');
        assert_eq!(app.search.sel, 4);
        for _ in 0..10 {
            press(&mut app, 'k');
        }
        assert_eq!(app.search.sel, 1);
    }

    #[test]
    fn enter_on_artist_reveals_in_library() {
        let mut app = app_with_results();
        app.library.artists = Loadable::Loaded(vec![
            ArtistRow::Favorites,
            ArtistRow::AllAlbums,
            ArtistRow::Artist(artist("a1")),
        ]);
        assert_eq!(
            key(&mut app, KeyCode::Enter),
            [Effect::LoadArtist("a1".into())]
        );
        assert_eq!(app.view, View::Library);
        assert_eq!(app.library.sel[0], 2);
    }

    #[test]
    fn enter_on_album_reveals_in_library() {
        let mut app = app_with_results();
        app.library.artists = Loadable::Loaded(vec![
            ArtistRow::Favorites,
            ArtistRow::AllAlbums,
            ArtistRow::Artist(artist("a1")),
        ]);
        press(&mut app, 'j');
        let fx = key(&mut app, KeyCode::Enter);
        assert_eq!(fx, [Effect::LoadArtist("a1".into())]);
        assert_eq!(app.view, View::Library);
        let fx = api(
            &mut app,
            ApiResult::Artist {
                id: "a1".into(),
                result: Ok(vec![album("b0", None), album("b1", Some("a1"))]),
            },
        );
        assert_eq!(fx, [Effect::LoadAlbum("b1".into())]);
    }

    #[test]
    fn enter_while_artists_load_reveals_once_they_arrive() {
        let mut app = app_with_results();
        app.library.artists = Loadable::Loading;
        press(&mut app, 'j');
        assert!(key(&mut app, KeyCode::Enter).is_empty());
        assert_eq!(app.view, View::Library);
        let fx = api(
            &mut app,
            ApiResult::Artists(Ok(vec![artist("a0"), artist("a1")])),
        );
        assert_eq!(fx, [Effect::LoadArtist("a1".into())]);
        let rows = app.library.artists.loaded().unwrap();
        assert_eq!(rows[app.library.sel[0]].id(), "a1");
    }

    #[test]
    fn enter_on_album_without_artist_stays_with_a_status() {
        let mut app = app_with_results();
        press(&mut app, 'j');
        press(&mut app, 'j');
        assert!(key(&mut app, KeyCode::Enter).is_empty());
        assert_eq!(app.view, View::Search);
        assert_eq!(app.status_text(), Some("Can't open this album"));
    }

    #[test]
    fn enter_on_song_plays_results() {
        let mut app = app_with_results();
        for _ in 0..4 {
            press(&mut app, 'j');
        }
        let fx = key(&mut app, KeyCode::Enter);
        assert_eq!(
            fx,
            [Effect::Player(PlayerCommand::PlayQueue {
                songs: results().songs,
                start: 1
            })]
        );
    }

    #[test]
    fn e_p_and_f_work_on_songs() {
        let mut app = app_with_results();
        assert!(press(&mut app, 'e').is_empty());
        for _ in 0..3 {
            press(&mut app, 'j');
        }
        assert_eq!(
            press(&mut app, 'e'),
            [Effect::Player(PlayerCommand::Append(vec![song("s1")]))]
        );
        assert_eq!(
            press(&mut app, 'p'),
            [Effect::Player(PlayerCommand::PlayNext(vec![song("s1")]))]
        );
        assert_eq!(
            press(&mut app, 'f'),
            [
                Effect::ToggleStar {
                    song_id: "s1".into(),
                    starred: true,
                },
                Effect::Player(crate::player::PlayerCommand::SetStarred {
                    song_id: "s1".into(),
                    starred: true,
                }),
            ]
        );
        let loaded = app.search.results.loaded().unwrap();
        assert!(loaded.songs[0].starred);
    }

    #[test]
    fn failed_star_reverts_in_search_results() {
        let mut app = app_with_results();
        for _ in 0..3 {
            press(&mut app, 'j');
        }
        press(&mut app, 'f');
        api(
            &mut app,
            ApiResult::Starred {
                song_id: "s1".into(),
                starred: true,
                result: Err("no".into()),
            },
        );
        assert!(!app.search.results.loaded().unwrap().songs[0].starred);
    }

    #[test]
    fn logout_clears_search() {
        let mut app = app_with_results();
        app.show_login("s", "");
        assert_eq!(app.search, SearchState::default());
    }
}
