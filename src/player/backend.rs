use std::future::Future;

/// mpv's `playlist_entry_id`: unique per entry for the life of one mpv process.
pub type EntryId = i64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoadMode {
    /// Clear the backend playlist and play this file now.
    Replace { start: Option<f64>, paused: bool },
    /// Add this file after the current entries (the gapless preload slot).
    Append,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EndReason {
    Eof,
    Stop,
    Quit,
    Error(String),
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BackendEvent {
    FileStarted(EntryId),
    PlaybackRestart,
    FileEnded {
        entry: EntryId,
        reason: EndReason,
    },
    Position(f64),
    Duration(f64),
    Paused(bool),
    /// The playlist ran out and nothing is loaded.
    Idle,
    /// The backend process died without being asked to.
    Crashed,
}

/// The audio output the player engine drives. Events arrive on the receiver
/// returned when the backend is created.
pub trait AudioBackend: Send {
    /// Loads `url`. `title` is the name the backend reports for the file, so
    /// the stream URL never stands in for it.
    fn load(
        &mut self,
        url: &str,
        title: &str,
        mode: LoadMode,
    ) -> impl Future<Output = anyhow::Result<EntryId>> + Send;
    fn remove_entry(&mut self, id: EntryId) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn set_pause(&mut self, paused: bool) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn seek_to(&mut self, secs: f64) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn set_volume(&mut self, vol: u8) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// Stops playback and clears the backend playlist.
    fn stop(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// Replaces the backend process. Events keep flowing on the same receiver.
    fn restart(&mut self) -> impl Future<Output = anyhow::Result<()>> + Send;
}
