use super::backend::{AudioBackend, BackendEvent, EndReason, EntryId, LoadMode};
use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a dead process's reader gets to forward its last events.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

const OBSERVED: [&str; 4] = ["time-pos", "pause", "duration", "idle-active"];

type Reply = Result<Value, String>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

static SPAWNS: AtomicU64 = AtomicU64::new(0);

/// Returns true when `mpv --version` runs successfully.
pub fn mpv_available() -> bool {
    std::process::Command::new("mpv")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The first spawn in a process uses `senritsu-mpv-<pid>.sock`. Later spawns
/// (restarts, or several backends in one test binary) add a counter so a
/// dying process can never collide with its replacement.
fn socket_path() -> PathBuf {
    let n = SPAWNS.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = if n == 0 {
        format!("senritsu-mpv-{pid}.sock")
    } else {
        format!("senritsu-mpv-{pid}-{n}.sock")
    };
    std::env::temp_dir().join(name)
}

/// mpv driven over its JSON IPC socket.
pub struct MpvBackend {
    volume: u8,
    events: mpsc::UnboundedSender<BackendEvent>,
    next_request: u64,
    conn: Option<Conn>,
}

/// One running mpv process and its socket.
struct Conn {
    writer: OwnedWriteHalf,
    pending: Pending,
    /// Set before a deliberate kill. Silences the reader and the crash report.
    shutdown: Arc<AtomicBool>,
    kill: Option<oneshot::Sender<()>>,
    watcher: Option<JoinHandle<()>>,
    pid: Option<u32>,
    socket: PathBuf,
}

impl MpvBackend {
    pub async fn spawn(
        volume: u8,
    ) -> anyhow::Result<(MpvBackend, mpsc::UnboundedReceiver<BackendEvent>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut backend = MpvBackend {
            volume,
            events: tx,
            next_request: 1,
            conn: None,
        };
        backend.start().await?;
        Ok((backend, rx))
    }

    pub fn pid(&self) -> Option<u32> {
        self.conn.as_ref().and_then(|c| c.pid)
    }

    async fn start(&mut self) -> anyhow::Result<()> {
        self.conn = Some(Conn::start(self.volume, self.events.clone()).await?);
        for (id, name) in (1u64..).zip(OBSERVED) {
            self.command(json!(["observe_property", id, name])).await?;
        }
        Ok(())
    }

    /// Sends one command and waits for its reply. Errors name only the
    /// command, never its arguments, because loadfile carries the stream URL.
    async fn command(&mut self, args: Value) -> anyhow::Result<Value> {
        let name = args
            .get(0)
            .and_then(Value::as_str)
            .unwrap_or("command")
            .to_owned();
        let conn = self.conn.as_mut().context("mpv is not running")?;
        let id = self.next_request;
        self.next_request += 1;

        let (tx, rx) = oneshot::channel();
        conn.pending.lock().unwrap().insert(id, tx);
        let mut line = serde_json::to_vec(&json!({ "command": args, "request_id": id }))?;
        line.push(b'\n');
        if let Err(e) = conn.writer.write_all(&line).await {
            conn.pending.lock().unwrap().remove(&id);
            bail!("mpv {name}: write failed: {e}");
        }
        match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
            Err(_) => {
                conn.pending.lock().unwrap().remove(&id);
                bail!("mpv {name}: no reply")
            }
            Ok(Err(_)) => bail!("mpv {name}: connection closed"),
            Ok(Ok(Err(e))) => bail!("mpv {name}: {e}"),
            Ok(Ok(Ok(data))) => Ok(data),
        }
    }
}

impl AudioBackend for MpvBackend {
    async fn load(&mut self, url: &str, mode: LoadMode) -> anyhow::Result<EntryId> {
        let args = match mode {
            LoadMode::Replace { start, paused } => {
                // Pause is set globally rather than as a per-file option: mpv
                // resets per-file options when a file ends, which would carry
                // the load-time pause state into the next gapless track.
                self.command(json!(["set_property", "pause", paused]))
                    .await?;
                match start.filter(|s| s.is_finite() && *s > 0.0) {
                    Some(s) => json!(["loadfile", url, "replace", -1, format!("start={s}")]),
                    None => json!(["loadfile", url, "replace"]),
                }
            }
            LoadMode::Append => json!(["loadfile", url, "append"]),
        };
        let data = self.command(args).await?;
        data.get("playlist_entry_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("mpv loadfile: reply had no playlist_entry_id"))
    }

    /// Removes the entry with this id. An id that is no longer in the
    /// playlist is not an error.
    async fn remove_entry(&mut self, id: EntryId) -> anyhow::Result<()> {
        let playlist = self.command(json!(["get_property", "playlist"])).await?;
        let index = playlist.as_array().and_then(|entries| {
            entries
                .iter()
                .position(|e| e.get("id").and_then(Value::as_i64) == Some(id))
        });
        if let Some(index) = index {
            self.command(json!(["playlist-remove", index])).await?;
        }
        Ok(())
    }

    async fn set_pause(&mut self, paused: bool) -> anyhow::Result<()> {
        self.command(json!(["set_property", "pause", paused]))
            .await
            .map(drop)
    }

    async fn seek_to(&mut self, secs: f64) -> anyhow::Result<()> {
        self.command(json!(["seek", secs, "absolute"]))
            .await
            .map(drop)
    }

    async fn set_volume(&mut self, vol: u8) -> anyhow::Result<()> {
        self.volume = vol;
        self.command(json!(["set_property", "volume", vol]))
            .await
            .map(drop)
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        self.command(json!(["stop"])).await.map(drop)
    }

    async fn restart(&mut self) -> anyhow::Result<()> {
        if let Some(old) = self.conn.take() {
            old.shut_down().await;
        }
        self.start().await
    }
}

impl Conn {
    async fn start(
        volume: u8,
        events: mpsc::UnboundedSender<BackendEvent>,
    ) -> anyhow::Result<Conn> {
        let socket = socket_path();
        let _ = std::fs::remove_file(&socket);
        let mut child = Command::new("mpv")
            .args([
                "--idle=yes",
                "--no-video",
                "--no-terminal",
                "--audio-display=no",
                "--gapless-audio=weak",
                "--prefetch-playlist=yes",
            ])
            .arg(format!("--input-ipc-server={}", socket.display()))
            .arg(format!("--volume={volume}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("couldn't start mpv")?;
        let pid = child.id();

        let deadline = tokio::time::Instant::now() + CONNECT_TIMEOUT;
        let stream = loop {
            if let Some(status) = child.try_wait()? {
                bail!("mpv exited during startup ({status})");
            }
            match UnixStream::connect(&socket).await {
                Ok(stream) => break stream,
                Err(e) if tokio::time::Instant::now() >= deadline => {
                    return Err(e).context("couldn't connect to mpv's IPC socket");
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        };
        tracing::debug!(pid, "mpv started");

        let (reader, writer) = stream.into_split();
        let pending: Pending = Arc::default();
        let shutdown = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(read_loop(
            reader,
            pending.clone(),
            events.clone(),
            shutdown.clone(),
        ));
        let (kill, kill_rx) = oneshot::channel();
        let watcher = tokio::spawn(watch(child, kill_rx, reader, events, shutdown.clone()));

        Ok(Conn {
            writer,
            pending,
            shutdown,
            kill: Some(kill),
            watcher: Some(watcher),
            pid,
            socket,
        })
    }

    /// Kills the process and waits until its reader and watcher are done, so
    /// no event from it can arrive after this returns.
    async fn shut_down(mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(kill) = self.kill.take() {
            let _ = kill.send(());
        }
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.await;
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // Dropping `kill` wakes the watcher, which kills the process. If the
        // runtime is gone, `kill_on_drop` covers it.
        self.shutdown.store(true, Ordering::SeqCst);
        self.kill.take();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Waits for the process to exit or for a kill request. On an unexpected exit
/// it lets the reader forward everything the process sent, then reports
/// `Crashed`, so `Crashed` is always the last event from that process.
async fn watch(
    mut child: Child,
    kill_rx: oneshot::Receiver<()>,
    reader: JoinHandle<()>,
    events: mpsc::UnboundedSender<BackendEvent>,
    shutdown: Arc<AtomicBool>,
) {
    let kill_requested = tokio::select! {
        _ = child.wait() => false,
        _ = kill_rx => true,
    };
    if kill_requested {
        shutdown.store(true, Ordering::SeqCst);
        let _ = child.kill().await;
    }
    let abort = reader.abort_handle();
    if tokio::time::timeout(DRAIN_TIMEOUT, reader).await.is_err() {
        abort.abort();
    }
    if !shutdown.load(Ordering::SeqCst) {
        tracing::warn!("mpv exited unexpectedly");
        let _ = events.send(BackendEvent::Crashed);
    }
}

/// Reads JSON lines: replies go to their waiting command, events to the
/// channel. Lines are never logged because they can contain stream URLs.
async fn read_loop(
    reader: OwnedReadHalf,
    pending: Pending,
    events: mpsc::UnboundedSender<BackendEvent>,
    shutdown: Arc<AtomicBool>,
) {
    let mut lines = BufReader::new(reader).lines();
    let mut idle: Option<bool> = None;
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg.get("event").is_some() {
            if shutdown.load(Ordering::SeqCst) {
                continue;
            }
            if let Some(ev) = parse_event(&msg, &mut idle) {
                let _ = events.send(ev);
            }
        } else if let Some(id) = msg.get("request_id").and_then(Value::as_u64) {
            let reply = match msg.get("error").and_then(Value::as_str) {
                Some("success") => Ok(msg.get("data").cloned().unwrap_or(Value::Null)),
                Some(err) => Err(err.to_owned()),
                None => Err("malformed reply".to_owned()),
            };
            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                let _ = tx.send(reply);
            }
        }
    }
    // Fail any command still waiting on this connection.
    pending.lock().unwrap().clear();
}

fn parse_event(msg: &Value, idle: &mut Option<bool>) -> Option<BackendEvent> {
    let entry = || msg.get("playlist_entry_id").and_then(Value::as_i64);
    match msg.get("event")?.as_str()? {
        "start-file" => Some(BackendEvent::FileStarted(entry()?)),
        "playback-restart" => Some(BackendEvent::PlaybackRestart),
        "end-file" => {
            let reason = match msg.get("reason").and_then(Value::as_str) {
                Some("eof") => EndReason::Eof,
                Some("stop") => EndReason::Stop,
                Some("quit") => EndReason::Quit,
                Some("error") => EndReason::Error(
                    msg.get("file_error")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_owned(),
                ),
                _ => EndReason::Other,
            };
            Some(BackendEvent::FileEnded {
                entry: entry()?,
                reason,
            })
        }
        "property-change" => {
            let data = msg.get("data")?;
            match msg.get("name")?.as_str()? {
                "time-pos" => data.as_f64().map(BackendEvent::Position),
                "duration" => data.as_f64().map(BackendEvent::Duration),
                "pause" => data.as_bool().map(BackendEvent::Paused),
                "idle-active" => {
                    let now = data.as_bool()?;
                    // Only a transition into idle counts; mpv starts idle.
                    let became_idle = now && *idle == Some(false);
                    *idle = Some(now);
                    became_idle.then_some(BackendEvent::Idle)
                }
                _ => None,
            }
        }
        _ => None,
    }
}
