//! API keys, held in the OS credential store and nowhere else.
//!
//! Master spec s12: keys never appear in logs, the database, exports or crash
//! reports. Two things enforce that here rather than leaving it to discipline:
//!
//! - [`Secret`] redacts itself in `Debug` and `Display`, so a key cannot reach
//!   a log line by being interpolated into a message.
//! - [`SecretStore`] exposes `has_key` separately from `get_key`, so the UI can
//!   show whether a key is set without ever receiving its value.
//!
//! On Windows the backing store is Windows Credential Manager, reached through
//! `keyring`'s v1 compatibility API, which initialises the native store on
//! first use.

use std::fmt;
use thiserror::Error;

/// The credential-store service name for every key this app owns.
pub const SERVICE: &str = "ws";

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("no credential store is available on this system: {0}")]
    NoStore(String),

    #[error("credential store rejected the request: {0}")]
    Store(String),
}

type Result<T> = std::result::Result<T, SecretError>;

/// A secret value that will not print itself.
///
/// `expose` is deliberately verbose at the call site: reading a secret should
/// be visible in review, and only `ws-providers` has a reason to do it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Call only when sending it to the provider it belongs to.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

fn map_err(e: keyring::Error) -> SecretError {
    match e {
        keyring::Error::NoDefaultStore => SecretError::NoStore(e.to_string()),
        other => SecretError::Store(other.to_string()),
    }
}

fn entry(account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, account).map_err(map_err)
}

/// Reads and writes API keys, one per backend id.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretStore;

impl SecretStore {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Store or replace the key for `account` (a backend id).
    ///
    /// # Errors
    /// Fails if the credential store is unavailable or rejects the write.
    pub fn set_key(&self, account: &str, secret: &Secret) -> Result<()> {
        entry(account)?
            .set_password(secret.expose())
            .map_err(map_err)?;
        // Note the account, never the value.
        tracing::info!(account, "stored API key");
        Ok(())
    }

    /// Fetch a key. Only provider adapters should call this.
    ///
    /// Returns `Ok(None)` when no key is set, which is an ordinary state, not
    /// an error.
    ///
    /// # Errors
    /// Fails if the credential store is unavailable or rejects the read.
    pub fn get_key(&self, account: &str) -> Result<Option<Secret>> {
        match entry(account)?.get_password() {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }

    /// Whether a key exists, without revealing it. This is what the UI calls.
    ///
    /// # Errors
    /// Fails if the credential store is unavailable or rejects the read.
    pub fn has_key(&self, account: &str) -> Result<bool> {
        Ok(self.get_key(account)?.is_some())
    }

    /// Remove a key. Deleting one that is not there succeeds, so that
    /// "delete all data" is idempotent.
    ///
    /// # Errors
    /// Fails if the credential store is unavailable or rejects the delete.
    pub fn delete_key(&self, account: &str) -> Result<()> {
        match entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                tracing::info!(account, "deleted API key");
                Ok(())
            }
            Err(e) => Err(map_err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_secret_does_not_print_itself() {
        let s = Secret::new("sk-ant-very-secret");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert_eq!(format!("{s}"), "<redacted>");
        assert!(!format!("{s:?} {s}").contains("very-secret"));
        // The value is still reachable where it is actually needed.
        assert_eq!(s.expose(), "sk-ant-very-secret");
    }

    #[test]
    fn a_secret_inside_a_struct_stays_redacted() {
        #[derive(Debug)]
        struct Config {
            // Read only by the derived Debug impl, which is exactly what this
            // test exercises.
            #[allow(dead_code)]
            key: Secret,
        }
        let c = Config {
            key: Secret::new("sk-leak-me"),
        };
        assert!(
            !format!("{c:?}").contains("leak-me"),
            "deriving Debug on a struct holding a Secret must not leak it"
        );
    }
}
