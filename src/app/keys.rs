use super::View;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// A global key binding, resolved before any view sees the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    ShowView(View),
    ToggleNowPlaying,
    ToggleHelp,
    Quit,
    TogglePause,
    Next,
    Prev,
    SeekForward,
    SeekBack,
    VolumeUp,
    VolumeDown,
    ToggleShuffle,
    CycleRepeat,
}

/// Seconds moved by `]` and `[`.
pub const SEEK_STEP: f64 = 10.0;
/// Percentage points changed by `=` and `-`.
pub const VOLUME_STEP: u8 = 5;

/// Maps a key press to a global action. Keys held with Ctrl or Alt never
/// match (except Ctrl+C, which quits), so they stay free for later bindings.
pub fn global_action(key: &KeyEvent) -> Option<Action> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return (key.code == KeyCode::Char('c')).then_some(Action::Quit);
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    Some(match c {
        '1' => Action::ShowView(View::Library),
        '2' => Action::ShowView(View::Playlists),
        '3' => Action::ShowView(View::Search),
        '4' => Action::ShowView(View::Queue),
        'z' => Action::ToggleNowPlaying,
        '?' => Action::ToggleHelp,
        'q' => Action::Quit,
        ' ' => Action::TogglePause,
        '.' => Action::Next,
        ',' => Action::Prev,
        ']' => Action::SeekForward,
        '[' => Action::SeekBack,
        '=' => Action::VolumeUp,
        '-' => Action::VolumeDown,
        's' => Action::ToggleShuffle,
        'r' => Action::CycleRepeat,
        _ => return None,
    })
}

/// Rows for the help overlay: key label and what it does.
pub const HELP: &[(&str, &str)] = &[
    ("1 2 3 4", "Library, Playlists, Search, Queue"),
    ("z", "Toggle Now Playing"),
    ("space", "Play / pause"),
    (". ,", "Next / previous track"),
    ("] [", "Seek +10 s / -10 s"),
    ("= -", "Volume +5 / -5"),
    ("s", "Toggle shuffle"),
    ("r", "Cycle repeat: off, all, one"),
    ("?", "Toggle this help"),
    ("Esc", "Close popup or help"),
    ("q", "Quit"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn shifted_symbols_still_match() {
        let ev = key(KeyCode::Char('?'), KeyModifiers::SHIFT);
        assert_eq!(global_action(&ev), Some(Action::ToggleHelp));
    }

    #[test]
    fn ctrl_c_quits_but_other_ctrl_keys_do_not_match() {
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(
            global_action(&key(KeyCode::Char('c'), ctrl)),
            Some(Action::Quit)
        );
        assert_eq!(global_action(&key(KeyCode::Char('q'), ctrl)), None);
    }
}
