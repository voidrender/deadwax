use senritsu::player::backend::{AudioBackend, BackendEvent, EndReason, LoadMode};
use senritsu::player::mpv::{MpvBackend, mpv_available};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, timeout_at};

const WAIT: Duration = Duration::from_secs(5);

/// Writes a 1-second, 8 kHz, mono, 16-bit PCM silent WAV.
fn write_silent_wav(path: &Path) {
    const RATE: u32 = 8000;
    const DATA_LEN: u32 = RATE * 2;
    let mut buf = Vec::with_capacity(44 + DATA_LEN as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + DATA_LEN).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&RATE.to_le_bytes());
    buf.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&DATA_LEN.to_le_bytes());
    buf.resize(44 + DATA_LEN as usize, 0);
    std::fs::write(path, buf).unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    a: String,
    b: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let a: PathBuf = dir.path().join("a.wav");
    let b: PathBuf = dir.path().join("b.wav");
    write_silent_wav(&a);
    write_silent_wav(&b);
    Fixture {
        a: a.to_string_lossy().into_owned(),
        b: b.to_string_lossy().into_owned(),
        _dir: dir,
    }
}

fn skip() -> bool {
    if !mpv_available() {
        eprintln!("mpv not installed, skipping");
        return true;
    }
    false
}

/// Receives events until `pred` matches or the deadline passes. Returns every
/// event seen (including the match) and whether the match happened.
async fn collect_until(
    rx: &mut UnboundedReceiver<BackendEvent>,
    within: Duration,
    mut pred: impl FnMut(&BackendEvent) -> bool,
) -> (Vec<BackendEvent>, bool) {
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    loop {
        match timeout_at(deadline, rx.recv()).await {
            Ok(Some(ev)) => {
                let done = pred(&ev);
                seen.push(ev);
                if done {
                    return (seen, true);
                }
            }
            _ => return (seen, false),
        }
    }
}

fn file_events(events: &[BackendEvent]) -> Vec<BackendEvent> {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                BackendEvent::FileStarted(_) | BackendEvent::FileEnded { .. }
            )
        })
        .cloned()
        .collect()
}

const PLAY: LoadMode = LoadMode::Replace {
    start: None,
    paused: false,
};

#[tokio::test]
async fn plays_two_files_gaplessly() {
    if skip() {
        return;
    }
    let f = fixture();
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    let e1 = mpv.load(&f.a, PLAY).await.unwrap();
    let e2 = mpv.load(&f.b, LoadMode::Append).await.unwrap();
    assert_ne!(e1, e2);

    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::FileStarted(e2)).await;
    assert!(ok, "never saw FileStarted(e2): {seen:?}");
    assert_eq!(
        file_events(&seen),
        vec![
            BackendEvent::FileStarted(e1),
            BackendEvent::FileEnded {
                entry: e1,
                reason: EndReason::Eof
            },
            BackendEvent::FileStarted(e2),
        ]
    );
}

#[tokio::test]
async fn pause_emits_paused() {
    if skip() {
        return;
    }
    let f = fixture();
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    let e1 = mpv.load(&f.a, PLAY).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::FileStarted(e1)).await;
    assert!(ok, "never started: {seen:?}");
    mpv.set_pause(true).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::Paused(true)).await;
    assert!(ok, "never saw Paused(true): {seen:?}");
}

#[tokio::test]
async fn remove_entry_drops_next() {
    if skip() {
        return;
    }
    let f = fixture();
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    let e1 = mpv.load(&f.a, PLAY).await.unwrap();
    let e2 = mpv.load(&f.b, LoadMode::Append).await.unwrap();
    mpv.remove_entry(e2).await.unwrap();

    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::Idle).await;
    assert!(ok, "never went idle: {seen:?}");
    assert_eq!(
        file_events(&seen),
        vec![
            BackendEvent::FileStarted(e1),
            BackendEvent::FileEnded {
                entry: e1,
                reason: EndReason::Eof
            },
        ]
    );
}

#[tokio::test]
async fn kill_reports_crashed() {
    if skip() {
        return;
    }
    let (mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    let pid = mpv.pid().expect("mpv pid");
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let (seen, ok) = collect_until(&mut rx, Duration::from_secs(2), |e| {
        *e == BackendEvent::Crashed
    })
    .await;
    assert!(ok, "never saw Crashed: {seen:?}");
}

#[tokio::test]
async fn load_with_start_seeks() {
    if skip() {
        return;
    }
    let f = fixture();
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    mpv.load(
        &f.a,
        LoadMode::Replace {
            start: Some(0.5),
            paused: false,
        },
    )
    .await
    .unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| matches!(e, BackendEvent::Position(_))).await;
    assert!(ok, "never saw Position: {seen:?}");
    let Some(BackendEvent::Position(pos)) = seen.last() else {
        unreachable!()
    };
    assert!(*pos >= 0.4, "first position {pos} should be near 0.5");
}

/// A pause made during a track must survive the move to the next track. mpv
/// resets per-file options when a file ends, so pause can't be one of them.
#[tokio::test]
async fn pause_persists_across_transition() {
    if skip() {
        return;
    }
    let f = fixture();
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    let e1 = mpv.load(&f.a, PLAY).await.unwrap();
    let e2 = mpv.load(&f.b, LoadMode::Append).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::PlaybackRestart).await;
    assert!(ok, "never started: {seen:?}");
    assert!(seen.contains(&BackendEvent::FileStarted(e1)));
    mpv.set_pause(true).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::Paused(true)).await;
    assert!(ok, "never paused: {seen:?}");
    // Seeking past the end while paused moves on to the next entry.
    mpv.seek_to(5.0).await.unwrap();

    let (mut seen, ok) =
        collect_until(&mut rx, WAIT, |e| *e == BackendEvent::FileStarted(e2)).await;
    assert!(ok, "never reached the next track: {seen:?}");
    let (after, _) = collect_until(&mut rx, Duration::from_millis(500), |e| {
        *e == BackendEvent::Paused(false)
    })
    .await;
    seen.extend(after);
    assert!(
        !seen.contains(&BackendEvent::Paused(false)),
        "next track unpaused itself: {seen:?}"
    );
}

/// Starting a new track from pause must not resume the old one first, so
/// the unpause comes after the new file has started.
#[tokio::test]
async fn replace_from_pause_unpauses_after_the_new_file_starts() {
    if skip() {
        return;
    }
    let f = fixture();
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    mpv.load(&f.a, PLAY).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::PlaybackRestart).await;
    assert!(ok, "never started: {seen:?}");
    mpv.set_pause(true).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::Paused(true)).await;
    assert!(ok, "never paused: {seen:?}");

    let e2 = mpv.load(&f.b, PLAY).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::FileStarted(e2)).await;
    assert!(ok, "never started the new file: {seen:?}");
    assert!(
        !seen.contains(&BackendEvent::Paused(false)),
        "the old track resumed before the new one started: {seen:?}"
    );
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::Paused(false)).await;
    assert!(ok, "the new track never unpaused: {seen:?}");
}

#[tokio::test]
async fn missing_file_reports_error() {
    if skip() {
        return;
    }
    let f = fixture();
    let missing = f.a.replace("a.wav", "missing.wav");
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    let e1 = mpv.load(&missing, PLAY).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| {
        matches!(e, BackendEvent::FileEnded { entry, reason: EndReason::Error(_) } if *entry == e1)
    })
    .await;
    assert!(ok, "never saw an error end: {seen:?}");
}

#[tokio::test]
async fn restart_keeps_events_without_crash() {
    if skip() {
        return;
    }
    let f = fixture();
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    let old_pid = mpv.pid();
    mpv.restart().await.unwrap();
    assert_ne!(mpv.pid(), old_pid);

    let e1 = mpv.load(&f.a, PLAY).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::FileStarted(e1)).await;
    assert!(ok, "no events after restart: {seen:?}");
    assert!(
        !seen.contains(&BackendEvent::Crashed),
        "restart reported a crash: {seen:?}"
    );
}

#[tokio::test]
async fn stop_ends_playback_without_crash() {
    if skip() {
        return;
    }
    let f = fixture();
    let (mut mpv, mut rx) = MpvBackend::spawn(0).await.unwrap();
    let e1 = mpv.load(&f.a, PLAY).await.unwrap();
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == BackendEvent::FileStarted(e1)).await;
    assert!(ok, "never started: {seen:?}");
    mpv.stop().await.unwrap();
    let stopped = BackendEvent::FileEnded {
        entry: e1,
        reason: EndReason::Stop,
    };
    let (seen, ok) = collect_until(&mut rx, WAIT, |e| *e == stopped).await;
    assert!(ok, "never saw a stop end: {seen:?}");
    assert!(!seen.contains(&BackendEvent::Crashed));
}

#[tokio::test]
async fn socket_lives_in_private_dir() {
    use std::os::unix::fs::PermissionsExt;
    if skip() {
        return;
    }
    let (mut mpv, _rx) = MpvBackend::spawn(0).await.unwrap();
    for _ in 0..2 {
        let socket = mpv.socket_path().expect("socket path").to_path_buf();
        let dir = socket.parent().unwrap().to_path_buf();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode,
            0o700,
            "socket dir {} has mode {mode:o}",
            dir.display()
        );
        assert_ne!(dir, std::env::temp_dir());
        mpv.restart().await.unwrap();
        assert!(!dir.exists(), "old socket dir should be removed");
    }
    let dir = mpv.socket_path().unwrap().parent().unwrap().to_path_buf();
    drop(mpv);
    assert!(!dir.exists(), "socket dir should be removed on drop");
}
