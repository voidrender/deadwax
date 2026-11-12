//! The player engine keeps the play queue and the audio backend in step.
//!
//! The backend holds at most two entries the engine cares about: the one
//! playing (`loaded_current`) and one appended after it (`loaded_next`) so
//! mpv can switch gaplessly. Events name entries by id, and any id that is
//! neither of those two is stale and ignored.

use crate::player::backend::{AudioBackend, BackendEvent, EndReason, EntryId, LoadMode};
use crate::player::queue::{Queue, Removed, RepeatMode};
use crate::player::{EngineEvent, PlayerCommand, PlayerSnapshot, Status, UrlFor};
use std::collections::VecDeque;
use tokio::sync::{mpsc, watch};

/// `Prev` restarts the current track instead when it has played this long.
const PREV_RESTART_SECS: f64 = 3.0;

pub struct Engine<B> {
    backend: B,
    queue: Queue,
    url_for: UrlFor,
    /// Backend entry id and queue uid of the playing entry.
    loaded_current: Option<(EntryId, u64)>,
    /// Backend entry id and queue uid of the gapless preload.
    loaded_next: Option<(EntryId, u64)>,
    status: Status,
    position: f64,
    duration: f64,
    volume: u8,
    /// `FileStarted` has arrived for `loaded_current`.
    current_started: bool,
    /// Emit `TrackStarted` when `loaded_current` starts. Off for crash
    /// recovery, which resumes a track that was already announced.
    announce: bool,
    /// `PlaybackRestart` has arrived for `loaded_current`, so seeks work.
    ready: bool,
    /// A seek requested before `ready`, applied on `PlaybackRestart`.
    pending_seek: Option<f64>,
    /// `loaded_current` finished and mpv should be moving on to `loaded_next`.
    current_ended: bool,
    /// Stream errors since something last played, to stop error loops.
    consecutive_errors: usize,
    /// The backend crashed and could not be restarted.
    backend_dead: bool,
    /// mpv's pause state as last set by the engine or reported by mpv.
    mpv_paused: bool,
    /// Pause changes the engine made that mpv has not reported back yet.
    pause_echoes: VecDeque<bool>,
    /// The next pause report is the initial one from a fresh process.
    skip_pause_report: bool,
    /// The queue changed since the last snapshot copied its items.
    items_dirty: bool,
    shut_down: bool,
}

impl<B: AudioBackend> Engine<B> {
    pub fn new(backend: B, url_for: UrlFor) -> Self {
        Self {
            backend,
            queue: Queue::new(),
            url_for,
            loaded_current: None,
            loaded_next: None,
            status: Status::Stopped,
            position: 0.0,
            duration: 0.0,
            volume: 100,
            current_started: false,
            announce: false,
            ready: false,
            pending_seek: None,
            current_ended: false,
            consecutive_errors: 0,
            backend_dead: false,
            mpv_paused: false,
            pause_echoes: VecDeque::new(),
            skip_pause_report: true,
            items_dirty: true,
            shut_down: false,
        }
    }

    pub fn is_shut_down(&self) -> bool {
        self.shut_down
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        let mut snap = PlayerSnapshot::default();
        self.fill_snapshot(&mut snap, true);
        snap
    }

    /// Brings `snap` up to date and reports whether anything changed. Queue
    /// items are only copied when the queue changed.
    pub fn update_snapshot(&mut self, snap: &mut PlayerSnapshot) -> bool {
        let changed = self.fill_snapshot(snap, self.items_dirty);
        self.items_dirty = false;
        changed
    }

    fn fill_snapshot(&self, snap: &mut PlayerSnapshot, with_items: bool) -> bool {
        let mut changed = false;
        if with_items {
            let items: Vec<_> = self.queue.display_items().into_iter().cloned().collect();
            if snap.items != items {
                snap.items = items;
                changed = true;
            }
        }
        let scalars = (
            self.queue.current_display_index(),
            self.status,
            self.position,
            self.duration,
            self.volume,
            self.queue.shuffle,
            self.queue.repeat,
        );
        let old = (
            snap.current,
            snap.status,
            snap.position,
            snap.duration,
            snap.volume,
            snap.shuffle,
            snap.repeat,
        );
        if old != scalars {
            (
                snap.current,
                snap.status,
                snap.position,
                snap.duration,
                snap.volume,
                snap.shuffle,
                snap.repeat,
            ) = scalars;
            changed = true;
        }
        changed
    }

    pub async fn handle_command(&mut self, cmd: PlayerCommand) -> Vec<EngineEvent> {
        let mut ev = Vec::new();
        self.items_dirty = true;
        match cmd {
            PlayerCommand::PlayQueue { songs, start } => {
                self.queue.replace(songs, start);
                self.play_current(None, false, &mut ev).await;
            }
            PlayerCommand::Append(songs) => {
                self.queue.append(songs);
                self.sync_next().await;
            }
            PlayerCommand::PlayNext(songs) => {
                self.queue.insert_next(songs);
                self.sync_next().await;
            }
            PlayerCommand::TogglePause => self.toggle_pause(&mut ev).await,
            PlayerCommand::Next => {
                let has_next = self.queue.pos.is_some_and(|p| {
                    p + 1 < self.queue.order.len() || self.queue.repeat == RepeatMode::All
                });
                if has_next {
                    self.queue.next();
                    self.play_current(None, self.status == Status::Paused, &mut ev)
                        .await;
                }
            }
            PlayerCommand::Prev => self.prev(&mut ev).await,
            PlayerCommand::SeekRelative(delta) => {
                if self.loaded_current.is_some() {
                    let mut target = (self.position + delta).max(0.0);
                    if self.duration > 0.0 {
                        target = target.min(self.duration);
                    }
                    self.seek(target, &mut ev).await;
                }
            }
            PlayerCommand::SeekTo(secs) => self.seek(secs.max(0.0), &mut ev).await,
            PlayerCommand::SetVolume(vol) => self.set_volume(vol, &mut ev).await,
            PlayerCommand::ToggleShuffle => {
                self.queue.set_shuffle(!self.queue.shuffle);
                self.sync_next().await;
            }
            PlayerCommand::CycleRepeat => {
                self.queue.cycle_repeat();
                self.sync_next().await;
            }
            PlayerCommand::JumpTo(idx) => {
                if self.queue.jump_to(idx).is_some() {
                    self.play_current(None, false, &mut ev).await;
                }
            }
            PlayerCommand::Remove(idx) => match self.queue.remove(idx) {
                Removed::Current if self.loaded_current.is_some() => {
                    if self.queue.current().is_some() {
                        let paused = self.status == Status::Paused;
                        self.play_current(None, paused, &mut ev).await;
                    } else {
                        self.stop(true, &mut ev).await;
                    }
                }
                Removed::Current | Removed::Other => self.sync_next().await,
                Removed::None => {}
            },
            PlayerCommand::Move { from, to } => {
                self.queue.move_item(from, to);
                self.sync_next().await;
            }
            PlayerCommand::Clear => {
                self.queue.clear();
                self.stop(true, &mut ev).await;
            }
            PlayerCommand::Restore(saved) => {
                let (position, volume) = (saved.position, saved.volume);
                self.queue = Queue::from_persisted(saved);
                self.set_volume(volume, &mut ev).await;
                if self.queue.current().is_some() {
                    self.play_current(Some(position), true, &mut ev).await;
                } else {
                    self.stop(true, &mut ev).await;
                }
            }
            PlayerCommand::SetStarred { song_id, starred } => {
                for item in &mut self.queue.items {
                    if item.song.id == song_id {
                        item.song.starred = starred;
                    }
                }
            }
            PlayerCommand::Shutdown => {
                if self.loaded_current.is_some()
                    && let Err(e) = self.backend.stop().await
                {
                    tracing::warn!("stopping backend at shutdown failed: {e:#}");
                }
                self.shut_down = true;
            }
        }
        ev
    }

    pub async fn handle_backend(&mut self, event: BackendEvent) -> Vec<EngineEvent> {
        let mut ev = Vec::new();
        match event {
            BackendEvent::FileStarted(id) => self.file_started(id, &mut ev).await,
            BackendEvent::PlaybackRestart => {
                if self.current_started {
                    self.ready = true;
                    self.consecutive_errors = 0;
                    if let Some(secs) = self.pending_seek.take() {
                        self.seek_now(secs).await;
                    }
                }
            }
            BackendEvent::FileEnded { entry, reason } => {
                if self.is_current(entry) {
                    self.current_ended(reason, &mut ev).await;
                }
            }
            BackendEvent::Position(pos) => {
                if self.current_started {
                    self.position = pos;
                    ev.push(EngineEvent::Position {
                        pos,
                        paused: self.status == Status::Paused,
                    });
                }
            }
            BackendEvent::Duration(d) => {
                if self.current_started {
                    self.duration = d;
                }
            }
            BackendEvent::Paused(paused) => self.pause_reported(paused, &mut ev),
            BackendEvent::Idle => {
                // mpv ran out after the current track ended, so the switch to
                // the preload never happened (a removal raced the gapless
                // switch). Carry on with the queue as if it had.
                if self.current_ended && self.loaded_current.is_some() {
                    tracing::warn!("backend went idle before the preloaded track started");
                    self.loaded_current = None;
                    self.loaded_next = None;
                    self.items_dirty = true;
                    if self.queue.advance().is_some() {
                        self.play_current(None, false, &mut ev).await;
                    } else {
                        self.stop(false, &mut ev).await;
                    }
                }
            }
            BackendEvent::Crashed => self.crashed(&mut ev).await,
        }
        ev
    }

    /// Adopts pause changes made outside the engine (media keys, mpv
    /// scripts, user config). Echoes of the engine's own changes and the
    /// initial report of a fresh process are not adopted.
    fn pause_reported(&mut self, paused: bool, ev: &mut Vec<EngineEvent>) {
        let initial = std::mem::take(&mut self.skip_pause_report);
        self.mpv_paused = paused;
        if let Some(at) = self.pause_echoes.iter().position(|&p| p == paused) {
            // mpv may coalesce rapid changes, so drop any skipped echoes too.
            self.pause_echoes.drain(..=at);
            return;
        }
        let engine_paused = self.status == Status::Paused;
        if initial || !self.current_started || paused == engine_paused {
            return;
        }
        tracing::info!(paused, "pause changed outside the engine");
        self.pause_echoes.clear();
        self.status = if paused {
            Status::Paused
        } else {
            Status::Playing
        };
        ev.push(EngineEvent::PauseChanged(paused));
    }

    /// Records a pause change the engine made so its echo is recognized.
    fn pause_set(&mut self, paused: bool) {
        if paused != self.mpv_paused {
            self.mpv_paused = paused;
            self.pause_echoes.push_back(paused);
        }
    }

    /// Resets pause tracking for a freshly started backend process.
    fn backend_restarted(&mut self) {
        self.mpv_paused = false;
        self.pause_echoes.clear();
        self.skip_pause_report = true;
    }

    fn is_current(&self, entry: EntryId) -> bool {
        self.loaded_current.is_some_and(|(id, _)| id == entry)
    }

    async fn file_started(&mut self, id: EntryId, ev: &mut Vec<EngineEvent>) {
        if let Some((next_id, next_uid)) = self.loaded_next
            && next_id == id
        {
            self.items_dirty = true;
            let advanced = self.queue.advance().map(|i| i.uid);
            if advanced != Some(next_uid) {
                // The queue and the backend disagree about what comes next.
                // Load whatever the queue says is current. The loaded ids
                // stay recorded so that, when the queue has ended, stopping
                // still halts the stale track mpv just started.
                tracing::warn!("preloaded track does not match the queue; reloading");
                self.play_current(None, self.status == Status::Paused, ev)
                    .await;
                return;
            }
            self.loaded_current = self.loaded_next.take();
            self.begin_track(None, true);
            self.mark_started(ev);
            self.sync_next().await;
        } else if self.is_current(id) && !self.current_started {
            self.mark_started(ev);
        } else {
            tracing::debug!(id, "ignoring stale file start");
        }
    }

    fn mark_started(&mut self, ev: &mut Vec<EngineEvent>) {
        self.current_started = true;
        if std::mem::take(&mut self.announce)
            && let Some(item) = self.queue.current()
        {
            tracing::info!(song_id = %item.song.id, title = %item.song.title, "track started");
            ev.push(EngineEvent::TrackStarted(item.song.clone()));
        }
    }

    async fn current_ended(&mut self, reason: EndReason, ev: &mut Vec<EngineEvent>) {
        match reason {
            EndReason::Eof => {
                if self.loaded_next.is_some() {
                    self.current_ended = true;
                } else if self.queue.peek_next().is_some() {
                    // The preload is missing (its append failed), so start
                    // the next track directly.
                    self.loaded_current = None;
                    self.items_dirty = true;
                    self.queue.advance();
                    self.play_current(None, false, ev).await;
                } else {
                    self.loaded_current = None;
                    self.stop(false, ev).await;
                }
            }
            EndReason::Error(msg) => {
                let title = self
                    .queue
                    .current()
                    .map(|i| i.song.title.clone())
                    .unwrap_or_default();
                tracing::warn!(%title, "stream failed: {msg}");
                ev.push(EngineEvent::Error(format!("Couldn't play {title}: {msg}")));
                self.consecutive_errors += 1;
                let current_uid = self.loaded_current.map(|(_, uid)| uid);
                let moves_on = self
                    .loaded_next
                    .is_some_and(|(_, uid)| Some(uid) != current_uid)
                    && self.consecutive_errors < self.queue.items.len();
                if moves_on {
                    self.current_ended = true;
                } else {
                    self.stop(true, ev).await;
                }
            }
            EndReason::Stop | EndReason::Other => {
                // The engine replaces or clears its own entries before mpv
                // reports them ended, so this one was stopped from elsewhere
                // (a removal racing the gapless switch). Reload where we were.
                tracing::warn!("current track stopped unexpectedly; reloading");
                let paused = self.status == Status::Paused;
                let at = self.position;
                self.play_current_from(Some(at), paused, false, ev).await;
            }
            // The process is going away. `Crashed` follows and recovers.
            EndReason::Quit => {}
        }
    }

    async fn crashed(&mut self, ev: &mut Vec<EngineEvent>) {
        tracing::warn!("audio backend crashed; restarting");
        let resume = self.loaded_current.is_some() && self.queue.current().is_some();
        let paused = self.status == Status::Paused;
        let at = self.position;
        self.loaded_current = None;
        self.loaded_next = None;
        if let Err(e) = self.backend.restart().await {
            tracing::error!("audio backend restart failed: {e:#}");
            self.backend_dead = true;
            ev.push(EngineEvent::Error(format!("Audio playback stopped: {e}")));
            self.stop(false, ev).await;
            return;
        }
        self.backend_restarted();
        if resume {
            self.play_current_from(Some(at), paused, false, ev).await;
        }
    }

    async fn toggle_pause(&mut self, ev: &mut Vec<EngineEvent>) {
        match self.status {
            Status::Stopped => {
                if self.queue.current().is_none() && self.queue.jump_to(0).is_none() {
                    return;
                }
                self.play_current(None, false, ev).await;
            }
            Status::Playing | Status::Paused => {
                let paused = self.status == Status::Playing;
                match self.backend.set_pause(paused).await {
                    Ok(()) => {
                        self.pause_set(paused);
                        self.status = if paused {
                            Status::Paused
                        } else {
                            Status::Playing
                        };
                        ev.push(EngineEvent::PauseChanged(paused));
                    }
                    Err(e) => tracing::warn!("set pause failed: {e:#}"),
                }
            }
        }
    }

    async fn prev(&mut self, ev: &mut Vec<EngineEvent>) {
        if self.loaded_current.is_some() && self.position > PREV_RESTART_SECS {
            return self.seek(0.0, ev).await;
        }
        let before = self.queue.current().map(|i| i.uid);
        let after = self.queue.prev().map(|i| i.uid);
        if after.is_none() {
            return;
        }
        if before != after || self.loaded_current.is_none() {
            self.play_current(None, self.status == Status::Paused, ev)
                .await;
        } else {
            self.seek(0.0, ev).await;
        }
    }

    async fn seek(&mut self, secs: f64, ev: &mut Vec<EngineEvent>) {
        if self.loaded_current.is_none() {
            return;
        }
        self.position = secs;
        ev.push(EngineEvent::Seeked(secs));
        if self.ready {
            self.seek_now(secs).await;
        } else {
            // mpv rejects seeks until the file has started decoding.
            self.pending_seek = Some(secs);
        }
    }

    async fn seek_now(&mut self, secs: f64) {
        if let Err(e) = self.backend.seek_to(secs).await {
            tracing::warn!("seek failed: {e:#}");
        }
    }

    async fn set_volume(&mut self, vol: u8, ev: &mut Vec<EngineEvent>) {
        let vol = vol.min(100);
        match self.backend.set_volume(vol).await {
            Ok(()) => self.volume = vol,
            Err(e) => {
                tracing::warn!("set volume failed: {e:#}");
                if !self.backend_dead {
                    ev.push(EngineEvent::Error("Couldn't change the volume".into()));
                }
            }
        }
    }

    async fn play_current(&mut self, start: Option<f64>, paused: bool, ev: &mut Vec<EngineEvent>) {
        self.play_current_from(start, paused, true, ev).await;
    }

    /// Loads the queue's current item over whatever the backend is playing,
    /// then preloads the item after it.
    async fn play_current_from(
        &mut self,
        start: Option<f64>,
        paused: bool,
        announce: bool,
        ev: &mut Vec<EngineEvent>,
    ) {
        let Some(item) = self.queue.current() else {
            return self.stop(true, ev).await;
        };
        let (uid, song) = (item.uid, item.song.clone());
        if self.backend_dead {
            if let Err(e) = self.backend.restart().await {
                tracing::error!("audio backend restart failed: {e:#}");
                ev.push(EngineEvent::Error(format!(
                    "Audio playback is unavailable: {e}"
                )));
                return self.stop(false, ev).await;
            }
            self.backend_dead = false;
            self.backend_restarted();
        }
        let url = (self.url_for)(&song);
        // Replace clears the backend playlist, preload included.
        self.loaded_next = None;
        match self
            .backend
            .load(&url, LoadMode::Replace { start, paused })
            .await
        {
            Ok(id) => {
                tracing::info!(song_id = %song.id, title = %song.title, "loading track");
                self.loaded_current = Some((id, uid));
                self.pause_set(paused);
                self.begin_track(start, announce);
                self.status = if paused {
                    Status::Paused
                } else {
                    Status::Playing
                };
                self.sync_next().await;
            }
            Err(e) => {
                tracing::warn!(song_id = %song.id, "load failed: {e:#}");
                ev.push(EngineEvent::Error(format!(
                    "Couldn't play {}: {e}",
                    song.title
                )));
                // mpv may still be playing the old track, so halt it even
                // though nothing is recorded as loaded any more.
                if let Err(e) = self.backend.stop().await {
                    tracing::warn!("stopping backend failed: {e:#}");
                }
                self.loaded_current = None;
                self.stop(false, ev).await;
            }
        }
    }

    /// Resets per-track state when `loaded_current` changes.
    fn begin_track(&mut self, start: Option<f64>, announce: bool) {
        self.current_started = false;
        self.announce = announce;
        self.ready = false;
        self.pending_seek = None;
        self.current_ended = false;
        self.position = start.unwrap_or(0.0);
        if let Some(item) = self.queue.current() {
            self.duration = f64::from(item.song.duration);
        }
    }

    /// Makes the backend's preload match `queue.peek_next()`.
    async fn sync_next(&mut self) {
        if self.loaded_current.is_none() {
            return;
        }
        let want = self.queue.peek_next().map(|i| (i.uid, i.song.clone()));
        if want.as_ref().map(|(uid, _)| *uid) == self.loaded_next.map(|(_, uid)| uid) {
            return;
        }
        if let Some((id, _)) = self.loaded_next {
            if let Err(e) = self.backend.remove_entry(id).await {
                // Keep it recorded. If it plays, the queue check in
                // file_started reloads the right track.
                tracing::warn!("removing preload failed: {e:#}");
                return;
            }
            self.loaded_next = None;
        }
        if let Some((uid, song)) = want {
            let url = (self.url_for)(&song);
            match self.backend.load(&url, LoadMode::Append).await {
                Ok(id) => self.loaded_next = Some((id, uid)),
                Err(e) => tracing::warn!(song_id = %song.id, "preload failed: {e:#}"),
            }
        }
    }

    /// Goes to Stopped, telling the backend to stop when `halt_backend` and
    /// something is loaded. Emits `Stopped` if the status changed.
    async fn stop(&mut self, halt_backend: bool, ev: &mut Vec<EngineEvent>) {
        let loaded = self.loaded_current.is_some() || self.loaded_next.is_some();
        self.loaded_current = None;
        self.loaded_next = None;
        if halt_backend
            && loaded
            && let Err(e) = self.backend.stop().await
        {
            tracing::warn!("stopping backend failed: {e:#}");
        }
        self.current_started = false;
        self.announce = false;
        self.ready = false;
        self.pending_seek = None;
        self.current_ended = false;
        self.position = 0.0;
        if self.status != Status::Stopped {
            self.status = Status::Stopped;
            ev.push(EngineEvent::Stopped);
        }
    }
}

/// The engine task. Each command or backend event is handled to completion
/// before the next is read, and backend calls only happen in the loop body,
/// never inside a `select!` branch that could be dropped mid-call.
pub async fn run<B: AudioBackend>(
    mut engine: Engine<B>,
    mut commands: mpsc::UnboundedReceiver<PlayerCommand>,
    mut backend_events: mpsc::UnboundedReceiver<BackendEvent>,
    snapshot: watch::Sender<PlayerSnapshot>,
    events: mpsc::UnboundedSender<EngineEvent>,
) {
    let mut backend_open = true;
    loop {
        let out = tokio::select! {
            biased;
            ev = backend_events.recv(), if backend_open => match ev {
                Some(ev) => engine.handle_backend(ev).await,
                None => {
                    backend_open = false;
                    continue;
                }
            },
            cmd = commands.recv() => match cmd {
                Some(cmd) => engine.handle_command(cmd).await,
                None => engine.handle_command(PlayerCommand::Shutdown).await,
            },
        };
        snapshot.send_if_modified(|snap| engine.update_snapshot(snap));
        for ev in out {
            let _ = events.send(ev);
        }
        if engine.is_shut_down() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::persist::PersistedQueue;
    use crate::subsonic::Song;
    use std::sync::Arc;

    #[derive(Clone, Debug, PartialEq)]
    enum Call {
        Load(String, LoadMode),
        Remove(EntryId),
        Pause(bool),
        Seek(f64),
        Volume(u8),
        Stop,
        Restart,
    }

    #[derive(Default)]
    struct FakeBackend {
        calls: Vec<Call>,
        next_id: i64,
        fail_restart: bool,
        fail_load: bool,
        fail_remove: bool,
    }

    impl AudioBackend for FakeBackend {
        async fn load(&mut self, url: &str, mode: LoadMode) -> anyhow::Result<EntryId> {
            self.calls.push(Call::Load(url.into(), mode));
            if self.fail_load {
                anyhow::bail!("mpv loadfile: no reply");
            }
            self.next_id += 1;
            Ok(self.next_id)
        }
        async fn remove_entry(&mut self, id: EntryId) -> anyhow::Result<()> {
            self.calls.push(Call::Remove(id));
            if self.fail_remove {
                anyhow::bail!("mpv get_property: no reply");
            }
            Ok(())
        }
        async fn set_pause(&mut self, paused: bool) -> anyhow::Result<()> {
            self.calls.push(Call::Pause(paused));
            Ok(())
        }
        async fn seek_to(&mut self, secs: f64) -> anyhow::Result<()> {
            self.calls.push(Call::Seek(secs));
            Ok(())
        }
        async fn set_volume(&mut self, vol: u8) -> anyhow::Result<()> {
            self.calls.push(Call::Volume(vol));
            Ok(())
        }
        async fn stop(&mut self) -> anyhow::Result<()> {
            self.calls.push(Call::Stop);
            Ok(())
        }
        async fn restart(&mut self) -> anyhow::Result<()> {
            self.calls.push(Call::Restart);
            if self.fail_restart {
                anyhow::bail!("mpv is gone");
            }
            self.next_id = 0;
            Ok(())
        }
    }

    const PLAY: LoadMode = LoadMode::Replace {
        start: None,
        paused: false,
    };

    fn load(url: &str, mode: LoadMode) -> Call {
        Call::Load(url.into(), mode)
    }

    fn append(url: &str) -> Call {
        load(url, LoadMode::Append)
    }

    fn song(id: &str) -> Song {
        Song {
            id: id.into(),
            title: format!("t{id}"),
            artist: String::new(),
            artist_id: None,
            album: String::new(),
            album_id: None,
            track: None,
            year: None,
            duration: 200,
            cover_art: None,
            suffix: None,
            bit_rate: None,
            starred: false,
        }
    }

    fn songs(n: usize) -> Vec<Song> {
        (0..n).map(|i| song(&i.to_string())).collect()
    }

    fn engine() -> Engine<FakeBackend> {
        let url_for: UrlFor = Arc::new(|s: &Song| format!("u:{}", s.id));
        let mut e = Engine::new(FakeBackend::default(), url_for);
        e.queue = Queue::with_seed(1);
        e
    }

    fn ended(entry: EntryId, reason: EndReason) -> BackendEvent {
        BackendEvent::FileEnded { entry, reason }
    }

    /// A queue of `n` songs with the first one audibly playing and the
    /// recorded calls cleared. Song 0 is entry 1 and song 1 is entry 2.
    async fn playing(n: usize) -> Engine<FakeBackend> {
        let mut e = engine();
        // A fresh mpv reports pause=false first.
        e.handle_backend(BackendEvent::Paused(false)).await;
        e.handle_command(PlayerCommand::PlayQueue {
            songs: songs(n),
            start: 0,
        })
        .await;
        e.handle_backend(BackendEvent::FileStarted(1)).await;
        e.handle_backend(BackendEvent::PlaybackRestart).await;
        e.backend.calls.clear();
        e
    }

    fn current_id(e: &Engine<FakeBackend>) -> Option<String> {
        e.queue.current().map(|i| i.song.id.clone())
    }

    #[tokio::test]
    async fn play_queue_loads_current_and_preloads_next() {
        let mut e = engine();
        e.handle_command(PlayerCommand::PlayQueue {
            songs: songs(3),
            start: 0,
        })
        .await;
        assert_eq!(e.backend.calls, [load("u:0", PLAY), append("u:1")]);
        assert_eq!(e.snapshot().status, Status::Playing);
        let ev = e.handle_backend(BackendEvent::FileStarted(1)).await;
        assert_eq!(ev, [EngineEvent::TrackStarted(song("0"))]);
    }

    #[tokio::test]
    async fn natural_advance_on_next_start() {
        let mut e = playing(3).await;
        assert!(e.handle_backend(ended(1, EndReason::Eof)).await.is_empty());
        let ev = e.handle_backend(BackendEvent::FileStarted(2)).await;
        assert_eq!(ev, [EngineEvent::TrackStarted(song("1"))]);
        assert_eq!(e.backend.calls, [append("u:2")]);
        assert_eq!(e.snapshot().current, Some(1));
    }

    #[tokio::test]
    async fn eof_at_end_of_queue_stops() {
        let mut e = playing(1).await;
        let ev = e.handle_backend(ended(1, EndReason::Eof)).await;
        assert_eq!(ev, [EngineEvent::Stopped]);
        assert_eq!(e.snapshot().status, Status::Stopped);
        assert!(e.handle_backend(BackendEvent::Idle).await.is_empty());
        assert!(e.backend.calls.is_empty());
    }

    #[tokio::test]
    async fn stream_error_reports_and_continues() {
        let mut e = playing(3).await;
        let ev = e
            .handle_backend(ended(1, EndReason::Error("loading failed".into())))
            .await;
        assert_eq!(
            ev,
            [EngineEvent::Error(
                "Couldn't play t0: loading failed".into()
            )]
        );
        let ev = e.handle_backend(BackendEvent::FileStarted(2)).await;
        assert_eq!(ev, [EngineEvent::TrackStarted(song("1"))]);
        assert_eq!(e.backend.calls, [append("u:2")]);
    }

    #[tokio::test]
    async fn error_on_repeat_one_does_not_loop() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::CycleRepeat).await;
        e.handle_command(PlayerCommand::CycleRepeat).await;
        assert_eq!(e.queue.repeat, RepeatMode::One);
        assert_eq!(e.backend.calls, [Call::Remove(2), append("u:0")]);
        e.backend.calls.clear();

        let ev = e
            .handle_backend(ended(1, EndReason::Error("boom".into())))
            .await;
        assert!(matches!(ev[0], EngineEvent::Error(ref m) if m.contains("t0")));
        assert_eq!(ev[1..], [EngineEvent::Stopped]);
        assert_eq!(e.backend.calls, [Call::Stop]);
        assert_eq!(e.snapshot().status, Status::Stopped);
        // mpv may already have started the duplicate entry; it is stale now.
        assert!(
            e.handle_backend(BackendEvent::FileStarted(3))
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn errors_through_whole_queue_stop_on_repeat_all() {
        let mut e = engine();
        e.handle_command(PlayerCommand::CycleRepeat).await;
        e.handle_command(PlayerCommand::PlayQueue {
            songs: songs(3),
            start: 0,
        })
        .await;
        let mut stopped = false;
        for id in 1..=3 {
            e.handle_backend(BackendEvent::FileStarted(id)).await;
            let ev = e
                .handle_backend(ended(id, EndReason::Error("404".into())))
                .await;
            stopped = ev.contains(&EngineEvent::Stopped);
        }
        assert!(stopped);
        assert_eq!(e.backend.calls.last(), Some(&Call::Stop));
    }

    #[tokio::test]
    async fn user_next_replaces_and_preloads() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::Next).await;
        assert_eq!(e.backend.calls, [load("u:1", PLAY), append("u:2")]);
        // Replace ends the old entry with Stop; that is not the end of the queue.
        assert!(e.handle_backend(ended(1, EndReason::Stop)).await.is_empty());
        let ev = e.handle_backend(BackendEvent::FileStarted(3)).await;
        assert_eq!(ev, [EngineEvent::TrackStarted(song("1"))]);
        assert_eq!(e.snapshot().status, Status::Playing);
        assert_eq!(e.backend.calls.len(), 2);
    }

    #[tokio::test]
    async fn play_next_resyncs_preload() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::PlayNext(vec![song("x")]))
            .await;
        assert_eq!(e.backend.calls, [Call::Remove(2), append("u:x")]);
    }

    #[tokio::test]
    async fn stale_preload_at_queue_end_stops_backend() {
        let mut e = playing(2).await;
        e.backend.fail_remove = true;
        // Removing the last item cannot drop its preload from mpv.
        e.handle_command(PlayerCommand::Remove(1)).await;
        assert_eq!(e.backend.calls, [Call::Remove(2)]);
        e.backend.calls.clear();
        e.handle_backend(ended(1, EndReason::Eof)).await;
        let ev = e.handle_backend(BackendEvent::FileStarted(2)).await;
        assert_eq!(ev, [EngineEvent::Stopped]);
        assert_eq!(e.backend.calls, [Call::Stop]);
        assert_eq!(e.snapshot().status, Status::Stopped);
    }

    #[tokio::test]
    async fn stale_file_started_is_ignored() {
        let mut e = playing(3).await;
        assert!(
            e.handle_backend(BackendEvent::FileStarted(99))
                .await
                .is_empty()
        );
        assert_eq!(current_id(&e).as_deref(), Some("0"));
        assert!(e.backend.calls.is_empty());
    }

    #[tokio::test]
    async fn rapid_next_ends_consistent() {
        let mut e = playing(10).await;
        for _ in 0..20 {
            e.handle_command(PlayerCommand::Next).await;
        }
        assert_eq!(current_id(&e).as_deref(), Some("9"));
        assert_eq!(e.loaded_next, None);
        assert!(!e.backend.calls.iter().any(|c| matches!(c, Call::Remove(_))));
        let calls = &e.backend.calls;
        for (i, call) in calls.iter().enumerate() {
            if let Call::Load(url, LoadMode::Replace { .. }) = call {
                let n: usize = url[2..].parse().unwrap();
                match calls.get(i + 1) {
                    Some(Call::Load(next, LoadMode::Append)) => {
                        assert_eq!(*next, format!("u:{}", n + 1));
                    }
                    other => {
                        assert_eq!(n, 9);
                        assert_eq!(other, None);
                    }
                }
            }
        }
        assert_eq!(calls.len(), 9 * 2 - 1);
    }

    #[tokio::test]
    async fn crash_restarts_and_resumes() {
        let mut e = playing(3).await;
        e.handle_backend(BackendEvent::Position(42.0)).await;
        e.handle_backend(BackendEvent::Crashed).await;
        assert_eq!(
            e.backend.calls,
            [
                Call::Restart,
                load(
                    "u:0",
                    LoadMode::Replace {
                        start: Some(42.0),
                        paused: false
                    }
                ),
                append("u:1"),
            ]
        );
        // The same track carries on, so it is not announced again.
        assert!(
            e.handle_backend(BackendEvent::FileStarted(1))
                .await
                .is_empty()
        );
        assert_eq!(e.snapshot().status, Status::Playing);
    }

    #[tokio::test]
    async fn crash_while_paused_resumes_paused() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::TogglePause).await;
        e.handle_backend(BackendEvent::Position(7.0)).await;
        e.handle_backend(BackendEvent::Crashed).await;
        // Every new mpv reports pause=false first.
        e.handle_backend(BackendEvent::Paused(false)).await;
        assert!(e.backend.calls.contains(&load(
            "u:0",
            LoadMode::Replace {
                start: Some(7.0),
                paused: true
            }
        )));
        assert_eq!(e.snapshot().status, Status::Paused);
    }

    #[tokio::test]
    async fn failed_restart_reports_and_stops() {
        let mut e = playing(3).await;
        e.backend.fail_restart = true;
        let ev = e.handle_backend(BackendEvent::Crashed).await;
        assert!(matches!(ev[0], EngineEvent::Error(_)));
        assert_eq!(ev[1..], [EngineEvent::Stopped]);
        assert_eq!(e.snapshot().status, Status::Stopped);
        assert_eq!(e.backend.calls, [Call::Restart]);

        // The next play tries to bring the backend back first.
        e.backend.fail_restart = false;
        e.handle_command(PlayerCommand::TogglePause).await;
        assert_eq!(
            e.backend.calls[1..],
            [Call::Restart, load("u:0", PLAY), append("u:1")]
        );
    }

    #[tokio::test]
    async fn failed_load_halts_old_track_and_stops() {
        let mut e = playing(3).await;
        e.backend.fail_load = true;
        let ev = e.handle_command(PlayerCommand::Next).await;
        assert_eq!(
            ev,
            [
                EngineEvent::Error("Couldn't play t1: mpv loadfile: no reply".into()),
                EngineEvent::Stopped,
            ]
        );
        assert_eq!(e.backend.calls, [load("u:1", PLAY), Call::Stop]);
        assert_eq!(e.snapshot().status, Status::Stopped);
    }

    #[tokio::test]
    async fn prev_after_three_seconds_restarts_track() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::JumpTo(1)).await;
        e.handle_backend(BackendEvent::FileStarted(3)).await;
        e.handle_backend(BackendEvent::PlaybackRestart).await;
        e.handle_backend(BackendEvent::Position(5.0)).await;
        e.backend.calls.clear();
        let ev = e.handle_command(PlayerCommand::Prev).await;
        assert_eq!(e.backend.calls, [Call::Seek(0.0)]);
        assert_eq!(ev, [EngineEvent::Seeked(0.0)]);
        assert_eq!(current_id(&e).as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn prev_early_goes_to_previous_track() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::JumpTo(1)).await;
        e.handle_backend(BackendEvent::FileStarted(3)).await;
        e.handle_backend(BackendEvent::Position(2.0)).await;
        e.backend.calls.clear();
        e.handle_command(PlayerCommand::Prev).await;
        assert_eq!(e.backend.calls, [load("u:0", PLAY), append("u:1")]);
        assert_eq!(current_id(&e).as_deref(), Some("0"));
    }

    #[tokio::test]
    async fn toggle_shuffle_resyncs_next() {
        let mut e = playing(10).await;
        e.queue.rng = rand::SeedableRng::seed_from_u64(2);
        e.handle_command(PlayerCommand::ToggleShuffle).await;
        let peek = e.queue.peek_next().unwrap().song.id.clone();
        assert_ne!(peek, "1");
        assert_eq!(
            e.backend.calls,
            [Call::Remove(2), append(&format!("u:{peek}"))]
        );
        assert!(e.snapshot().shuffle);
        let ev = e.handle_backend(BackendEvent::FileStarted(3)).await;
        assert_eq!(ev, [EngineEvent::TrackStarted(song(&peek))]);
    }

    #[tokio::test]
    async fn restore_loads_paused_at_position() {
        let mut e = engine();
        e.handle_command(PlayerCommand::Restore(PersistedQueue {
            songs: songs(3),
            order: vec![0, 1, 2],
            current: Some(1),
            position: 42.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume: 60,
        }))
        .await;
        assert_eq!(
            e.backend.calls,
            [
                Call::Volume(60),
                load(
                    "u:1",
                    LoadMode::Replace {
                        start: Some(42.0),
                        paused: true
                    }
                ),
                append("u:2"),
            ]
        );
        // The observer's initial pause=false from spawn must not unpause us.
        e.handle_backend(BackendEvent::Paused(false)).await;
        e.handle_backend(BackendEvent::Paused(true)).await;
        let ev = e.handle_backend(BackendEvent::FileStarted(1)).await;
        assert_eq!(ev, [EngineEvent::TrackStarted(song("1"))]);
        let snap = e.snapshot();
        assert_eq!(snap.status, Status::Paused);
        assert_eq!(snap.position, 42.0);
        assert_eq!(snap.volume, 60);
        assert_eq!(snap.current, Some(1));

        e.backend.calls.clear();
        let ev = e.handle_command(PlayerCommand::TogglePause).await;
        assert_eq!(e.backend.calls, [Call::Pause(false)]);
        assert_eq!(ev, [EngineEvent::PauseChanged(false)]);
    }

    #[tokio::test]
    async fn external_pause_is_adopted() {
        let mut e = playing(3).await;
        // A media key or mpv script pauses mpv behind the engine's back.
        let ev = e.handle_backend(BackendEvent::Paused(true)).await;
        assert_eq!(ev, [EngineEvent::PauseChanged(true)]);
        assert_eq!(e.snapshot().status, Status::Paused);
        let ev = e.handle_command(PlayerCommand::TogglePause).await;
        assert_eq!(ev, [EngineEvent::PauseChanged(false)]);
        assert_eq!(e.backend.calls, [Call::Pause(false)]);
        assert!(
            e.handle_backend(BackendEvent::Paused(false))
                .await
                .is_empty()
        );
        assert_eq!(e.snapshot().status, Status::Playing);
    }

    #[tokio::test]
    async fn own_pause_echoes_are_ignored() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::TogglePause).await;
        e.handle_command(PlayerCommand::TogglePause).await;
        assert_eq!(e.backend.calls, [Call::Pause(true), Call::Pause(false)]);
        assert!(
            e.handle_backend(BackendEvent::Paused(true))
                .await
                .is_empty()
        );
        assert_eq!(e.snapshot().status, Status::Playing);
        assert!(
            e.handle_backend(BackendEvent::Paused(false))
                .await
                .is_empty()
        );
        assert_eq!(e.snapshot().status, Status::Playing);
        // With the echoes consumed, the next report is external again.
        let ev = e.handle_backend(BackendEvent::Paused(true)).await;
        assert_eq!(ev, [EngineEvent::PauseChanged(true)]);
    }

    #[tokio::test]
    async fn first_pause_report_after_restart_is_ignored() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::TogglePause).await;
        e.handle_backend(BackendEvent::Paused(true)).await;
        e.handle_backend(BackendEvent::Crashed).await;
        assert!(
            e.handle_backend(BackendEvent::Paused(false))
                .await
                .is_empty()
        );
        e.handle_backend(BackendEvent::FileStarted(1)).await;
        assert!(
            e.handle_backend(BackendEvent::Paused(true))
                .await
                .is_empty()
        );
        assert_eq!(e.snapshot().status, Status::Paused);
        e.backend.calls.clear();
        e.handle_command(PlayerCommand::TogglePause).await;
        assert_eq!(e.backend.calls, [Call::Pause(false)]);
    }

    #[tokio::test]
    async fn seek_before_playback_restart_is_deferred() {
        let mut e = engine();
        e.handle_command(PlayerCommand::PlayQueue {
            songs: songs(2),
            start: 0,
        })
        .await;
        e.handle_backend(BackendEvent::FileStarted(1)).await;
        e.backend.calls.clear();
        let ev = e.handle_command(PlayerCommand::SeekTo(30.0)).await;
        assert_eq!(ev, [EngineEvent::Seeked(30.0)]);
        assert!(e.backend.calls.is_empty());
        e.handle_backend(BackendEvent::PlaybackRestart).await;
        assert_eq!(e.backend.calls, [Call::Seek(30.0)]);
        e.handle_backend(BackendEvent::PlaybackRestart).await;
        assert_eq!(e.backend.calls.len(), 1);
    }

    #[tokio::test]
    async fn seek_relative_clamps_to_track() {
        let mut e = playing(2).await;
        e.handle_backend(BackendEvent::Position(5.0)).await;
        e.handle_command(PlayerCommand::SeekRelative(-10.0)).await;
        e.handle_backend(BackendEvent::Duration(100.0)).await;
        e.handle_command(PlayerCommand::SeekRelative(500.0)).await;
        assert_eq!(e.backend.calls, [Call::Seek(0.0), Call::Seek(100.0)]);
    }

    #[tokio::test]
    async fn remove_race_recovers_when_mpv_goes_idle() {
        let mut e = playing(3).await;
        // The gapless switch to entry 2 lands while PlayNext removes it.
        e.handle_command(PlayerCommand::PlayNext(vec![song("x")]))
            .await;
        e.backend.calls.clear();
        assert!(e.handle_backend(ended(1, EndReason::Eof)).await.is_empty());
        assert!(
            e.handle_backend(BackendEvent::FileStarted(2))
                .await
                .is_empty()
        );
        assert!(e.handle_backend(ended(2, EndReason::Stop)).await.is_empty());
        e.handle_backend(BackendEvent::Idle).await;
        assert_eq!(e.backend.calls, [load("u:x", PLAY), append("u:1")]);
        let ev = e.handle_backend(BackendEvent::FileStarted(4)).await;
        assert_eq!(ev, [EngineEvent::TrackStarted(song("x"))]);
        assert_eq!(current_id(&e).as_deref(), Some("x"));
    }

    #[tokio::test]
    async fn remove_race_follows_mpv_onto_new_preload() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::PlayNext(vec![song("x")]))
            .await;
        e.backend.calls.clear();
        e.handle_backend(ended(1, EndReason::Eof)).await;
        e.handle_backend(BackendEvent::FileStarted(2)).await;
        e.handle_backend(ended(2, EndReason::Stop)).await;
        let ev = e.handle_backend(BackendEvent::FileStarted(3)).await;
        assert_eq!(ev, [EngineEvent::TrackStarted(song("x"))]);
        assert_eq!(current_id(&e).as_deref(), Some("x"));
        assert_eq!(e.backend.calls, [append("u:1")]);
    }

    #[tokio::test]
    async fn unrequested_stop_of_current_resyncs() {
        let mut e = playing(3).await;
        e.handle_backend(BackendEvent::Position(10.0)).await;
        let ev = e.handle_backend(ended(1, EndReason::Stop)).await;
        assert!(ev.is_empty());
        assert_eq!(
            e.backend.calls,
            [
                load(
                    "u:0",
                    LoadMode::Replace {
                        start: Some(10.0),
                        paused: false
                    }
                ),
                append("u:1"),
            ]
        );
    }

    #[tokio::test]
    async fn idle_before_new_track_starts_is_ignored() {
        let mut e = playing(1).await;
        e.handle_backend(ended(1, EndReason::Eof)).await;
        e.handle_command(PlayerCommand::JumpTo(0)).await;
        e.backend.calls.clear();
        assert!(e.handle_backend(BackendEvent::Idle).await.is_empty());
        assert!(e.backend.calls.is_empty());
        assert_eq!(e.snapshot().status, Status::Playing);
    }

    #[tokio::test]
    async fn remove_current_plays_following_track() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::Remove(0)).await;
        assert_eq!(e.backend.calls, [load("u:1", PLAY), append("u:2")]);
        assert_eq!(e.snapshot().items.len(), 2);
    }

    #[tokio::test]
    async fn clear_stops_playback() {
        let mut e = playing(3).await;
        let ev = e.handle_command(PlayerCommand::Clear).await;
        assert_eq!(ev, [EngineEvent::Stopped]);
        assert_eq!(e.backend.calls, [Call::Stop]);
        let snap = e.snapshot();
        assert!(snap.items.is_empty());
        assert_eq!(snap.status, Status::Stopped);
        assert!(e.handle_backend(ended(1, EndReason::Stop)).await.is_empty());
        assert!(e.handle_backend(BackendEvent::Idle).await.is_empty());
        assert_eq!(e.backend.calls.len(), 1);
    }

    #[tokio::test]
    async fn set_starred_updates_matching_items() {
        let mut e = playing(3).await;
        e.handle_command(PlayerCommand::Append(vec![song("1")]))
            .await;
        e.handle_command(PlayerCommand::SetStarred {
            song_id: "1".into(),
            starred: true,
        })
        .await;
        let starred: Vec<bool> = e.snapshot().items.iter().map(|i| i.song.starred).collect();
        assert_eq!(starred, [false, true, false, true]);
    }

    #[tokio::test]
    async fn set_starred_publishes_the_new_flag() {
        let mut e = playing(2).await;
        let mut snap = e.snapshot();
        e.update_snapshot(&mut snap);
        e.handle_command(PlayerCommand::SetStarred {
            song_id: "0".into(),
            starred: true,
        })
        .await;
        assert!(e.update_snapshot(&mut snap));
        assert!(snap.items[0].song.starred);
    }

    #[tokio::test]
    async fn spawned_engine_publishes_snapshots() {
        let (tx, rx) = mpsc::unbounded_channel();
        let url_for: UrlFor = Arc::new(|s: &Song| format!("u:{}", s.id));
        let mut h = crate::player::spawn_engine(FakeBackend::default(), rx, url_for);
        h.commands
            .send(PlayerCommand::PlayQueue {
                songs: songs(2),
                start: 0,
            })
            .unwrap();
        h.snapshot.changed().await.unwrap();
        assert_eq!(h.snapshot.borrow_and_update().status, Status::Playing);

        tx.send(BackendEvent::FileStarted(1)).unwrap();
        tx.send(BackendEvent::Position(3.0)).unwrap();
        assert_eq!(
            h.events.recv().await,
            Some(EngineEvent::TrackStarted(song("0")))
        );
        assert_eq!(
            h.events.recv().await,
            Some(EngineEvent::Position {
                pos: 3.0,
                paused: false
            })
        );
        h.snapshot.changed().await.unwrap();
        while h.snapshot.borrow().position != 3.0 {
            h.snapshot.changed().await.unwrap();
        }

        h.commands.send(PlayerCommand::Shutdown).unwrap();
        assert_eq!(h.events.recv().await, None);
    }
}
