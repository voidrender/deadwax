use directories::ProjectDirs;
use std::path::{Path, PathBuf};

/// Config directory: `$XDG_CONFIG_HOME/senritsu`, else `~/.config/senritsu`.
pub fn config_dir_from(xdg: Option<&Path>, home: &Path) -> PathBuf {
    match xdg {
        Some(x) => x.join("senritsu"),
        None => home.join(".config").join("senritsu"),
    }
}

pub fn config_dir() -> PathBuf {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let home = directories::BaseDirs::new()
        .map_or_else(|| PathBuf::from("."), |b| b.home_dir().to_path_buf());
    config_dir_from(xdg.as_deref(), &home)
}

fn project_dirs() -> ProjectDirs {
    ProjectDirs::from("", "", "senritsu").expect("home directory is available")
}

fn ensure(dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!("could not create {}: {e}", dir.display());
    }
}

/// Platform cache directory, created on first use.
pub fn cache_dir() -> PathBuf {
    let dir = project_dirs().cache_dir().to_path_buf();
    ensure(&dir);
    dir
}

/// Platform data directory, created on first use.
pub fn data_dir() -> PathBuf {
    let dir = project_dirs().data_dir().to_path_buf();
    ensure(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_prefers_xdg() {
        assert_eq!(
            config_dir_from(Some(Path::new("/x")), Path::new("/home/a")),
            PathBuf::from("/x/senritsu")
        );
        assert_eq!(
            config_dir_from(None, Path::new("/home/a")),
            PathBuf::from("/home/a/.config/senritsu")
        );
    }
}
