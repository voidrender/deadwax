//! Track-change system notifications.

#[cfg(target_os = "linux")]
mod linux;
mod macos;

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub use linux::LinuxNotifier;
pub use macos::MacNotifier;

/// What to show when a track starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackNotification {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub image_path: Option<PathBuf>,
}

pub trait Notifier: Send + Sync {
    fn notify(&self, n: &TrackNotification) -> anyhow::Result<()>;
}

/// Notifier for the current platform.
pub fn platform_notifier() -> Box<dyn Notifier> {
    #[cfg(target_os = "linux")]
    {
        Box::new(LinuxNotifier)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Box::new(MacNotifier::from_env())
    }
}
