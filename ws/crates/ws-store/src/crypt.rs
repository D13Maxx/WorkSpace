//! The key that keeps chats unreadable on disk: SQLCipher encrypts the
//! database with it, and blobs (images, attachments, file snapshots) are
//! sealed with it as XChaCha20-Poly1305.

use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::error::{Result, StoreError};

/// How a sealed blob starts, so old plain blobs still read.
pub(crate) const MAGIC: &[u8] = b"WSENC1\0";
const NONCE: usize = 24;

/// 256 random bits. Never printed: `Debug` shows nothing of it.
#[derive(Clone)]
pub struct DataKey([u8; 32]);

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DataKey(<redacted>)")
    }
}

impl DataKey {
    /// A new key from the operating system's random source.
    #[must_use]
    pub fn generate() -> Self {
        Self(XChaCha20Poly1305::generate_key(&mut OsRng).into())
    }

    /// A key as [`DataKey::to_hex`] wrote it.
    #[must_use]
    pub fn from_hex(text: &str) -> Option<Self> {
        hex::decode(text.trim()).ok()?.try_into().ok().map(Self)
    }

    /// For keeping in the credential store.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// SQLCipher's raw-key literal, `"x'…'"`: used as is, no passphrase stretching.
    pub(crate) fn sql(&self) -> String {
        format!("\"x'{}'\"", self.to_hex())
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&self.0.into())
    }

    /// `MAGIC`, a random nonce, then the ciphertext and its tag.
    pub(crate) fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let sealed = self
            .cipher()
            .encrypt(&nonce, plain)
            .map_err(|_| StoreError::Encryption("could not encrypt a blob".to_owned()))?;
        let mut out = Vec::with_capacity(MAGIC.len() + NONCE + sealed.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }

    /// What [`DataKey::seal`] wrote. Fails on a wrong key or altered bytes.
    pub(crate) fn open(&self, sealed: &[u8]) -> Result<Vec<u8>> {
        let body = sealed
            .strip_prefix(MAGIC)
            .filter(|b| b.len() >= NONCE)
            .ok_or_else(|| StoreError::Encryption("not a sealed blob".to_owned()))?;
        let (nonce, text) = body.split_at(NONCE);
        self.cipher()
            .decrypt(XNonce::from_slice(nonce), text)
            .map_err(|_| {
                StoreError::Encryption(
                    "a blob could not be decrypted: wrong key, or it was altered".to_owned(),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn a_sealed_blob_opens_only_with_its_key_and_unaltered() {
        let key = DataKey::generate();
        let sealed = key.seal(b"secret picture").unwrap();
        assert!(sealed.starts_with(MAGIC));
        assert!(!sealed.windows(6).any(|w| w == b"secret"), "no plain text");
        assert_eq!(key.open(&sealed).unwrap(), b"secret picture");
        assert!(DataKey::generate().open(&sealed).is_err(), "wrong key");
        let mut altered = sealed.clone();
        *altered.last_mut().unwrap() ^= 1;
        assert!(key.open(&altered).is_err(), "altered");
        assert_ne!(
            key.seal(b"x").unwrap(),
            key.seal(b"x").unwrap(),
            "fresh nonce each time"
        );
    }

    #[test]
    fn a_key_round_trips_through_hex_and_never_prints() {
        let key = DataKey::generate();
        let back = DataKey::from_hex(&key.to_hex()).unwrap();
        assert_eq!(back.0, key.0);
        assert!(DataKey::from_hex("abcd").is_none(), "too short");
        assert!(!format!("{key:?}").contains(&key.to_hex()));
    }
}
