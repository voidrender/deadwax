use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Application ID of the Deadwax app in the Discord developer portal. It is a
/// public identifier, not a secret, so it ships in the binary.
pub const DEFAULT_DISCORD_APP_ID: &str = "1570279686548881418";

fn default_image() -> String {
    "deadwax".to_string()
}

fn default_true() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_true(v: &bool) -> bool {
    *v
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub server_url: String,
    pub username: String,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub discord: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord_app_id: Option<String>,
    #[serde(default = "default_image")]
    pub discord_default_image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_protocol: Option<String>,
}

impl Config {
    /// The Discord application ID to show presence under, or `None` when
    /// presence is turned off. A non-empty `discord_app_id` overrides the
    /// built-in ID.
    pub fn presence_app_id(&self) -> Option<&str> {
        if !self.discord {
            return None;
        }
        let configured = self.discord_app_id.as_deref().map(str::trim);
        Some(
            configured
                .filter(|id| !id.is_empty())
                .unwrap_or(DEFAULT_DISCORD_APP_ID),
        )
    }

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
            discord: false,
            discord_app_id: Some("123".into()),
            discord_default_image: "deadwax".into(),
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
        assert_eq!(c.discord_default_image, "deadwax");
    }

    fn parse(extra: &str) -> Config {
        toml::from_str(&format!(
            "server_url = \"https://x\"\nusername = \"a\"\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn presence_uses_built_in_id_by_default() {
        let c = parse("");
        assert!(c.discord);
        assert_eq!(c.presence_app_id(), Some(DEFAULT_DISCORD_APP_ID));
    }

    #[test]
    fn presence_prefers_configured_id() {
        assert_eq!(
            parse("discord_app_id = \" 42 \"").presence_app_id(),
            Some("42")
        );
    }

    #[test]
    fn presence_ignores_blank_configured_id() {
        let c = parse("discord_app_id = \"  \"");
        assert_eq!(c.presence_app_id(), Some(DEFAULT_DISCORD_APP_ID));
    }

    #[test]
    fn presence_off_when_disabled() {
        let c = parse("discord = false\ndiscord_app_id = \"42\"");
        assert_eq!(c.presence_app_id(), None);
    }

    #[test]
    fn enabled_discord_is_not_written() {
        let text = toml::to_string_pretty(&parse("")).unwrap();
        assert!(!text.contains("discord ="), "{text}");
    }
}
