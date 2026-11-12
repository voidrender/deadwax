use super::{App, Effect, Popup};
use crate::player::PlayerCommand;
use crate::player::queue::QueueItem;
use crate::subsonic::Song;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Selection state for the queue view. The rows themselves come from the
/// player snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueueViewState {
    pub sel: usize,
    /// The uid of the selected item, so the selection stays on it when the
    /// queue changes around it.
    anchor: Option<u64>,
}

impl QueueViewState {
    /// Re-aims the selection after a new snapshot. It stays on the same item
    /// when that item is still queued, and is clamped otherwise.
    pub fn sync(&mut self, items: &[QueueItem]) {
        let found = self
            .anchor
            .and_then(|uid| items.iter().position(|i| i.uid == uid));
        self.sel = found.unwrap_or(self.sel);
        self.clamp(items);
    }

    fn clamp(&mut self, items: &[QueueItem]) {
        self.sel = self.sel.min(items.len().saturating_sub(1));
        self.anchor = items.get(self.sel).map(|i| i.uid);
    }

    fn move_selection(&mut self, items: &[QueueItem], delta: isize) {
        self.sel = self
            .sel
            .saturating_add_signed(delta)
            .min(items.len().saturating_sub(1));
        self.anchor = items.get(self.sel).map(|i| i.uid);
    }
}

impl App {
    pub fn selected_queue_song(&self) -> Option<&Song> {
        self.player.items.get(self.queue.sel).map(|i| &i.song)
    }

    pub(super) fn handle_queue_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Vec::new();
        }
        let len = self.player.items.len();
        if len == 0 {
            return Vec::new();
        }
        let sel = self.queue.sel.min(len - 1);
        let player = |cmd| vec![Effect::Player(cmd)];
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.queue.move_selection(&self.player.items, 1);
                Vec::new()
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.queue.move_selection(&self.player.items, -1);
                Vec::new()
            }
            KeyCode::Enter => player(PlayerCommand::JumpTo(sel)),
            KeyCode::Char('d') => player(PlayerCommand::Remove(sel)),
            KeyCode::Char('J') if sel + 1 < len => {
                // The item keeps its uid, so the next snapshot lands on it.
                self.queue.sel = sel + 1;
                player(PlayerCommand::Move {
                    from: sel,
                    to: sel + 1,
                })
            }
            KeyCode::Char('K') if sel > 0 => {
                self.queue.sel = sel - 1;
                player(PlayerCommand::Move {
                    from: sel,
                    to: sel - 1,
                })
            }
            KeyCode::Char('c') => {
                self.popup = Some(Popup::Confirm {
                    message: "Clear the queue?".into(),
                    on_yes: Box::new(Effect::Player(PlayerCommand::Clear)),
                });
                Vec::new()
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{AppEvent, View};
    use crate::player::PlayerSnapshot;

    fn song(id: &str) -> Song {
        Song {
            id: id.into(),
            title: format!("Title {id}"),
            artist: "Artist".into(),
            artist_id: None,
            album: "Album".into(),
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

    fn snap(uids: &[u64]) -> PlayerSnapshot {
        PlayerSnapshot {
            items: uids
                .iter()
                .map(|&uid| QueueItem {
                    uid,
                    song: song(&uid.to_string()),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn app_with(uids: &[u64]) -> App {
        let mut app = App::new();
        app.view = View::Queue;
        app.handle_event(AppEvent::Snapshot(snap(uids)));
        app
    }

    fn press(app: &mut App, c: char) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )))
    }

    fn enter(app: &mut App) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
    }

    #[test]
    fn enter_jumps() {
        let mut app = app_with(&[1, 2, 3]);
        press(&mut app, 'j');
        assert_eq!(enter(&mut app), [Effect::Player(PlayerCommand::JumpTo(1))]);
    }

    #[test]
    fn move_down_follows_selection() {
        let mut app = app_with(&[1, 2, 3]);
        assert_eq!(
            press(&mut app, 'J'),
            [Effect::Player(PlayerCommand::Move { from: 0, to: 1 })]
        );
        assert_eq!(app.queue.sel, 1);
        app.handle_event(AppEvent::Snapshot(snap(&[2, 1, 3])));
        assert_eq!(app.queue.sel, 1);
        assert_eq!(
            press(&mut app, 'K'),
            [Effect::Player(PlayerCommand::Move { from: 1, to: 0 })]
        );
        assert_eq!(app.queue.sel, 0);
    }

    #[test]
    fn move_at_the_ends_is_a_no_op() {
        let mut app = app_with(&[1, 2]);
        assert!(press(&mut app, 'K').is_empty());
        press(&mut app, 'j');
        assert!(press(&mut app, 'J').is_empty());
        assert_eq!(app.queue.sel, 1);
    }

    #[test]
    fn selection_clamped_after_shrink() {
        let mut app = app_with(&[1, 2, 3]);
        press(&mut app, 'j');
        press(&mut app, 'j');
        app.handle_event(AppEvent::Snapshot(snap(&[1])));
        assert_eq!(app.queue.sel, 0);
        app.handle_event(AppEvent::Snapshot(snap(&[])));
        assert_eq!(app.queue.sel, 0);
    }

    #[test]
    fn selection_stays_on_the_same_item() {
        let mut app = app_with(&[1, 2, 3]);
        press(&mut app, 'j');
        press(&mut app, 'j');
        app.handle_event(AppEvent::Snapshot(snap(&[1, 3])));
        assert_eq!(app.queue.sel, 1);
        app.handle_event(AppEvent::Snapshot(snap(&[9, 1, 3])));
        assert_eq!(app.queue.sel, 2);
    }

    #[test]
    fn remove_keeps_the_index_and_clamps() {
        let mut app = app_with(&[1, 2, 3]);
        press(&mut app, 'j');
        assert_eq!(
            press(&mut app, 'd'),
            [Effect::Player(PlayerCommand::Remove(1))]
        );
        app.handle_event(AppEvent::Snapshot(snap(&[1, 3])));
        assert_eq!(app.queue.sel, 1);
        app.handle_event(AppEvent::Snapshot(snap(&[1])));
        assert_eq!(app.queue.sel, 0);
    }

    #[test]
    fn c_asks_before_clearing() {
        let mut app = app_with(&[1]);
        assert!(press(&mut app, 'c').is_empty());
        assert_eq!(
            app.popup,
            Some(Popup::Confirm {
                message: "Clear the queue?".into(),
                on_yes: Box::new(Effect::Player(PlayerCommand::Clear)),
            })
        );
        assert_eq!(press(&mut app, 'y'), [Effect::Player(PlayerCommand::Clear)]);
    }

    #[test]
    fn empty_queue_ignores_view_keys_but_not_globals() {
        let mut app = app_with(&[]);
        for c in ['j', 'k', 'd', 'J', 'K', 'c'] {
            assert!(press(&mut app, c).is_empty(), "key {c}");
        }
        assert!(enter(&mut app).is_empty());
        assert!(app.popup.is_none());
        assert_eq!(
            press(&mut app, ' '),
            [Effect::Player(PlayerCommand::TogglePause)]
        );
    }

    #[test]
    fn keys_are_view_scoped() {
        let mut app = app_with(&[1, 2]);
        app.view = View::Library;
        assert!(press(&mut app, 'd').is_empty());
        app.view = View::Queue;
        app.help = true;
        assert!(press(&mut app, 'd').is_empty());
        assert!(!app.help);
    }

    #[test]
    fn f_and_a_act_on_the_selected_row() {
        let mut app = app_with(&[1, 2]);
        press(&mut app, 'j');
        assert_eq!(
            press(&mut app, 'f'),
            [Effect::ToggleStar {
                song_id: "2".into(),
                starred: true
            }]
        );
        assert_eq!(app.add_to_playlist_targets(), ["2"]);
    }
}
