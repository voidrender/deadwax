//! Discord Rich Presence worker. Only public art URLs and track metadata are sent.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{Activity, ActivityType, Assets, Timestamps};
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
    let start_ts = now_unix - info.position.max(0.0) as i64;
    let end_ts = (info.duration > 0).then(|| start_ts + i64::from(info.duration));
    let large_text = if info.album.is_empty() {
        &info.title
    } else {
        &info.album
    };
    ActivitySpec {
        details: fit(&info.title),
        state: fit(&format!("by {}", info.artist)),
        large_image: fit(info.image_url.as_deref().unwrap_or(default_image)),
        large_text: fit(large_text),
        start_ts,
        end_ts,
    }
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
        .name("senritsu-discord".into())
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
        let mut ts = Timestamps::new().start(spec.start_ts * 1000);
        if let Some(end) = spec.end_ts {
            ts = ts.end(end * 1000);
        }
        let assets = Assets::new()
            .large_image(&spec.large_image)
            .large_text(&spec.large_text);
        client.set_activity(
            Activity::new()
                .activity_type(ActivityType::Listening)
                .details(&spec.details)
                .state(&spec.state)
                .timestamps(ts)
                .assets(assets),
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
        .map(|d| d.as_secs() as i64)
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
        let a = build_activity(&info(), "senritsu", 1000);
        assert_eq!(a.start_ts, 970);
        assert_eq!(a.end_ts, Some(1170));
        assert_eq!(a.details, "Song");
        assert_eq!(a.state, "by Artist");
        assert_eq!(a.large_image, "https://art.example/c.jpg");
        assert_eq!(a.large_text, "Album");
    }

    #[test]
    fn zero_duration_has_no_end() {
        let mut i = info();
        i.duration = 0;
        assert_eq!(build_activity(&i, "senritsu", 1000).end_ts, None);
    }

    #[test]
    fn missing_image_uses_default_key() {
        let mut i = info();
        i.image_url = None;
        assert_eq!(build_activity(&i, "senritsu", 1000).large_image, "senritsu");
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
}
