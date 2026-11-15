//! The info popup: details and cover for whatever is selected.

use super::library::{ArtistRow, Column, FAVORITES_ID};
use super::playlists::PlColumn;
use super::search::SearchRow;
use super::{App, Effect, Popup, Screen, View};
use crate::subsonic::{Album, Artist, Playlist, Song};
use crossterm::event::{KeyCode, KeyEvent};

/// What the info popup describes. It holds a copy taken when the popup
/// opened.
#[derive(Clone, Debug, PartialEq)]
pub enum Info {
    Song(Song),
    Album(Album),
    Artist(Artist),
    Playlist(Playlist),
}

impl Info {
    pub fn cover_art(&self) -> Option<&str> {
        match self {
            Self::Song(s) => s.cover_art.as_deref(),
            Self::Album(a) => a.cover_art.as_deref(),
            Self::Artist(a) => a.cover_art.as_deref(),
            Self::Playlist(p) => p.cover_art.as_deref(),
        }
    }

    /// The kind of item, for the popup's title.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Song(_) => "Track",
            Self::Album(_) => "Album",
            Self::Artist(_) => "Artist",
            Self::Playlist(_) => "Playlist",
        }
    }
}

impl App {
    /// The item `i` describes: the current track in Now Playing, otherwise
    /// the selection in the focused column of the active view.
    pub fn info_target(&self) -> Option<Info> {
        if self.screen != Screen::Main {
            return None;
        }
        if self.now_playing {
            return self.selected_song().cloned().map(Info::Song);
        }
        match self.view {
            View::Library => {
                let lib = &self.library;
                match lib.focus {
                    Column::Artists => match lib.artists.loaded()?.get(lib.sel[0])? {
                        ArtistRow::Artist(a) => Some(Info::Artist(a.clone())),
                        ArtistRow::Favorites => None,
                    },
                    Column::Albums => lib
                        .albums
                        .loaded()?
                        .get(lib.sel[1])
                        .filter(|a| a.id != FAVORITES_ID)
                        .cloned()
                        .map(Info::Album),
                    Column::Tracks => lib.selected_track().cloned().map(Info::Song),
                }
            }
            View::Search => {
                let search = &self.search;
                if search.editing {
                    return None;
                }
                let results = search.results.loaded()?;
                match search.selected_row()? {
                    SearchRow::Artist(i) => results.artists.get(i).cloned().map(Info::Artist),
                    SearchRow::Album(i) => results.albums.get(i).cloned().map(Info::Album),
                    SearchRow::Song(i) => results.songs.get(i).cloned().map(Info::Song),
                    SearchRow::Header(_) => None,
                }
            }
            View::Playlists => match self.playlists.focus {
                PlColumn::Lists => self.playlists.selected_list().cloned().map(Info::Playlist),
                PlColumn::Tracks => self.playlists.selected_track().cloned().map(Info::Song),
            },
            View::Queue => self.selected_queue_song().cloned().map(Info::Song),
        }
    }

    /// Opens the info popup for the selection, loading its cover if needed.
    pub(super) fn open_info(&mut self) -> Vec<Effect> {
        let Some(info) = self.info_target() else {
            self.set_status("Nothing selected");
            return Vec::new();
        };
        self.help = false;
        let effects = match info.cover_art() {
            Some(id) if self.art.expect_info(id.to_string()) => {
                vec![Effect::LoadCover(id.to_string())]
            }
            Some(_) => Vec::new(),
            None => {
                self.art.clear_info();
                Vec::new()
            }
        };
        self.popup = Some(Popup::Info(Box::new(info)));
        effects
    }

    /// Keys while the info popup is open. Returns the popup to keep open.
    pub(super) fn info_key(&mut self, info: Box<Info>, key: KeyEvent) -> Option<Popup> {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('i' | 'q') => {
                self.art.clear_info();
                None
            }
            _ => Some(Popup::Info(info)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppEvent;
    use crate::app::library::Loadable;
    use crate::player::PlayerSnapshot;
    use crate::player::queue::QueueItem;
    use crate::subsonic::SearchResults;
    use crossterm::event::KeyModifiers;

    fn press(app: &mut App, c: char) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )))
    }

    fn song(id: &str, cover: Option<&str>) -> Song {
        Song {
            id: id.into(),
            title: format!("Song {id}"),
            cover_art: cover.map(Into::into),
            ..Default::default()
        }
    }

    fn cover() -> image::DynamicImage {
        image::RgbImage::from_pixel(8, 8, image::Rgb([10, 20, 30])).into()
    }

    fn shown(app: &App) -> Option<&Info> {
        match &app.popup {
            Some(Popup::Info(info)) => Some(info),
            _ => None,
        }
    }

    #[test]
    fn library_columns_describe_their_selection() {
        let mut app = App::logged_in("s");
        let artist = Artist {
            id: "ar1".into(),
            name: "Aimer".into(),
            ..Default::default()
        };
        app.library.artists = Loadable::Loaded(vec![
            ArtistRow::Favorites,
            ArtistRow::Artist(artist.clone()),
        ]);
        let album = Album {
            id: "al1".into(),
            name: "Penny Rain".into(),
            ..Default::default()
        };
        app.library.albums = Loadable::Loaded(vec![album.clone()]);
        app.library.tracks = Loadable::Loaded(vec![song("s1", None)]);

        // The Favorites row has nothing to describe.
        assert_eq!(app.info_target(), None);
        app.library.sel[0] = 1;
        assert_eq!(app.info_target(), Some(Info::Artist(artist)));
        app.library.focus = Column::Albums;
        assert_eq!(app.info_target(), Some(Info::Album(album)));
        app.library.focus = Column::Tracks;
        assert_eq!(app.info_target(), Some(Info::Song(song("s1", None))));
    }

    #[test]
    fn search_rows_and_playlists_describe_their_selection() {
        let mut app = App::logged_in("s");
        app.view = View::Search;
        app.search.results = Loadable::Loaded(SearchResults {
            songs: vec![song("s1", None)],
            ..Default::default()
        });
        // Row 0 is the "Songs" header.
        app.search.sel = 1;
        assert_eq!(app.info_target(), Some(Info::Song(song("s1", None))));
        app.search.editing = true;
        assert_eq!(app.info_target(), None);

        app.view = View::Playlists;
        let list = Playlist {
            id: "pl1".into(),
            name: "Night Drive".into(),
            ..Default::default()
        };
        app.playlists.lists = Loadable::Loaded(vec![list.clone()]);
        assert_eq!(app.info_target(), Some(Info::Playlist(list)));
    }

    #[test]
    fn i_on_a_playlist_opens_info_instead_of_renaming() {
        let mut app = App::logged_in("s");
        app.view = View::Playlists;
        app.playlists.lists = Loadable::Loaded(vec![Playlist {
            id: "pl1".into(),
            name: "Night Drive".into(),
            ..Default::default()
        }]);
        press(&mut app, 'i');
        assert!(
            matches!(shown(&app), Some(Info::Playlist(_))),
            "{:?}",
            app.popup
        );
    }

    #[test]
    fn opening_loads_the_cover_and_closing_forgets_it() {
        let mut app = App::logged_in("s");
        app.view = View::Queue;
        app.player = PlayerSnapshot {
            items: vec![QueueItem {
                uid: 1,
                song: song("s1", Some("al-9")),
            }],
            ..Default::default()
        };
        assert_eq!(press(&mut app, 'i'), [Effect::LoadCover("al-9".into())]);
        app.handle_event(AppEvent::CoverReady {
            cover_id: "al-9".into(),
            image: cover(),
        });
        assert!(app.art.has_info_image());
        // The current track wanted no cover, so it stays without one.
        assert!(!app.art.has_image());

        // Keys other than the close keys leave it open.
        press(&mut app, 'j');
        assert!(shown(&app).is_some());
        press(&mut app, 'i');
        assert!(app.popup.is_none());
        assert!(!app.art.has_info_image());
        assert_eq!(app.art.info_cover_id, None);
    }

    #[test]
    fn the_current_cover_is_reused_without_a_load() {
        let mut app = App::logged_in("s");
        app.player = PlayerSnapshot {
            items: vec![QueueItem {
                uid: 1,
                song: song("s1", Some("al-1")),
            }],
            current: Some(0),
            ..Default::default()
        };
        assert_eq!(app.sync_cover(), [Effect::LoadCover("al-1".into())]);
        app.handle_event(AppEvent::CoverReady {
            cover_id: "al-1".into(),
            image: cover(),
        });
        app.now_playing = true;
        assert_eq!(press(&mut app, 'i'), []);
        assert!(app.art.has_info_image());
        assert!(matches!(shown(&app), Some(Info::Song(s)) if s.id == "s1"));
    }

    #[test]
    fn nothing_selected_says_so() {
        let mut app = App::logged_in("s");
        app.view = View::Queue;
        assert_eq!(press(&mut app, 'i'), []);
        assert!(app.popup.is_none());
        assert_eq!(app.status_text(), Some("Nothing selected"));
    }
}
