use crate::subsonic::Song;

/// Largest gap between two position updates that still counts as normal playback.
const MAX_TICK_SECS: f64 = 2.0;

/// Listened seconds needed before a track counts as played.
///
/// A duration of 0 (unknown) uses the 240 second cap.
pub fn threshold_secs(duration: u32) -> f64 {
    if duration == 0 {
        240.0
    } else {
        (f64::from(duration) / 2.0).min(240.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrobbleAction {
    NowPlaying { song_id: String },
    Submit { song_id: String, time_ms: i64 },
}

#[derive(Debug)]
struct Current {
    song_id: String,
    started_ms: i64,
    threshold: f64,
    listened: f64,
    last_pos: Option<f64>,
    submitted: bool,
}

/// Counts actual listening time for the current track.
///
/// Time comes from the caller (`now_ms`), never from the clock.
#[derive(Debug, Default)]
pub struct ScrobbleTracker {
    current: Option<Current>,
}

impl ScrobbleTracker {
    /// Begins a new play. Any previous track is abandoned without a submit.
    pub fn track_started(&mut self, song: &Song, now_ms: i64) -> Vec<ScrobbleAction> {
        self.current = Some(Current {
            song_id: song.id.clone(),
            started_ms: now_ms,
            threshold: threshold_secs(song.duration),
            listened: 0.0,
            last_pos: None,
            submitted: false,
        });
        vec![ScrobbleAction::NowPlaying {
            song_id: song.id.clone(),
        }]
    }

    /// Feeds a playback position update.
    ///
    /// Time is added only when the position advanced by more than 0 and at most
    /// 2 seconds since the previous update and playback is not paused. Forward
    /// seeks and backward seeks add nothing, and the next delta is measured
    /// from the new position. The first update after `track_started` has no
    /// previous position: if it is at most 2 seconds in, it counts from 0;
    /// otherwise it only records the position.
    pub fn position(&mut self, pos: f64, paused: bool) -> Vec<ScrobbleAction> {
        let Some(cur) = self.current.as_mut() else {
            return Vec::new();
        };
        match cur.last_pos {
            None if pos > 0.0 && pos <= MAX_TICK_SECS && !paused => cur.listened += pos,
            Some(last) => {
                let delta = pos - last;
                if !paused && delta > 0.0 && delta <= MAX_TICK_SECS {
                    cur.listened += delta;
                }
            }
            None => {}
        }
        cur.last_pos = Some(pos);

        if !cur.submitted && cur.listened >= cur.threshold {
            cur.submitted = true;
            return vec![ScrobbleAction::Submit {
                song_id: cur.song_id.clone(),
                time_ms: cur.started_ms,
            }];
        }
        Vec::new()
    }

    #[cfg(test)]
    fn listened(&self) -> f64 {
        self.current.as_ref().map_or(0.0, |c| c.listened)
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests compare exact, known values")]
mod tests {
    use super::*;

    fn song(id: &str, duration: u32) -> Song {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": "t", "duration": duration
        }))
        .unwrap()
    }

    /// Ticks every 0.5 s from `from` up to `to`, collecting submits.
    fn run(t: &mut ScrobbleTracker, from: f64, to: f64) -> Vec<(f64, ScrobbleAction)> {
        let mut out = Vec::new();
        let mut pos = from;
        while pos <= to + 1e-9 {
            for a in t.position(pos, false) {
                out.push((t.listened(), a));
            }
            pos += 0.5;
        }
        out
    }

    #[test]
    fn now_playing_on_start() {
        let mut t = ScrobbleTracker::default();
        let actions = t.track_started(&song("a", 200), 1000);
        assert_eq!(
            actions,
            vec![ScrobbleAction::NowPlaying {
                song_id: "a".into()
            }]
        );
    }

    #[test]
    fn threshold_values() {
        assert_eq!(threshold_secs(0), 240.0);
        assert_eq!(threshold_secs(200), 100.0);
        assert_eq!(threshold_secs(600), 240.0);
    }

    #[test]
    fn submits_at_half_duration() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 200), 1234);
        let hits = run(&mut t, 0.0, 200.0);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 100.0);
        assert_eq!(
            hits[0].1,
            ScrobbleAction::Submit {
                song_id: "a".into(),
                time_ms: 1234
            }
        );
    }

    #[test]
    fn four_minute_cap() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 600), 0);
        let hits = run(&mut t, 0.0, 600.0);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 240.0);
    }

    #[test]
    fn zero_duration_uses_four_minutes() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 0), 0);
        let hits = run(&mut t, 0.0, 300.0);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, 240.0);
    }

    #[test]
    fn seek_forward_does_not_count() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 300), 0);
        let mut hits = run(&mut t, 0.0, 10.0);
        hits.extend(run(&mut t, 150.0, 160.0));
        assert!(hits.is_empty());
        assert!((t.listened() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn backward_seek_adds_nothing_then_resumes() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 300), 0);
        run(&mut t, 0.0, 10.0);
        run(&mut t, 5.0, 8.0);
        assert!((t.listened() - 13.0).abs() < 1e-9);
    }

    #[test]
    fn first_position_rules() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 300), 0);
        t.position(1.0, false);
        assert_eq!(t.listened(), 1.0);

        t.track_started(&song("a", 300), 0);
        t.position(50.0, false);
        assert_eq!(t.listened(), 0.0);
        t.position(50.5, false);
        assert_eq!(t.listened(), 0.5);
    }

    #[test]
    fn paused_ticks_do_not_count() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 300), 0);
        t.position(0.0, false);
        for i in 1..=20 {
            t.position(f64::from(i) * 0.5, true);
        }
        assert_eq!(t.listened(), 0.0);
        t.position(10.5, false);
        assert_eq!(t.listened(), 0.5);
    }

    #[test]
    fn skip_before_threshold_never_submits() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 200), 0);
        assert!(run(&mut t, 0.0, 50.0).is_empty());
        t.track_started(&song("b", 200), 10);
        let hits = run(&mut t, 0.0, 200.0);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].1,
            ScrobbleAction::Submit {
                song_id: "b".into(),
                time_ms: 10
            }
        );
    }

    #[test]
    fn replay_can_scrobble_again() {
        let mut t = ScrobbleTracker::default();
        t.track_started(&song("a", 100), 1);
        assert_eq!(run(&mut t, 0.0, 100.0).len(), 1);
        t.track_started(&song("a", 100), 2);
        let hits = run(&mut t, 0.0, 100.0);
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].1,
            ScrobbleAction::Submit {
                song_id: "a".into(),
                time_ms: 2
            }
        );
    }
}
