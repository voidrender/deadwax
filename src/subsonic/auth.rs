use md5::{Digest, Md5};
use rand::RngExt;
use rand::distr::Alphanumeric;

/// Credentials for Subsonic token auth.
#[derive(Clone)]
pub enum Auth {
    Password(String),
    Token { token: String, salt: String },
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Auth(..)")
    }
}

/// Lowercase hex MD5 digest.
pub(crate) fn md5_hex(input: &str) -> String {
    use std::fmt::Write;
    Md5::digest(input).iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

impl Auth {
    /// Returns `(t, s)`. A password yields a fresh random salt on every call.
    pub fn params(&self) -> (String, String) {
        match self {
            Self::Password(password) => {
                let salt: String = rand::rng()
                    .sample_iter(Alphanumeric)
                    .take(12)
                    .map(char::from)
                    .collect();
                let token = md5_hex(&format!("{password}{salt}"));
                (token, salt)
            }
            Self::Token { token, salt } => (token.clone(), salt.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_hex_matches_known_vector() {
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn password_salt_is_fresh_and_12_chars() {
        let a = Auth::Password("pw".into());
        let (t1, s1) = a.params();
        let (t2, s2) = a.params();
        assert_eq!(s1.len(), 12);
        assert_ne!(s1, s2);
        assert_ne!(t1, t2);
    }
}
