use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Mutex;

/// What to look up. Only public metadata, never anything from the private server.
pub struct ArtQuery {
    pub artist: String,
    pub album: String,
    pub release_mbid: Option<String>,
}

/// Base URLs of the public art services (overridable for tests).
pub struct Endpoints {
    pub caa: String,
    pub itunes: String,
    pub deezer: String,
}

impl Endpoints {
    pub fn public() -> Self {
        Self {
            caa: "https://coverartarchive.org".into(),
            itunes: "https://itunes.apple.com".into(),
            deezer: "https://api.deezer.com".into(),
        }
    }
}

/// Outcome of one lookup step.
enum Step {
    Hit(String),
    Miss,
    /// Transient failure (network error, timeout, 5xx). Never cached.
    Failed,
}

/// Finds a public cover image URL for Discord. It owns its own HTTP client and
/// is only ever given artist/album names or `MusicBrainz` ids.
pub struct PublicArtResolver {
    http: reqwest::Client,
    endpoints: Endpoints,
    cache_path: PathBuf,
    cache: Mutex<HashMap<String, Option<String>>>,
}

impl PublicArtResolver {
    pub fn new(endpoints: Endpoints, cache_path: PathBuf) -> Self {
        Self::with_timeout(endpoints, cache_path, Duration::from_secs(5))
    }

    pub(crate) fn with_timeout(
        endpoints: Endpoints,
        cache_path: PathBuf,
        timeout: Duration,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent(concat!("deadwax/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("static reqwest client config is valid");
        // A missing or corrupt cache file starts empty.
        let cache = std::fs::read(&cache_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Self {
            http,
            endpoints,
            cache_path,
            cache: Mutex::new(cache),
        }
    }

    /// Returns a public image URL, or `None` when no source has one.
    ///
    /// Definitive results (including misses) are cached. Lookups that hit a
    /// network error are not, so they are retried next time.
    pub async fn resolve(&self, q: &ArtQuery) -> Option<String> {
        let key = match &q.release_mbid {
            Some(m) => m.clone(),
            None => format!("{}\u{1f}{}", normalize(&q.artist), normalize(&q.album)),
        };
        if let Some(hit) = self.cache.lock().await.get(&key) {
            return hit.clone();
        }

        let mut failed = false;
        let mut found = None;
        if let Some(mbid) = &q.release_mbid {
            match self.caa(mbid).await {
                Step::Hit(u) => found = Some(u),
                Step::Failed => failed = true,
                Step::Miss => {}
            }
        }
        if found.is_none() {
            match self.itunes(q).await {
                Step::Hit(u) => found = Some(u),
                Step::Failed => failed = true,
                Step::Miss => {}
            }
        }
        if found.is_none() {
            match self.deezer(q).await {
                Step::Hit(u) => found = Some(u),
                Step::Failed => failed = true,
                Step::Miss => {}
            }
        }

        if found.is_some() || !failed {
            let mut cache = self.cache.lock().await;
            cache.insert(key, found.clone());
            if let Err(e) = self.persist(&cache).await {
                tracing::warn!("could not save public art cache: {e}");
            }
        }
        found
    }

    async fn persist(&self, cache: &HashMap<String, Option<String>>) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(cache)?;
        if let Some(dir) = self.cache_path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let tmp = self.cache_path.with_extension("json.tmp");
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, &self.cache_path).await?;
        Ok(())
    }

    async fn caa(&self, mbid: &str) -> Step {
        let url = format!("{}/release/{mbid}/front-500", self.endpoints.caa);
        // The HEAD follows redirects, so success means the image exists. The
        // canonical URL is kept rather than the archive.org node it lands on.
        match self.http.head(&url).send().await {
            Ok(r) if r.status().is_success() => Step::Hit(url),
            Ok(r) if is_transient(r.status()) => Step::Failed,
            Ok(_) => Step::Miss,
            Err(_) => Step::Failed,
        }
    }

    async fn get_json(&self, url: String, params: &[(&str, String)]) -> Result<Value, Step> {
        let resp = self
            .http
            .get(url)
            .query(params)
            .send()
            .await
            .map_err(|_| Step::Failed)?;
        if is_transient(resp.status()) {
            return Err(Step::Failed);
        }
        if !resp.status().is_success() {
            return Err(Step::Miss);
        }
        // A failed body read is transient; a body that is not JSON is a definitive miss.
        let bytes = resp.bytes().await.map_err(|_| Step::Failed)?;
        serde_json::from_slice(&bytes).map_err(|_| Step::Miss)
    }

    async fn itunes(&self, q: &ArtQuery) -> Step {
        let params = [
            ("term", format!("{} {}", q.artist, q.album)),
            ("entity", "album".to_string()),
            ("limit", "10".to_string()),
        ];
        let body = match self
            .get_json(format!("{}/search", self.endpoints.itunes), &params)
            .await
        {
            Ok(b) => b,
            Err(s) => return s,
        };
        let found = first_match(&body["results"], q, "artistName", "collectionName")
            .and_then(|r| r["artworkUrl100"].as_str())
            .map(|u| u.replace("100x100bb", "600x600bb"));
        found.map_or(Step::Miss, Step::Hit)
    }

    async fn deezer(&self, q: &ArtQuery) -> Step {
        let params = [(
            "q",
            format!("artist:\"{}\" album:\"{}\"", q.artist, q.album),
        )];
        let body = match self
            .get_json(format!("{}/search/album", self.endpoints.deezer), &params)
            .await
        {
            Ok(b) => b,
            Err(s) => return s,
        };
        let found = body["data"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| {
                r["artist"]["name"]
                    .as_str()
                    .is_some_and(|a| normalize(a) == normalize(&q.artist))
                    && r["title"]
                        .as_str()
                        .is_some_and(|t| normalize(t) == normalize(&q.album))
            })
            .and_then(|r| r["cover_xl"].as_str())
            .map(str::to_string);
        found.map_or(Step::Miss, Step::Hit)
    }
}

/// Statuses that mean "try again later" rather than "no such album".
/// iTunes answers 403 when it throttles.
fn is_transient(status: reqwest::StatusCode) -> bool {
    status.is_server_error() || matches!(status.as_u16(), 403 | 408 | 429)
}

fn first_match<'a>(
    results: &'a Value,
    q: &ArtQuery,
    artist_key: &str,
    album_key: &str,
) -> Option<&'a Value> {
    results.as_array()?.iter().find(|r| {
        r[artist_key]
            .as_str()
            .is_some_and(|a| normalize(a) == normalize(&q.artist))
            && r[album_key]
                .as_str()
                .is_some_and(|t| normalize(t) == normalize(&q.album))
    })
}

/// Canonical form for comparing artist and album names across services.
///
/// Lowercases, drops one trailing `(...)` or `[...]` group (such as
/// "(Remastered)"), turns every non-alphanumeric character into a space, and
/// collapses whitespace. A name that is only a bracket group is left intact.
pub fn normalize(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    let stripped = strip_trailing_group(&lower).unwrap_or(&lower);
    stripped
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_trailing_group(s: &str) -> Option<&str> {
    let open = match s.chars().last()? {
        ')' => '(',
        ']' => '[',
        _ => return None,
    };
    let start = s.rfind(open)?;
    let rest = s[..start].trim_end();
    (!rest.is_empty()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn eps(s: &MockServer) -> Endpoints {
        Endpoints {
            caa: s.uri(),
            itunes: s.uri(),
            deezer: s.uri(),
        }
    }

    fn q(artist: &str, album: &str, mbid: Option<&str>) -> ArtQuery {
        ArtQuery {
            artist: artist.into(),
            album: album.into(),
            release_mbid: mbid.map(Into::into),
        }
    }

    fn itunes_body(artist: &str, album: &str) -> serde_json::Value {
        serde_json::json!({"resultCount": 1, "results": [{
            "artistName": artist, "collectionName": album,
            "artworkUrl100": "https://img.example/a/100x100bb.jpg"
        }]})
    }

    async fn requests(s: &MockServer) -> usize {
        s.received_requests().await.unwrap().len()
    }

    #[tokio::test]
    async fn mbid_hit_uses_cover_art_archive() {
        let s = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/release/mb-1/front-500"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/img/final.jpg"))
            .mount(&s)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/img/final.jpg"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&s)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let r = PublicArtResolver::new(eps(&s), dir.path().join("pub.json"));
        let url = r
            .resolve(&q("Boards of Canada", "MHTRTC", Some("mb-1")))
            .await
            .unwrap();
        // The canonical URL is cached, not the redirect target, which is a
        // mirror node that can change.
        assert_eq!(url, format!("{}/release/mb-1/front-500", s.uri()));
    }

    #[tokio::test]
    async fn mbid_redirect_to_missing_image_is_a_miss() {
        let s = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/release/mb-1/front-500"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", "/img/gone.jpg"))
            .mount(&s)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/img/gone.jpg"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "resultCount": 0, "results": []
            })))
            .mount(&s)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let r = PublicArtResolver::new(eps(&s), dir.path().join("pub.json"));
        assert_eq!(r.resolve(&q("A", "B", Some("mb-1"))).await, None);
    }

    #[tokio::test]
    async fn mbid_miss_falls_back_to_itunes() {
        let s = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/release/mb-1/front-500"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(itunes_body(
                "Boards of Canada",
                "Music Has the Right to Children (Remastered)",
            )))
            .mount(&s)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let r = PublicArtResolver::new(eps(&s), dir.path().join("pub.json"));
        let url = r
            .resolve(&q(
                "Boards of Canada",
                "Music Has the Right to Children",
                Some("mb-1"),
            ))
            .await
            .unwrap();
        assert!(url.contains("600x600bb"), "{url}");
    }

    #[tokio::test]
    async fn itunes_wrong_artist_falls_to_deezer() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(itunes_body("Someone Else", "Album")),
            )
            .mount(&s)
            .await;
        Mock::given(method("GET")).and(path("/search/album"))
            .and(query_param("q", "artist:\"Artist\" album:\"Album\""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": [
                {"title": "Album", "cover_xl": "https://dz.example/xl.jpg", "artist": {"name": "Artist"}}
            ]})))
            .mount(&s).await;
        let dir = tempfile::tempdir().unwrap();
        let r = PublicArtResolver::new(eps(&s), dir.path().join("pub.json"));
        assert_eq!(
            r.resolve(&q("Artist", "Album", None)).await.as_deref(),
            Some("https://dz.example/xl.jpg")
        );
    }

    #[tokio::test]
    async fn special_characters_are_encoded_in_queries() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("term", "Simon & Garfunkel 夜に駆ける"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(itunes_body("Simon & Garfunkel", "夜に駆ける")),
            )
            .expect(1)
            .mount(&s)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let r = PublicArtResolver::new(eps(&s), dir.path().join("pub.json"));
        let url = r
            .resolve(&q("Simon & Garfunkel", "夜に駆ける", None))
            .await
            .unwrap();
        assert!(url.contains("600x600bb"));
    }

    #[tokio::test]
    async fn total_miss_is_cached() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"results": []})),
            )
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/album"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&s)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("pub.json");
        let r = PublicArtResolver::new(eps(&s), cache.clone());
        assert_eq!(r.resolve(&q("A", "B", None)).await, None);
        let after_first = requests(&s).await;
        assert!(after_first > 0);
        assert_eq!(r.resolve(&q("A", "B", None)).await, None);
        assert_eq!(requests(&s).await, after_first);
        // Persisted: a fresh resolver also makes no requests.
        let r2 = PublicArtResolver::new(eps(&s), cache);
        assert_eq!(r2.resolve(&q("A", "B", None)).await, None);
        assert_eq!(requests(&s).await, after_first);
    }

    #[tokio::test]
    async fn network_error_is_not_cached() {
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", l.local_addr().unwrap())
        };
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("pub.json");
        let bad = Endpoints {
            caa: dead.clone(),
            itunes: dead.clone(),
            deezer: dead,
        };
        let r = PublicArtResolver::new(bad, cache.clone());
        assert_eq!(r.resolve(&q("A", "B", Some("mb-9"))).await, None);

        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(itunes_body("A", "B")))
            .mount(&s)
            .await;
        let r2 = PublicArtResolver::new(eps(&s), cache);
        assert!(r2.resolve(&q("A", "B", Some("mb-9"))).await.is_some());
    }

    #[tokio::test]
    async fn throttled_itunes_is_not_cached() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/album"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .mount(&s)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let r = PublicArtResolver::new(eps(&s), dir.path().join("pub.json"));
        assert_eq!(r.resolve(&q("A", "B", None)).await, None);
        let first = requests(&s).await;
        assert_eq!(r.resolve(&q("A", "B", None)).await, None);
        assert!(requests(&s).await > first, "second resolve must retry");
    }

    #[tokio::test]
    async fn caa_404_falls_through_and_is_cached() {
        let s = MockServer::start().await;
        Mock::given(method("HEAD"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"results": []})),
            )
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/album"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&s)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let r = PublicArtResolver::new(eps(&s), dir.path().join("pub.json"));
        assert_eq!(r.resolve(&q("A", "B", Some("mb-2"))).await, None);
        let first = requests(&s).await;
        assert_eq!(r.resolve(&q("A", "B", Some("mb-2"))).await, None);
        assert_eq!(requests(&s).await, first);
    }

    #[tokio::test]
    async fn slow_response_is_not_cached() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(itunes_body("A", "B"))
                    .set_delay(Duration::from_millis(500)),
            )
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/album"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&s)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let r = PublicArtResolver::with_timeout(
            eps(&s),
            dir.path().join("pub.json"),
            Duration::from_millis(100),
        );
        assert_eq!(r.resolve(&q("A", "B", None)).await, None);
        let first = requests(&s).await;
        assert_eq!(r.resolve(&q("A", "B", None)).await, None);
        assert!(requests(&s).await > first, "timeout must not be cached");
    }

    #[test]
    fn normalize_strips_edition_suffixes() {
        assert_eq!(
            normalize("Music Has the Right to Children (Remastered)"),
            normalize("music has the right to children")
        );
        assert_eq!(normalize("Album [Deluxe Edition]"), "album");
        assert_eq!(
            normalize("  AC/DC:  Back--in  Black "),
            "ac dc back in black"
        );
    }
}
