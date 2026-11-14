//! Discord Rich Presence worker. Only public art URLs and track metadata are sent.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{
    Activity, ActivityType, Assets, Button, StatusDisplayType, Timestamps,
};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq)]
pub struct PresenceInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub image_url: Option<String>,
    pub position: f64,
    pub duration: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivitySpec {
    pub details: String,
    pub state: String,
    pub large_image: String,
    pub large_text: String,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
}

/// Makes a string acceptable to Discord: at least 2 bytes, at most 128 bytes.
pub fn fit(s: &str) -> String {
    const MAX: usize = 128;
    let mut end = s.len().min(MAX);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    while out.len() < 2 {
        out.push(' ');
    }
    out
}

pub fn build_activity(info: &PresenceInfo, default_image: &str, now_unix: i64) -> ActivitySpec {
    let position = if info.position.is_finite() && info.position > 0.0 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "whole seconds are wanted, and the cast saturates"
        )]
        let secs = info.position as i64;
        secs
    } else {
        0
    };
    let start_ts = now_unix.saturating_sub(position);
    let end_ts = (info.duration > 0).then(|| start_ts.saturating_add(i64::from(info.duration)));
    let large_text = if info.album.is_empty() {
        &info.title
    } else {
        &info.album
    };
    ActivitySpec {
        details: fit(&info.title),
        state: fit(&info.artist),
        large_image: match info.image_url.as_deref() {
            Some(url) if url.len() <= MAX_IMAGE_URL_BYTES => url.to_string(),
            _ => fit(default_image),
        },
        large_text: fit(large_text),
        start_ts,
        end_ts,
    }
}

/// Label of the presence button that links to the project.
const REPO_BUTTON_LABEL: &str = "Get Deadwax";

/// Where the presence button points.
const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");

/// Discord accepts image URLs up to 256 bytes; longer ones cannot be truncated safely.
const MAX_IMAGE_URL_BYTES: usize = 256;

/// Converts unix seconds to milliseconds, or `None` on overflow.
fn secs_to_millis(secs: i64) -> Option<i64> {
    secs.checked_mul(1000)
}

/// Messages accepted by the presence worker.
#[derive(Debug, Clone)]
pub enum PresenceMsg {
    Update(PresenceInfo),
    Clear,
    Shutdown,
}

const RETRY_INTERVAL: Duration = Duration::from_secs(15);

/// Spawns the presence worker thread. The returned sender never blocks the caller.
pub fn spawn_presence(app_id: String, default_image: String) -> mpsc::UnboundedSender<PresenceMsg> {
    let (tx, rx) = mpsc::unbounded_channel();
    let spawned = std::thread::Builder::new()
        .name("deadwax-discord".into())
        .spawn(move || run_worker(&app_id, &default_image, rx));
    if let Err(e) = spawned {
        tracing::debug!("discord worker spawn failed: {e}");
    }
    tx
}

struct Worker {
    app_id: String,
    client: Option<DiscordIpcClient>,
}

impl Worker {
    fn apply(&mut self, spec: &ActivitySpec) -> bool {
        match self.try_apply(spec) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!("discord presence update failed: {e}");
                self.client = None;
                false
            }
        }
    }

    fn try_apply(
        &mut self,
        spec: &ActivitySpec,
    ) -> Result<(), discord_rich_presence::error::Error> {
        if self.client.is_none() {
            let mut client = DiscordIpcClient::new(&self.app_id);
            client.connect()?;
            self.client = Some(client);
        }
        let client = self.client.as_mut().expect("client set above");
        // The crate documents timestamps in milliseconds; the spec is in seconds.
        let mut ts = Timestamps::new();
        if let Some(start) = secs_to_millis(spec.start_ts) {
            ts = ts.start(start);
            if let Some(end) = spec.end_ts.and_then(secs_to_millis) {
                ts = ts.end(end);
            }
        }
        let assets = Assets::new()
            .large_image(&spec.large_image)
            .large_text(&spec.large_text);
        client.set_activity(
            Activity::new()
                .activity_type(ActivityType::Listening)
                // The member list shows the artist instead of the app name.
                .status_display_type(StatusDisplayType::State)
                .details(&spec.details)
                .state(&spec.state)
                .timestamps(ts)
                .assets(assets)
                .buttons(vec![Button::new(REPO_BUTTON_LABEL, REPO_URL)]),
        )
    }

    fn clear(&mut self) {
        if let Some(client) = self.client.as_mut()
            && let Err(e) = client.clear_activity()
        {
            tracing::debug!("discord presence clear failed: {e}");
            self.client = None;
        }
    }

    fn close(&mut self) {
        self.clear();
        if let Some(mut client) = self.client.take()
            && let Err(e) = client.close()
        {
            tracing::debug!("discord close failed: {e}");
        }
    }
}

fn run_worker(app_id: &str, default_image: &str, mut rx: mpsc::UnboundedReceiver<PresenceMsg>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::debug!("discord runtime failed: {e}");
            return;
        }
    };
    let mut worker = Worker {
        app_id: app_id.to_string(),
        client: None,
    };
    // A spec that still needs to reach Discord.
    let mut pending: Option<ActivitySpec> = None;
    loop {
        // `Err(())` means the retry interval elapsed with no message.
        let next = rt.block_on(async {
            if pending.is_some() {
                tokio::time::timeout(RETRY_INTERVAL, rx.recv())
                    .await
                    .map_err(|_| ())
            } else {
                Ok(rx.recv().await)
            }
        });
        match next {
            Ok(Some(PresenceMsg::Update(info))) => {
                let spec = build_activity(&info, default_image, unix_now());
                pending = (!worker.apply(&spec)).then_some(spec);
            }
            Ok(Some(PresenceMsg::Clear)) => {
                pending = None;
                worker.clear();
            }
            Ok(Some(PresenceMsg::Shutdown) | None) => break,
            Err(()) => {
                if let Some(spec) = pending.take()
                    && !worker.apply(&spec)
                {
                    pending = Some(spec);
                }
            }
        }
    }
    worker.close();
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> PresenceInfo {
        PresenceInfo {
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            image_url: Some("https://art.example/c.jpg".into()),
            position: 30.0,
            duration: 200,
        }
    }

    #[test]
    fn timestamps_reflect_position() {
        let a = build_activity(&info(), "deadwax", 1000);
        assert_eq!(a.start_ts, 970);
        assert_eq!(a.end_ts, Some(1170));
        assert_eq!(a.details, "Song");
        assert_eq!(a.state, "Artist");
        assert_eq!(a.large_image, "https://art.example/c.jpg");
        assert_eq!(a.large_text, "Album");
    }

    #[test]
    fn repo_button_fits_discord_limits() {
        assert!((1..=32).contains(&REPO_BUTTON_LABEL.len()));
        assert!(REPO_URL.starts_with("https://"));
        assert!(REPO_URL.len() <= 512);
    }

    #[test]
    fn zero_duration_has_no_end() {
        let mut i = info();
        i.duration = 0;
        assert_eq!(build_activity(&i, "deadwax", 1000).end_ts, None);
    }

    #[test]
    fn missing_image_uses_default_key() {
        let mut i = info();
        i.image_url = None;
        assert_eq!(build_activity(&i, "deadwax", 1000).large_image, "deadwax");
    }

    #[test]
    fn short_and_long_strings_are_fixed() {
        let mut i = info();
        i.title = "X".into();
        assert_eq!(build_activity(&i, "k", 1000).details, "X ");
        assert_eq!(fit(""), "  ");
        i.title = "旋".repeat(60);
        let d = build_activity(&i, "k", 1000).details;
        assert!(d.len() <= 128);
        assert!(d.len() >= 2);
        assert_eq!(d, "旋".repeat(42));
    }

    #[test]
    fn empty_album_uses_title_for_large_text() {
        let mut i = info();
        i.album = String::new();
        assert_eq!(build_activity(&i, "k", 1000).large_text, "Song");
    }

    #[test]
    fn long_image_urls_fall_back_to_default() {
        let url = |n: usize| format!("https://a.example/{}", "x".repeat(n - 18));
        let mut i = info();
        i.image_url = Some(url(256));
        assert_eq!(build_activity(&i, "k", 1000).large_image, url(256));
        i.image_url = Some(url(257));
        assert_eq!(build_activity(&i, "k", 1000).large_image, "k ");
        let multi = format!("https://a.example/{}", "旋".repeat(79));
        assert!(multi.len() <= 256 && multi.len() > 128);
        i.image_url = Some(multi.clone());
        assert_eq!(build_activity(&i, "k", 1000).large_image, multi);
    }

    #[test]
    fn non_finite_position_is_treated_as_zero() {
        for p in [f64::INFINITY, f64::NAN, f64::NEG_INFINITY, -5.0] {
            let mut i = info();
            i.position = p;
            let a = build_activity(&i, "k", 1000);
            assert_eq!(a.start_ts, 1000);
            assert_eq!(a.end_ts, Some(1200));
        }
    }

    #[test]
    fn millis_conversion_does_not_overflow() {
        assert_eq!(secs_to_millis(1), Some(1000));
        assert_eq!(secs_to_millis(i64::MAX), None);
        assert_eq!(secs_to_millis(i64::MIN), None);
    }
}
