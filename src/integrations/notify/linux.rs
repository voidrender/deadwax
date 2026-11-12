use notify_rust::{Hint, Notification, Timeout};

use super::{Notifier, TrackNotification};

pub struct LinuxNotifier;

impl Notifier for LinuxNotifier {
    fn notify(&self, n: &TrackNotification) -> anyhow::Result<()> {
        let mut note = Notification::new();
        note.appname("Senritsu")
            .summary(&n.title)
            .body(&format!("{}\n{}", n.artist, n.album))
            .timeout(Timeout::Milliseconds(5000));
        if let Some(path) = &n.image_path {
            note.hint(Hint::ImagePath(path.to_string_lossy().into_owned()));
        }
        note.show()?;
        Ok(())
    }
}
