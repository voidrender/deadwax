//! Turns engine events into scrobbles, notifications, and Discord presence.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::task::JoinHandle;

use crate::art::ArtQuery;
use crate::integrations::discord::{PresenceInfo, PresenceMsg};
use crate::integrations::notify::TrackNotification;
use crate::player::EngineEvent;
use crate::scrobble::{ScrobbleAction, ScrobbleTracker};
use crate::subsonic::Song;

/// What the hub needs from the outside world. The sync methods must not
/// block; they hand the work off.
pub trait HubPorts: Send {
    fn scrobble(&self, a: ScrobbleAction);
    fn notify(&self, n: TrackNotification);
    fn presence(&self, m: PresenceMsg);
    /// The album's release MBID. The outer `None` means the
    /// lookup failed and may be tried again.
    fn album_mbid(&self, album_id: &str) -> BoxFuture<'_, Option<Option<String>>>;
    fn cover_path(&self, cover_id: &str) -> BoxFuture<'_, Option<PathBuf>>;
    fn public_art(&self, q: ArtQuery) -> BoxFuture<'_, Option<String>>;

    /// Unix time in milliseconds, used to age a position when presence is
    /// sent after a slow lookup.
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// Follows the engine and tells the scrobbler, the notifier, and Discord.
///
/// A track is announced (now playing, notification, presence) only once it
/// is actually playing: on the first unpaused position or an unpause. A
/// restored queue starts its track paused, and that must stay quiet.
///
/// The cover and art lookups for an announcement run in their own task, so
/// the hub keeps reading events. A lookup whose track has since changed or
/// stopped sends nothing.
pub struct Hub<P: HubPorts> {
    ports: Arc<P>,
    tracker: ScrobbleTracker,
    current: Option<Song>,
    last_pos: f64,
    mbids: Arc<Mutex<HashMap<String, Option<String>>>>,
    /// The current track has been announced.
    announced: bool,
    /// Presence state, shared with the lookup task.
    shown: Arc<Mutex<Shown>>,
    lookup: Option<JoinHandle<()>>,
    /// How long a track must keep playing before it is announced, so
    /// skipping through tracks does not post a notification for each one.
    settle: Duration,
}

/// The default for `Hub::settle`.
const SETTLE: Duration = Duration::from_secs(1);

/// What Discord should show. Updated under one lock so a lookup can't
/// send an update after the hub has moved on or cleared the presence.
#[derive(Default)]
struct Shown {
    /// Bumped on every track change and stop.
    generation: u64,
    song: Option<Song>,
    /// Public art found for the song, reused on resume and seek.
    image_url: Option<String>,
    /// Playback position at unix time `at_ms`.
    position: f64,
    at_ms: i64,
    paused: bool,
}

impl Shown {
    /// The position at `now_ms`, moved on by the time since it was recorded
    /// unless paused.
    fn position_at(&self, now_ms: i64) -> f64 {
        if self.paused {
            return self.position;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "an elapsed time in milliseconds fits in f64's mantissa"
        )]
        let elapsed = (now_ms - self.at_ms).max(0) as f64 / 1000.0;
        let pos = self.position + elapsed;
        match self.song.as_ref().map(|s| f64::from(s.duration)) {
            Some(d) if d > 0.0 => pos.min(d),
            _ => pos,
        }
    }

    fn set_position(&mut self, pos: f64, now_ms: i64) {
        self.position = pos;
        self.at_ms = now_ms;
    }

    fn update(&self, now_ms: i64) -> Option<PresenceMsg> {
        let song = self.song.as_ref()?;
        Some(PresenceMsg::Update(PresenceInfo {
            title: song.title.clone(),
            artist: song.artist.clone(),
            album: song.album.clone(),
            image_url: self.image_url.clone(),
            position: self.position_at(now_ms),
            duration: song.duration,
        }))
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<P: HubPorts + Sync + 'static> Hub<P> {
    pub fn new(ports: P) -> Self {
        Self {
            ports: Arc::new(ports),
            tracker: ScrobbleTracker::default(),
            current: None,
            last_pos: 0.0,
            mbids: Arc::default(),
            announced: false,
            shown: Arc::default(),
            lookup: None,
            settle: SETTLE,
        }
    }

    /// `now_ms` is when the event happened, in unix milliseconds.
    pub fn on_event(&mut self, ev: &EngineEvent, now_ms: i64) {
        match ev {
            EngineEvent::TrackStarted(song) => {
                self.cancel_lookup();
                self.current = Some(Song::clone(song));
                self.last_pos = 0.0;
                self.announced = false;
                let mut shown = lock(&self.shown);
                shown.generation += 1;
                shown.song = Some(Song::clone(song));
                shown.image_url = None;
                shown.set_position(0.0, now_ms);
            }
            &EngineEvent::Position { pos, paused } => {
                self.last_pos = pos;
                {
                    let mut shown = lock(&self.shown);
                    shown.set_position(pos, now_ms);
                    shown.paused = paused;
                }
                if !self.announced && !paused {
                    self.announce(now_ms);
                }
                if self.announced {
                    for a in self.tracker.position(pos, paused) {
                        self.ports.scrobble(a);
                    }
                }
            }
            &EngineEvent::Seeked(pos) => {
                self.last_pos = pos;
                let mut shown = lock(&self.shown);
                shown.set_position(pos, now_ms);
                if self.announced && !shown.paused {
                    self.send_update(&shown, now_ms);
                }
            }
            EngineEvent::PauseChanged(true) => {
                let mut shown = lock(&self.shown);
                let pos = shown.position_at(now_ms);
                shown.set_position(pos, now_ms);
                shown.paused = true;
                self.ports.presence(PresenceMsg::Clear);
            }
            EngineEvent::PauseChanged(false) => {
                {
                    let mut shown = lock(&self.shown);
                    shown.at_ms = now_ms;
                    shown.paused = false;
                    if self.announced {
                        self.send_update(&shown, now_ms);
                    }
                }
                if !self.announced {
                    self.announce(now_ms);
                }
            }
            EngineEvent::Stopped => {
                self.cancel_lookup();
                self.current = None;
                self.announced = false;
                let mut shown = lock(&self.shown);
                shown.generation += 1;
                shown.song = None;
                shown.image_url = None;
                self.ports.presence(PresenceMsg::Clear);
            }
            EngineEvent::Error(_) => {}
        }
    }

    /// Sends the presence for the current track again, for when presence
    /// was just turned back on.
    pub fn refresh_presence(&self, now_ms: i64) {
        if !self.announced {
            return;
        }
        let shown = lock(&self.shown);
        if !shown.paused {
            self.send_update(&shown, now_ms);
        }
    }

    fn send_update(&self, shown: &Shown, now_ms: i64) {
        if let Some(m) = shown.update(now_ms) {
            self.ports.presence(m);
        }
    }

    fn cancel_lookup(&mut self) {
        if let Some(task) = self.lookup.take() {
            task.abort();
        }
    }

    /// Scrobbles now playing right away, then starts the lookups that end
    /// in the notification and the presence.
    fn announce(&mut self, now_ms: i64) {
        let Some(song) = self.current.clone() else {
            return;
        };
        self.announced = true;
        for a in self.tracker.track_started(&song, now_ms) {
            self.ports.scrobble(a);
        }
        let generation = lock(&self.shown).generation;
        self.cancel_lookup();
        self.lookup = Some(tokio::spawn(lookup(
            Arc::clone(&self.ports),
            Arc::clone(&self.shown),
            Arc::clone(&self.mbids),
            generation,
            song,
            self.settle,
        )));
    }

    /// Waits for the running lookup to finish.
    #[cfg(test)]
    async fn settle(&mut self) {
        if let Some(task) = self.lookup.take() {
            let _ = task.await;
        }
    }
}

/// Waits out `settle`, fetches the cover and shows the notification, then
/// finds public art and shows the presence. A track change aborts the task,
/// and each step is skipped once `generation` is stale.
async fn lookup<P: HubPorts + Sync>(
    ports: Arc<P>,
    shown: Arc<Mutex<Shown>>,
    mbids: Arc<Mutex<HashMap<String, Option<String>>>>,
    generation: u64,
    song: Song,
    settle: Duration,
) {
    tokio::time::sleep(settle).await;
    if lock(&shown).generation != generation {
        return;
    }
    let image_path = match &song.cover_art {
        Some(id) => ports.cover_path(id).await,
        None => None,
    };
    {
        if lock(&shown).generation != generation {
            return;
        }
        ports.notify(TrackNotification {
            title: song.title.clone(),
            artist: song.artist.clone(),
            album: song.album.clone(),
            image_path,
        });
    }

    let release_mbid = match &song.album_id {
        Some(id) => {
            let known = lock(&mbids).get(id).cloned();
            if let Some(m) = known {
                m
            } else if let Some(found) = ports.album_mbid(id).await {
                lock(&mbids).insert(id.clone(), found.clone());
                found
            } else {
                None
            }
        }
        None => None,
    };
    // Only names and the MusicBrainz id go out, never anything that points
    // at the private server.
    let image_url = ports
        .public_art(ArtQuery {
            artist: song.artist.clone(),
            album: song.album.clone(),
            release_mbid,
        })
        .await;

    let mut current = lock(&shown);
    if current.generation != generation {
        return;
    }
    current.image_url = image_url;
    if !current.paused
        && let Some(m) = current.update(ports.now_ms())
    {
        ports.presence(m);
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests compare exact, known values")]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::SeqCst;

    const PUBLIC_URL: &str = "https://coverartarchive.org/release/m1/front-500.jpg";
    const SERVER: &str = "music.example";

    #[derive(Debug, Clone)]
    enum Call {
        Scrobble(ScrobbleAction),
        Notify(TrackNotification),
        Presence(PresenceMsg),
        AlbumMbid(String),
        PublicArt { mbid: Option<String> },
    }

    #[derive(Clone, Default)]
    struct FakePorts {
        calls: Arc<Mutex<Vec<Call>>>,
        no_public_art: bool,
        /// The cover named "slow" waits for this before it is returned.
        slow_cover: Arc<tokio::sync::Notify>,
        /// The fake clock, in unix milliseconds.
        clock: Arc<std::sync::atomic::AtomicI64>,
        /// How far the clock moves during a public art lookup.
        art_delay_ms: i64,
        /// How many album lookups fail before they start to succeed.
        mbid_failures: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FakePorts {
        fn record(&self, c: Call) {
            self.calls.lock().unwrap().push(c);
        }
        fn take(&self) -> Vec<Call> {
            std::mem::take(&mut *self.calls.lock().unwrap())
        }
    }

    impl HubPorts for FakePorts {
        fn scrobble(&self, a: ScrobbleAction) {
            self.record(Call::Scrobble(a));
        }
        fn notify(&self, n: TrackNotification) {
            self.record(Call::Notify(n));
        }
        fn presence(&self, m: PresenceMsg) {
            self.record(Call::Presence(m));
        }
        fn album_mbid(&self, album_id: &str) -> BoxFuture<'_, Option<Option<String>>> {
            self.record(Call::AlbumMbid(album_id.to_string()));
            let failed = self
                .mbid_failures
                .fetch_update(SeqCst, SeqCst, |n| n.checked_sub(1))
                .is_ok();
            Box::pin(async move { (!failed).then(|| Some("m1".to_string())) })
        }
        fn cover_path(&self, cover_id: &str) -> BoxFuture<'_, Option<PathBuf>> {
            // The private cache path is named after the server, so a leak
            // into presence would be visible.
            let p = PathBuf::from(format!("/cache/{SERVER}/{cover_id}-600.img"));
            let gate = (cover_id == "slow").then(|| Arc::clone(&self.slow_cover));
            Box::pin(async move {
                if let Some(gate) = gate {
                    gate.notified().await;
                }
                Some(p)
            })
        }
        fn public_art(&self, q: ArtQuery) -> BoxFuture<'_, Option<String>> {
            self.record(Call::PublicArt {
                mbid: q.release_mbid.clone(),
            });
            let url = (!self.no_public_art).then(|| PUBLIC_URL.to_string());
            self.clock.fetch_add(self.art_delay_ms, SeqCst);
            Box::pin(async move { url })
        }
        fn now_ms(&self) -> i64 {
            self.clock.load(SeqCst)
        }
    }

    fn song(id: &str, album_id: Option<&str>) -> Song {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": format!("T{id}"), "artist": "Ar", "album": "Al",
            "albumId": album_id, "coverArt": album_id, "duration": 200,
        }))
        .unwrap()
    }

    /// A hub that announces at once. `rapid_skips_notify_only_the_last_track`
    /// covers the settle delay.
    fn hub() -> (Hub<FakePorts>, FakePorts) {
        let ports = FakePorts::default();
        let mut hub = Hub::new(ports.clone());
        hub.settle = Duration::ZERO;
        (hub, ports)
    }

    fn playing(pos: f64) -> EngineEvent {
        EngineEvent::Position { pos, paused: false }
    }

    fn updates(calls: &[Call]) -> Vec<PresenceInfo> {
        calls
            .iter()
            .filter_map(|c| match c {
                Call::Presence(PresenceMsg::Update(i)) => Some(i.clone()),
                _ => None,
            })
            .collect()
    }

    fn clears(calls: &[Call]) -> usize {
        calls
            .iter()
            .filter(|c| matches!(c, Call::Presence(PresenceMsg::Clear)))
            .count()
    }

    fn notifications(calls: &[Call]) -> Vec<TrackNotification> {
        calls
            .iter()
            .filter_map(|c| match c {
                Call::Notify(n) => Some(n.clone()),
                _ => None,
            })
            .collect()
    }

    fn scrobbles(calls: &[Call]) -> Vec<ScrobbleAction> {
        calls
            .iter()
            .filter_map(|c| match c {
                Call::Scrobble(a) => Some(a.clone()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn track_start_notifies_with_cover_and_updates_presence() {
        let (mut h, ports) = hub();
        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s1", Some("al1")))),
            1000,
        );
        h.on_event(&playing(0.2), 1200);
        h.settle().await;
        let calls = ports.take();

        assert_eq!(
            scrobbles(&calls),
            [ScrobbleAction::NowPlaying {
                song_id: "s1".into()
            }]
        );
        assert_eq!(
            notifications(&calls),
            [TrackNotification {
                title: "Ts1".into(),
                artist: "Ar".into(),
                album: "Al".into(),
                image_path: Some(PathBuf::from(format!("/cache/{SERVER}/al1-600.img"))),
            }]
        );
        let ups = updates(&calls);
        assert_eq!(ups.len(), 1, "{calls:?}");
        assert_eq!(ups[0].title, "Ts1");
        assert_eq!(ups[0].artist, "Ar");
        assert_eq!(ups[0].album, "Al");
        assert_eq!(ups[0].duration, 200);
        assert_eq!(ups[0].image_url.as_deref(), Some(PUBLIC_URL));

        // Listening past the threshold submits once, stamped at the start.
        let mut pos = 0.2;
        while pos < 120.0 {
            pos += 0.5;
            h.on_event(&playing(pos), 2000);
        }
        assert_eq!(
            scrobbles(&ports.take()),
            [ScrobbleAction::Submit {
                song_id: "s1".into(),
                time_ms: 1200
            }]
        );
    }

    #[tokio::test]
    async fn pause_clears_presence_and_resume_restores() {
        let (mut h, ports) = hub();
        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s1", Some("al1")))),
            0,
        );
        h.on_event(&playing(0.5), 0);
        h.settle().await;
        h.on_event(&playing(30.0), 0);
        ports.take();

        h.on_event(&EngineEvent::PauseChanged(true), 0);
        let calls = ports.take();
        assert_eq!(clears(&calls), 1);
        assert!(updates(&calls).is_empty());

        h.on_event(&EngineEvent::PauseChanged(false), 0);
        let ups = updates(&ports.take());
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].position, 30.0);
        assert_eq!(ups[0].image_url.as_deref(), Some(PUBLIC_URL));

        h.on_event(&EngineEvent::Seeked(90.0), 0);
        let calls = ports.take();
        let ups = updates(&calls);
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].position, 90.0);
        // The image is reused, not looked up again.
        assert!(!calls.iter().any(|c| matches!(c, Call::PublicArt { .. })));

        h.on_event(&EngineEvent::Stopped, 0);
        assert_eq!(clears(&ports.take()), 1);
    }

    #[tokio::test]
    async fn restored_paused_track_waits_until_playing() {
        let (mut h, ports) = hub();
        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s1", Some("al1")))),
            0,
        );
        h.on_event(
            &EngineEvent::Position {
                pos: 42.0,
                paused: true,
            },
            0,
        );
        let calls = ports.take();
        assert!(scrobbles(&calls).is_empty(), "{calls:?}");
        assert!(notifications(&calls).is_empty(), "{calls:?}");
        assert!(updates(&calls).is_empty(), "{calls:?}");

        h.on_event(&EngineEvent::PauseChanged(false), 5000);

        h.settle().await;
        let calls = ports.take();
        assert_eq!(
            scrobbles(&calls),
            [ScrobbleAction::NowPlaying {
                song_id: "s1".into()
            }]
        );
        assert_eq!(notifications(&calls).len(), 1);
        let ups = updates(&calls);
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].position, 42.0);
    }

    #[tokio::test]
    async fn a_skipped_tracks_lookup_sends_nothing() {
        let (mut h, ports) = hub();
        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s1", Some("slow")))),
            0,
        );
        h.on_event(&playing(0.1), 0);
        // The lookup for s1 starts and waits on its cover.
        tokio::task::yield_now().await;
        let stale = lock(&h.shown).generation;

        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s2", Some("al2")))),
            0,
        );
        h.on_event(&playing(0.1), 0);
        h.settle().await;
        ports.slow_cover.notify_waiters();
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        // A lookup that was already past its await when s2 started.
        lookup(
            Arc::clone(&h.ports),
            Arc::clone(&h.shown),
            Arc::clone(&h.mbids),
            stale,
            song("s1", Some("al1")),
            Duration::ZERO,
        )
        .await;

        let calls = ports.take();
        let titles: Vec<_> = notifications(&calls).into_iter().map(|n| n.title).collect();
        assert_eq!(titles, ["Ts2"], "{calls:?}");
        let shown: Vec<_> = updates(&calls).into_iter().map(|u| u.title).collect();
        assert_eq!(shown, ["Ts2"], "{calls:?}");
        // Now playing for s1 went out as soon as it played.
        assert_eq!(
            scrobbles(&calls),
            [
                ScrobbleAction::NowPlaying {
                    song_id: "s1".into()
                },
                ScrobbleAction::NowPlaying {
                    song_id: "s2".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn refresh_resends_the_current_track_unless_paused() {
        let (mut h, ports) = hub();
        h.refresh_presence(0);
        assert!(updates(&ports.take()).is_empty());
        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s1", Some("al1")))),
            0,
        );
        h.on_event(&playing(0.1), 0);
        h.settle().await;
        ports.take();

        h.refresh_presence(1000);
        let titles: Vec<_> = updates(&ports.take())
            .into_iter()
            .map(|u| u.title)
            .collect();
        assert_eq!(titles, ["Ts1"]);
        h.on_event(&EngineEvent::PauseChanged(true), 2000);
        ports.take();
        h.refresh_presence(3000);
        assert!(updates(&ports.take()).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn rapid_skips_notify_only_the_last_track() {
        let ports = FakePorts::default();
        let mut h = Hub::new(ports.clone());
        for id in ["s1", "s2", "s3"] {
            h.on_event(&EngineEvent::TrackStarted(Box::new(song(id, None))), 0);
            h.on_event(&playing(0.1), 0);
            // A quarter second per track, well inside the settle time.
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        h.settle().await;
        let calls = ports.take();
        let titles: Vec<_> = notifications(&calls).into_iter().map(|n| n.title).collect();
        assert_eq!(titles, ["Ts3"], "{calls:?}");
        let shown: Vec<_> = updates(&calls).into_iter().map(|u| u.title).collect();
        assert_eq!(shown, ["Ts3"], "{calls:?}");
    }

    #[tokio::test]
    async fn late_presence_accounts_for_lookup_time() {
        let ports = FakePorts {
            art_delay_ms: 3000,
            ..FakePorts::default()
        };
        ports.clock.store(1000, SeqCst);
        let mut h = Hub::new(ports.clone());
        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s1", Some("al1")))),
            900,
        );
        h.on_event(&playing(10.0), 1000);
        h.settle().await;
        let ups = updates(&ports.take());
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].position, 13.0);

        // While paused the position holds still.
        h.on_event(&EngineEvent::PauseChanged(true), 5000);
        h.on_event(&EngineEvent::PauseChanged(false), 9000);
        let ups = updates(&ports.take());
        assert_eq!(ups[0].position, 14.0);
    }

    #[tokio::test]
    async fn mbid_lookup_is_memoized() {
        let (mut h, ports) = hub();
        for id in ["s1", "s2"] {
            h.on_event(
                &EngineEvent::TrackStarted(Box::new(song(id, Some("al1")))),
                0,
            );
            h.on_event(&playing(0.1), 0);
            h.settle().await;
        }
        // No album id: nothing to look up.
        h.on_event(&EngineEvent::TrackStarted(Box::new(song("s3", None))), 0);
        h.on_event(&playing(0.1), 0);
        h.settle().await;
        let calls = ports.take();
        let lookups: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                Call::AlbumMbid(id) => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(lookups, ["al1"], "{calls:?}");
        let mbids: Vec<_> = calls
            .iter()
            .filter_map(|c| match c {
                Call::PublicArt { mbid } => Some(mbid.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(mbids, [Some("m1".into()), Some("m1".into()), None]);
    }

    #[tokio::test]
    async fn a_failed_mbid_lookup_is_retried() {
        let (mut h, ports) = hub();
        ports.mbid_failures.store(1, SeqCst);
        for id in ["s1", "s2", "s3"] {
            h.on_event(
                &EngineEvent::TrackStarted(Box::new(song(id, Some("al1")))),
                0,
            );
            h.on_event(&playing(0.1), 0);
            h.settle().await;
        }
        let calls = ports.take();
        let lookups = calls
            .iter()
            .filter(|c| matches!(c, Call::AlbumMbid(_)))
            .count();
        assert_eq!(lookups, 2, "{calls:?}");
    }

    #[tokio::test]
    async fn presence_never_contains_server_url() {
        let (mut h, ports) = hub();
        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s1", Some("al1")))),
            0,
        );
        h.on_event(&playing(0.1), 0);
        h.settle().await;
        let ups = updates(&ports.take());
        assert_eq!(ups.len(), 1);
        let url = ups[0].image_url.as_deref().unwrap();
        assert_eq!(url, PUBLIC_URL);
        assert!(!url.contains(SERVER));

        // Without a public hit, presence has no image at all, never the
        // private cover.
        let ports = FakePorts {
            no_public_art: true,
            ..FakePorts::default()
        };
        let mut h = Hub::new(ports.clone());
        h.on_event(
            &EngineEvent::TrackStarted(Box::new(song("s1", Some("al1")))),
            0,
        );
        h.on_event(&playing(0.1), 0);
        h.settle().await;
        h.on_event(&EngineEvent::Seeked(10.0), 0);
        let calls = ports.take();
        let ups = updates(&calls);
        assert_eq!(ups.len(), 2);
        for u in &ups {
            assert_eq!(u.image_url, None);
            assert!(!format!("{u:?}").contains(SERVER), "{u:?}");
        }
        // The notification still gets the private cover file.
        assert!(notifications(&calls)[0].image_path.is_some());
    }
}
