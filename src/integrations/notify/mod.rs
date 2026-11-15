//! Track-change system notifications.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub use linux::LinuxNotifier;
#[cfg(target_os = "macos")]
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
    #[cfg(target_os = "macos")]
    {
        Box::new(MacNotifier::from_env())
    }
}
