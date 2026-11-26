pub mod backend;
pub mod engine;
pub mod mpv;
pub mod persist;
pub mod queue;

use crate::subsonic::Song;
use backend::{AudioBackend, BackendEvent};
use persist::PersistedQueue;
use queue::{QueueItem, RepeatMode};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// Builds the stream URL for a song. Its output carries auth, so it must
/// never be logged or shown.
pub type UrlFor = Arc<dyn Fn(&Song) -> String + Send + Sync>;

#[derive(Clone, Debug, PartialEq)]
pub enum PlayerCommand {
    PlayQueue {
        songs: Vec<Song>,
        start: usize,
    },
    Append(Vec<Song>),
    PlayNext(Vec<Song>),
    TogglePause,
    /// Pauses or resumes. Unlike `TogglePause`, sending it twice leaves the
    /// same state, so a caller working from an old snapshot can't undo
    /// itself.
    SetPaused(bool),
    Next,
    Prev,
    SeekRelative(f64),
    /// Like `SeekRelative`, but a target past the end of the track goes to
    /// the next track instead, as MPRIS asks. The engine decides from its
    /// own position, so quick seeks in a row add up.
    SeekRelativeOrNext(f64),
    SeekTo(f64),
    /// Moves the volume by this many points, clamped to 0..=100. It is
    /// relative so that quick presses add up even before a snapshot shows
    /// the last one.
    ChangeVolume(i16),
    ToggleShuffle,
    CycleRepeat,
    JumpTo(usize),
    Remove(usize),
    Move {
        from: usize,
        to: usize,
    },
    Clear,
    Restore(PersistedQueue),
    SetStarred {
        song_id: String,
        starred: bool,
    },
    Shutdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayerSnapshot {
    /// Queue items in display order.
    pub items: Vec<QueueItem>,
    /// For each display position, the item's place in the unshuffled
    /// order. Saving needs it so shuffle can be turned off after a restart.
    pub order: Vec<usize>,
    pub current: Option<usize>,
    pub status: Status,
    pub position: f64,
    pub duration: f64,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: RepeatMode,
}

impl PlayerSnapshot {
    /// The current track's length in seconds. Before the player reports
    /// it, this is the length the server listed.
    pub fn track_duration(&self) -> f64 {
        if self.duration > 0.0 {
            return self.duration;
        }
        self.current
            .and_then(|i| self.items.get(i))
            .map_or(0.0, |item| f64::from(item.song.duration))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EngineEvent {
    TrackStarted(Box<Song>),
    Position { pos: f64, paused: bool },
    Seeked(f64),
    PauseChanged(bool),
    Stopped,
    Error(String),
}

pub struct EngineHandle {
    pub commands: mpsc::UnboundedSender<PlayerCommand>,
    pub snapshot: watch::Receiver<PlayerSnapshot>,
    pub events: mpsc::UnboundedReceiver<EngineEvent>,
}

/// Starts the engine task. It runs until `Shutdown` arrives or every
/// command sender is dropped.
pub fn spawn_engine<B: AudioBackend + 'static>(
    backend: B,
    backend_events: mpsc::UnboundedReceiver<BackendEvent>,
    url_for: UrlFor,
) -> EngineHandle {
    let (commands, command_rx) = mpsc::unbounded_channel();
    let engine = engine::Engine::new(backend, url_for);
    let (snapshot_tx, snapshot) = watch::channel(engine.snapshot());
    let (events_tx, events) = mpsc::unbounded_channel();
    tokio::spawn(engine::run(
        engine,
        command_rx,
        backend_events,
        snapshot_tx,
        events_tx,
    ));
    EngineHandle {
        commands,
        snapshot,
        events,
    }
}
