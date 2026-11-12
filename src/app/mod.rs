pub mod keys;

use crate::player::{EngineEvent, PlayerCommand, PlayerSnapshot};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use image::DynamicImage;
use keys::{Action, SEEK_STEP, VOLUME_STEP};
use std::time::{Duration, Instant};

/// How long a status message replaces the view tabs.
pub const STATUS_TTL: Duration = Duration::from_secs(4);

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
}

#[derive(Clone, Debug, PartialEq)]
pub enum Popup {
    Confirm {
        message: String,
        on_yes: Box<Effect>,
    },
}

/// Results of async work spawned by the runtime. The task that issues a
/// request adds its variant here.
#[derive(Debug)]
pub enum ApiResult {}

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
}

/// Side effects the runtime carries out for the app. Player commands go
/// straight to the engine; anything async is spawned and reports back as an
/// `AppEvent`.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Player(PlayerCommand),
    Quit,
}

#[derive(Debug, Default)]
pub struct App {
    pub screen: Screen,
    pub view: View,
    pub now_playing: bool,
    pub help: bool,
    pub popup: Option<Popup>,
    pub status: Option<(String, Instant)>,
    pub player: PlayerSnapshot,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
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
        match ev {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Resize => Vec::new(),
            AppEvent::Tick => {
                self.expire_status(Instant::now());
                Vec::new()
            }
            AppEvent::Snapshot(snap) => {
                self.player = snap;
                Vec::new()
            }
            AppEvent::Engine(ev) => {
                if let EngineEvent::Error(msg) = ev {
                    self.set_status(msg);
                }
                Vec::new()
            }
            AppEvent::Api(result) => match result {},
            AppEvent::CoverReady { .. } => Vec::new(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        if self.popup.is_some() {
            return self.handle_popup_key(key);
        }
        if self.help && key.code == KeyCode::Esc {
            self.help = false;
            return Vec::new();
        }
        match keys::global_action(&key) {
            Some(action) => self.run_action(action),
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
        };
        self.popup = keep;
        effects
    }

    fn run_action(&mut self, action: Action) -> Vec<Effect> {
        let cmd = match action {
            Action::ShowView(view) => {
                self.view = view;
                self.now_playing = false;
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
            Action::TogglePause => PlayerCommand::TogglePause,
            Action::Next => PlayerCommand::Next,
            Action::Prev => PlayerCommand::Prev,
            Action::SeekForward => PlayerCommand::SeekRelative(SEEK_STEP),
            Action::SeekBack => PlayerCommand::SeekRelative(-SEEK_STEP),
            Action::VolumeUp => {
                PlayerCommand::SetVolume(self.player.volume.saturating_add(VOLUME_STEP).min(100))
            }
            Action::VolumeDown => {
                PlayerCommand::SetVolume(self.player.volume.saturating_sub(VOLUME_STEP))
            }
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
        assert!(press(&mut app, '2').is_empty());
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
    fn volume_keys_step_and_clamp() {
        let mut app = App::new();
        app.player.volume = 50;
        assert_eq!(
            press(&mut app, '='),
            [Effect::Player(PlayerCommand::SetVolume(55))]
        );
        assert_eq!(
            press(&mut app, '-'),
            [Effect::Player(PlayerCommand::SetVolume(45))]
        );
        app.player.volume = 98;
        assert_eq!(
            press(&mut app, '='),
            [Effect::Player(PlayerCommand::SetVolume(100))]
        );
        app.player.volume = 3;
        assert_eq!(
            press(&mut app, '-'),
            [Effect::Player(PlayerCommand::SetVolume(0))]
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
