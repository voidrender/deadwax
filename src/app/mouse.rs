//! The mouse. Each frame, the UI records where it drew the things a click
//! can select and how far each list is scrolled. A left click acts on the
//! topmost area under the pointer, a second click on the same row acts like
//! Enter, and the wheel scrolls the list under the pointer.

use super::library::Column;
use super::login::LoginField;
use super::playlists::PlColumn;
use super::settings::ROWS;
use super::{App, Effect, Popup, Screen, View, keys::Action};
use crate::player::PlayerCommand;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Longest gap between the two clicks of a double click.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows one notch of the wheel scrolls.
pub const WHEEL_ROWS: isize = 3;

/// A list a click can focus or select a row in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum List {
    Library(Column),
    Playlists(PlColumn),
    Search,
    Queue,
    Picker,
    Settings,
}

/// What a click on a recorded area selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Tab(View),
    Help,
    /// The box around a list. Focuses the list and keeps its selection.
    Pane(List),
    /// The visible rows of a list, one per line. `first` is the index of
    /// the top row.
    Rows {
        list: List,
        first: usize,
    },
    SearchInput,
    /// The playback strip, or the close hint in Now Playing.
    NowPlaying,
    /// A progress bar. A click seeks to the same fraction of the track.
    Seek,
    LoginField(LoginField),
}

/// How a list was scrolled when it was last drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Scroll {
    offset: usize,
    sel: usize,
    len: usize,
    height: usize,
}

/// The clickable areas of the last frame, in drawing order, and the scroll
/// position of every list drawn so far.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HitMap {
    areas: Vec<(Rect, Target)>,
    scrolls: HashMap<List, Scroll>,
}

impl HitMap {
    /// Forgets the last frame's areas. Scroll positions stay.
    pub fn clear(&mut self) {
        self.areas.clear();
    }

    pub fn add(&mut self, area: Rect, target: Target) {
        if !area.is_empty() {
            self.areas.push((area, target));
        }
    }

    /// The last area drawn under the cell at `x`, `y`, so a popup wins over
    /// the view behind it.
    pub fn at(&self, x: u16, y: u16) -> Option<(Rect, Target)> {
        self.areas
            .iter()
            .rev()
            .find(|(area, _)| area.contains(Position { x, y }))
            .copied()
    }

    /// The index of the top row to draw for `list`. The list keeps its last
    /// position, and scrolls only as far as it takes to show `sel` after
    /// the selection, the rows or the height change.
    pub fn scroll(&mut self, list: List, sel: usize, len: usize, height: usize) -> usize {
        self.scroll_showing(list, sel, 0, len, height)
    }

    /// Like `scroll`, but scrolling up to the selection also shows the
    /// `above` rows over it, such as a section header.
    pub fn scroll_showing(
        &mut self,
        list: List,
        sel: usize,
        above: usize,
        len: usize,
        height: usize,
    ) -> usize {
        let top = sel.saturating_sub(above.min(height.saturating_sub(1)));
        let last = self.scrolls.get(&list).copied();
        let mut offset = last.map_or(0, |s| s.offset);
        let moved = last.is_none_or(|s| s.sel != sel || s.len != len || s.height != height);
        if moved {
            if top < offset {
                offset = top;
            } else if sel >= offset + height {
                offset = (sel + 1).saturating_sub(height);
            }
        }
        offset = offset.min(len.saturating_sub(height));
        self.scrolls.insert(
            list,
            Scroll {
                offset,
                sel,
                len,
                height,
            },
        );
        offset
    }

    /// Scrolls `list` by `rows` without moving its selection.
    fn scroll_by(&mut self, list: List, rows: isize) {
        if let Some(s) = self.scrolls.get_mut(&list) {
            s.offset = s
                .offset
                .saturating_add_signed(rows)
                .min(s.len.saturating_sub(s.height));
        }
    }
}

impl App {
    pub(super) fn handle_mouse(&mut self, ev: MouseEvent) -> Vec<Effect> {
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.click(ev.column, ev.row),
            MouseEventKind::ScrollDown => {
                self.wheel(ev.column, ev.row, WHEEL_ROWS);
                Vec::new()
            }
            MouseEventKind::ScrollUp => {
                self.wheel(ev.column, ev.row, -WHEEL_ROWS);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn click(&mut self, x: u16, y: u16) -> Vec<Effect> {
        let last_click = self.last_click.take();
        // A click closes the help overlay, as any key that isn't bound does.
        if self.help {
            self.help = false;
            return Vec::new();
        }
        let Some((area, target)) = self.hits.at(x, y) else {
            return Vec::new();
        };
        let row = match target {
            Target::Rows { first, .. } => Some(first + usize::from(y - area.y)),
            _ => None,
        };
        let (selected, mut effects) = if self.popup.is_some() {
            (self.click_popup(target, row), Vec::new())
        } else if self.screen == Screen::Login {
            if let Target::LoginField(field) = target {
                self.login.focus = field;
            }
            (false, Vec::new())
        } else {
            self.click_main(target, row, area, x)
        };
        // A row clicked twice in a row acts like Enter on it.
        if let (true, Target::Rows { list, .. }, Some(row)) = (selected, target, row) {
            let now = Instant::now();
            let double = last_click.is_some_and(|(at, l, r)| {
                l == list && r == row && now.saturating_duration_since(at) <= DOUBLE_CLICK
            });
            if double {
                effects.extend(self.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
            } else {
                self.last_click = Some((now, list, row));
            }
        }
        effects
    }

    /// A click on the main screen. Returns whether it selected a row, and
    /// the effects.
    fn click_main(
        &mut self,
        target: Target,
        row: Option<usize>,
        area: Rect,
        x: u16,
    ) -> (bool, Vec<Effect>) {
        if target != Target::SearchInput {
            self.search.editing = false;
        }
        let effects = match target {
            Target::Tab(view) => self.run_action(Action::ShowView(view)),
            Target::Help => self.run_action(Action::ToggleHelp),
            Target::NowPlaying => self.run_action(Action::ToggleNowPlaying),
            Target::Seek => self.seek_to(area, x).into_iter().collect(),
            Target::SearchInput => {
                self.view = View::Search;
                self.search.editing = true;
                Vec::new()
            }
            Target::Pane(list) | Target::Rows { list, .. } => return self.click_list(list, row),
            Target::LoginField(_) => Vec::new(),
        };
        (false, effects)
    }

    /// Seeks to the fraction of the track that column `x` sits at in the
    /// bar drawn at `bar`.
    fn seek_to(&self, bar: Rect, x: u16) -> Option<Effect> {
        let duration = self.player.track_duration();
        if self.player.current.is_none() || duration <= 0.0 || bar.width == 0 {
            return None;
        }
        let fraction = f64::from(x.saturating_sub(bar.x)) / f64::from(bar.width);
        Some(Effect::Player(PlayerCommand::SeekTo(duration * fraction)))
    }

    /// Focuses `list` in the shown view, and selects `row` in it when the
    /// click landed on one. Returns whether it did, and the effects.
    fn click_list(&mut self, list: List, row: Option<usize>) -> (bool, Vec<Effect>) {
        match list {
            List::Library(col) => self.library.click(col, row),
            List::Playlists(col) => self.playlists.click(col, row),
            List::Search => (self.search.click(row), Vec::new()),
            List::Queue => match row {
                Some(row) if row < self.player.items.len() => {
                    self.queue.select(&self.player.items, row);
                    (true, Vec::new())
                }
                _ => (false, Vec::new()),
            },
            List::Picker | List::Settings => (false, Vec::new()),
        }
    }

    /// Clicks while a popup is open only reach the popup's own rows.
    /// Returns whether one was selected.
    fn click_popup(&mut self, target: Target, row: Option<usize>) -> bool {
        let (Target::Rows { list, .. }, Some(row)) = (target, row) else {
            return false;
        };
        match (list, &mut self.popup) {
            (List::Picker, Some(Popup::PlaylistPicker(picker))) => picker.click(row),
            (List::Settings, Some(Popup::Settings { sel })) if row < ROWS.len() => {
                *sel = row;
                true
            }
            _ => false,
        }
    }

    /// Scrolls the list under the pointer by `rows`. While a popup is open
    /// only its own list scrolls.
    fn wheel(&mut self, x: u16, y: u16, rows: isize) {
        if self.help {
            return;
        }
        let Some((_, Target::Pane(list) | Target::Rows { list, .. })) = self.hits.at(x, y) else {
            return;
        };
        let in_popup = matches!(list, List::Picker | List::Settings);
        if in_popup == self.popup.is_some() {
            self.hits.scroll_by(list, rows);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::library::Loadable;
    use crate::app::{ApiResult, AppEvent};
    use crate::player::PlayerSnapshot;
    use crate::player::queue::QueueItem;
    use crate::subsonic::{Album, Artist, Playlist, Song};
    use crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    const W: u16 = 100;
    const H: u16 = 30;

    fn draw(app: &mut App) -> String {
        let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        crate::ui::testing::buffer_text(term.backend().buffer())
    }

    /// Draws the app, then clicks the first cell showing `text`.
    fn click_text(app: &mut App, text: &str) -> Vec<Effect> {
        let screen = draw(app);
        let (y, line) = screen
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains(text))
            .unwrap_or_else(|| panic!("{text:?} not on screen:\n{screen}"));
        let x = line[..line.find(text).unwrap()].chars().count();
        click(app, u16::try_from(x).unwrap(), u16::try_from(y).unwrap())
    }

    fn click(app: &mut App, x: u16, y: u16) -> Vec<Effect> {
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }))
    }

    fn song(id: &str) -> Song {
        Song {
            id: id.into(),
            title: format!("Song {id}"),
            artist: "Artist".into(),
            duration: 100,
            ..Default::default()
        }
    }

    fn album(id: &str) -> Album {
        Album {
            id: id.into(),
            name: format!("Album {id}"),
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

    fn library_app() -> App {
        let mut app = App::logged_in("s");
        app.on_start();
        app.handle_event(AppEvent::Api(ApiResult::Artists(Ok(vec![
            artist("a"),
            artist("b"),
        ]))));
        app
    }

    #[test]
    fn clicking_a_tab_shows_its_view() {
        let mut app = library_app();
        click_text(&mut app, "Queue");
        assert_eq!(app.view, View::Queue);
        app.now_playing = true;
        click_text(&mut app, "Playlists");
        assert_eq!(app.view, View::Playlists);
        assert!(!app.now_playing);
    }

    #[test]
    fn clicking_the_keys_hint_opens_help_and_any_click_closes_it() {
        let mut app = library_app();
        click_text(&mut app, "? keys");
        assert!(app.help);
        click(&mut app, 0, 0);
        assert!(!app.help);
        assert_eq!(app.view, View::Library);
    }

    #[test]
    fn clicking_an_artist_selects_it_and_loads_its_albums() {
        let mut app = library_app();
        let fx = click_text(&mut app, "Artist b");
        assert_eq!(fx, [Effect::LoadArtist("b".into())]);
        assert_eq!(app.library.focus, Column::Artists);
        assert_eq!(app.library.sel[0], 3);
        // The selected row again changes nothing.
        assert!(click_text(&mut app, "Artist b").is_empty());
    }

    #[test]
    fn clicking_a_track_focuses_its_column() {
        let mut app = library_app();
        click_text(&mut app, "Artist a");
        app.handle_event(AppEvent::Api(ApiResult::Artist {
            id: "a".into(),
            result: Ok(vec![album("x")]),
        }));
        app.handle_event(AppEvent::Api(ApiResult::Album {
            id: "x".into(),
            result: Ok(vec![song("1"), song("2"), song("3")]),
        }));
        click_text(&mut app, "Song 3");
        assert_eq!(app.library.focus, Column::Tracks);
        assert_eq!(app.library.sel[2], 2);
        // A click on a pane's title only moves the focus.
        click_text(&mut app, "Albums ·");
        assert_eq!(app.library.focus, Column::Albums);
        assert_eq!(app.library.sel[2], 2);
    }

    #[test]
    fn clicking_below_the_last_row_only_focuses() {
        let mut app = library_app();
        app.library.focus = Column::Tracks;
        draw(&mut app);
        click(&mut app, 1, 20);
        assert_eq!(app.library.focus, Column::Artists);
        assert_eq!(app.library.sel[0], 0);
    }

    fn queue_app(n: u64) -> App {
        let mut app = App::logged_in("s");
        app.view = View::Queue;
        let items = (0..n)
            .map(|i| QueueItem {
                uid: i,
                song: song(&i.to_string()),
            })
            .collect();
        app.handle_event(AppEvent::Snapshot(PlayerSnapshot {
            items,
            current: Some(0),
            duration: 100.0,
            ..Default::default()
        }));
        app
    }

    /// The screen row showing `text`.
    fn row_of(app: &mut App, text: &str) -> Option<usize> {
        draw(app).lines().position(|l| l.contains(text))
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) -> Vec<Effect> {
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }))
    }

    #[test]
    fn clicking_a_row_in_a_scrolled_list_keeps_it_in_place() {
        let mut app = queue_app(60);
        app.queue.select(&app.player.items, 50);
        let before = row_of(&mut app, "Song 40 ");
        click_text(&mut app, "Song 40 ");
        assert_eq!(app.queue.sel, 40);
        assert_eq!(app.selected_queue_song().map(|s| s.id.as_str()), Some("40"));
        assert_eq!(row_of(&mut app, "Song 40 "), before);
    }

    #[test]
    fn moving_up_from_the_bottom_does_not_scroll() {
        let mut app = queue_app(60);
        app.queue.select(&app.player.items, 50);
        let before = row_of(&mut app, "Song 50 ");
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('k'),
            KeyModifiers::NONE,
        )));
        assert_eq!(row_of(&mut app, "Song 50 "), before);
        // Moving past the top edge scrolls just enough to show the row.
        app.queue.select(&app.player.items, 0);
        assert_eq!(row_of(&mut app, "Song 0 "), Some(2));
    }

    #[test]
    fn the_wheel_scrolls_the_list_and_keeps_the_selection() {
        let mut app = queue_app(60);
        let top = row_of(&mut app, "Song 0 ").unwrap();
        let y = u16::try_from(top).unwrap();
        mouse(&mut app, MouseEventKind::ScrollDown, 5, y);
        assert_eq!(row_of(&mut app, "Song 3 "), Some(top));
        assert_eq!(app.queue.sel, 0);
        mouse(&mut app, MouseEventKind::ScrollUp, 5, y);
        mouse(&mut app, MouseEventKind::ScrollUp, 5, y);
        assert_eq!(row_of(&mut app, "Song 0 "), Some(top));
        // A popup stops the list behind it from scrolling.
        app.popup = Some(Popup::Settings { sel: 0 });
        draw(&mut app);
        mouse(&mut app, MouseEventKind::ScrollDown, 5, y);
        app.popup = None;
        assert_eq!(row_of(&mut app, "Song 0 "), Some(top));
    }

    #[test]
    fn double_clicking_a_row_acts_like_enter() {
        let mut app = queue_app(5);
        assert!(click_text(&mut app, "Song 3 ").is_empty());
        assert_eq!(
            click_text(&mut app, "Song 3 "),
            [Effect::Player(PlayerCommand::JumpTo(3))]
        );
        // A third click starts over.
        assert!(click_text(&mut app, "Song 3 ").is_empty());
    }

    #[test]
    fn slow_or_different_second_clicks_are_single_clicks() {
        let mut app = queue_app(5);
        click_text(&mut app, "Song 3 ");
        app.last_click = app
            .last_click
            .map(|(at, list, row)| (at.checked_sub(DOUBLE_CLICK * 2).unwrap(), list, row));
        assert!(click_text(&mut app, "Song 3 ").is_empty());
        assert!(click_text(&mut app, "Song 2 ").is_empty());
        assert_eq!(app.queue.sel, 2);
    }

    #[test]
    fn double_clicking_a_track_plays_it() {
        let mut app = library_app();
        click_text(&mut app, "Artist a");
        app.handle_event(AppEvent::Api(ApiResult::Artist {
            id: "a".into(),
            result: Ok(vec![album("x")]),
        }));
        let songs = vec![song("1"), song("2")];
        app.handle_event(AppEvent::Api(ApiResult::Album {
            id: "x".into(),
            result: Ok(songs.clone()),
        }));
        click_text(&mut app, "Song 2");
        assert_eq!(
            click_text(&mut app, "Song 2"),
            [Effect::Player(PlayerCommand::PlayQueue { songs, start: 1 })]
        );
    }

    /// The screen row and first column of the progress bar, and its width.
    fn bar(app: &mut App) -> (u16, u16, usize) {
        let screen = draw(app);
        let (y, line) = screen
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains("0:00 ─"))
            .unwrap_or_else(|| panic!("no bar:\n{screen}"));
        let start = line.find("0:00 ─").unwrap() + "0:00 ".len();
        let x = line[..start].chars().count();
        let width = line[start..].chars().take_while(|c| *c == '─').count();
        (u16::try_from(x).unwrap(), u16::try_from(y).unwrap(), width)
    }

    #[test]
    fn clicking_the_progress_bar_seeks() {
        let mut app = queue_app(1);
        let (x, y, width) = bar(&mut app);
        let fx = click(&mut app, x + u16::try_from(width / 2).unwrap(), y);
        let [Effect::Player(PlayerCommand::SeekTo(to))] = fx[..] else {
            panic!("{fx:?}");
        };
        assert!((40.0..=60.0).contains(&to), "{to}");
        assert!(!app.now_playing);

        app.now_playing = true;
        let (x, y, _) = bar(&mut app);
        assert_eq!(
            click(&mut app, x, y),
            [Effect::Player(PlayerCommand::SeekTo(0.0))]
        );
    }

    #[test]
    fn the_strip_opens_now_playing_and_its_hint_closes_it() {
        let mut app = library_app();
        click_text(&mut app, "Nothing playing");
        assert!(app.now_playing);
        let mut app = queue_app(1);
        app.now_playing = true;
        click_text(&mut app, "z close");
        assert!(!app.now_playing);
    }

    #[test]
    fn scroll_shows_the_selection_and_otherwise_stays() {
        let mut hits = HitMap::default();
        let list = List::Queue;
        assert_eq!(hits.scroll(list, 0, 50, 10), 0);
        assert_eq!(hits.scroll(list, 15, 50, 10), 6);
        assert_eq!(hits.scroll(list, 8, 50, 10), 6);
        assert_eq!(hits.scroll(list, 2, 50, 10), 2);
        hits.scroll_by(list, 100);
        assert_eq!(hits.scroll(list, 2, 50, 10), 40);
        // Fewer rows pull the view back so it stays full.
        assert_eq!(hits.scroll(list, 2, 20, 10), 2);
        assert_eq!(hits.scroll(list, 0, 5, 10), 0);
    }

    #[test]
    fn moving_up_to_the_first_result_shows_its_header() {
        let mut app = library_app();
        app.view = View::Search;
        app.search.input = "song".into();
        app.search.seq = 1;
        app.search.apply(
            1,
            Ok(crate::subsonic::SearchResults {
                songs: (0..60).map(|i| song(&i.to_string())).collect(),
                ..Default::default()
            }),
        );
        let header = row_of(&mut app, "Songs").unwrap();
        let press = |app: &mut App, c| {
            app.handle_event(AppEvent::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
            draw(app);
        };
        for _ in 0..50 {
            press(&mut app, 'j');
        }
        assert_eq!(row_of(&mut app, "Songs"), None);
        for _ in 0..50 {
            press(&mut app, 'k');
        }
        assert_eq!(row_of(&mut app, "Songs"), Some(header));
    }

    #[test]
    fn search_input_and_results_take_clicks() {
        let mut app = library_app();
        app.view = View::Search;
        click_text(&mut app, "Press / to search");
        assert!(app.search.editing);
        app.search.input = "song".into();
        app.search.seq = 1;
        app.search.apply(
            1,
            Ok(crate::subsonic::SearchResults {
                songs: vec![song("1"), song("2")],
                ..Default::default()
            }),
        );
        click_text(&mut app, "Song 2");
        assert!(!app.search.editing);
        assert_eq!(app.search.selected_song().map(|s| s.id.as_str()), Some("2"));
        // Headers are not selectable.
        click_text(&mut app, "Songs");
        assert_eq!(app.search.selected_song().map(|s| s.id.as_str()), Some("2"));
    }

    #[test]
    fn popups_take_clicks_on_their_rows_and_ignore_the_rest() {
        let mut app = library_app();
        app.popup = Some(Popup::Settings { sel: 0 });
        click_text(&mut app, "Notifications");
        assert_eq!(app.popup, Some(Popup::Settings { sel: 2 }));
        // The view behind the popup does not react.
        click_text(&mut app, "Queue");
        assert_eq!(app.view, View::Library);

        let playlist = |id: &str| Playlist {
            id: id.into(),
            name: format!("List {id}"),
            ..Default::default()
        };
        app.popup = Some(Popup::PlaylistPicker(crate::app::playlists::PickerState {
            song_ids: vec!["1".into()],
            songs: vec![song("1")],
            lists: Loadable::Loaded(vec![playlist("p"), playlist("q")]),
            sel: 0,
        }));
        click_text(&mut app, "List q");
        let Some(Popup::PlaylistPicker(picker)) = &app.popup else {
            panic!("picker closed");
        };
        assert_eq!(picker.sel, 2);
    }

    #[test]
    fn clicking_a_login_field_focuses_it() {
        let mut app = App::login_screen("", "");
        click_text(&mut app, "Password");
        assert_eq!(app.login.focus, LoginField::Password);
    }

    #[test]
    fn other_buttons_do_nothing() {
        let mut app = library_app();
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 1,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));
        assert_eq!(app.library.sel[0], 0);
    }
}
