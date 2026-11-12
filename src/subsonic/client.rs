use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use url::Url;

use super::models::{ArtistIndexes, SongList};
use super::{AlbumWithSongs, Artist, ArtistWithAlbums, Auth, SearchResults, Song, SubsonicError};

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
        let body: Value = resp.json().await?;
        Self::unwrap_envelope(body)
    }

    fn unwrap_envelope(mut body: Value) -> Result<Value, SubsonicError> {
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
                    .and_then(|c| i32::try_from(c).ok())
                    .unwrap_or(0),
                message: err
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            });
        }
        Ok(inner.take())
    }

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

    pub async fn get_artists(&self) -> Result<Vec<Artist>, SubsonicError> {
        let idx: ArtistIndexes = self.call("getArtists", &[], "artists").await?;
        Ok(idx.index.into_iter().flat_map(|i| i.artist).collect())
    }

    pub async fn get_artist(&self, id: &str) -> Result<ArtistWithAlbums, SubsonicError> {
        self.call("getArtist", &[("id", id.to_string())], "artist")
            .await
    }

    pub async fn get_album(&self, id: &str) -> Result<AlbumWithSongs, SubsonicError> {
        self.call("getAlbum", &[("id", id.to_string())], "album")
            .await
    }

    pub async fn get_random_songs(&self, size: u32) -> Result<Vec<Song>, SubsonicError> {
        let list: SongList = self
            .call(
                "getRandomSongs",
                &[("size", size.to_string())],
                "randomSongs",
            )
            .await?;
        Ok(list.song)
    }

    pub async fn search(&self, query: &str, limit: u32) -> Result<SearchResults, SubsonicError> {
        let n = limit.to_string();
        self.call(
            "search3",
            &[
                ("query", query.to_string()),
                ("artistCount", n.clone()),
                ("albumCount", n.clone()),
                ("songCount", n),
            ],
            "searchResult3",
        )
        .await
    }

    pub async fn get_starred_songs(&self) -> Result<Vec<Song>, SubsonicError> {
        let list: SongList = self.call("getStarred2", &[], "starred2").await?;
        Ok(list.song)
    }

    pub async fn star(&self, song_id: &str) -> Result<(), SubsonicError> {
        self.call_unit("star", &[("id", song_id.to_string())]).await
    }

    pub async fn unstar(&self, song_id: &str) -> Result<(), SubsonicError> {
        self.call_unit("unstar", &[("id", song_id.to_string())])
            .await
    }

    pub async fn scrobble(
        &self,
        song_id: &str,
        time_ms: i64,
        submission: bool,
    ) -> Result<(), SubsonicError> {
        self.call_unit(
            "scrobble",
            &[
                ("id", song_id.to_string()),
                ("time", time_ms.to_string()),
                ("submission", submission.to_string()),
            ],
        )
        .await
    }

    /// Fetches raw image bytes. A JSON body means the server returned an error envelope.
    pub async fn cover_art(&self, cover_id: &str, size: u32) -> Result<Vec<u8>, SubsonicError> {
        let resp = self
            .http
            .get(format!("{}/rest/getCoverArt", self.base))
            .query(&self.query(&[("id", cover_id.to_string()), ("size", size.to_string())]))
            .send()
            .await?
            .error_for_status()?;
        let is_json = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("json"));
        if is_json {
            let body: Value = resp.json().await?;
            Self::unwrap_envelope(body)?;
            return Err(SubsonicError::Parse("expected image data".into()));
        }
        Ok(resp.bytes().await?.to_vec())
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

    fn fixture(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.to_string(), "application/json")
    }

    fn client_for(server: &MockServer) -> SubsonicClient {
        SubsonicClient::new(&server.uri(), "alice", Auth::Password("p".into())).unwrap()
    }

    #[tokio::test]
    async fn get_artists_flattens_indexes() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getArtists"))
            .respond_with(fixture(include_str!(
                "../../tests/fixtures/get_artists.json"
            )))
            .mount(&server)
            .await;
        let artists = client_for(&server).get_artists().await.unwrap();
        let names: Vec<_> = artists.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["Aimer", "Ado", "YOASOBI"]);
        assert_eq!(artists[0].album_count, 4);
        assert_eq!(artists[1].cover_art, None);
    }

    #[tokio::test]
    async fn get_artist_parses_albums() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getArtist"))
            .and(query_param("id", "ar-1"))
            .respond_with(fixture(include_str!(
                "../../tests/fixtures/get_artist.json"
            )))
            .mount(&server)
            .await;
        let a = client_for(&server).get_artist("ar-1").await.unwrap();
        assert_eq!(a.artist.name, "Aimer");
        assert_eq!(a.albums.len(), 2);
        assert_eq!(a.albums[1].year, Some(2020));
    }

    #[tokio::test]
    async fn get_album_parses_songs_and_starred() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getAlbum"))
            .and(query_param("id", "al-1"))
            .respond_with(fixture(include_str!("../../tests/fixtures/get_album.json")))
            .mount(&server)
            .await;
        let a = client_for(&server).get_album("al-1").await.unwrap();
        assert_eq!(a.songs.len(), 2);
        assert!(!a.songs[0].starred);
        assert!(a.songs[1].starred);
        assert_eq!(a.songs[0].bit_rate, Some(900));
        assert_eq!(a.songs[0].suffix.as_deref(), Some("flac"));
        let s = &a.songs[0];
        assert_eq!(s.disc_number, Some(1));
        assert_eq!(s.genre.as_deref(), Some("J-Pop"));
        assert_eq!(s.play_count, Some(12));
        assert_eq!(s.size, Some(31_457_280));
        assert_eq!(s.sampling_rate, Some(96_000));
        assert_eq!(s.bit_depth, Some(24));
        assert_eq!(a.songs[1].sampling_rate, None);
        assert_eq!(a.album.genre.as_deref(), Some("J-Pop"));
        assert_eq!(a.album.play_count, Some(7));
        assert_eq!(
            a.album.music_brainz_id.as_deref(),
            Some("1b2c3d4e-0000-4000-8000-123456789abc")
        );
    }

    #[tokio::test]
    async fn album_with_missing_fields_parses() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getAlbum"))
            .respond_with(fixture(include_str!(
                "../../tests/fixtures/get_album_sparse.json"
            )))
            .mount(&server)
            .await;
        let a = client_for(&server).get_album("al-9").await.unwrap();
        let s = &a.songs[0];
        assert_eq!(s.title, "旋律");
        assert_eq!(s.duration, 0);
        assert_eq!(s.cover_art, None);
        assert_eq!(s.year, None);
        assert_eq!(s.album, "");
        assert!(!s.starred);
        assert_eq!(a.album.artist, "");
    }

    #[tokio::test]
    async fn random_songs_sends_size() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getRandomSongs"))
            .and(query_param("size", "200"))
            .respond_with(fixture(include_str!(
                "../../tests/fixtures/get_random_songs.json"
            )))
            .mount(&server)
            .await;
        let songs = client_for(&server).get_random_songs(200).await.unwrap();
        assert_eq!(songs.len(), 2);
    }

    #[tokio::test]
    async fn search_returns_grouped_results() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/search3"))
            .and(query_param("query", "aimer"))
            .and(query_param("artistCount", "20"))
            .and(query_param("albumCount", "20"))
            .and(query_param("songCount", "20"))
            .respond_with(fixture(include_str!("../../tests/fixtures/search3.json")))
            .mount(&server)
            .await;
        let r = client_for(&server).search("aimer", 20).await.unwrap();
        assert_eq!((r.artists.len(), r.albums.len(), r.songs.len()), (1, 2, 3));
    }

    #[tokio::test]
    async fn starred_songs_come_from_getstarred2() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getStarred2"))
            .respond_with(fixture(include_str!(
                "../../tests/fixtures/get_starred2.json"
            )))
            .mount(&server)
            .await;
        let songs = client_for(&server).get_starred_songs().await.unwrap();
        assert_eq!(songs.len(), 2);
        assert!(songs.iter().all(|s| s.starred));
    }

    #[tokio::test]
    async fn star_and_unstar_send_id() {
        let server = MockServer::start().await;
        for ep in ["star", "unstar"] {
            Mock::given(path(format!("/rest/{ep}")))
                .and(query_param("id", "so-1"))
                .respond_with(ok())
                .expect(1)
                .mount(&server)
                .await;
        }
        let c = client_for(&server);
        c.star("so-1").await.unwrap();
        c.unstar("so-1").await.unwrap();
    }

    #[tokio::test]
    async fn scrobble_sends_submission_and_time() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/scrobble"))
            .and(query_param("id", "so-1"))
            .and(query_param("submission", "true"))
            .and(query_param("time", "1700000000000"))
            .respond_with(ok())
            .expect(1)
            .mount(&server)
            .await;
        client_for(&server)
            .scrobble("so-1", 1_700_000_000_000, true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cover_art_returns_bytes() {
        let server = MockServer::start().await;
        let bytes: Vec<u8> = vec![0xff, 0xd8, 0xff, 0xe0, 0, 1, 2, 3];
        Mock::given(path("/rest/getCoverArt"))
            .and(query_param("id", "al-1"))
            .and(query_param("size", "300"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(bytes.clone(), "image/jpeg"))
            .mount(&server)
            .await;
        let got = client_for(&server).cover_art("al-1", 300).await.unwrap();
        assert_eq!(got, bytes);
    }

    #[tokio::test]
    async fn cover_art_json_body_is_api_error() {
        let server = MockServer::start().await;
        Mock::given(path("/rest/getCoverArt"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"subsonic-response":{"status":"failed","error":{"code":70,"message":"not found"}}}"#,
                "application/json",
            ))
            .mount(&server)
            .await;
        let err = client_for(&server).cover_art("x", 300).await.unwrap_err();
        assert!(
            matches!(err, SubsonicError::Api { code: 70, .. }),
            "{err:?}"
        );
    }
}
