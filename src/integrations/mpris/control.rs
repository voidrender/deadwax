//! MPRIS method calls, as player commands.

use super::state::MprisState;
use crate::player::PlayerCommand;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    /// Moves by this many microseconds.
    Seek(i64),
    SetPosition {
        track_id: String,
        position_us: i64,
    },
}

/// The commands that carry out `c`. Nothing is sent while no track is
/// loaded.
pub fn commands(c: Control, state: &MprisState) -> Vec<PlayerCommand> {
    if !state.has_track() {
        return Vec::new();
    }
    match c {
        Control::PlayPause => vec![PlayerCommand::TogglePause],
        // Not toggles: `state` can lag behind the engine, and a toggle
        // sent from a stale state would undo an earlier one.
        Control::Play => vec![PlayerCommand::SetPaused(false)],
        Control::Pause => vec![PlayerCommand::SetPaused(true)],
        // Deadwax has no stopped-but-loaded state to go to.
        Control::Stop => Vec::new(),
        Control::Next => vec![PlayerCommand::Next],
        Control::Previous => vec![PlayerCommand::Prev],
        Control::Seek(us) => vec![PlayerCommand::SeekRelative(seconds(us))],
        Control::SetPosition {
            track_id,
            position_us,
        } => {
            // The spec says to ignore a call for another track, or for a
            // position outside the track.
            let meta = state.metadata();
            let in_track = position_us >= 0 && meta.length_us.is_none_or(|len| position_us <= len);
            if track_id == meta.track_id && in_track {
                vec![PlayerCommand::SeekTo(seconds(position_us))]
            } else {
                Vec::new()
            }
        }
    }
}

fn seconds(us: i64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "track positions are far inside f64's mantissa"
    )]
    let us = us as f64;
    us / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::EngineEvent;
    use crate::subsonic::Song;

    fn playing() -> MprisState {
        let song: Song = serde_json::from_value(serde_json::json!({
            "id": "s1", "title": "T", "duration": 200,
        }))
        .unwrap();
        let mut s = MprisState::default();
        s.apply(&EngineEvent::TrackStarted(Box::new(song)));
        s
    }

    fn paused() -> MprisState {
        let mut s = playing();
        s.apply(&EngineEvent::PauseChanged(true));
        s
    }

    #[test]
    fn play_and_pause_set_the_state_and_play_pause_toggles() {
        // The engine applies these at execution time, so they hold even
        // when this state is behind.
        for s in [playing(), paused()] {
            assert_eq!(
                commands(Control::Play, &s),
                [PlayerCommand::SetPaused(false)]
            );
            assert_eq!(
                commands(Control::Pause, &s),
                [PlayerCommand::SetPaused(true)]
            );
            assert_eq!(
                commands(Control::PlayPause, &s),
                [PlayerCommand::TogglePause]
            );
        }
    }

    #[test]
    fn nothing_is_sent_without_a_track() {
        let none = MprisState::default();
        let all = [
            Control::PlayPause,
            Control::Play,
            Control::Pause,
            Control::Stop,
            Control::Next,
            Control::Previous,
            Control::Seek(1_000_000),
            Control::SetPosition {
                track_id: none.metadata().track_id,
                position_us: 0,
            },
        ];
        for c in all {
            assert!(commands(c.clone(), &none).is_empty(), "{c:?}");
        }
    }

    #[test]
    fn next_previous_and_seek_map_to_engine_commands() {
        let s = playing();
        assert_eq!(commands(Control::Next, &s), [PlayerCommand::Next]);
        assert_eq!(commands(Control::Previous, &s), [PlayerCommand::Prev]);
        assert_eq!(
            commands(Control::Seek(-5_000_000), &s),
            [PlayerCommand::SeekRelative(-5.0)]
        );
        assert!(commands(Control::Stop, &s).is_empty());
    }

    #[test]
    fn set_position_needs_the_current_track() {
        let s = playing();
        let current = s.metadata().track_id;
        let at = |track_id: &str, position_us| Control::SetPosition {
            track_id: track_id.to_string(),
            position_us,
        };
        assert_eq!(
            commands(at(&current, 30_000_000), &s),
            [PlayerCommand::SeekTo(30.0)]
        );
        assert!(commands(at("/org/deadwax/track/0", 30_000_000), &s).is_empty());
        assert!(commands(at(&current, -1), &s).is_empty());
        assert!(commands(at(&current, 200_000_001), &s).is_empty());
    }
}
