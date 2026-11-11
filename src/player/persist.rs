use crate::player::queue::{Queue, RepeatMode};
use crate::subsonic::Song;
use serde::{Deserialize, Serialize};

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
    fn damaged_order_and_current_are_repaired() {
        let p = PersistedQueue {
            songs: vec![song("a", false), song("b", false)],
            order: vec![1, 1],
            current: Some(5),
            position: 0.0,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume: 100,
        };
        let q = Queue::from_persisted(p);
        assert_eq!(q.order, vec![0, 1]);
        assert_eq!(q.pos, None);
    }
}
