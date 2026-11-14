use crate::player::queue::{Queue, RepeatMode};
use crate::player::{PlayerSnapshot, Status};
use crate::subsonic::Song;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The queue state saved between runs. `order` and `current` use the same
/// meaning as `Queue::order` and `Queue::pos`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersistedQueue {
    pub songs: Vec<Song>,
    pub order: Vec<usize>,
    pub current: Option<usize>,
    pub position: f64,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub volume: u8,
    /// `"{username}@{server_url}"` of the login the queue belongs to. A
    /// queue is only restored for that login; empty never matches.
    #[serde(default)]
    pub account: String,
}

impl PersistedQueue {
    /// Writes the state to `path` through a temp file and a rename, so a
    /// crash mid-write never leaves a half-written file.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("creating the state directory")?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, serde_json::to_vec(self)?).context("writing queue state")?;
        std::fs::rename(&tmp, path).context("replacing queue state")?;
        Ok(())
    }

    /// Reads saved state. A missing file is `None`; an unreadable or corrupt
    /// one is an error.
    pub fn load(path: &Path) -> anyhow::Result<Option<PersistedQueue>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("parsing queue state")?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("reading queue state"),
        }
    }

    /// Builds saved state from what the player last published. Songs are
    /// saved in their unshuffled order, with the shuffled order beside them.
    /// When the snapshot's order does not fit its items, display order
    /// stands in for both. A stopped player is saved at the start of its
    /// track.
    pub fn from_snapshot(snap: &PlayerSnapshot) -> PersistedQueue {
        let len = snap.items.len();
        let mut slots: Vec<Option<Song>> = vec![None; len];
        let fits = snap.order.len() == len
            && snap
                .order
                .iter()
                .zip(&snap.items)
                .all(|(&at, item)| at < len && slots[at].replace(item.song.clone()).is_none());
        let (songs, order) = if fits {
            (slots.into_iter().flatten().collect(), snap.order.clone())
        } else {
            (
                snap.items.iter().map(|i| i.song.clone()).collect(),
                (0..len).collect(),
            )
        };
        PersistedQueue {
            songs,
            order,
            current: snap.current,
            position: if snap.status == Status::Stopped {
                0.0
            } else {
                snap.position
            },
            shuffle: snap.shuffle,
            repeat: snap.repeat,
            volume: snap.volume,
            account: String::new(),
        }
    }
}

impl Queue {
    pub fn to_persisted(&self, position: f64, volume: u8) -> PersistedQueue {
        PersistedQueue {
            songs: self.items.iter().map(|i| i.song.clone()).collect(),
            order: self.order.clone(),
            current: self.pos,
            position,
            shuffle: self.shuffle,
            repeat: self.repeat,
            volume,
            account: String::new(),
        }
    }

    /// Rebuilds a queue from saved state. A damaged `order` falls back to
    /// the saved song order, and an out-of-range `current` is dropped.
    pub fn from_persisted(p: PersistedQueue) -> Queue {
        let mut q = Queue::new();
        let len = p.songs.len();
        q.append(p.songs);
        let mut seen = vec![false; len];
        let valid = p.order.len() == len
            && p.order
                .iter()
                .all(|&i| i < len && !std::mem::replace(&mut seen[i], true));
        if valid {
            q.order = p.order;
        }
        q.pos = p.current.filter(|&c| c < len);
        q.shuffle = p.shuffle;
        q.repeat = p.repeat;
        q
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests compare exact, known values")]
mod tests {
    use super::*;

    fn song(id: &str, starred: bool) -> Song {
        Song {
            id: id.into(),
            title: format!("t{id}"),
            artist: String::new(),
            artist_id: None,
            album: String::new(),
            album_id: None,
            track: None,
            year: None,
            duration: 100,
            cover_art: None,
            suffix: None,
            bit_rate: None,
            starred,
            ..Default::default()
        }
    }

    #[test]
    fn round_trips_through_json() {
        let mut q = Queue::with_seed(1);
        q.replace(vec![song("a", true), song("b", false), song("c", false)], 1);
        q.set_shuffle(true);
        q.repeat = RepeatMode::All;
        let saved = q.to_persisted(12.5, 70);
        let json = serde_json::to_string(&saved).unwrap();
        let loaded: PersistedQueue = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, saved);

        let r = Queue::from_persisted(loaded);
        assert_eq!(r.order, q.order);
        assert_eq!(r.current().unwrap().song.id, "b");
        assert!(r.items[0].song.starred);
        assert!(r.shuffle);
        assert_eq!(r.repeat, RepeatMode::All);
    }

    #[test]
    fn persisted_queue_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("state.json");
        assert_eq!(PersistedQueue::load(&path).unwrap(), None);

        let mut q = Queue::with_seed(3);
        q.replace(vec![song("a", false), song("b", true)], 1);
        q.repeat = RepeatMode::One;
        let saved = q.to_persisted(33.0, 55);
        saved.save(&path).unwrap();
        assert_eq!(PersistedQueue::load(&path).unwrap(), Some(saved.clone()));
        // Saving again replaces the file and leaves no temp file behind.
        let second = q.to_persisted(40.0, 55);
        second.save(&path).unwrap();
        assert_eq!(PersistedQueue::load(&path).unwrap(), Some(second));
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["state.json"]);

        std::fs::write(&path, "{broken").unwrap();
        assert!(PersistedQueue::load(&path).is_err());
    }

    #[test]
    fn snapshot_becomes_a_restorable_queue() {
        use crate::player::queue::QueueItem;
        let items = ["c", "a", "b"]
            .iter()
            .enumerate()
            .map(|(i, id)| QueueItem {
                uid: i as u64,
                song: song(id, false),
            })
            .collect();
        let snap = PlayerSnapshot {
            items,
            order: Vec::new(),
            current: Some(1),
            status: Status::Paused,
            position: 12.0,
            duration: 100.0,
            volume: 40,
            shuffle: true,
            repeat: RepeatMode::All,
        };
        let p = PersistedQueue::from_snapshot(&snap);
        assert_eq!(p.position, 12.0);
        assert_eq!(p.volume, 40);
        let q = Queue::from_persisted(p);
        let ids: Vec<_> = q
            .display_items()
            .iter()
            .map(|i| i.song.id.clone())
            .collect();
        assert_eq!(ids, ["c", "a", "b"]);
        assert_eq!(q.current().unwrap().song.id, "a");
        assert!(q.shuffle);
        assert_eq!(q.repeat, RepeatMode::All);

        // A stopped player restarts its track from the top.
        let stopped = PlayerSnapshot {
            status: Status::Stopped,
            ..snap
        };
        assert_eq!(PersistedQueue::from_snapshot(&stopped).position, 0.0);
    }

    #[test]
    fn a_shuffled_snapshot_keeps_its_original_order() {
        let mut q = Queue::with_seed(7);
        q.replace(
            ["a", "b", "c", "d", "e"].map(|id| song(id, false)).to_vec(),
            0,
        );
        q.set_shuffle(true);
        let shuffled: Vec<_> = q
            .display_items()
            .iter()
            .map(|i| i.song.id.clone())
            .collect();
        let snap = PlayerSnapshot {
            items: q.display_items().into_iter().cloned().collect(),
            order: q.order.clone(),
            current: Some(0),
            shuffle: true,
            ..PlayerSnapshot::default()
        };

        let mut r = Queue::from_persisted(PersistedQueue::from_snapshot(&snap));
        let ids = |q: &Queue| -> Vec<String> {
            q.display_items()
                .iter()
                .map(|i| i.song.id.clone())
                .collect()
        };
        assert_eq!(ids(&r), shuffled);
        r.set_shuffle(false);
        assert_eq!(ids(&r), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn damaged_order_and_current_are_repaired() {
        let p = PersistedQueue {
            songs: vec![song("a", false), song("b", false)],
            order: vec![1, 1],
            current: Some(5),
            position: 0.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume: 100,
            account: String::new(),
        };
        let q = Queue::from_persisted(p);
        assert_eq!(q.order, vec![0, 1]);
        assert_eq!(q.pos, None);
    }
}
