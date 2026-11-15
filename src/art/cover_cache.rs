use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context;

use crate::subsonic::SubsonicClient;
use crate::subsonic::auth::md5_hex;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// On-disk cache of server cover art, keyed by server, cover id and size.
pub struct CoverCache {
    dir: PathBuf,
    client: SubsonicClient,
}

impl CoverCache {
    pub fn new(dir: PathBuf, client: SubsonicClient) -> Self {
        Self { dir, client }
    }

    /// Cover ids are only unique per server, so the name hashes both.
    fn path_for(&self, cover_id: &str, size: u32) -> PathBuf {
        let key = md5_hex(&format!("{}\u{1f}{cover_id}", self.client.server_url()));
        self.dir.join(format!("{key}-{size}.img"))
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
        assert!(a.to_str().unwrap().ends_with("-300.img"));
    }

    #[test]
    fn cover_paths_differ_by_server_and_stay_in_the_cache() {
        let one = CoverCache::new(PathBuf::from("/cache"), client("http://one.example"));
        let two = CoverCache::new(PathBuf::from("/cache"), client("http://two.example"));
        assert_ne!(one.path_for("al-12", 64), two.path_for("al-12", 64));
        assert_ne!(one.path_for("a/b", 64), one.path_for("a?b", 64));
        let p = one.path_for("al-1/../x", 64);
        let name = p.file_name().unwrap().to_str().unwrap();
        assert!(!name.contains('/') && !name.contains(".."));
        assert_eq!(p.parent().unwrap(), PathBuf::from("/cache"));
    }
}
