//! Turns engine events into scrobbles, notifications, and Discord presence.

use std::collections::HashMap;
use std::path::PathBuf;

use futures::future::BoxFuture;

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
    fn album_mbid(&self, album_id: &str) -> BoxFuture<'_, Option<String>>;
    fn cover_path(&self, cover_id: &str) -> BoxFuture<'_, Option<PathBuf>>;
    fn public_art(&self, q: ArtQuery) -> BoxFuture<'_, Option<String>>;
}

/// Follows the engine and tells the scrobbler, the notifier, and Discord.
///
/// A track is announced (now playing, notification, presence) only once it
/// is actually playing: on the first unpaused position or an unpause. A
/// restored queue starts its track paused, and that must stay quiet.
pub struct Hub<P: HubPorts> {
    ports: P,
    tracker: ScrobbleTracker,
    current: Option<Song>,
    last_pos: f64,
    mbids: HashMap<String, Option<String>>,
    /// The current track has been announced.
    announced: bool,
    paused: bool,
    /// Public art found for the current track, reused on resume and seek.
    image_url: Option<String>,
}

impl<P: HubPorts> Hub<P> {
    pub fn new(ports: P) -> Self {
        Self {
            ports,
            tracker: ScrobbleTracker::default(),
            current: None,
            last_pos: 0.0,
            mbids: HashMap::new(),
            announced: false,
            paused: false,
            image_url: None,
        }
    }

    pub async fn on_event(&mut self, ev: &EngineEvent, now_ms: i64) {
        match ev {
            EngineEvent::TrackStarted(song) => {
                self.current = Some(song.clone());
                self.last_pos = 0.0;
                self.announced = false;
                self.image_url = None;
            }
            &EngineEvent::Position { pos, paused } => {
                self.last_pos = pos;
                self.paused = paused;
                if !self.announced && !paused {
                    self.announce(now_ms).await;
                }
                if self.announced {
                    for a in self.tracker.position(pos, paused) {
                        self.ports.scrobble(a);
                    }
                }
            }
            &EngineEvent::Seeked(pos) => {
                self.last_pos = pos;
                if self.announced && !self.paused {
                    self.update_presence();
                }
            }
            EngineEvent::PauseChanged(true) => {
                self.paused = true;
                self.ports.presence(PresenceMsg::Clear);
            }
            EngineEvent::PauseChanged(false) => {
                self.paused = false;
                if self.announced {
                    self.update_presence();
                } else {
                    self.announce(now_ms).await;
                }
            }
            EngineEvent::Stopped => {
                self.current = None;
                self.announced = false;
                self.image_url = None;
                self.ports.presence(PresenceMsg::Clear);
            }
            EngineEvent::Error(_) => {}
        }
    }

    /// Scrobbles now playing, shows the notification, then looks up public
    /// art and shows the presence.
    async fn announce(&mut self, now_ms: i64) {
        let Some(song) = self.current.clone() else {
            return;
        };
        self.announced = true;
        for a in self.tracker.track_started(&song, now_ms) {
            self.ports.scrobble(a);
        }

        let image_path = match &song.cover_art {
            Some(id) => self.ports.cover_path(id).await,
            None => None,
        };
        self.ports.notify(TrackNotification {
            title: song.title.clone(),
            artist: song.artist.clone(),
            album: song.album.clone(),
            image_path,
        });

        let release_mbid = match &song.album_id {
            Some(id) => self.album_mbid(id).await,
            None => None,
        };
        // Only names and the MusicBrainz id go out, never anything that
        // points at the private server.
        self.image_url = self
            .ports
            .public_art(ArtQuery {
                artist: song.artist.clone(),
                album: song.album.clone(),
                release_mbid,
            })
            .await;
        self.update_presence();
    }

    async fn album_mbid(&mut self, album_id: &str) -> Option<String> {
        if let Some(known) = self.mbids.get(album_id) {
            return known.clone();
        }
        let found = self.ports.album_mbid(album_id).await;
        self.mbids.insert(album_id.to_string(), found.clone());
        found
    }

    fn update_presence(&self) {
        let Some(song) = &self.current else {
            return;
        };
        self.ports.presence(PresenceMsg::Update(PresenceInfo {
            title: song.title.clone(),
            artist: song.artist.clone(),
            album: song.album.clone(),
            image_url: self.image_url.clone(),
            position: self.last_pos,
            duration: song.duration,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

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
        fn album_mbid(&self, album_id: &str) -> BoxFuture<'_, Option<String>> {
            self.record(Call::AlbumMbid(album_id.to_string()));
            Box::pin(async { Some("m1".to_string()) })
        }
        fn cover_path(&self, cover_id: &str) -> BoxFuture<'_, Option<PathBuf>> {
            // The private cache path is named after the server, so a leak
            // into presence would be visible.
            let p = PathBuf::from(format!("/cache/{SERVER}/{cover_id}-600.img"));
            Box::pin(async move { Some(p) })
        }
        fn public_art(&self, q: ArtQuery) -> BoxFuture<'_, Option<String>> {
            self.record(Call::PublicArt {
                mbid: q.release_mbid.clone(),
            });
            let url = (!self.no_public_art).then(|| PUBLIC_URL.to_string());
            Box::pin(async move { url })
        }
    }

    fn song(id: &str, album_id: Option<&str>) -> Song {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": format!("T{id}"), "artist": "Ar", "album": "Al",
            "albumId": album_id, "coverArt": album_id, "duration": 200,
        }))
        .unwrap()
    }

    fn hub() -> (Hub<FakePorts>, FakePorts) {
        let ports = FakePorts::default();
        (Hub::new(ports.clone()), ports)
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
        h.on_event(&EngineEvent::TrackStarted(song("s1", Some("al1"))), 1000)
            .await;
        h.on_event(&playing(0.2), 1200).await;
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
            h.on_event(&playing(pos), 2000).await;
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
        h.on_event(&EngineEvent::TrackStarted(song("s1", Some("al1"))), 0)
            .await;
        h.on_event(&playing(0.5), 0).await;
        h.on_event(&playing(30.0), 0).await;
        ports.take();

        h.on_event(&EngineEvent::PauseChanged(true), 0).await;
        let calls = ports.take();
        assert_eq!(clears(&calls), 1);
        assert!(updates(&calls).is_empty());

        h.on_event(&EngineEvent::PauseChanged(false), 0).await;
        let ups = updates(&ports.take());
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].position, 30.0);
        assert_eq!(ups[0].image_url.as_deref(), Some(PUBLIC_URL));

        h.on_event(&EngineEvent::Seeked(90.0), 0).await;
        let calls = ports.take();
        let ups = updates(&calls);
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0].position, 90.0);
        // The image is reused, not looked up again.
        assert!(!calls.iter().any(|c| matches!(c, Call::PublicArt { .. })));

        h.on_event(&EngineEvent::Stopped, 0).await;
        assert_eq!(clears(&ports.take()), 1);
    }

    #[tokio::test]
    async fn restored_paused_track_waits_until_playing() {
        let (mut h, ports) = hub();
        h.on_event(&EngineEvent::TrackStarted(song("s1", Some("al1"))), 0)
            .await;
        h.on_event(
            &EngineEvent::Position {
                pos: 42.0,
                paused: true,
            },
            0,
        )
        .await;
        let calls = ports.take();
        assert!(scrobbles(&calls).is_empty(), "{calls:?}");
        assert!(notifications(&calls).is_empty(), "{calls:?}");
        assert!(updates(&calls).is_empty(), "{calls:?}");

        h.on_event(&EngineEvent::PauseChanged(false), 5000).await;
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
    async fn mbid_lookup_is_memoized() {
        let (mut h, ports) = hub();
        for id in ["s1", "s2"] {
            h.on_event(&EngineEvent::TrackStarted(song(id, Some("al1"))), 0)
                .await;
            h.on_event(&playing(0.1), 0).await;
        }
        // No album id: nothing to look up.
        h.on_event(&EngineEvent::TrackStarted(song("s3", None)), 0)
            .await;
        h.on_event(&playing(0.1), 0).await;
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
    async fn presence_never_contains_server_url() {
        let (mut h, ports) = hub();
        h.on_event(&EngineEvent::TrackStarted(song("s1", Some("al1"))), 0)
            .await;
        h.on_event(&playing(0.1), 0).await;
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
        h.on_event(&EngineEvent::TrackStarted(song("s1", Some("al1"))), 0)
            .await;
        h.on_event(&playing(0.1), 0).await;
        h.on_event(&EngineEvent::Seeked(10.0), 0).await;
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
