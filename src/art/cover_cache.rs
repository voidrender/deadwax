use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;

use crate::subsonic::SubsonicClient;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// On-disk cache of server cover art, keyed by cover id and size.
pub struct CoverCache {
    dir: PathBuf,
    client: SubsonicClient,
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl CoverCache {
    pub fn new(dir: PathBuf, client: SubsonicClient) -> Self {
        Self { dir, client }
    }

    fn path_for(&self, cover_id: &str, size: u32) -> PathBuf {
        self.dir.join(format!("{}-{size}.img", sanitize(cover_id)))
    }

    /// Returns the cached file, fetching and storing it first if missing.
    ///
    /// Concurrent calls for the same id may both fetch. The write goes to a
    /// unique temp file and is renamed, so the final file is always whole.
    pub async fn get(&self, cover_id: &str, size: u32) -> anyhow::Result<PathBuf> {
        let path = self.path_for(cover_id, size);
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            return Ok(path);
        }
        let bytes = self.client.cover_art(cover_id, size).await?;
        tokio::fs::create_dir_all(&self.dir)
            .await
            .context("create cover cache dir")?;
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("img.{}.{n}.tmp", std::process::id()));
        tokio::fs::write(&tmp, &bytes)
            .await
            .context("write cover temp file")?;
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e).context("rename cover file");
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subsonic::Auth;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(uri: &str) -> SubsonicClient {
        SubsonicClient::new(uri, "alice", Auth::Password("p".into())).unwrap()
    }

    #[tokio::test]
    async fn cover_cache_fetches_once() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/getCoverArt"))
            .and(query_param("id", "al-1"))
            .and(query_param("size", "300"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/jpeg")
                    .set_body_bytes(vec![1u8, 2, 3]),
            )
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = CoverCache::new(dir.path().join("art"), client(&server.uri()));
        let a = cache.get("al-1", 300).await.unwrap();
        let b = cache.get("al-1", 300).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), vec![1, 2, 3]);
        assert_eq!(a.file_name().unwrap(), "al-1-300.img");
    }

    #[test]
    fn cover_cache_sanitizes_ids() {
        let cache = CoverCache::new(PathBuf::from("/cache"), client("http://127.0.0.1:1"));
        let p = cache.path_for("al-1/../x", 64);
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(!name.contains('/'));
        assert!(!name.contains(".."));
        assert_eq!(p.parent().unwrap(), PathBuf::from("/cache"));
        assert_eq!(name, "al-1____x-64.img");
    }
}
