use super::library::{Column, Loadable, position};
use super::sort::{self, PlaylistSort};
use super::{ApiResult, App, Effect, Popup, Screen, View};
use crate::player::PlayerCommand;
use crate::subsonic::{Playlist, Song};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlColumn {
    #[default]
    Lists,
    Tracks,
}

impl PlColumn {
    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaylistsState {
    pub lists: Loadable<Vec<Playlist>>,
    pub tracks: Loadable<Vec<Song>>,
    pub tracks_for: Option<String>,
    pub focus: PlColumn,
    pub sel: [usize; 2],
    /// Mutation effects sent whose result has not arrived yet. Reloads of
    /// the shown playlist are held back until this is zero, so they cannot
    /// undo newer optimistic edits.
    pending: usize,
    pub list_sort: PlaylistSort,
    /// Server order of the loaded playlists.
    list_ranks: HashMap<String, usize>,
}

/// State of the add-to-playlist popup. Row 0 is "+ New playlist" and row
/// `n` is playlist `n - 1`.
#[derive(Clone, Debug, PartialEq)]
pub struct PickerState {
    pub song_ids: Vec<String>,
    /// The same songs, so an add to the shown playlist can update it locally.
    pub songs: Vec<Song>,
    pub lists: Loadable<Vec<Playlist>>,
    pub sel: usize,
}

impl PickerState {
    fn last_row(&self) -> usize {
        self.lists.loaded().map_or(0, Vec::len)
    }
}

/// What a submitted text input is for.
#[derive(Clone, Debug, PartialEq)]
pub enum TextPurpose {
    NewPlaylist { song_ids: Vec<String> },
    RenamePlaylist { id: String },
}

pub fn songs_label(n: usize) -> String {
    format!("{n} {}", if n == 1 { "song" } else { "songs" })
}

impl PlaylistsState {
    /// Starts loading the playlists the first time they are needed (or
    /// retries after a failure).
    pub fn start(&mut self) -> Vec<Effect> {
        if matches!(self.lists, Loadable::NotLoaded | Loadable::Failed(_)) {
            self.lists = Loadable::Loading;
            return vec![Effect::LoadPlaylists];
        }
        Vec::new()
    }

    fn selected_list(&self) -> Option<&Playlist> {
        self.lists.loaded()?.get(self.sel[0])
    }

    pub fn selected_track(&self) -> Option<&Song> {
        self.tracks.loaded()?.get(self.sel[1])
    }

    /// The song `f` and `a` act on: the selected track while the tracks
    /// column has focus.
    pub fn focused_song(&self) -> Option<&Song> {
        (self.focus == PlColumn::Tracks)
            .then(|| self.selected_track())
            .flatten()
    }

    pub fn set_starred(&mut self, song_id: &str, starred: bool) {
        if let Loadable::Loaded(songs) = &mut self.tracks {
            for s in songs.iter_mut().filter(|s| s.id == song_id) {
                s.starred = starred;
            }
        }
    }

    /// Moves the playlist list to its next sort order, keeping the
    /// selected playlist selected.
    pub fn cycle_sort(&mut self) {
        self.list_sort = self.list_sort.next();
        if let Loadable::Loaded(lists) = &mut self.lists {
            let keep = lists.get(self.sel[0]).map(|p| p.id.clone());
            sort::sort_playlists(lists, self.list_sort, &self.list_ranks);
            self.sel[0] = position(lists, keep.as_deref(), |p| &p.id);
        }
    }

    fn len(&self, col: PlColumn) -> usize {
        match col {
            PlColumn::Lists => self.lists.loaded().map_or(0, Vec::len),
            PlColumn::Tracks => self.tracks.loaded().map_or(0, Vec::len),
        }
    }

    fn select_list(&mut self) -> Vec<Effect> {
        self.tracks = Loadable::NotLoaded;
        self.tracks_for = None;
        self.sel[1] = 0;
        let Some(id) = self.selected_list().map(|p| p.id.clone()) else {
            return Vec::new();
        };
        self.tracks_for = Some(id.clone());
        self.tracks = Loadable::Loading;
        vec![Effect::LoadPlaylist(id)]
    }

    /// Applies the playlist list. `Err` carries a load failure to show.
    fn apply_lists(
        &mut self,
        result: Result<Vec<Playlist>, String>,
    ) -> Result<Vec<Effect>, String> {
        if self.lists == Loadable::NotLoaded {
            return Ok(Vec::new());
        }
        match result {
            Ok(mut lists) => {
                let first_load = self.lists.loaded().is_none();
                let previous = self.selected_list().map(|p| p.id.clone());
                self.list_ranks = sort::rank(lists.iter().map(|p| p.id.as_str()));
                sort::sort_playlists(&mut lists, self.list_sort, &self.list_ranks);
                self.lists = Loadable::Loaded(lists);
                let len = self.len(PlColumn::Lists);
                self.sel[0] = self.sel[0].min(len.saturating_sub(1));
                if let Some(prev) = previous
                    && let Some(i) = self
                        .lists
                        .loaded()
                        .and_then(|l| l.iter().position(|p| p.id == prev))
                {
                    self.sel[0] = i;
                }
                let now = self.selected_list().map(|p| p.id.clone());
                if first_load || now != self.tracks_for {
                    Ok(self.select_list())
                } else {
                    Ok(Vec::new())
                }
            }
            Err(msg) => {
                // A failed refresh keeps the list that is already shown.
                if self.lists.loaded().is_none() {
                    self.lists = Loadable::Failed(msg.clone());
                }
                Err(msg)
            }
        }
    }

    fn apply_tracks(&mut self, id: &str, result: Result<Vec<Song>, String>) -> Result<(), String> {
        if self.tracks != Loadable::Loading || self.tracks_for.as_deref() != Some(id) {
            return Ok(());
        }
        match result {
            Ok(songs) => {
                self.tracks = Loadable::Loaded(songs);
                Ok(())
            }
            Err(msg) => {
                self.tracks = Loadable::Failed(msg.clone());
                Err(msg)
            }
        }
    }

    /// Applies the server's copy of a playlist after the worker changed it.
    /// Skipped while local edits are still in flight.
    fn apply_reload(&mut self, id: &str, result: Result<Vec<Song>, String>) -> Result<(), String> {
        if self.pending > 0 || self.tracks_for.as_deref() != Some(id) {
            return Ok(());
        }
        match result {
            Ok(songs) => {
                self.sel[1] = self.sel[1].min(songs.len().saturating_sub(1));
                self.tracks = Loadable::Loaded(songs);
                Ok(())
            }
            Err(msg) => {
                // Edits stay refused until a retry loads the playlist.
                self.tracks = Loadable::Failed(msg.clone());
                Err(msg)
            }
        }
    }

    /// After a failed edit the local copy of `id` is no longer trusted. It
    /// shows as loading, and edits are refused, until the reload lands.
    fn mark_refreshing(&mut self, id: Option<&str>) {
        if let Some(id) = id
            && self.tracks_for.as_deref() == Some(id)
            && self.tracks.loaded().is_some()
        {
            self.tracks = Loadable::Loading;
        }
    }

    /// Appends songs the server is about to receive, when `id` is shown.
    fn append_local(&mut self, id: &str, added: &[Song]) {
        if self.tracks_for.as_deref() == Some(id)
            && let Loadable::Loaded(songs) = &mut self.tracks
        {
            songs.extend_from_slice(added);
        }
        if let Loadable::Loaded(lists) = &mut self.lists
            && let Some(p) = lists.iter_mut().find(|p| p.id == id)
        {
            p.song_count = p
                .song_count
                .saturating_add(u32::try_from(added.len()).unwrap_or(u32::MAX));
        }
    }

    /// Counts the mutations among freshly returned effects.
    fn note_effects(&mut self, effects: &[Effect]) {
        self.pending += effects.iter().filter(|e| e.is_playlist_mutation()).count();
    }

    /// Removes the selected track locally and returns the server edit.
    fn remove_selected(&mut self) -> Option<Effect> {
        let id = self.tracks_for.clone()?;
        let index = self.sel[1];
        let Loadable::Loaded(songs) = &mut self.tracks else {
            return None;
        };
        if index >= songs.len() {
            return None;
        }
        songs.remove(index);
        self.sel[1] = index.min(songs.len().saturating_sub(1));
        if let Loadable::Loaded(lists) = &mut self.lists
            && let Some(p) = lists.iter_mut().find(|p| p.id == id)
        {
            p.song_count = p.song_count.saturating_sub(1);
        }
        Some(Effect::RemoveFromPlaylist { id, index })
    }

    /// Moves the selected track one place and returns the server edit.
    fn move_selected(&mut self, delta: isize) -> Option<Effect> {
        let id = self.tracks_for.clone()?;
        let from = self.sel[1];
        let Loadable::Loaded(songs) = &mut self.tracks else {
            return None;
        };
        let to = from
            .checked_add_signed(delta)
            .filter(|n| *n < songs.len())?;
        if from >= songs.len() {
            return None;
        }
        songs.swap(from, to);
        self.sel[1] = to;
        Some(Effect::ReplacePlaylistSongs {
            id,
            song_ids: songs.iter().map(|s| s.id.clone()).collect(),
        })
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
            PlColumn::Lists => self.select_list(),
            PlColumn::Tracks => Vec::new(),
        }
    }

    /// Moves focus right, or retries a failed load.
    fn drill_in(&mut self) -> Vec<Effect> {
        if matches!(self.lists, Loadable::Failed(_)) {
            self.lists = Loadable::NotLoaded;
            return self.start();
        }
        if matches!(self.tracks, Loadable::Failed(_)) {
            return self.select_list();
        }
        if self.focus == PlColumn::Lists && self.len(PlColumn::Lists) > 0 {
            self.focus = PlColumn::Tracks;
        }
        Vec::new()
    }

    fn enter(&mut self) -> Vec<Effect> {
        let start = match self.focus {
            PlColumn::Lists => 0,
            PlColumn::Tracks => self.sel[1],
        };
        match self.tracks.loaded() {
            Some(songs) if start < songs.len() => {
                vec![Effect::Player(PlayerCommand::PlayQueue {
                    songs: songs.clone(),
                    start,
                })]
            }
            _ => self.drill_in(),
        }
    }

    /// `e` (append) and `p` (play next) on the focused track or playlist.
    fn queue(&self, next: bool) -> Vec<Effect> {
        let songs: Vec<Song> = match self.focus {
            PlColumn::Tracks => self.selected_track().cloned().into_iter().collect(),
            PlColumn::Lists => self.tracks.loaded().cloned().unwrap_or_default(),
        };
        if songs.is_empty() {
            return Vec::new();
        }
        vec![Effect::Player(if next {
            PlayerCommand::PlayNext(songs)
        } else {
            PlayerCommand::Append(songs)
        })]
    }
}

impl App {
    /// Song ids the global `a` key would add. Views extend this.
    pub fn add_to_playlist_targets(&self) -> Vec<String> {
        self.add_to_playlist_songs()
            .into_iter()
            .map(|s| s.id)
            .collect()
    }

    fn add_to_playlist_songs(&self) -> Vec<Song> {
        if self.screen != Screen::Main || self.now_playing {
            return Vec::new();
        }
        let one = |s: Option<&Song>| s.cloned().into_iter().collect();
        let all = |t: Option<&Vec<Song>>| t.cloned().unwrap_or_default();
        match self.view {
            View::Library => match self.library.focus {
                Column::Tracks => one(self.library.selected_track()),
                Column::Albums => all(self.library.tracks.loaded()),
                Column::Artists => Vec::new(),
            },
            View::Search => one(self.search.selected_song()),
            View::Playlists => match self.playlists.focus {
                PlColumn::Tracks => one(self.playlists.selected_track()),
                PlColumn::Lists => all(self.playlists.tracks.loaded()),
            },
            View::Queue => one(self.selected_queue_song()),
        }
    }

    /// Opens the picker for the current target, or says there is none.
    pub(super) fn open_picker(&mut self) -> Vec<Effect> {
        let songs = self.add_to_playlist_songs();
        if songs.is_empty() {
            self.set_status("Nothing to add");
            return Vec::new();
        }
        let effects = self.playlists.start();
        let song_ids = songs.iter().map(|s| s.id.clone()).collect();
        self.popup = Some(Popup::PlaylistPicker(PickerState {
            song_ids,
            songs,
            lists: self.playlists.lists.clone(),
            sel: 0,
        }));
        effects
    }

    pub(super) fn handle_playlists_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Vec::new();
        }
        if matches!(key.code, KeyCode::Char('d' | 'J' | 'K'))
            && self.playlists.focus == PlColumn::Tracks
            && self.playlists.tracks == Loadable::Loading
        {
            self.set_status("Playlist is refreshing");
            return Vec::new();
        }
        let pl = &mut self.playlists;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => pl.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => pl.move_selection(-1),
            KeyCode::Char('h') | KeyCode::Left => {
                pl.focus = PlColumn::Lists;
                Vec::new()
            }
            KeyCode::Char('l') | KeyCode::Right => pl.drill_in(),
            KeyCode::Enter => pl.enter(),
            KeyCode::Char('e') => pl.queue(false),
            KeyCode::Char('p') => pl.queue(true),
            KeyCode::Char('d') if pl.focus == PlColumn::Tracks => {
                pl.remove_selected().into_iter().collect()
            }
            KeyCode::Char('J') if pl.focus == PlColumn::Tracks => {
                pl.move_selected(1).into_iter().collect()
            }
            KeyCode::Char('K') if pl.focus == PlColumn::Tracks => {
                pl.move_selected(-1).into_iter().collect()
            }
            KeyCode::Char('n') => {
                self.popup = Some(Popup::TextInput {
                    title: "New playlist".into(),
                    value: String::new(),
                    on_submit: TextPurpose::NewPlaylist {
                        song_ids: Vec::new(),
                    },
                });
                Vec::new()
            }
            KeyCode::Char('E') => {
                if let Some(p) = pl.selected_list() {
                    self.popup = Some(Popup::TextInput {
                        title: "Rename playlist".into(),
                        value: p.name.clone(),
                        on_submit: TextPurpose::RenamePlaylist { id: p.id.clone() },
                    });
                }
                Vec::new()
            }
            KeyCode::Char('D') => {
                if let Some(p) = pl.selected_list() {
                    self.popup = Some(Popup::Confirm {
                        message: format!("Delete playlist \"{}\"?", p.name),
                        on_yes: Box::new(Effect::DeletePlaylist(p.id.clone())),
                    });
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Keys for the text input popup. Returns the popup to keep open.
    pub(super) fn text_input_key(
        &mut self,
        title: String,
        mut value: String,
        on_submit: TextPurpose,
        key: KeyEvent,
    ) -> (Option<Popup>, Vec<Effect>) {
        let keep = |title, value, on_submit| {
            Some(Popup::TextInput {
                title,
                value,
                on_submit,
            })
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let effects = if key.code == KeyCode::Char('c') {
                vec![Effect::Quit]
            } else {
                Vec::new()
            };
            return (keep(title, value, on_submit), effects);
        }
        match key.code {
            KeyCode::Esc => (None, Vec::new()),
            KeyCode::Enter => {
                let name = value.trim().to_string();
                if name.is_empty() {
                    self.set_status("Enter a playlist name");
                    return (keep(title, value, on_submit), Vec::new());
                }
                let effect = match on_submit {
                    TextPurpose::NewPlaylist { song_ids } => {
                        Effect::CreatePlaylist { name, song_ids }
                    }
                    TextPurpose::RenamePlaylist { id } => Effect::RenamePlaylist { id, name },
                };
                (None, vec![effect])
            }
            KeyCode::Backspace => {
                value.pop();
                (keep(title, value, on_submit), Vec::new())
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                value.push(c);
                (keep(title, value, on_submit), Vec::new())
            }
            _ => (keep(title, value, on_submit), Vec::new()),
        }
    }

    /// Keys for the add-to-playlist picker. Returns the popup to keep open.
    pub(super) fn picker_key(
        &mut self,
        mut picker: PickerState,
        key: KeyEvent,
    ) -> (Option<Popup>, Vec<Effect>) {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let effects = if key.code == KeyCode::Char('c') {
                vec![Effect::Quit]
            } else {
                Vec::new()
            };
            return (Some(Popup::PlaylistPicker(picker)), effects);
        }
        match key.code {
            KeyCode::Esc => return (None, Vec::new()),
            KeyCode::Char('j') | KeyCode::Down => {
                picker.sel = (picker.sel + 1).min(picker.last_row());
            }
            KeyCode::Char('k') | KeyCode::Up => picker.sel = picker.sel.saturating_sub(1),
            KeyCode::Enter if picker.sel == 0 => {
                let popup = Popup::TextInput {
                    title: "New playlist".into(),
                    value: String::new(),
                    on_submit: TextPurpose::NewPlaylist {
                        song_ids: picker.song_ids,
                    },
                };
                return (Some(popup), Vec::new());
            }
            KeyCode::Enter => {
                if let Some(p) = picker.lists.loaded().and_then(|l| l.get(picker.sel - 1)) {
                    let id = p.id.clone();
                    self.playlists.append_local(&id, &picker.songs);
                    let effect = Effect::AddToPlaylist {
                        id,
                        song_ids: picker.song_ids,
                    };
                    return (None, vec![effect]);
                }
            }
            _ => {}
        }
        (Some(Popup::PlaylistPicker(picker)), Vec::new())
    }

    /// Counts mutation effects as they leave the app (see `pending`).
    pub(super) fn note_playlist_effects(&mut self, effects: &[Effect]) {
        self.playlists.note_effects(effects);
    }

    /// Handles the playlist API results. Anything else is left alone.
    pub(super) fn handle_playlist_result(&mut self, result: ApiResult) -> Vec<Effect> {
        match result {
            ApiResult::Playlists(r) => {
                let applied = self.playlists.apply_lists(r);
                if let Some(Popup::PlaylistPicker(p)) = &mut self.popup {
                    p.lists = self.playlists.lists.clone();
                    p.sel = p.sel.min(p.last_row());
                }
                applied.unwrap_or_else(|msg| {
                    self.set_status(format!("Couldn't load playlists: {msg}"));
                    Vec::new()
                })
            }
            ApiResult::Playlist { id, result } => {
                if let Err(msg) = self.playlists.apply_tracks(&id, result) {
                    self.set_status(format!("Couldn't load the playlist: {msg}"));
                }
                Vec::new()
            }
            ApiResult::PlaylistReloaded { id, result } => {
                if let Err(msg) = self.playlists.apply_reload(&id, result) {
                    self.set_status(format!("Couldn't refresh the playlist: {msg}"));
                }
                Vec::new()
            }
            ApiResult::PlaylistMutated { result, id } => {
                self.playlists.pending = self.playlists.pending.saturating_sub(1);
                match result {
                    Ok(msg) if msg.is_empty() => {}
                    Ok(msg) => self.set_status(msg),
                    Err(msg) => {
                        self.set_status(format!("Couldn't update the playlist: {msg}"));
                        self.playlists.mark_refreshing(id.as_deref());
                        // The worker refuses edits to this playlist until the
                        // fence, sent in order behind them, is processed.
                        return id.map(Effect::ResyncPlaylist).into_iter().collect();
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppEvent;
    use crate::app::library::Column;
    use crate::app::sort::Sorts;
    use crossterm::event::KeyModifiers;

    fn key(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn press(app: &mut App, c: char) -> Vec<Effect> {
        key(app, KeyCode::Char(c))
    }

    fn api(app: &mut App, r: ApiResult) -> Vec<Effect> {
        app.handle_event(AppEvent::Api(r))
    }

    fn mutated(result: Result<String, String>, id: Option<&str>) -> ApiResult {
        ApiResult::PlaylistMutated {
            result,
            id: id.map(str::to_string),
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
        }
    }

    fn playlist(id: &str, name: &str) -> Playlist {
        Playlist {
            id: id.into(),
            name: name.into(),
            song_count: 4,
            duration: 0,
            owner: None,
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(std::string::ToString::to_string).collect()
    }

    /// An app on the Playlists view with two playlists and four tracks of
    /// the first one loaded, focus on the tracks.
    fn app_with_playlist() -> App {
        let mut app = App::new();
        app.view = View::Playlists;
        app.playlists.lists =
            Loadable::Loaded(vec![playlist("pl1", "Mix"), playlist("pl2", "Jazz")]);
        app.playlists.tracks_for = Some("pl1".into());
        app.playlists.tracks = Loadable::Loaded(vec![song("a"), song("b"), song("c"), song("d")]);
        app.playlists.focus = PlColumn::Tracks;
        app
    }

    fn track_ids(app: &App) -> Vec<String> {
        app.playlists
            .tracks
            .loaded()
            .unwrap()
            .iter()
            .map(|s| s.id.clone())
            .collect()
    }

    #[test]
    fn move_track_reorders_and_replaces() {
        let mut app = app_with_playlist();
        app.playlists.sel[1] = 2;
        let effects = press(&mut app, 'K');
        assert_eq!(track_ids(&app), ids(&["a", "c", "b", "d"]));
        assert_eq!(app.playlists.sel[1], 1);
        assert_eq!(
            effects,
            [Effect::ReplacePlaylistSongs {
                id: "pl1".into(),
                song_ids: ids(&["a", "c", "b", "d"]),
            }]
        );
        let effects = press(&mut app, 'J');
        assert_eq!(track_ids(&app), ids(&["a", "b", "c", "d"]));
        assert_eq!(effects.len(), 1);
    }

    #[test]
    fn move_track_at_the_edges_does_nothing() {
        let mut app = app_with_playlist();
        assert!(press(&mut app, 'K').is_empty());
        app.playlists.sel[1] = 3;
        assert!(press(&mut app, 'J').is_empty());
        assert_eq!(track_ids(&app), ids(&["a", "b", "c", "d"]));
    }

    #[test]
    fn remove_track_sends_index() {
        let mut app = app_with_playlist();
        app.playlists.sel[1] = 1;
        let effects = press(&mut app, 'd');
        assert_eq!(track_ids(&app), ids(&["a", "c", "d"]));
        assert_eq!(
            effects,
            [Effect::RemoveFromPlaylist {
                id: "pl1".into(),
                index: 1,
            }]
        );
        app.playlists.sel[1] = 2;
        press(&mut app, 'd');
        assert_eq!(app.playlists.sel[1], 1);
    }

    #[test]
    fn picker_new_playlist_creates_with_songs() {
        let mut app = app_with_playlist();
        press(&mut app, 'a');
        assert!(matches!(app.popup, Some(Popup::PlaylistPicker(_))));
        key(&mut app, KeyCode::Enter);
        assert!(matches!(
            app.popup,
            Some(Popup::TextInput {
                on_submit: TextPurpose::NewPlaylist { .. },
                ..
            })
        ));
        for c in "Mix 2".chars() {
            assert!(press(&mut app, c).is_empty());
        }
        let effects = key(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::CreatePlaylist {
                name: "Mix 2".into(),
                song_ids: ids(&["a"]),
            }]
        );
        assert!(app.popup.is_none());
    }

    #[test]
    fn picker_existing_adds_songs() {
        let mut app = app_with_playlist();
        press(&mut app, 'a');
        press(&mut app, 'j');
        press(&mut app, 'j');
        let effects = key(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::AddToPlaylist {
                id: "pl2".into(),
                song_ids: ids(&["a"]),
            }]
        );
        assert!(app.popup.is_none());
    }

    #[test]
    fn picker_selection_stops_at_the_last_row() {
        let mut app = app_with_playlist();
        press(&mut app, 'a');
        for _ in 0..10 {
            press(&mut app, 'j');
        }
        let Some(Popup::PlaylistPicker(p)) = &app.popup else {
            panic!("no picker");
        };
        assert_eq!(p.sel, 2);
    }

    #[test]
    fn picker_loads_playlists_when_not_loaded() {
        let mut app = app_with_playlist();
        app.playlists.lists = Loadable::NotLoaded;
        let effects = press(&mut app, 'a');
        assert_eq!(effects, [Effect::LoadPlaylists]);
        let Some(Popup::PlaylistPicker(p)) = &app.popup else {
            panic!("no picker");
        };
        assert_eq!(p.lists, Loadable::Loading);
        api(
            &mut app,
            ApiResult::Playlists(Ok(vec![playlist("pl9", "New")])),
        );
        let Some(Popup::PlaylistPicker(p)) = &app.popup else {
            panic!("no picker");
        };
        assert_eq!(p.lists.loaded().map(Vec::len), Some(1));
    }

    #[test]
    fn delete_requires_confirm() {
        let mut app = app_with_playlist();
        app.playlists.focus = PlColumn::Lists;
        assert!(press(&mut app, 'D').is_empty());
        assert_eq!(
            app.popup,
            Some(Popup::Confirm {
                message: "Delete playlist \"Mix\"?".into(),
                on_yes: Box::new(Effect::DeletePlaylist("pl1".into())),
            })
        );
        assert_eq!(press(&mut app, 'y'), [Effect::DeletePlaylist("pl1".into())]);
    }

    #[test]
    fn new_and_rename_open_text_inputs() {
        let mut app = app_with_playlist();
        app.playlists.focus = PlColumn::Lists;
        press(&mut app, 'n');
        assert_eq!(
            app.popup,
            Some(Popup::TextInput {
                title: "New playlist".into(),
                value: String::new(),
                on_submit: TextPurpose::NewPlaylist { song_ids: vec![] },
            })
        );
        key(&mut app, KeyCode::Esc);
        assert!(app.popup.is_none());

        press(&mut app, 'E');
        for _ in 0..3 {
            key(&mut app, KeyCode::Backspace);
        }
        press(&mut app, 'X');
        press(&mut app, 'q');
        assert!(!app.should_quit);
        let effects = key(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::RenamePlaylist {
                id: "pl1".into(),
                name: "Xq".into(),
            }]
        );
    }

    #[test]
    fn blank_names_are_rejected() {
        let mut app = app_with_playlist();
        press(&mut app, 'n');
        press(&mut app, ' ');
        assert!(key(&mut app, KeyCode::Enter).is_empty());
        assert!(app.popup.is_some());
        assert_eq!(app.status_text(), Some("Enter a playlist name"));
    }

    #[test]
    fn ctrl_c_quits_from_a_text_input() {
        let mut app = app_with_playlist();
        press(&mut app, 'n');
        let effects = app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(effects, [Effect::Quit]);
    }

    #[test]
    fn add_with_no_target_says_so() {
        let mut app = App::new();
        assert!(press(&mut app, 'a').is_empty());
        assert!(app.popup.is_none());
        assert_eq!(app.status_text(), Some("Nothing to add"));
    }

    #[test]
    fn add_from_library_albums_uses_every_loaded_track() {
        let mut app = App::new();
        app.library.focus = Column::Albums;
        app.library.tracks = Loadable::Loaded(vec![song("x"), song("y")]);
        press(&mut app, 'a');
        let Some(Popup::PlaylistPicker(p)) = &app.popup else {
            panic!("no picker");
        };
        assert_eq!(p.song_ids, ids(&["x", "y"]));
    }

    #[test]
    fn add_from_the_playlist_list_uses_all_its_tracks() {
        let mut app = app_with_playlist();
        app.playlists.focus = PlColumn::Lists;
        assert_eq!(app.add_to_playlist_targets(), ids(&["a", "b", "c", "d"]));
    }

    #[test]
    fn selecting_a_playlist_loads_its_tracks() {
        let mut app = app_with_playlist();
        app.playlists.focus = PlColumn::Lists;
        let effects = press(&mut app, 'j');
        assert_eq!(effects, [Effect::LoadPlaylist("pl2".into())]);
        assert_eq!(app.playlists.tracks, Loadable::Loading);
    }

    #[test]
    fn showing_the_view_loads_playlists_once() {
        let mut app = App::new();
        assert_eq!(press(&mut app, '2'), [Effect::LoadPlaylists]);
        assert!(press(&mut app, '1').is_empty());
        assert!(press(&mut app, '2').is_empty());
        let effects = api(
            &mut app,
            ApiResult::Playlists(Ok(vec![playlist("pl1", "Mix")])),
        );
        assert_eq!(effects, [Effect::LoadPlaylist("pl1".into())]);
    }

    #[test]
    fn reloads_wait_for_pending_edits() {
        let mut app = app_with_playlist();
        app.playlists.sel[1] = 2;
        press(&mut app, 'K');
        press(&mut app, 'K');
        let stale = Ok(vec![song("z")]);
        api(
            &mut app,
            ApiResult::PlaylistReloaded {
                id: "pl1".into(),
                result: stale.clone(),
            },
        );
        assert_eq!(track_ids(&app).len(), 4);
        api(&mut app, mutated(Ok(String::new()), None));
        api(&mut app, mutated(Ok(String::new()), None));
        api(
            &mut app,
            ApiResult::PlaylistReloaded {
                id: "pl1".into(),
                result: stale,
            },
        );
        assert_eq!(track_ids(&app), ids(&["z"]));
    }

    #[test]
    fn mutation_results_set_status() {
        let mut app = app_with_playlist();
        api(&mut app, mutated(Ok("Added 3 songs to Mix".into()), None));
        assert_eq!(app.status_text(), Some("Added 3 songs to Mix"));
        api(&mut app, mutated(Err("boom".into()), None));
        assert_eq!(
            app.status_text(),
            Some("Couldn't update the playlist: boom")
        );
    }

    #[test]
    fn deleting_the_selected_playlist_loads_the_next() {
        let mut app = app_with_playlist();
        let effects = api(
            &mut app,
            ApiResult::Playlists(Ok(vec![playlist("pl2", "Jazz")])),
        );
        assert_eq!(effects, [Effect::LoadPlaylist("pl2".into())]);
    }

    #[test]
    fn enter_plays_from_the_selected_track() {
        let mut app = app_with_playlist();
        app.playlists.sel[1] = 2;
        let effects = key(&mut app, KeyCode::Enter);
        let [Effect::Player(PlayerCommand::PlayQueue { songs, start })] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!((songs.len(), *start), (4, 2));
    }

    #[test]
    fn starring_updates_playlist_tracks() {
        let mut app = app_with_playlist();
        app.playlists.set_starred("b", true);
        assert!(app.playlists.tracks.loaded().unwrap()[1].starred);
    }

    #[test]
    fn adding_to_the_shown_playlist_appends_locally_before_a_move() {
        let mut app = app_with_playlist();
        app.library.focus = Column::Tracks;
        app.library.tracks = Loadable::Loaded(vec![song("new")]);
        app.view = View::Library;
        press(&mut app, 'a');
        press(&mut app, 'j');
        let effects = key(&mut app, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::AddToPlaylist {
                id: "pl1".into(),
                song_ids: ids(&["new"]),
            }]
        );
        assert_eq!(track_ids(&app), ids(&["a", "b", "c", "d", "new"]));
        app.view = View::Playlists;
        app.playlists.sel[1] = 4;
        let effects = press(&mut app, 'K');
        assert_eq!(
            effects,
            [Effect::ReplacePlaylistSongs {
                id: "pl1".into(),
                song_ids: ids(&["a", "b", "c", "new", "d"]),
            }]
        );
    }

    #[test]
    fn a_failed_edit_refuses_more_until_the_reload_lands() {
        let mut app = app_with_playlist();
        press(&mut app, 'd');
        press(&mut app, 'd');
        let fence = api(&mut app, mutated(Err("boom".into()), Some("pl1")));
        assert_eq!(fence, [Effect::ResyncPlaylist("pl1".into())]);
        assert_eq!(app.playlists.tracks, Loadable::Loading);
        assert!(press(&mut app, 'd').is_empty());
        assert_eq!(app.status_text(), Some("Playlist is refreshing"));
        assert!(press(&mut app, 'J').is_empty());
        // The skipped second job settles, then the reload lands.
        api(&mut app, mutated(Ok(String::new()), None));
        api(
            &mut app,
            ApiResult::PlaylistReloaded {
                id: "pl1".into(),
                result: Ok(vec![song("a"), song("b")]),
            },
        );
        assert_eq!(track_ids(&app), ids(&["a", "b"]));
        assert_eq!(press(&mut app, 'd').len(), 1);
    }

    #[test]
    fn a_failed_reload_makes_the_playlist_read_only() {
        let mut app = app_with_playlist();
        api(
            &mut app,
            ApiResult::PlaylistReloaded {
                id: "pl1".into(),
                result: Err("down".into()),
            },
        );
        assert!(matches!(app.playlists.tracks, Loadable::Failed(_)));
        assert!(press(&mut app, 'd').is_empty());
        assert!(app.playlists.tracks.loaded().is_none());
    }

    #[test]
    fn results_from_an_earlier_session_are_ignored() {
        let mut app = app_with_playlist();
        press(&mut app, 'd');
        let stale = ApiResult::Session {
            session: app.session + 1,
            inner: Box::new(mutated(Ok("Removed".into()), Some("pl1"))),
        };
        api(&mut app, stale);
        assert_eq!(app.playlists.pending, 1);
        assert_eq!(app.status_text(), None);
        let current = ApiResult::Session {
            session: app.session,
            inner: Box::new(mutated(Ok("Removed".into()), Some("pl1"))),
        };
        api(&mut app, current);
        assert_eq!(app.playlists.pending, 0);
        app.show_login("s", "u");
        assert_eq!(app.session, 1);
    }

    #[test]
    fn the_fence_read_applies_while_other_edits_are_pending() {
        let mut app = app_with_playlist();
        press(&mut app, 'd');
        press(&mut app, 'd');
        press(&mut app, 'd');
        api(&mut app, mutated(Err("boom".into()), Some("pl1")));
        assert_eq!(app.playlists.pending, 2);
        api(
            &mut app,
            ApiResult::Playlist {
                id: "pl1".into(),
                result: Ok(vec![song("a"), song("b")]),
            },
        );
        assert_eq!(track_ids(&app), ids(&["a", "b"]));
    }

    fn list_ids(app: &App) -> Vec<&str> {
        app.playlists
            .lists
            .loaded()
            .unwrap()
            .iter()
            .map(|p| p.id.as_str())
            .collect()
    }

    #[test]
    fn o_sorts_playlists_and_reloads_keep_the_order() {
        let mut app = app_with_playlist();
        app.playlists.focus = PlColumn::Lists;
        let fx = press(&mut app, 'o');
        assert_eq!(
            fx,
            [Effect::SaveSorts(Sorts {
                playlists: PlaylistSort::Name,
                ..Sorts::default()
            })]
        );
        assert_eq!(list_ids(&app), ["pl2", "pl1"]);
        assert_eq!(app.playlists.sel[0], 1);
        api(
            &mut app,
            ApiResult::Playlists(Ok(vec![
                playlist("pl1", "Mix"),
                playlist("pl2", "Jazz"),
                playlist("pl3", "blues"),
            ])),
        );
        assert_eq!(list_ids(&app), ["pl3", "pl2", "pl1"]);
        assert_eq!(app.playlists.sel[0], 2);
        press(&mut app, 'o');
        assert_eq!(list_ids(&app), ["pl1", "pl2", "pl3"]);
        assert_eq!(app.playlists.sel[0], 0);
    }

    #[test]
    fn o_leaves_playlist_tracks_in_playlist_order() {
        let mut app = app_with_playlist();
        assert_eq!(press(&mut app, 'o'), []);
        assert_eq!(track_ids(&app), ids(&["a", "b", "c", "d"]));
        assert_eq!(app.playlists.list_sort, PlaylistSort::Server);
    }
}
