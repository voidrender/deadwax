use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use super::{Auth, SubsonicError};

const API_VERSION: &str = "1.16.1";
const CLIENT_NAME: &str = "senritsu";

/// Async Subsonic API client. Cheap to clone.
#[derive(Clone)]
pub struct SubsonicClient {
    http: reqwest::Client,
    base: String,
    username: String,
    auth: Auth,
}

impl SubsonicClient {
    pub fn new(server_url: &str, username: &str, auth: Auth) -> Result<Self, SubsonicError> {
        Self::with_timeout(server_url, username, auth, Duration::from_secs(10))
    }

    pub(crate) fn with_timeout(
        server_url: &str,
        username: &str,
        auth: Auth,
        timeout: Duration,
    ) -> Result<Self, SubsonicError> {
        let parsed = Url::parse(server_url)
            .map_err(|_| SubsonicError::Parse("invalid server URL".into()))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(SubsonicError::Parse("invalid server URL".into()));
        }
        let http = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self {
            http,
            base: server_url.trim_end_matches('/').to_string(),
            username: username.to_string(),
            auth,
        })
    }

    pub fn server_url(&self) -> &str {
        &self.base
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    fn query(&self, extra: &[(&str, String)]) -> Vec<(String, String)> {
        let (t, s) = self.auth.params();
        let mut q = vec![
            ("u".to_string(), self.username.clone()),
            ("t".to_string(), t),
            ("s".to_string(), s),
            ("v".to_string(), API_VERSION.to_string()),
            ("c".to_string(), CLIENT_NAME.to_string()),
            ("f".to_string(), "json".to_string()),
        ];
        q.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
        q
    }

    /// Builds a stream URL for a song. It carries auth, so never log or display it.
    pub fn stream_url(&self, song_id: &str) -> String {
        let q = self.query(&[
            ("id", song_id.to_string()),
            ("estimateContentLength", "true".to_string()),
        ]);
        let mut url = Url::parse(&format!("{}/rest/stream", self.base))
            .expect("base URL validated in constructor");
        url.query_pairs_mut().extend_pairs(q);
        url.into()
    }

    pub async fn ping(&self) -> Result<(), SubsonicError> {
        self.call_unit("ping", &[]).await
    }

    async fn envelope(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<Value, SubsonicError> {
        let resp = self
            .http
            .get(format!("{}/rest/{endpoint}", self.base))
            .query(&self.query(params))
            .send()
            .await?
            .error_for_status()?;
        let mut body: Value = resp.json().await?;
        let mut inner = body
            .get_mut("subsonic-response")
            .map(Value::take)
            .ok_or_else(|| SubsonicError::Parse("missing subsonic-response".into()))?;
        if inner.get("status").and_then(Value::as_str) == Some("failed") {
            let err = inner.get("error");
            return Err(SubsonicError::Api {
                code: err
                    .and_then(|e| e.get("code"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0) as i32,
                message: err
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            });
        }
        Ok(inner.take())
    }

    #[allow(dead_code)] // first consumer arrives in the library task
    /// GET `/rest/<endpoint>` and deserialize the field `key` of the response envelope.
    pub(crate) async fn call<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
        key: &str,
    ) -> Result<T, SubsonicError> {
        let mut inner = self.envelope(endpoint, params).await?;
        let field = inner
            .get_mut(key)
            .map(Value::take)
            .ok_or_else(|| SubsonicError::Parse(format!("missing field {key}")))?;
        serde_json::from_value(field).map_err(|e| SubsonicError::Parse(e.to_string()))
    }

    /// Like `call` for endpoints with no payload.
    pub(crate) async fn call_unit(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<(), SubsonicError> {
        self.envelope(endpoint, params).await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;
    use wiremock::matchers::{path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::auth::md5_hex;
    use super::*;

    fn ok() -> ResponseTemplate {
        ResponseTemplate::new(200)
            .set_body_json(json!({"subsonic-response":{"status":"ok","version":"1.16.1"}}))
    }

    #[tokio::test]
    async fn ping_sends_token_auth() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/ping"))
            .and(query_param("u", "alice"))
            .and(query_param("v", "1.16.1"))
            .and(query_param("c", "senritsu"))
            .and(query_param("f", "json"))
            .respond_with(ok())
            .mount(&server)
            .await;
        let c =
            SubsonicClient::new(&server.uri(), "alice", Auth::Password("sesame".into())).unwrap();
        c.ping().await.unwrap();
        let req = &server.received_requests().await.unwrap()[0];
        let q: HashMap<_, _> = req.url.query_pairs().into_owned().collect();
        assert_eq!(q["t"], md5_hex(&format!("sesame{}", q["s"])));
        assert!(!q.contains_key("p"));
    }

    #[tokio::test]
    async fn failed_status_maps_to_api_error() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/ping"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"subsonic-response":{
                "status":"failed","error":{"code":40,"message":"Wrong username or password"}}})),
            )
            .mount(&server)
            .await;
        let c = SubsonicClient::new(&server.uri(), "a", Auth::Password("p".into())).unwrap();
        let err = c.ping().await.unwrap_err();
        assert!(
            matches!(err, SubsonicError::Api { code: 40, .. }),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn token_auth_is_sent_verbatim() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/ping"))
            .and(query_param("t", "abc"))
            .and(query_param("s", "xyz"))
            .respond_with(ok())
            .mount(&server)
            .await;
        let auth = Auth::Token {
            token: "abc".into(),
            salt: "xyz".into(),
        };
        SubsonicClient::new(&server.uri(), "a", auth)
            .unwrap()
            .ping()
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn call_deserializes_key_and_keeps_path_prefix() {
        let server = MockServer::start().await;
        Mock::given(path("/music/rest/getThing"))
            .and(query_param("id", "7"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"subsonic-response":{
                "status":"ok","thing":{"n":3}}})),
            )
            .mount(&server)
            .await;
        let url = format!("{}/music/", server.uri());
        let c = SubsonicClient::new(&url, "a", Auth::Password("p".into())).unwrap();
        let v: Value = c
            .call("getThing", &[("id", "7".to_string())], "thing")
            .await
            .unwrap();
        assert_eq!(v["n"], 3);
        assert_eq!(c.server_url(), format!("{}/music", server.uri()));
    }

    #[tokio::test]
    async fn request_timeout_is_reported_and_redacted() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/ping"))
            .respond_with(ok().set_delay(Duration::from_secs(11)))
            .mount(&server)
            .await;
        let c = SubsonicClient::with_timeout(
            &server.uri(),
            "a",
            Auth::Password("p".into()),
            Duration::from_millis(200),
        )
        .unwrap();
        let err = c.ping().await.unwrap_err();
        assert!(matches!(err, SubsonicError::Network(_)), "{err:?}");
        let s = err.to_string();
        assert!(!s.contains("t=") && !s.contains("s=") && !s.contains(&server.uri()));
    }

    #[test]
    fn stream_url_has_id_and_auth() {
        let c =
            SubsonicClient::new("http://h:4533/music", "a", Auth::Password("p".into())).unwrap();
        let u = c.stream_url("so-1");
        for needle in [
            "/music/rest/stream?",
            "id=so-1",
            "t=",
            "s=",
            "estimateContentLength=true",
        ] {
            assert!(u.contains(needle), "{u} missing {needle}");
        }
    }
}
