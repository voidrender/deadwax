use super::Effect;
use crate::subsonic::SubsonicError;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoginField {
    #[default]
    Server,
    Username,
    Password,
}

#[derive(Clone, Default, PartialEq)]
pub struct LoginState {
    pub server: String,
    pub username: String,
    pub password: String,
    pub focus: LoginField,
    pub error: Option<String>,
    pub busy: bool,
    pub warning: Option<String>,
}

impl std::fmt::Debug for LoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginState")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("focus", &self.focus)
            .field("error", &self.error)
            .field("busy", &self.busy)
            .finish_non_exhaustive()
    }
}

/// Trims the input, adds `https://` when it has no scheme, and strips
/// trailing slashes.
pub fn normalize_server_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

/// Whether the login would go out unencrypted beyond the local network:
/// an `http://` server that is not loopback, a private address, or a
/// local-only name. Plain HTTP is fine for a server on the LAN.
pub fn cleartext_warning(server: &str) -> bool {
    let Ok(url) = url::Url::parse(&normalize_server_url(server)) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => {
            let [a, b, ..] = ip.octets();
            // 100.64.0.0/10 is CGNAT space, which Tailscale uses.
            let cgnat = a == 100 && (64..128).contains(&b);
            !(ip.is_loopback() || ip.is_private() || ip.is_link_local() || cgnat)
        }
        Some(url::Host::Ipv6(ip)) => {
            !(ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local())
        }
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.');
            // A name without a dot, such as "nas", only resolves on the LAN.
            name.contains('.')
                && ![".local", ".lan", ".home.arpa", ".internal"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
        }
        None => false,
    }
}

pub fn login_error_message(e: &SubsonicError) -> String {
    match e {
        SubsonicError::Api { code: 40, .. } => "Wrong username or password".to_string(),
        SubsonicError::Network(_) => "Couldn't reach the server. Check the URL.".to_string(),
        SubsonicError::Api { message, .. } | SubsonicError::Parse(message) => {
            format!("Server error: {message}")
        }
    }
}

impl LoginField {
    fn next(self) -> Self {
        match self {
            Self::Server => Self::Username,
            Self::Username => Self::Password,
            Self::Password => Self::Server,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Server => Self::Password,
            Self::Username => Self::Server,
            Self::Password => Self::Username,
        }
    }
}

impl LoginState {
    fn field_mut(&mut self) -> &mut String {
        match self.focus {
            LoginField::Server => &mut self.server,
            LoginField::Username => &mut self.username,
            LoginField::Password => &mut self.password,
        }
    }

    /// On this screen only editing keys are live. The global keymap is off so
    /// every printable key reaches the focused field.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return if key.code == KeyCode::Char('c') {
                vec![Effect::Quit]
            } else {
                Vec::new()
            };
        }
        match key.code {
            KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::BackTab => self.focus = self.focus.prev(),
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => self.error = None,
            KeyCode::Backspace => {
                self.field_mut().pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.field_mut().push(c);
            }
            _ => {}
        }
        Vec::new()
    }

    fn submit(&mut self) -> Vec<Effect> {
        if self.busy {
            return Vec::new();
        }
        let server = normalize_server_url(&self.server);
        let username = self.username.trim().to_string();
        if server.is_empty() || username.is_empty() || self.password.is_empty() {
            self.error = Some("Enter a server, username, and password".to_string());
            return Vec::new();
        }
        self.busy = true;
        self.error = None;
        vec![Effect::Login {
            server,
            username,
            password: self.password.clone(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(s: &mut LoginState, code: KeyCode) -> Vec<Effect> {
        s.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_str(s: &mut LoginState, text: &str) {
        for c in text.chars() {
            press(s, KeyCode::Char(c));
        }
    }

    #[test]
    fn normalize_server_url_cases() {
        assert_eq!(
            normalize_server_url("music.example.com/"),
            "https://music.example.com"
        );
        assert_eq!(normalize_server_url("http://x:4533"), "http://x:4533");
        assert_eq!(normalize_server_url("  https://a.b//  "), "https://a.b");
        assert_eq!(normalize_server_url("   "), "");
    }

    #[test]
    fn cleartext_warning_skips_https_and_local_servers() {
        for server in [
            "https://music.example.com",
            "music.example.com",
            "http://localhost:4533",
            "http://nas:4533",
            "http://127.0.0.1",
            "http://127.4.5.6",
            "http://[::1]:4533",
            "http://10.0.0.2",
            "http://172.16.0.1",
            "http://172.31.255.255",
            "http://192.168.1.20:4533",
            "http://169.254.0.1",
            "http://100.64.0.1",
            "http://100.127.255.255",
            "http://[fd00::1]",
            "http://nas.local",
            "http://nas.lan/",
            "http://nas.home.arpa",
            "http://music.internal",
            "",
        ] {
            assert!(!cleartext_warning(server), "{server}");
        }
    }

    #[test]
    fn cleartext_warning_flags_public_http_servers() {
        for server in [
            "http://music.example.com",
            "http://8.8.8.8",
            "http://172.32.0.1",
            "http://100.128.0.1",
            "http://[2001:db8::1]",
            "http://notlocal.lanparty.com",
        ] {
            assert!(cleartext_warning(server), "{server}");
        }
    }

    #[test]
    fn tab_cycles_fields() {
        let mut s = LoginState::default();
        assert_eq!(s.focus, LoginField::Server);
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.focus, LoginField::Username);
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.focus, LoginField::Password);
        press(&mut s, KeyCode::Tab);
        assert_eq!(s.focus, LoginField::Server);
        press(&mut s, KeyCode::BackTab);
        assert_eq!(s.focus, LoginField::Password);
        press(&mut s, KeyCode::BackTab);
        assert_eq!(s.focus, LoginField::Username);
    }

    #[test]
    fn typing_and_backspace_edit_the_focused_field() {
        let mut s = LoginState::default();
        type_str(&mut s, "abc");
        press(&mut s, KeyCode::Backspace);
        assert_eq!(s.server, "ab");
        press(&mut s, KeyCode::Tab);
        type_str(&mut s, "q?X");
        assert_eq!(s.username, "q?X");
    }

    #[test]
    fn enter_emits_login_effect() {
        let mut s = LoginState::default();
        type_str(&mut s, "music.example.com/");
        press(&mut s, KeyCode::Tab);
        type_str(&mut s, "alice");
        press(&mut s, KeyCode::Tab);
        type_str(&mut s, "sesame");
        // Enter submits from any field.
        press(&mut s, KeyCode::Tab);
        let effects = press(&mut s, KeyCode::Enter);
        assert_eq!(
            effects,
            [Effect::Login {
                server: "https://music.example.com".into(),
                username: "alice".into(),
                password: "sesame".into(),
            }]
        );
        assert!(s.busy);
        assert!(s.error.is_none());
    }

    #[test]
    fn enter_while_busy_is_ignored() {
        let mut s = LoginState {
            server: "a.b".into(),
            username: "u".into(),
            password: "p".into(),
            busy: true,
            ..Default::default()
        };
        assert!(press(&mut s, KeyCode::Enter).is_empty());
    }

    #[test]
    fn enter_with_missing_fields_shows_error() {
        let mut s = LoginState::default();
        type_str(&mut s, "a.b");
        assert!(press(&mut s, KeyCode::Enter).is_empty());
        assert!(!s.busy);
        assert!(s.error.is_some());
        press(&mut s, KeyCode::Esc);
        assert!(s.error.is_none());
    }

    #[test]
    fn ctrl_c_quits_and_other_ctrl_keys_type_nothing() {
        let mut s = LoginState::default();
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        assert_eq!(s.handle_key(ctrl('c')), [Effect::Quit]);
        assert!(s.handle_key(ctrl('x')).is_empty());
        assert!(s.server.is_empty());
    }

    #[test]
    fn login_error_messages() {
        assert_eq!(
            login_error_message(&SubsonicError::Api {
                code: 40,
                message: "Wrong".into()
            }),
            "Wrong username or password"
        );
        assert_eq!(
            login_error_message(&SubsonicError::Network("boom".into())),
            "Couldn't reach the server. Check the URL."
        );
        assert_eq!(
            login_error_message(&SubsonicError::Api {
                code: 70,
                message: "Not found".into()
            }),
            "Server error: Not found"
        );
        assert_eq!(
            login_error_message(&SubsonicError::Parse("bad".into())),
            "Server error: bad"
        );
    }
}
