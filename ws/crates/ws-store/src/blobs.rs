//! Content-addressed blob storage.
//!
//! Images and files live on disk at `blobs/{sha[0..2]}/{sha256}`, not in the
//! database. Content addressing makes writes idempotent: storing the same
//! image twice costs one copy, and a blob's name is a checksum of itself, so
//! corruption is detectable rather than silent.
//!
//! With a key and encryption on, a blob is sealed on disk (see `crypt`); its
//! name stays the hash of its plain contents. Plain blobs from before still read.

use crate::crypt::{DataKey, MAGIC};
use crate::error::{Result, StoreError};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
    /// Opens sealed blobs; seals new ones when `encrypt` is on.
    key: Option<DataKey>,
    encrypt: bool,
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
        Ok(Self {
            root,
            key: None,
            encrypt: false,
        })
    }

    /// Read sealed blobs with `key`, and seal new ones if `encrypt`.
    #[must_use]
    pub fn with_key(mut self, key: Option<DataKey>, encrypt: bool) -> Self {
        self.key = key;
        self.encrypt = encrypt;
        self
    }

    fn key(&self) -> Result<&DataKey> {
        self.key.as_ref().ok_or_else(|| {
            StoreError::Encryption("a blob is encrypted and there is no key".to_owned())
        })
    }

    /// Bring every blob to how this store keeps them: sealed if it encrypts,
    /// plain if not. Returns how many it rewrote. Safe to run again.
    ///
    /// # Errors
    /// A blob cannot be read, converted or written back.
    pub fn convert_all(&self) -> Result<usize> {
        let mut done = 0;
        for fan in fs::read_dir(&self.root)?.flatten() {
            if !fan.path().is_dir() {
                continue;
            }
            for entry in fs::read_dir(fan.path())?.flatten() {
                let path = entry.path();
                if path.extension().is_some() {
                    continue; // a .partial from a crash
                }
                // Only the start, to skip what is already right: this runs at every start.
                if is_sealed(&path)? == self.encrypt {
                    continue;
                }
                let bytes = fs::read(&path)?;
                let new = if self.encrypt {
                    self.key()?.seal(&bytes)?
                } else {
                    self.key()?.open(&bytes)?
                };
                write_new(&path, &new)?;
                done += 1;
            }
        }
        Ok(done)
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
        match &self.key {
            Some(key) if self.encrypt => write_new(&path, &key.seal(bytes)?)?,
            _ => write_new(&path, bytes)?,
        }
        Ok(sha)
    }

    /// Read a blob back, verifying it still hashes to its own name.
    ///
    /// # Errors
    /// Fails if the blob is missing, unreadable, or does not match its hash.
    pub fn get(&self, sha256: &str) -> Result<Vec<u8>> {
        let mut bytes = fs::read(self.path_for(sha256))?;
        if bytes.starts_with(MAGIC) {
            bytes = self.key()?.open(&bytes)?;
        }
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

fn is_sealed(path: &Path) -> Result<bool> {
    use std::io::Read as _;
    let mut head = Vec::with_capacity(MAGIC.len());
    fs::File::open(path)?
        .take(MAGIC.len() as u64)
        .read_to_end(&mut head)?;
    Ok(head == MAGIC)
}

/// Write to a temporary name and rename, so a crash mid-write cannot leave
/// a short file sitting under a hash that claims to describe it.
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("partial");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    // Tests may unwrap and expect: a panic is the failure report.
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
    fn encrypted_blobs_are_sealed_on_disk_and_convert_both_ways() {
        let (_dir, plain) = store();
        let old = plain.put(b"an old picture").unwrap();
        let key = DataKey::generate();
        let sealed = plain.clone().with_key(Some(key.clone()), true);
        let new = sealed.put(b"a new picture").unwrap();
        let on_disk = |sha: &str| fs::read(sealed.path_for(sha)).unwrap();
        assert!(on_disk(&new).starts_with(MAGIC));
        assert_eq!(
            sealed.get(&old).unwrap(),
            b"an old picture",
            "plain still reads"
        );
        assert!(plain.get(&new).is_err(), "no key, no picture");

        assert_eq!(sealed.convert_all().unwrap(), 1, "only the old one");
        assert!(on_disk(&old).starts_with(MAGIC));
        assert_eq!(sealed.convert_all().unwrap(), 0, "already done");

        let opened = plain.with_key(Some(key), false);
        assert_eq!(opened.convert_all().unwrap(), 2);
        assert_eq!(on_disk(&new), b"a new picture");
        assert_eq!(opened.get(&new).unwrap(), b"a new picture");
    }

    #[test]
    fn blobs_fan_out_by_hash_prefix() {
        let (_dir, store) = store();
        let sha = store.put(b"content").unwrap();
        let expected = store.root().join(&sha[0..2]).join(&sha);
        assert!(expected.exists(), "blob should live under its hash prefix");
    }
}
