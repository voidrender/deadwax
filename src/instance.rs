use std::fs::{File, TryLockError};
use std::io;
use std::path::Path;

/// Takes the lock that keeps a second Deadwax from running. The lock lasts
/// as long as the returned file is open. `Ok(None)` means another process
/// holds it.
pub fn lock(path: &Path) -> io::Result<Option<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_fails_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/deadwax.lock");
        let first = lock(&path).unwrap();
        assert!(first.is_some());
        assert!(lock(&path).unwrap().is_none());
        drop(first);
        assert!(lock(&path).unwrap().is_some());
    }
}
