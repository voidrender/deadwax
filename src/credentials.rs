use crate::subsonic::Auth;
use anyhow::Context;
use std::path::PathBuf;

const SERVICE: &str = "senritsu";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// No usable secret store (no backend, platform failure, or no storage access).
    Unavailable(String),
    Other(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "secret store unavailable: {m}"),
            Self::Other(m) => write!(f, "secret store error: {m}"),
        }
    }
}

impl std::error::Error for SecretError {}

pub trait SecretBackend {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// The OS keychain (macOS Keychain, Linux Secret Service).
pub struct KeyringBackend;

impl KeyringBackend {
    fn entry(account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(SERVICE, account).map_err(map_err)
    }
}

fn map_err(e: keyring::Error) -> SecretError {
    match e {
        keyring::Error::PlatformFailure(_)
        | keyring::Error::NoStorageAccess(_)
        | keyring::Error::NoDefaultStore
        | keyring::Error::NotSupportedByStore(_) => SecretError::Unavailable(e.to_string()),
        other => SecretError::Other(other.to_string()),
    }
}

impl SecretBackend for KeyringBackend {
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        match Self::entry(account)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        Self::entry(account)?.set_password(secret).map_err(map_err)
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedTo {
    Keychain,
    File,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TokenFile {
    salt: String,
    token: String,
}

pub struct CredentialStore<S: SecretBackend> {
    secrets: S,
    file_path: PathBuf,
}

fn account(server_url: &str, username: &str) -> String {
    format!("{username}@{server_url}")
}

impl<S: SecretBackend> CredentialStore<S> {
    pub fn new(secrets: S, file_path: PathBuf) -> Self {
        Self { secrets, file_path }
    }

    /// Stores the password in the keychain. When no keychain is available,
    /// writes only a salted token to a 0600 file.
    pub fn save(
        &self,
        server_url: &str,
        username: &str,
        password: &str,
    ) -> anyhow::Result<SavedTo> {
        match self.secrets.set(&account(server_url, username), password) {
            Ok(()) => {
                self.remove_file()?;
                Ok(SavedTo::Keychain)
            }
            Err(SecretError::Unavailable(reason)) => {
                tracing::warn!("keychain unavailable, using token file: {reason}");
                self.write_file(password)?;
                Ok(SavedTo::File)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Prefers the keychain password, then the token file.
    pub fn load(&self, server_url: &str, username: &str) -> anyhow::Result<Option<Auth>> {
        match self.secrets.get(&account(server_url, username)) {
            Ok(Some(password)) => return Ok(Some(Auth::Password(password))),
            Ok(None) | Err(SecretError::Unavailable(_)) => {}
            Err(e) => return Err(e.into()),
        }
        let text = match std::fs::read_to_string(&self.file_path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context("reading credentials file"),
        };
        let file: TokenFile = toml::from_str(&text).context("parsing credentials file")?;
        Ok(Some(Auth::Token {
            token: file.token,
            salt: file.salt,
        }))
    }

    /// Deletes the keychain entry and the token file. Succeeds when nothing is stored.
    pub fn clear(&self, server_url: &str, username: &str) -> anyhow::Result<()> {
        match self.secrets.delete(&account(server_url, username)) {
            Ok(()) | Err(SecretError::Unavailable(_)) => {}
            Err(e) => return Err(e.into()),
        }
        self.remove_file()
    }

    fn remove_file(&self) -> anyhow::Result<()> {
        match std::fs::remove_file(&self.file_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).context("removing credentials file"),
        }
    }

    fn write_file(&self, password: &str) -> anyhow::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let (token, salt) = Auth::Password(password.to_owned()).params();
        let body = toml::to_string(&TokenFile { salt, token })?;
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent).context("creating config directory")?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&self.file_path)
            .context("opening credentials file")?;
        // `mode` only applies on creation, so tighten a pre-existing file too.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(body.as_bytes())
            .context("writing credentials file")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic::auth::md5_hex;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;

    struct FakeSecrets {
        map: RefCell<HashMap<String, String>>,
        available: bool,
    }

    impl FakeSecrets {
        fn new(available: bool) -> Self {
            Self {
                map: RefCell::new(HashMap::new()),
                available,
            }
        }
        fn check(&self) -> Result<(), SecretError> {
            if self.available {
                Ok(())
            } else {
                Err(SecretError::Unavailable("no store".into()))
            }
        }
    }

    impl SecretBackend for FakeSecrets {
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            self.check()?;
            Ok(self.map.borrow().get(account).cloned())
        }
        fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
            self.check()?;
            self.map.borrow_mut().insert(account.into(), secret.into());
            Ok(())
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.check()?;
            self.map.borrow_mut().remove(account);
            Ok(())
        }
    }

    const URL: &str = "https://music.example.com";

    fn store(available: bool) -> (tempfile::TempDir, CredentialStore<FakeSecrets>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        (dir, CredentialStore::new(FakeSecrets::new(available), path))
    }

    #[test]
    fn saves_to_keychain_when_available() {
        let (_dir, s) = store(true);
        assert_eq!(s.save(URL, "me", "pw").unwrap(), SavedTo::Keychain);
        assert!(!s.file_path.exists());
        match s.load(URL, "me").unwrap() {
            Some(Auth::Password(p)) => assert_eq!(p, "pw"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn falls_back_to_0600_file_without_password() {
        let (_dir, s) = store(false);
        assert_eq!(s.save(URL, "me", "pw").unwrap(), SavedTo::File);
        let mode = std::fs::metadata(&s.file_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = std::fs::read_to_string(&s.file_path).unwrap();
        assert!(text.contains("salt") && text.contains("token"));
        assert!(!text.contains("pw"));
        match s.load(URL, "me").unwrap() {
            Some(Auth::Token { token, salt }) => assert_eq!(token, md5_hex(&format!("pw{salt}"))),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn clear_removes_both() {
        let (_dir, s) = store(true);
        s.save(URL, "me", "pw").unwrap();
        std::fs::write(&s.file_path, "salt = \"a\"\ntoken = \"b\"\n").unwrap();
        s.clear(URL, "me").unwrap();
        assert!(!s.file_path.exists());
        assert!(s.load(URL, "me").unwrap().is_none());
        s.clear(URL, "me").unwrap();
    }

    #[test]
    fn load_with_nothing_stored_is_none() {
        let (_dir, s) = store(true);
        assert!(s.load(URL, "me").unwrap().is_none());
        let (_dir, s) = store(false);
        assert!(s.load(URL, "me").unwrap().is_none());
    }
}
