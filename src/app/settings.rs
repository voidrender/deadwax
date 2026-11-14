//! The settings popup: options from `config.toml` that can change while the
//! app runs.

use super::{App, Effect, Popup, Screen};
use crate::config::Config;
use crate::ui::art::{ProtocolChoice, parse_protocol};
use crossterm::event::{KeyCode, KeyEvent};

/// The options the settings popup shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub discord: bool,
    pub notifications: bool,
    /// Applies on the next start, since detecting it means querying the
    /// terminal, which can't happen while the app reads keys.
    pub image_protocol: ProtocolChoice,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            discord: true,
            notifications: true,
            image_protocol: ProtocolChoice::Auto,
        }
    }
}

impl Settings {
    pub fn from_config(c: &Config) -> Self {
        Self {
            discord: c.discord,
            notifications: c.notifications,
            image_protocol: parse_protocol(c.image_protocol.as_deref()),
        }
    }

    /// Writes these options into `c`, leaving its other fields alone.
    pub fn apply_to(&self, c: &mut Config) {
        c.discord = self.discord;
        c.notifications = self.notifications;
        c.image_protocol = protocol_value(self.image_protocol).map(str::to_string);
    }
}

/// The rows of the popup, top to bottom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Discord,
    Notifications,
    ImageProtocol,
}

pub const ROWS: [Row; 3] = [Row::Discord, Row::Notifications, Row::ImageProtocol];

impl Row {
    pub fn label(self) -> &'static str {
        match self {
            Self::Discord => "Discord presence",
            Self::Notifications => "Notifications",
            Self::ImageProtocol => "Album art",
        }
    }
}

const PROTOCOLS: [ProtocolChoice; 5] = [
    ProtocolChoice::Auto,
    ProtocolChoice::Kitty,
    ProtocolChoice::Iterm2,
    ProtocolChoice::Sixel,
    ProtocolChoice::Halfblocks,
];

/// How a protocol choice is written in `config.toml`. Auto is the default,
/// so it is left out of the file.
fn protocol_value(p: ProtocolChoice) -> Option<&'static str> {
    match p {
        ProtocolChoice::Auto => None,
        ProtocolChoice::Kitty => Some("kitty"),
        ProtocolChoice::Iterm2 => Some("iterm2"),
        ProtocolChoice::Sixel => Some("sixel"),
        ProtocolChoice::Halfblocks => Some("halfblocks"),
    }
}

pub fn protocol_label(p: ProtocolChoice) -> &'static str {
    match p {
        ProtocolChoice::Auto => "Auto",
        ProtocolChoice::Kitty => "Kitty",
        ProtocolChoice::Iterm2 => "iTerm2",
        ProtocolChoice::Sixel => "Sixel",
        ProtocolChoice::Halfblocks => "Half-blocks",
    }
}

fn step_protocol(p: ProtocolChoice, forward: bool) -> ProtocolChoice {
    let i = PROTOCOLS.iter().position(|&x| x == p).unwrap_or(0);
    let n = PROTOCOLS.len();
    PROTOCOLS[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }]
}

impl App {
    pub(super) fn open_settings(&mut self) -> Vec<Effect> {
        if self.screen != Screen::Main {
            return Vec::new();
        }
        self.help = false;
        self.popup = Some(Popup::Settings { sel: 0 });
        Vec::new()
    }

    /// Keys while the settings popup is open. Every change is saved at once.
    pub(super) fn settings_key(
        &mut self,
        sel: usize,
        key: KeyEvent,
    ) -> (Option<Popup>, Vec<Effect>) {
        let sel = sel.min(ROWS.len() - 1);
        let forward = match key.code {
            KeyCode::Esc | KeyCode::Char('q' | 'S') => return (None, Vec::new()),
            KeyCode::Char('j') | KeyCode::Down => {
                let sel = (sel + 1).min(ROWS.len() - 1);
                return (Some(Popup::Settings { sel }), Vec::new());
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let sel = sel.saturating_sub(1);
                return (Some(Popup::Settings { sel }), Vec::new());
            }
            KeyCode::Enter | KeyCode::Char(' ' | 'l') | KeyCode::Right => true,
            KeyCode::Char('h') | KeyCode::Left => false,
            _ => return (Some(Popup::Settings { sel }), Vec::new()),
        };
        let s = &mut self.settings;
        match ROWS[sel] {
            Row::Discord => s.discord = !s.discord,
            Row::Notifications => s.notifications = !s.notifications,
            Row::ImageProtocol => s.image_protocol = step_protocol(s.image_protocol, forward),
        }
        (
            Some(Popup::Settings { sel }),
            vec![Effect::SaveSettings(self.settings.clone())],
        )
    }

    /// Whether the album art choice differs from the one this run started
    /// with, so it needs a restart.
    pub fn image_protocol_pending(&self) -> bool {
        self.settings.image_protocol != self.image_protocol_at_start
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppEvent;
    use crossterm::event::KeyModifiers;

    fn key(app: &mut App, code: KeyCode) -> Vec<Effect> {
        app.handle_event(AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn sel(app: &App) -> Option<usize> {
        match app.popup {
            Some(Popup::Settings { sel }) => Some(sel),
            _ => None,
        }
    }

    #[test]
    fn capital_s_opens_and_closes_settings() {
        let mut app = App::logged_in("s");
        key(&mut app, KeyCode::Char('S'));
        assert_eq!(sel(&app), Some(0));
        key(&mut app, KeyCode::Char('S'));
        assert_eq!(sel(&app), None);
    }

    #[test]
    fn toggles_save_right_away() {
        let mut app = App::logged_in("s");
        key(&mut app, KeyCode::Char('S'));
        let fx = key(&mut app, KeyCode::Enter);
        assert!(!app.settings.discord);
        assert_eq!(fx, [Effect::SaveSettings(app.settings.clone())]);

        key(&mut app, KeyCode::Char('j'));
        key(&mut app, KeyCode::Char(' '));
        assert!(!app.settings.notifications);

        // Other keys leave it open and change nothing.
        assert!(key(&mut app, KeyCode::Char('x')).is_empty());
        assert_eq!(sel(&app), Some(1));
    }

    #[test]
    fn album_art_cycles_both_ways_and_flags_a_restart() {
        let mut app = App::logged_in("s");
        key(&mut app, KeyCode::Char('S'));
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_eq!(sel(&app), Some(2));
        assert!(!app.image_protocol_pending());
        key(&mut app, KeyCode::Left);
        assert_eq!(app.settings.image_protocol, ProtocolChoice::Halfblocks);
        assert!(app.image_protocol_pending());
        key(&mut app, KeyCode::Right);
        assert_eq!(app.settings.image_protocol, ProtocolChoice::Auto);
        assert!(!app.image_protocol_pending());
    }

    #[test]
    fn settings_round_trip_through_the_config() {
        let mut c: Config = toml::from_str("server_url = \"https://x\"\nusername = \"a\"").unwrap();
        let s = Settings {
            discord: false,
            notifications: false,
            image_protocol: ProtocolChoice::Iterm2,
        };
        s.apply_to(&mut c);
        assert_eq!(c.image_protocol.as_deref(), Some("iterm2"));
        assert_eq!(Settings::from_config(&c), s);
        Settings::default().apply_to(&mut c);
        assert_eq!(c.image_protocol, None);
    }

    #[test]
    fn not_on_the_login_screen() {
        let mut app = App::login_screen("", "");
        key(&mut app, KeyCode::Char('S'));
        assert_eq!(sel(&app), None);
    }
}
