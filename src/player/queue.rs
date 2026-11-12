use crate::subsonic::Song;
use rand::SeedableRng;
use rand::rngs::{StdRng, SysRng};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq)]
pub struct QueueItem {
    pub uid: u64,
    pub song: Song,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removed {
    Current,
    Other,
    None,
}

/// The play queue. `order` maps display positions to indices in `items`
/// (identity when not shuffled) and `pos` is an index into `order`.
#[derive(Debug)]
pub struct Queue {
    pub items: Vec<QueueItem>,
    pub order: Vec<usize>,
    pub pos: Option<usize>,
    pub shuffle: bool,
    pub repeat: RepeatMode,
    pub rng: StdRng,
    next_uid: u64,
}

impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}

impl Queue {
    pub fn new() -> Self {
        Self::from_rng(StdRng::try_from_rng(&mut SysRng).expect("OS entropy unavailable"))
    }

    pub fn with_seed(seed: u64) -> Self {
        Self::from_rng(StdRng::seed_from_u64(seed))
    }

    fn from_rng(rng: StdRng) -> Self {
        Self {
            items: Vec::new(),
            order: Vec::new(),
            pos: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            rng,
            next_uid: 0,
        }
    }

    fn wrap(&mut self, songs: Vec<Song>) -> Vec<QueueItem> {
        songs
            .into_iter()
            .map(|song| {
                let uid = self.next_uid;
                self.next_uid += 1;
                QueueItem { uid, song }
            })
            .collect()
    }

    fn shuffle_after_current(&mut self) {
        let from = self.pos.map_or(0, |p| p + 1);
        self.order[from..].shuffle(&mut self.rng);
    }

    pub fn replace(&mut self, songs: Vec<Song>, start: usize) {
        self.items = self.wrap(songs);
        self.order = (0..self.items.len()).collect();
        self.pos = if self.items.is_empty() {
            None
        } else {
            Some(start.min(self.items.len() - 1))
        };
        if self.shuffle {
            self.shuffle_after_current();
        }
    }

    pub fn append(&mut self, songs: Vec<Song>) {
        let new = self.wrap(songs);
        for item in new {
            self.order.push(self.items.len());
            self.items.push(item);
        }
    }

    pub fn insert_next(&mut self, songs: Vec<Song>) {
        let Some(pos) = self.pos else {
            return self.append(songs);
        };
        let new = self.wrap(songs);
        let count = new.len();
        if self.shuffle {
            let base = self.items.len();
            self.items.extend(new);
            let at = pos + 1;
            self.order.splice(at..at, base..base + count);
        } else {
            let at = pos + 1;
            self.items.splice(at..at, new);
            self.order = (0..self.items.len()).collect();
        }
    }

    pub fn remove(&mut self, display_idx: usize) -> Removed {
        if display_idx >= self.order.len() {
            return Removed::None;
        }
        let item_idx = self.order.remove(display_idx);
        self.items.remove(item_idx);
        for i in &mut self.order {
            if *i > item_idx {
                *i -= 1;
            }
        }
        match self.pos {
            Some(p) if p == display_idx => {
                if p >= self.order.len() {
                    self.pos = None;
                }
                Removed::Current
            }
            Some(p) if p > display_idx => {
                self.pos = Some(p - 1);
                Removed::Other
            }
            _ => Removed::Other,
        }
    }

    pub fn move_item(&mut self, from: usize, to: usize) {
        let len = self.order.len();
        if from >= len || to >= len || from == to {
            return;
        }
        let current_uid = self.current().map(|i| i.uid);
        if self.shuffle {
            let v = self.order.remove(from);
            self.order.insert(to, v);
        } else {
            let item = self.items.remove(from);
            self.items.insert(to, item);
        }
        if let Some(uid) = current_uid {
            self.pos = self.order.iter().position(|&i| self.items[i].uid == uid);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.pos = None;
    }

    pub fn jump_to(&mut self, display_idx: usize) -> Option<&QueueItem> {
        if display_idx >= self.order.len() {
            return None;
        }
        self.pos = Some(display_idx);
        self.current()
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.pos.map(|p| &self.items[self.order[p]])
    }

    pub fn peek_next(&self) -> Option<&QueueItem> {
        let pos = self.pos?;
        if self.repeat == RepeatMode::One {
            return self.current();
        }
        if pos + 1 < self.order.len() {
            Some(&self.items[self.order[pos + 1]])
        } else if self.repeat == RepeatMode::All {
            self.order.first().map(|&i| &self.items[i])
        } else {
            None
        }
    }

    /// Natural end of a track: repeat One stays on the same item.
    pub fn advance(&mut self) -> Option<&QueueItem> {
        if self.repeat == RepeatMode::One {
            return self.current();
        }
        self.step_forward()
    }

    /// User skip: ignores repeat One, wraps only for repeat All.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&QueueItem> {
        self.step_forward()
    }

    fn step_forward(&mut self) -> Option<&QueueItem> {
        let pos = self.pos?;
        if pos + 1 < self.order.len() {
            self.pos = Some(pos + 1);
        } else if self.repeat == RepeatMode::All {
            if self.shuffle {
                // Keep the item peek_next reported first, and never replay the finished one.
                let first = self.order[0];
                self.order.shuffle(&mut self.rng);
                if let Some(at) = self.order.iter().position(|&i| i == first) {
                    self.order.swap(0, at);
                }
            }
            self.pos = Some(0);
        } else {
            self.pos = None;
        }
        self.current()
    }

    pub fn prev(&mut self) -> Option<&QueueItem> {
        let pos = self.pos?;
        self.pos = Some(pos.saturating_sub(1));
        self.current()
    }

    pub fn set_shuffle(&mut self, on: bool) {
        if self.shuffle == on {
            return;
        }
        self.shuffle = on;
        if on {
            self.shuffle_after_current();
        } else {
            let current = self.pos.map(|p| self.order[p]);
            self.order = (0..self.items.len()).collect();
            self.pos = current;
        }
    }

    pub fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        };
    }

    pub fn display_items(&self) -> Vec<&QueueItem> {
        self.order.iter().map(|&i| &self.items[i]).collect()
    }

    pub fn current_display_index(&self) -> Option<usize> {
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn songs(n: usize) -> Vec<Song> {
        (0..n)
            .map(|i| Song {
                id: i.to_string(),
                title: format!("t{i}"),
                artist: String::new(),
                artist_id: None,
                album: String::new(),
                album_id: None,
                track: None,
                year: None,
                duration: 0,
                cover_art: None,
                suffix: None,
                bit_rate: None,
                starred: false,
                ..Default::default()
            })
            .collect()
    }

    fn id(item: Option<&QueueItem>) -> Option<&str> {
        item.map(|i| i.song.id.as_str())
    }

    fn ids(q: &Queue) -> Vec<String> {
        q.display_items()
            .iter()
            .map(|i| i.song.id.clone())
            .collect()
    }

    #[test]
    fn replace_starts_at_index() {
        let mut q = Queue::with_seed(1);
        q.replace(songs(10), 3);
        assert_eq!(id(q.current()), Some("3"));
        assert_eq!(id(q.peek_next()), Some("4"));
    }

    #[test]
    fn advance_stops_at_end_when_repeat_off() {
        let mut q = Queue::with_seed(1);
        q.replace(songs(3), 2);
        assert!(q.advance().is_none());
        assert!(q.current().is_none());
    }

    #[test]
    fn repeat_all_wraps() {
        let mut q = Queue::with_seed(1);
        q.replace(songs(3), 2);
        q.repeat = RepeatMode::All;
        assert_eq!(id(q.advance()), Some("0"));
    }

    #[test]
    fn repeat_one_peeks_current_but_next_skips() {
        let mut q = Queue::with_seed(1);
        q.replace(songs(3), 0);
        q.repeat = RepeatMode::One;
        assert_eq!(id(q.peek_next()), Some("0"));
        assert_eq!(id(q.advance()), Some("0"));
        assert_eq!(id(q.next()), Some("1"));
    }

    #[test]
    fn shuffle_keeps_current_and_restores_order() {
        let mut q = Queue::with_seed(3);
        q.replace(songs(10), 3);
        q.set_shuffle(true);
        assert_eq!(id(q.current()), Some("3"));
        let shuffled = ids(&q);
        assert_eq!(shuffled[..4], ["0", "1", "2", "3"]);
        let mut sorted = shuffled.clone();
        sorted.sort();
        let expected: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        assert_eq!(sorted, expected);
        q.set_shuffle(false);
        assert_eq!(ids(&q), expected);
        assert_eq!(id(q.current()), Some("3"));
    }

    #[test]
    fn insert_next_while_shuffled_is_next() {
        let mut q = Queue::with_seed(3);
        q.replace(songs(10), 3);
        q.set_shuffle(true);
        let mut extra = songs(1);
        extra[0].id = "x".into();
        q.insert_next(extra);
        assert_eq!(id(q.peek_next()), Some("x"));
        assert_eq!(id(q.current()), Some("3"));
    }

    #[test]
    fn remove_current_promotes_following() {
        let mut q = Queue::with_seed(1);
        q.replace(songs(10), 3);
        assert_eq!(q.remove(3), Removed::Current);
        assert_eq!(id(q.current()), Some("4"));
        assert_eq!(q.remove(0), Removed::Other);
        assert_eq!(id(q.current()), Some("4"));
        assert_eq!(q.remove(99), Removed::None);
    }

    #[test]
    fn remove_last_current_clears_pos() {
        let mut q = Queue::with_seed(1);
        q.replace(songs(3), 2);
        assert_eq!(q.remove(2), Removed::Current);
        assert!(q.current().is_none());
    }

    #[test]
    fn move_item_reorders_display() {
        let mut q = Queue::with_seed(1);
        q.replace(songs(8), 5);
        q.move_item(5, 1);
        assert_eq!(ids(&q)[..6], ["0", "5", "1", "2", "3", "4"]);
        assert_eq!(id(q.current()), Some("5"));
        assert_eq!(q.current_display_index(), Some(1));
    }

    #[test]
    fn repeat_all_shuffle_reshuffles_each_pass() {
        let mut q = Queue::with_seed(7);
        q.replace(songs(10), 0);
        q.set_shuffle(true);
        q.repeat = RepeatMode::All;
        let before = ids(&q);
        for _ in 0..10 {
            q.advance();
        }
        let after = ids(&q);
        assert_ne!(before, after);
        let (mut a, mut b) = (before, after);
        a.sort();
        b.sort();
        assert_eq!(a, b);
        assert_eq!(q.current_display_index(), Some(0));
    }

    #[test]
    fn shuffled_wrap_matches_peek_and_avoids_repeat() {
        for seed in 0..20 {
            let mut q = Queue::with_seed(seed);
            q.replace(songs(10), 0);
            q.set_shuffle(true);
            q.repeat = RepeatMode::All;
            for _ in 0..9 {
                q.advance();
            }
            let finished = q.current().unwrap().uid;
            let peeked = q.peek_next().unwrap().uid;
            let landed = q.advance().unwrap().uid;
            assert_eq!(peeked, landed, "seed {seed}");
            assert_ne!(landed, finished, "seed {seed}");
        }
    }

    #[test]
    fn same_song_twice_has_distinct_uids() {
        let mut q = Queue::with_seed(1);
        let s = songs(1);
        q.append(s.clone());
        q.append(s);
        assert_eq!(q.items[0].song, q.items[1].song);
        assert_ne!(q.items[0].uid, q.items[1].uid);
    }
}
