use std::path::{Path, PathBuf};

use anyhow::bail;

use super::{Notifier, TrackNotification};

/// Posts notifications through the signed `SenritsuNotifier.app` helper.
pub struct MacNotifier {
    pub app_path: PathBuf,
}

impl MacNotifier {
    /// Uses `SENRITSU_NOTIFIER_APP` when set, else `~/Applications/SenritsuNotifier.app`.
    pub fn from_env() -> Self {
        let app_path = std::env::var_os("SENRITSU_NOTIFIER_APP")
            .map(PathBuf::from)
            .unwrap_or_else(default_app_path);
        Self { app_path }
    }
}

fn default_app_path() -> PathBuf {
    directories::UserDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .unwrap_or_default()
        .join("Applications/SenritsuNotifier.app")
}

/// Arguments passed to the helper after `--args`.
pub fn helper_args(n: &TrackNotification) -> Vec<String> {
    let mut args = vec![
        "--title".to_string(),
        n.title.clone(),
        "--subtitle".to_string(),
        n.artist.clone(),
        "--body".to_string(),
        n.album.clone(),
    ];
    if let Some(path) = &n.image_path {
        args.push("--image".to_string());
        args.push(path.to_string_lossy().into_owned());
    }
    args
}

fn ensure_installed(app_path: &Path) -> anyhow::Result<()> {
    if !app_path.exists() {
        bail!("Notifier helper not installed. Run scripts/install-macos-notifier.sh");
    }
    Ok(())
}

impl Notifier for MacNotifier {
    fn notify(&self, n: &TrackNotification) -> anyhow::Result<()> {
        ensure_installed(&self.app_path)?;
        self.launch(n)
    }
}

impl MacNotifier {
    #[cfg(target_os = "macos")]
    fn launch(&self, n: &TrackNotification) -> anyhow::Result<()> {
        let mut child = std::process::Command::new("open")
            .args(["-n", "-g", "-a"])
            .arg(&self.app_path)
            .arg("--args")
            .args(helper_args(n))
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to run open: {e}"))?;
        // `open` returns quickly; reap it off-thread so no zombie remains.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    fn launch(&self, _n: &TrackNotification) -> anyhow::Result<()> {
        bail!("macOS notifications are unavailable on this platform")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(image: Option<&str>) -> TrackNotification {
        TrackNotification {
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            image_path: image.map(PathBuf::from),
        }
    }

    #[test]
    fn helper_args_include_image_when_present() {
        assert_eq!(
            helper_args(&note(Some("/tmp/c.jpg"))),
            [
                "--title",
                "Song",
                "--subtitle",
                "Artist",
                "--body",
                "Album",
                "--image",
                "/tmp/c.jpg"
            ]
        );
    }

    #[test]
    fn helper_args_omit_image_when_absent() {
        assert_eq!(
            helper_args(&note(None)),
            ["--title", "Song", "--subtitle", "Artist", "--body", "Album"]
        );
    }

    #[test]
    fn missing_bundle_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let notifier = MacNotifier {
            app_path: dir.path().join("SenritsuNotifier.app"),
        };
        let err = notifier.notify(&note(None)).unwrap_err().to_string();
        assert!(err.contains("install-macos-notifier.sh"), "{err}");
    }
}
