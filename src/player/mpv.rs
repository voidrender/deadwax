use super::backend::{AudioBackend, BackendEvent, EndReason, EntryId, LoadMode};
use anyhow::{Context, anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a dead process's reader gets to forward its last events.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a replacing load waits for the new file to start before it
/// unpauses anyway.
const START_TIMEOUT: Duration = Duration::from_secs(5);

const OBSERVED: [&str; 4] = ["time-pos", "pause", "duration", "idle-active"];

type Reply = Result<Value, String>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Reply>>>>;

/// Returns true when `mpv --version` runs successfully.
pub fn mpv_available() -> bool {
    std::process::Command::new("mpv")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Creates a fresh directory only this user can enter, for one mpv socket.
/// A predictable path in a shared temp dir could be taken over by another
/// user, who would then receive our auth-bearing stream URLs.
fn private_socket_dir() -> anyhow::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("deadwax-mpv-")
        .tempdir()
        .context("couldn't create the mpv socket directory")?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    let mode = std::fs::metadata(dir.path())?.permissions().mode() & 0o777;
    if mode != 0o700 {
        bail!("mpv socket directory has mode {mode:o}, expected 700");
    }
    Ok(dir)
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
    /// The newest playlist entry mpv has started. Entry ids only grow.
    started: watch::Receiver<EntryId>,
    /// Set before a deliberate kill. Silences the reader and the crash report.
    shutdown: Arc<AtomicBool>,
    kill: Option<oneshot::Sender<()>>,
    watcher: Option<JoinHandle<()>>,
    pid: Option<u32>,
    socket: PathBuf,
    /// Removed (with the socket) when the connection is dropped.
    _socket_dir: tempfile::TempDir,
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

    pub fn socket_path(&self) -> Option<&Path> {
        self.conn.as_ref().map(|c| c.socket.as_path())
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
            .or_else(|| args.get("name"))
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

/// Per-file options for `loadfile`. The title uses mpv's `%len%` quoting
/// (length in bytes), so commas and equals signs in it stay literal.
fn file_options(start: Option<f64>, title: &str) -> String {
    let mut opts = String::new();
    if let Some(s) = start.filter(|s| s.is_finite() && *s > 0.0) {
        let _ = write!(opts, "start={s},");
    }
    let _ = write!(opts, "force-media-title=%{}%{title}", title.len());
    opts
}

impl AudioBackend for MpvBackend {
    async fn load(&mut self, url: &str, title: &str, mode: LoadMode) -> anyhow::Result<EntryId> {
        // Pause is set globally rather than as a per-file option: mpv resets
        // per-file options when a file ends, which would carry the load-time
        // pause state into the next gapless track. Pausing goes before the
        // load and unpausing after the new file starts, so the old track
        // never resumes.
        let unpause_after = match mode {
            LoadMode::Replace { paused: true, .. } => {
                self.command(json!(["set_property", "pause", true])).await?;
                false
            }
            LoadMode::Replace { paused: false, .. } => true,
            LoadMode::Append => false,
        };
        let (flag, start) = match mode {
            LoadMode::Replace { start, .. } => ("replace", start),
            LoadMode::Append => ("append", None),
        };
        // Named arguments, because mpv 0.38 inserted an `index` argument
        // before `options`. The positional form can't work on both sides.
        let args = json!({
            "name": "loadfile",
            "url": url,
            "flags": flag,
            "options": file_options(start, title),
        });
        // Taken before the load so a quick start-file is not missed.
        let mut started = self
            .conn
            .as_ref()
            .context("mpv is not running")?
            .started
            .clone();
        let data = self.command(args).await?;
        let id = data
            .get("playlist_entry_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("mpv loadfile: reply had no playlist_entry_id"))?;
        if unpause_after {
            // mpv replies once the entry is queued, but the old file stops a
            // moment later. Unpausing before the new file starts can resume
            // the old one for an instant.
            let new_file = started.wait_for(|last| *last >= id);
            if tokio::time::timeout(START_TIMEOUT, new_file).await.is_err() {
                tracing::debug!("mpv: new file did not start in time, unpausing anyway");
            }
            self.command(json!(["set_property", "pause", false]))
                .await?;
        }
        Ok(id)
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
        let socket_dir = private_socket_dir()?;
        let socket = socket_dir.path().join("mpv.sock");
        let mut child = Command::new("mpv")
            .args([
                "--idle=yes",
                "--no-video",
                "--no-terminal",
                "--audio-display=no",
                "--gapless-audio=weak",
                "--prefetch-playlist=yes",
                // User config still loads (for the audio device), but these
                // would change how files end or repeat, or hand the stream
                // URL to scripts.
                "--keep-open=no",
                "--load-scripts=no",
                "--ytdl=no",
                "--resume-playback=no",
                "--save-position-on-quit=no",
                "--loop-file=no",
                "--loop-playlist=no",
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
        if let (Some(ours), Ok(Some(peer))) = (pid, stream.peer_cred().map(|c| c.pid()))
            && i32::try_from(ours).ok() != Some(peer)
        {
            bail!("mpv IPC socket is owned by another process");
        }
        tracing::debug!(pid, "mpv started");

        let (reader, writer) = stream.into_split();
        let pending: Pending = Arc::default();
        let (started_tx, started) = watch::channel(0);
        let shutdown = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(read_loop(
            reader,
            pending.clone(),
            events.clone(),
            started_tx,
            shutdown.clone(),
        ));
        let (kill, kill_rx) = oneshot::channel();
        let watcher = tokio::spawn(watch(child, kill_rx, reader, events, shutdown.clone()));

        Ok(Conn {
            writer,
            pending,
            started,
            shutdown,
            kill: Some(kill),
            watcher: Some(watcher),
            pid,
            socket,
            _socket_dir: socket_dir,
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
    }
}

enum WatchEnd {
    Exited,
    KillRequested,
    ReaderDied,
}

/// Waits for the process to exit, a kill request, or the reader to stop.
/// On an unexpected exit it lets the reader forward everything the process
/// sent, then reports `Crashed`, so `Crashed` is always the last event from
/// that process. A reader that stops while mpv lives would leave every
/// command hanging, so that kills mpv and counts as a crash too.
async fn watch(
    mut child: Child,
    mut kill_rx: oneshot::Receiver<()>,
    mut reader: JoinHandle<()>,
    events: mpsc::UnboundedSender<BackendEvent>,
    shutdown: Arc<AtomicBool>,
) {
    let end = tokio::select! {
        _ = child.wait() => WatchEnd::Exited,
        _ = &mut kill_rx => WatchEnd::KillRequested,
        _ = &mut reader => WatchEnd::ReaderDied,
    };
    match end {
        WatchEnd::KillRequested => {
            shutdown.store(true, Ordering::SeqCst);
            let _ = child.kill().await;
        }
        WatchEnd::ReaderDied => {
            if !shutdown.load(Ordering::SeqCst) {
                tracing::warn!("mpv IPC reader stopped, killing mpv");
            }
            let _ = child.kill().await;
        }
        WatchEnd::Exited => {}
    }
    if !matches!(end, WatchEnd::ReaderDied)
        && tokio::time::timeout(DRAIN_TIMEOUT, &mut reader)
            .await
            .is_err()
    {
        reader.abort();
        // Wait for the abort to land so no event can follow `Crashed`.
        let _ = reader.await;
    }
    if !shutdown.load(Ordering::SeqCst) {
        tracing::warn!("mpv exited unexpectedly");
        let _ = events.send(BackendEvent::Crashed);
    }
}

/// Parses one IPC line. mpv doesn't promise UTF-8 (file names and tags can
/// hold raw bytes), so invalid sequences are replaced rather than dropping
/// the whole message. Returns None for anything that isn't JSON.
fn parse_line(bytes: &[u8]) -> Option<Value> {
    serde_json::from_slice(bytes)
        .ok()
        .or_else(|| serde_json::from_str(&String::from_utf8_lossy(bytes)).ok())
        .filter(Value::is_object)
}

/// Reads JSON lines: replies go to their waiting command, events to the
/// channel. Lines are never logged because they can contain stream URLs.
async fn read_loop<R: AsyncRead + Unpin>(
    reader: R,
    pending: Pending,
    events: mpsc::UnboundedSender<BackendEvent>,
    started: watch::Sender<EntryId>,
    shutdown: Arc<AtomicBool>,
) {
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut idle: Option<bool> = None;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(kind = ?e.kind(), "mpv IPC read failed");
                break;
            }
        }
        let Some(msg) = parse_line(&line) else {
            tracing::debug!(len = line.len(), "skipping unparseable mpv IPC line");
            continue;
        };
        if msg.get("event").is_some() {
            if shutdown.load(Ordering::SeqCst) {
                continue;
            }
            if let Some(ev) = parse_event(&msg, &mut idle) {
                if let BackendEvent::FileStarted(id) = ev {
                    started.send_if_modified(|last| {
                        let newer = id > *last;
                        if newer {
                            *last = id;
                        }
                        newer
                    });
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_options_quote_the_title_by_byte_length() {
        let title = "Roygbiv, Pt. 2 = \"Live\" · 旋律";
        assert_eq!(
            file_options(None, title),
            format!("force-media-title=%{}%{title}", title.len())
        );
        assert_eq!(
            file_options(Some(1.5), "a,b=c"),
            "start=1.5,force-media-title=%5%a,b=c"
        );
        assert_eq!(file_options(Some(f64::NAN), ""), "force-media-title=%0%");
    }
    use tokio::io::AsyncWriteExt;

    #[test]
    fn parse_line_skips_garbage_and_keeps_lossy_utf8() {
        assert_eq!(parse_line(b"\xff\xfe\n"), None);
        assert_eq!(parse_line(b"not json\n"), None);
        assert_eq!(parse_line(b"\n"), None);
        let ok = parse_line(b"{\"event\":\"start-file\",\"playlist_entry_id\":3}\n").unwrap();
        assert_eq!(ok["playlist_entry_id"], 3);
        // A reply whose string data holds invalid UTF-8 must still resolve.
        let lossy = parse_line(
            b"{\"data\":[{\"filename\":\"caf\xe9.flac\",\"id\":7}],\"request_id\":9,\"error\":\"success\"}\n",
        )
        .unwrap();
        assert_eq!(lossy["request_id"], 9);
        assert_eq!(lossy["data"][0]["id"], 7);
    }

    #[tokio::test]
    async fn reader_survives_bad_lines() {
        let (mut tx_side, rx_side) = tokio::io::duplex(1024);
        let (events, mut rx) = mpsc::unbounded_channel();
        let (started_tx, started) = watch::channel(0);
        let reader = tokio::spawn(read_loop(
            rx_side,
            Pending::default(),
            events,
            started_tx,
            Arc::new(AtomicBool::new(false)),
        ));
        tx_side
            .write_all(
                b"\xff\xfe\xfd\nnot json\n{\"event\":\"start-file\",\"playlist_entry_id\":4}\n",
            )
            .await
            .unwrap();
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap();
        assert_eq!(ev, Some(BackendEvent::FileStarted(4)));
        assert_eq!(*started.borrow(), 4);
        assert!(!reader.is_finished());
    }

    #[tokio::test]
    async fn reader_death_kills_child_and_reports_crash() {
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let (_kill, kill_rx) = oneshot::channel();
        let reader = tokio::spawn(async {});
        let (events, mut rx) = mpsc::unbounded_channel();
        let watcher = tokio::spawn(watch(
            child,
            kill_rx,
            reader,
            events,
            Arc::new(AtomicBool::new(false)),
        ));
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap();
        assert_eq!(ev, Some(BackendEvent::Crashed));
        watcher.await.unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(!alive, "child should be killed");
    }
}
