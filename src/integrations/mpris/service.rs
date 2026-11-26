//! The MPRIS objects on the session bus.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use zbus::object_server::{InterfaceRef, SignalEmitter};
use zbus::{ObjectPath, OwnedVariant, Variant};
use zbus::{connection, interface};

use super::control::{Control, commands};
use super::state::{Change, Metadata, MprisState};
use crate::player::{EngineEvent, PlayerCommand};

pub const BUS_NAME: &str = "org.mpris.MediaPlayer2.deadwax";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";

/// How long `Mpris::stop` waits for the name to be released.
const STOP_GRACE: Duration = Duration::from_secs(1);

/// Finds the local file for a cover art id. `None` when there is none.
pub type CoverLookup = Arc<dyn Fn(String) -> BoxFuture<'static, Option<PathBuf>> + Send + Sync>;

/// The running service. Engine events sent to `events` update what the bus
/// shows.
pub struct Mpris {
    pub events: mpsc::UnboundedSender<EngineEvent>,
    task: JoinHandle<()>,
}

impl Mpris {
    /// Ends the service and gives up the bus name.
    pub async fn stop(self) {
        drop(self.events);
        if tokio::time::timeout(STOP_GRACE, self.task).await.is_err() {
            tracing::warn!("MPRIS did not stop in time");
        }
    }
}

/// The session bus, as named by the environment.
pub fn session_bus() -> connection::Builder<'static> {
    connection::Builder::session()
}

/// Connects through `bus`, takes the MPRIS name, and serves the player.
/// Returns `None`, after one warning in the log, when that fails.
pub async fn spawn(
    bus: connection::Builder<'static>,
    player: mpsc::UnboundedSender<PlayerCommand>,
    cover: CoverLookup,
) -> Option<Mpris> {
    let shared = Arc::new(Shared {
        state: Mutex::default(),
        player,
    });
    let conn = bus
        .name(BUS_NAME)
        .serve_at(OBJECT_PATH, Root)
        .serve_at(
            OBJECT_PATH,
            Player {
                shared: Arc::clone(&shared),
            },
        )
        .build()
        .await;
    let conn = match conn {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("MPRIS unavailable: {e}");
            return None;
        }
    };
    let iface = match conn
        .object_server()
        .interface::<_, Player>(OBJECT_PATH)
        .await
    {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("MPRIS player interface missing: {e}");
            return None;
        }
    };
    let (events, rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(run(conn, iface, shared, rx, cover));
    Some(Mpris { events, task })
}

/// Applies engine events and covers until the event sender drops, then
/// releases the name.
async fn run(
    conn: zbus::Connection,
    iface: InterfaceRef<Player>,
    shared: Arc<Shared>,
    mut events: mpsc::UnboundedReceiver<EngineEvent>,
    cover: CoverLookup,
) {
    let (cover_tx, mut covers) = mpsc::unbounded_channel::<(u64, PathBuf)>();
    loop {
        let changes = tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { break };
                let mut state = shared.lock();
                let changes = state.apply(&ev);
                // A lookup for a skipped track still finishes, and
                // `set_cover` drops its result.
                if let (EngineEvent::TrackStarted(_), Some(id)) = (&ev, state.cover_id()) {
                    let lookup = cover(id.to_string());
                    let generation = state.generation();
                    let tx = cover_tx.clone();
                    tokio::spawn(async move {
                        if let Some(path) = lookup.await {
                            let _ = tx.send((generation, path));
                        }
                    });
                }
                changes
            }
            Some((generation, path)) = covers.recv() => {
                if shared.lock().set_cover(generation, &path) {
                    vec![Change::Metadata]
                } else {
                    Vec::new()
                }
            }
        };
        for change in changes {
            if let Err(e) = announce(&iface, change).await {
                tracing::debug!("MPRIS signal failed: {e}");
            }
        }
    }
    if let Err(e) = conn.release_name(BUS_NAME).await {
        tracing::debug!("releasing the MPRIS name failed: {e}");
    }
}

async fn announce(iface: &InterfaceRef<Player>, change: Change) -> zbus::Result<()> {
    let emitter = iface.signal_emitter();
    let player = iface.get().await;
    match change {
        Change::Metadata => {
            player.metadata_changed(emitter).await?;
            player.can_play_changed(emitter).await?;
            player.can_pause_changed(emitter).await?;
            player.can_seek_changed(emitter).await?;
            player.can_go_next_changed(emitter).await?;
            player.can_go_previous_changed(emitter).await?;
        }
        Change::Status => player.playback_status_changed(emitter).await?,
        Change::Seeked(us) => Player::seeked(emitter, us).await?,
    }
    Ok(())
}

struct Shared {
    state: Mutex<MprisState>,
    player: mpsc::UnboundedSender<PlayerCommand>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, MprisState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn control(&self, c: Control) {
        for cmd in commands(c, &self.lock()) {
            let _ = self.player.send(cmd);
        }
    }
}

fn owned<'a>(v: impl Into<Variant<'a>>) -> OwnedVariant {
    v.into()
        .try_into_owned()
        .expect("metadata values hold no file descriptors")
}

fn to_dbus(m: Metadata) -> HashMap<String, OwnedVariant> {
    let mut map = HashMap::new();
    let track_id = ObjectPath::try_from(m.track_id).expect("track ids are object paths");
    map.insert("mpris:trackid".to_string(), owned(track_id));
    if let Some(len) = m.length_us {
        map.insert("mpris:length".to_string(), owned(len));
    }
    if let Some(title) = m.title {
        map.insert("xesam:title".to_string(), owned(title));
    }
    if !m.artists.is_empty() {
        map.insert("xesam:artist".to_string(), owned(m.artists));
    }
    if let Some(album) = m.album {
        map.insert("xesam:album".to_string(), owned(album));
    }
    if let Some(url) = m.art_url {
        map.insert("mpris:artUrl".to_string(), owned(url));
    }
    map
}

/// `org.mpris.MediaPlayer2`. Deadwax runs in a terminal, so it can't be
/// raised, and quitting is left to its own keys.
struct Root;

#[expect(
    clippy::unused_self,
    reason = "zbus calls every interface member through &self"
)]
#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}

    fn quit(&self) {}

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_quit(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_raise(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn identity(&self) -> &'static str {
        "Deadwax"
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn desktop_entry(&self) -> &'static str {
        "deadwax"
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// `org.mpris.MediaPlayer2.Player`.
struct Player {
    shared: Arc<Shared>,
}

#[expect(
    clippy::unused_self,
    reason = "zbus calls every interface member through &self"
)]
#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.shared.control(Control::Next);
    }

    fn previous(&self) {
        self.shared.control(Control::Previous);
    }

    fn pause(&self) {
        self.shared.control(Control::Pause);
    }

    fn play_pause(&self) {
        self.shared.control(Control::PlayPause);
    }

    fn stop(&self) {
        self.shared.control(Control::Stop);
    }

    fn play(&self) {
        self.shared.control(Control::Play);
    }

    fn seek(&self, offset: i64) {
        self.shared.control(Control::Seek(offset));
    }

    fn set_position(&self, track_id: &ObjectPath<'_>, position: i64) {
        self.shared.control(Control::SetPosition {
            track_id: track_id.as_str().to_string(),
            position_us: position,
        });
    }

    /// Deadwax only plays from its server, so there is nothing to open.
    fn open_uri(&self, uri: &str) {
        let _ = uri;
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> &'static str {
        self.shared.lock().status().as_str()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn minimum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn maximum_rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedVariant> {
        to_dbus(self.shared.lock().metadata())
    }

    /// Clients work the position out from `Rate` between `Seeked`
    /// signals, so changes are not signalled.
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.shared.lock().position_us()
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.shared.lock().has_track()
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.shared.lock().has_track()
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.shared.lock().has_track()
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.shared.lock().has_track()
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.shared.lock().has_track()
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }
}
