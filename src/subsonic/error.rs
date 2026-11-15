use std::fmt;

/// Errors from talking to a Subsonic server. Messages never contain URLs or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsonicError {
    Network(String),
    Api { code: i32, message: String },
    Parse(String),
}

impl fmt::Display for SubsonicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(m) => write!(f, "network error: {m}"),
            Self::Api { code, message } => write!(f, "server error {code}: {message}"),
            Self::Parse(m) => write!(f, "unexpected response: {m}"),
        }
    }
}

impl std::error::Error for SubsonicError {}

impl From<reqwest::Error> for SubsonicError {
    fn from(e: reqwest::Error) -> Self {
        let e = e.without_url();
        if e.is_decode() {
            Self::Parse(e.to_string())
        } else {
            Self::Network(e.to_string())
        }
    }
}
