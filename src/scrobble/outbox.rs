use std::collections::VecDeque;
use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::subsonic::SubsonicError;

/// A scrobble waiting to reach the server. `time_ms` is when playback started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingScrobble {
    pub song_id: String,
    pub time_ms: i64,
}

/// Persistent FIFO of scrobbles that have not been sent yet.
#[derive(Debug)]
pub struct Outbox {
    path: PathBuf,
    items: VecDeque<PendingScrobble>,
}

impl Outbox {
    /// Opens the outbox file. A missing file is an empty outbox. A corrupt file
    /// is renamed to `<name>.corrupt` and the outbox starts empty.
    pub fn open(path: PathBuf) -> anyhow::Result<Outbox> {
        let items = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<VecDeque<PendingScrobble>>(&bytes) {
                Ok(items) => items,
                Err(e) => {
                    tracing::warn!("scrobble outbox is corrupt, starting empty: {e}");
                    let mut aside = path.clone().into_os_string();
                    aside.push(".corrupt");
                    if let Err(e) = std::fs::rename(&path, &aside) {
                        tracing::warn!("could not move corrupt outbox aside: {e}");
                    }
                    VecDeque::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => VecDeque::new(),
            Err(e) => return Err(e).with_context(|| "reading scrobble outbox"),
        };
        Ok(Outbox { path, items })
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Appends a scrobble and persists the outbox.
    pub fn push(&mut self, p: PendingScrobble) -> anyhow::Result<()> {
        self.items.push_back(p);
        self.save()
    }

    /// Sends pending scrobbles oldest first and persists what remains.
    ///
    /// A `Network` error stops the flush and keeps that scrobble and the rest.
    /// `Api` and `Parse` errors drop the scrobble, since retrying cannot help.
    /// Returns the number sent successfully.
    pub async fn flush<F, Fut>(&mut self, mut send: F) -> anyhow::Result<usize>
    where
        F: FnMut(PendingScrobble) -> Fut,
        Fut: Future<Output = Result<(), SubsonicError>>,
    {
        let mut sent = 0;
        while let Some(front) = self.items.front().cloned() {
            match send(front).await {
                Ok(()) => sent += 1,
                Err(SubsonicError::Network(_)) => break,
                Err(e) => tracing::warn!("dropping scrobble rejected by server: {e}"),
            }
            self.items.pop_front();
        }
        self.save()?;
        Ok(sent)
    }

    fn save(&self) -> anyhow::Result<()> {
        write_atomic(&self.path, &serde_json::to_vec(&self.items)?)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| "creating outbox directory")?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes).with_context(|| "writing scrobble outbox")?;
    std::fs::rename(&tmp, path).with_context(|| "replacing scrobble outbox")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn p(id: &str, t: i64) -> PendingScrobble {
        PendingScrobble {
            song_id: id.into(),
            time_ms: t,
        }
    }

    fn outbox_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("sub").join("outbox.json")
    }

    #[tokio::test]
    async fn network_failure_keeps_scrobble() {
        let dir = tempfile::tempdir().unwrap();
        let path = outbox_path(&dir);
        let mut ob = Outbox::open(path.clone()).unwrap();
        ob.push(p("a", 1)).unwrap();
        ob.push(p("b", 2)).unwrap();
        let n = ob
            .flush(|_| async { Err(SubsonicError::Network("down".into())) })
            .await
            .unwrap();
        assert_eq!(n, 0);
        let re = Outbox::open(path).unwrap();
        assert_eq!(re.items, VecDeque::from(vec![p("a", 1), p("b", 2)]));
    }

    #[tokio::test]
    async fn api_error_drops_scrobble() {
        let dir = tempfile::tempdir().unwrap();
        let path = outbox_path(&dir);
        let mut ob = Outbox::open(path.clone()).unwrap();
        ob.push(p("a", 1)).unwrap();
        ob.push(p("b", 2)).unwrap();
        let n = ob
            .flush(|s| async move {
                if s.song_id == "a" {
                    Err(SubsonicError::Api {
                        code: 70,
                        message: "gone".into(),
                    })
                } else {
                    Ok(())
                }
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
        assert!(ob.is_empty());
        assert!(Outbox::open(path).unwrap().is_empty());
    }

    #[tokio::test]
    async fn parse_error_drops_scrobble() {
        let dir = tempfile::tempdir().unwrap();
        let mut ob = Outbox::open(outbox_path(&dir)).unwrap();
        ob.push(p("a", 1)).unwrap();
        let n = ob
            .flush(|_| async { Err(SubsonicError::Parse("bad".into())) })
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert!(ob.is_empty());
    }

    #[tokio::test]
    async fn flush_sends_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut ob = Outbox::open(outbox_path(&dir)).unwrap();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            ob.push(p(id, i as i64)).unwrap();
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let n = ob
            .flush(move |s| {
                let s2 = s2.clone();
                async move {
                    s2.lock().unwrap().push(s.song_id);
                    Ok(())
                }
            })
            .await
            .unwrap();
        assert_eq!(n, 3);
        assert_eq!(*seen.lock().unwrap(), vec!["a", "b", "c"]);
        assert!(ob.is_empty());
    }

    #[tokio::test]
    async fn network_error_midway_keeps_remainder() {
        let dir = tempfile::tempdir().unwrap();
        let path = outbox_path(&dir);
        let mut ob = Outbox::open(path.clone()).unwrap();
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            ob.push(p(id, i as i64)).unwrap();
        }
        let n = ob
            .flush(|s| async move {
                if s.song_id == "b" {
                    Err(SubsonicError::Network("down".into()))
                } else {
                    Ok(())
                }
            })
            .await
            .unwrap();
        assert_eq!(n, 1);
        let re = Outbox::open(path).unwrap();
        assert_eq!(re.items, VecDeque::from(vec![p("b", 1), p("c", 2)]));
    }

    #[test]
    fn missing_file_opens_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Outbox::open(outbox_path(&dir)).unwrap().is_empty());
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.json");
        std::fs::write(&path, b"{not json").unwrap();
        let ob = Outbox::open(path.clone()).unwrap();
        assert!(ob.is_empty());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(dir.path().join("outbox.json.corrupt")).unwrap(),
            b"{not json"
        );
    }
}
