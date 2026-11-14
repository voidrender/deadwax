use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::bail;

use super::{Notifier, TrackNotification};

/// Posts notifications through the signed `DeadwaxNotifier.app` helper.
pub struct MacNotifier {
    pub app_path: PathBuf,
}

impl MacNotifier {
    /// Finds the helper app. See [`resolve_app_path`] for the search order.
    pub fn from_env() -> Self {
        let exe = std::env::current_exe().and_then(std::fs::canonicalize).ok();
        let home = directories::UserDirs::new().map(|d| d.home_dir().to_path_buf());
        Self {
            app_path: resolve_app_path(
                std::env::var_os("DEADWAX_NOTIFIER_APP"),
                exe.as_deref(),
                home.as_deref(),
            ),
        }
    }
}

const APP_NAME: &str = "DeadwaxNotifier.app";

/// Picks the helper app path, in this order:
///
/// 1. `DEADWAX_NOTIFIER_APP`, when set.
/// 2. `../libexec/DeadwaxNotifier.app` next to the executable, when it
///    exists. Package managers such as Homebrew install it there. `exe` must
///    already have symlinks resolved, because Homebrew's `bin/deadwax` is a
///    symlink into the package's own directory.
/// 3. `~/Applications/DeadwaxNotifier.app`, where the install script puts it.
fn resolve_app_path(env: Option<OsString>, exe: Option<&Path>, home: Option<&Path>) -> PathBuf {
    if let Some(path) = env {
        return PathBuf::from(path);
    }
    let bundled = exe
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(|prefix| prefix.join("libexec").join(APP_NAME));
    if let Some(path) = bundled.filter(|p| p.exists()) {
        return path;
    }
    home.unwrap_or_else(|| Path::new(""))
        .join("Applications")
        .join(APP_NAME)
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
    /// Runs the helper's executable directly and waits for it, so its exit
    /// code arrives. Launching through `open` would hide it. The helper
    /// gives up after 5 seconds on its own. This blocks, so callers run it
    /// on a blocking thread.
    fn launch(&self, n: &TrackNotification) -> anyhow::Result<()> {
        let exe = self.app_path.join("Contents/MacOS/DeadwaxNotifier");
        // Output is captured so nothing the helper prints lands on the TUI.
        let out = std::process::Command::new(&exe)
            .args(helper_args(n))
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| anyhow::anyhow!("couldn't run {}: {e}", exe.display()))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.trim().is_empty() {
            tracing::debug!("notifier: {}", stderr.trim());
        }
        exit_result(out.status.code())
    }
}

/// Maps the helper's exit code (see `macos/DeadwaxNotifier/main.swift`).
fn exit_result(code: Option<i32>) -> anyhow::Result<()> {
    match code {
        Some(0) => Ok(()),
        Some(2) => bail!(
            "macOS isn't allowing them. Turn on notifications for Deadwax in \
             System Settings > Notifications"
        ),
        Some(code) => bail!("the helper failed with exit code {code}"),
        None => bail!("the helper was stopped by a signal"),
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
            app_path: dir.path().join("DeadwaxNotifier.app"),
        };
        let err = notifier.notify(&note(None)).unwrap_err().to_string();
        assert!(err.contains("install-macos-notifier.sh"), "{err}");
    }

    /// A stand-in helper app whose executable exits with `code`.
    fn fake_helper(dir: &tempfile::TempDir, code: i32) -> MacNotifier {
        use std::os::unix::fs::PermissionsExt;
        let app = dir.path().join("DeadwaxNotifier.app");
        let bin = app.join("Contents/MacOS");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("DeadwaxNotifier");
        std::fs::write(&exe, format!("#!/bin/sh\necho noise\nexit {code}\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        MacNotifier { app_path: app }
    }

    #[test]
    fn helper_exit_codes_become_errors() {
        let dir = tempfile::tempdir().unwrap();
        fake_helper(&dir, 0).notify(&note(None)).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let err = fake_helper(&dir, 2).notify(&note(None)).unwrap_err();
        assert!(err.to_string().contains("System Settings"), "{err}");

        let dir = tempfile::tempdir().unwrap();
        let err = fake_helper(&dir, 1).notify(&note(None)).unwrap_err();
        assert!(err.to_string().contains("exit code 1"), "{err}");
    }

    #[test]
    fn env_var_wins() {
        let dir = tempfile::tempdir().unwrap();
        let exe = bundled_prefix(&dir).join("bin/deadwax");
        let got = resolve_app_path(Some("/x/Custom.app".into()), Some(&exe), Some(dir.path()));
        assert_eq!(got, PathBuf::from("/x/Custom.app"));
    }

    #[test]
    fn bundled_helper_beats_home() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = bundled_prefix(&dir);
        let got = resolve_app_path(None, Some(&prefix.join("bin/deadwax")), Some(dir.path()));
        assert_eq!(got, prefix.join("libexec/DeadwaxNotifier.app"));
    }

    #[test]
    fn falls_back_to_home_applications() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("cargo/bin/deadwax");
        let got = resolve_app_path(None, Some(&exe), Some(Path::new("/Users/a")));
        assert_eq!(
            got,
            PathBuf::from("/Users/a/Applications/DeadwaxNotifier.app")
        );
    }

    /// Lays out `<dir>/Cellar/deadwax/1.0/libexec/DeadwaxNotifier.app`, the
    /// way Homebrew installs it, and returns the version directory.
    fn bundled_prefix(dir: &tempfile::TempDir) -> PathBuf {
        let prefix = dir.path().join("Cellar/deadwax/1.0");
        std::fs::create_dir_all(prefix.join("libexec/DeadwaxNotifier.app")).unwrap();
        prefix
    }
}
