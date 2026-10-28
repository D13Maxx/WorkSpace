//! Content-addressed blob storage (master spec s10).
//!
//! Images and files live on disk at `blobs/{sha[0..2]}/{sha256}`, not in the
//! database. Content addressing makes writes idempotent: storing the same
//! image twice costs one copy, and a blob's name is a checksum of itself, so
//! corruption is detectable rather than silent.

use crate::error::{Result, StoreError};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

/// Hex-encoded SHA-256 of `bytes`.
#[must_use]
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

impl BlobStore {
    /// Blobs are stored under `root`, which is created if missing.
    ///
    /// # Errors
    /// Fails if the directory cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// Two-level fan-out keeps any single directory from holding every blob.
    fn path_for(&self, sha256: &str) -> PathBuf {
        let prefix = sha256.get(0..2).unwrap_or("__");
        self.root.join(prefix).join(sha256)
    }

    /// Store `bytes`, returning their hash. Writing the same content twice is
    /// a no-op the second time.
    ///
    /// # Errors
    /// Fails if the blob cannot be written.
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let sha = hash_bytes(bytes);
        let path = self.path_for(&sha);
        if path.exists() {
            return Ok(sha);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write to a temporary name and rename, so a crash mid-write cannot
        // leave a short file sitting under a hash that claims to describe it.
        let tmp = path.with_extension("partial");
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(sha)
    }

    /// Read a blob back, verifying it still hashes to its own name.
    ///
    /// # Errors
    /// Fails if the blob is missing, unreadable, or does not match its hash.
    pub fn get(&self, sha256: &str) -> Result<Vec<u8>> {
        let bytes = fs::read(self.path_for(sha256))?;
        let actual = hash_bytes(&bytes);
        if actual != sha256 {
            return Err(StoreError::BlobCorrupt {
                expected: sha256.to_owned(),
                actual,
            });
        }
        Ok(bytes)
    }

    #[must_use]
    pub fn exists(&self, sha256: &str) -> bool {
        self.path_for(sha256).exists()
    }

    /// Remove a blob. Used by garbage collection after a conversation is
    /// deleted; never call it for a blob something still references.
    ///
    /// # Errors
    /// Fails if the file exists but cannot be removed.
    pub fn remove(&self, sha256: &str) -> Result<()> {
        let path = self.path_for(sha256);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn store() -> (tempfile::TempDir, BlobStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_blob_round_trips() {
        let (_dir, store) = store();
        let sha = store.put(b"hello world").unwrap();
        assert_eq!(store.get(&sha).unwrap(), b"hello world");
    }

    #[test]
    fn storing_the_same_content_twice_yields_one_copy() {
        let (_dir, store) = store();
        let a = store.put(b"same").unwrap();
        let b = store.put(b"same").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn a_tampered_blob_is_reported_rather_than_returned() {
        let (_dir, store) = store();
        let sha = store.put(b"original").unwrap();
        fs::write(store.path_for(&sha), b"tampered").unwrap();

        let err = store.get(&sha).unwrap_err();
        assert!(matches!(err, StoreError::BlobCorrupt { .. }));
    }

    #[test]
    fn removing_a_missing_blob_is_not_an_error() {
        let (_dir, store) = store();
        store.remove(&"0".repeat(64)).unwrap();
    }

    #[test]
    fn blobs_fan_out_by_hash_prefix() {
        let (_dir, store) = store();
        let sha = store.put(b"content").unwrap();
        let expected = store.root().join(&sha[0..2]).join(&sha);
        assert!(expected.exists(), "blob should live under its hash prefix");
    }
}
