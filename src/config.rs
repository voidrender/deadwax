use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

fn default_image() -> String {
    "senritsu".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub server_url: String,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord_app_id: Option<String>,
    #[serde(default = "default_image")]
    pub discord_default_image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_protocol: Option<String>,
}

impl Config {
    /// Loads the config, returning `None` when the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Config>> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let c =
                    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                Ok(Some(c))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Writes the config, creating the parent directory if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let c = Config {
            server_url: "https://music.example".into(),
            username: "alice".into(),
            discord_app_id: Some("123".into()),
            discord_default_image: "senritsu".into(),
            image_protocol: None,
        };
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), Some(c));
    }

    #[test]
    fn save_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/config.toml");
        let c: Config = toml::from_str("server_url = \"https://x\"\nusername = \"a\"").unwrap();
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), Some(c));
    }

    #[test]
    fn missing_config_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Config::load(&dir.path().join("nope.toml")).unwrap(), None);
    }

    #[test]
    fn default_image_defaults_when_absent() {
        let c: Config = toml::from_str("server_url = \"https://x\"\nusername = \"a\"").unwrap();
        assert_eq!(c.discord_default_image, "senritsu");
    }
}
