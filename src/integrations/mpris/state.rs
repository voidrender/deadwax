//! What the MPRIS player shows, folded from engine events.

use std::path::Path;

use crate::player::EngineEvent;
use crate::subsonic::Song;

/// The track id MPRIS reserves for "no track".
pub const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Playback {
    Playing,
    Paused,
    #[default]
    Stopped,
}

impl Playback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
        }
    }
}

/// The track's metadata. It holds names, the length, and the local cover
/// file, never the stream URL, which carries the server's auth token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    /// An object path.
    pub track_id: String,
    pub length_us: Option<i64>,
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    /// A `file://` URL for the cached cover.
    pub art_url: Option<String>,
}

/// Something the bus must be told about.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Change {
    Metadata,
    Status,
    /// A seek, to this position in microseconds.
    Seeked(i64),
}

#[derive(Debug, Default)]
pub struct MprisState {
    song: Option<Song>,
    status: Playback,
    position: f64,
    art_url: Option<String>,
    /// Bumped on every track change and stop, so a cover found for an
    /// earlier track can be told apart.
    generation: u64,
}

fn micros(secs: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "track positions are far inside i64 microseconds"
    )]
    let us = (secs * 1_000_000.0).round() as i64;
    us
}

fn non_empty(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.to_string())
}

impl MprisState {
    pub fn apply(&mut self, ev: &EngineEvent) -> Vec<Change> {
        match ev {
            EngineEvent::TrackStarted(song) => {
                self.generation += 1;
                self.song = Some(Song::clone(song));
                self.position = 0.0;
                self.art_url = None;
                self.status = Playback::Playing;
                vec![Change::Metadata, Change::Status]
            }
            &EngineEvent::Position { pos, paused } => {
                self.position = pos;
                self.set_paused(paused)
            }
            &EngineEvent::PauseChanged(paused) => self.set_paused(paused),
            &EngineEvent::Seeked(pos) => {
                self.position = pos;
                vec![Change::Seeked(micros(pos))]
            }
            EngineEvent::Stopped => {
                self.generation += 1;
                self.song = None;
                self.position = 0.0;
                self.art_url = None;
                self.status = Playback::Stopped;
                vec![Change::Metadata, Change::Status]
            }
            EngineEvent::Error(_) => Vec::new(),
        }
    }

    fn set_paused(&mut self, paused: bool) -> Vec<Change> {
        if self.song.is_none() {
            return Vec::new();
        }
        let status = if paused {
            Playback::Paused
        } else {
            Playback::Playing
        };
        if status == self.status {
            return Vec::new();
        }
        self.status = status;
        vec![Change::Status]
    }

    /// Sets the cover for the track of `generation`. Returns false, and
    /// changes nothing, once that track is no longer current.
    pub fn set_cover(&mut self, generation: u64, path: &Path) -> bool {
        if generation != self.generation || self.song.is_none() {
            return false;
        }
        let Ok(url) = url::Url::from_file_path(path) else {
            return false;
        };
        self.art_url = Some(url.into());
        true
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The current song's cover art id.
    pub fn cover_id(&self) -> Option<&str> {
        self.song.as_ref()?.cover_art.as_deref()
    }

    pub fn has_track(&self) -> bool {
        self.song.is_some()
    }

    pub fn status(&self) -> Playback {
        self.status
    }

    pub fn position_us(&self) -> i64 {
        micros(self.position)
    }

    pub fn metadata(&self) -> Metadata {
        let Some(song) = &self.song else {
            return Metadata {
                track_id: NO_TRACK.to_string(),
                length_us: None,
                title: None,
                artists: Vec::new(),
                album: None,
                art_url: None,
            };
        };
        Metadata {
            track_id: format!("/org/deadwax/track/{}", self.generation),
            length_us: (song.duration > 0).then(|| i64::from(song.duration) * 1_000_000),
            title: non_empty(&song.title),
            artists: non_empty(&song.artist).into_iter().collect(),
            album: non_empty(&song.album),
            art_url: self.art_url.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn song(id: &str) -> Song {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": "T", "artist": "Ar", "album": "Al",
            "coverArt": "c1", "duration": 200,
        }))
        .unwrap()
    }

    fn started(id: &str) -> EngineEvent {
        EngineEvent::TrackStarted(Box::new(song(id)))
    }

    #[test]
    fn track_start_publishes_metadata_without_a_stream_url() {
        let mut s = MprisState::default();
        assert_eq!(s.apply(&started("s1")), [Change::Metadata, Change::Status]);
        assert_eq!(s.status(), Playback::Playing);
        assert_eq!(
            s.metadata(),
            Metadata {
                track_id: "/org/deadwax/track/1".into(),
                length_us: Some(200_000_000),
                title: Some("T".into()),
                artists: vec!["Ar".into()],
                album: Some("Al".into()),
                art_url: None,
            }
        );
        assert_eq!(s.cover_id(), Some("c1"));
    }

    #[test]
    fn a_restored_track_shows_paused() {
        let mut s = MprisState::default();
        s.apply(&started("s1"));
        let changes = s.apply(&EngineEvent::Position {
            pos: 42.0,
            paused: true,
        });
        assert_eq!(changes, [Change::Status]);
        assert_eq!(s.status(), Playback::Paused);
        assert_eq!(s.position_us(), 42_000_000);
    }

    #[test]
    fn seek_and_pause_report_changes_only_when_they_change() {
        let mut s = MprisState::default();
        s.apply(&started("s1"));
        assert!(s.apply(&EngineEvent::PauseChanged(false)).is_empty());
        assert!(
            s.apply(&EngineEvent::Position {
                pos: 1.0,
                paused: false
            })
            .is_empty()
        );
        assert_eq!(s.apply(&EngineEvent::PauseChanged(true)), [Change::Status]);
        assert_eq!(
            s.apply(&EngineEvent::Seeked(30.5)),
            [Change::Seeked(30_500_000)]
        );
        assert_eq!(s.position_us(), 30_500_000);
        assert!(s.apply(&EngineEvent::Error("x".into())).is_empty());
    }

    #[test]
    fn stop_clears_the_track() {
        let mut s = MprisState::default();
        s.apply(&started("s1"));
        assert_eq!(
            s.apply(&EngineEvent::Stopped),
            [Change::Metadata, Change::Status]
        );
        assert!(!s.has_track());
        assert_eq!(s.status(), Playback::Stopped);
        let m = s.metadata();
        assert_eq!(m.track_id, NO_TRACK);
        assert_eq!(m.title, None);
        // A pause report after the stop changes nothing.
        assert!(s.apply(&EngineEvent::PauseChanged(true)).is_empty());
    }

    #[test]
    fn a_cover_for_a_skipped_track_is_dropped() {
        let mut s = MprisState::default();
        s.apply(&started("s1"));
        let old = s.generation();
        s.apply(&started("s2"));
        let path = PathBuf::from("/cache/c1.img");
        assert!(!s.set_cover(old, &path));
        assert_eq!(s.metadata().art_url, None);
        assert!(s.set_cover(s.generation(), &path));
        assert_eq!(
            s.metadata().art_url.as_deref(),
            Some("file:///cache/c1.img")
        );
        // The next track starts without the old cover.
        s.apply(&started("s3"));
        assert_eq!(s.metadata().art_url, None);
    }

    #[test]
    fn art_url_is_percent_encoded() {
        let mut s = MprisState::default();
        s.apply(&started("s1"));
        assert!(s.set_cover(s.generation(), Path::new("/cache/my covers/ä.img")));
        assert_eq!(
            s.metadata().art_url.as_deref(),
            Some("file:///cache/my%20covers/%C3%A4.img")
        );
    }

    #[test]
    fn empty_names_are_left_out() {
        let mut s = MprisState::default();
        let mut bare = song("s1");
        bare.artist.clear();
        bare.album = " ".into();
        bare.duration = 0;
        s.apply(&EngineEvent::TrackStarted(Box::new(bare)));
        let m = s.metadata();
        assert!(m.artists.is_empty());
        assert_eq!(m.album, None);
        assert_eq!(m.length_us, None);
    }
}
