//! SQLite persistence and the blob store (master spec s10).
//!
//! The database enforces the invariants that matter rather than trusting
//! callers: triggers block edits to message content and single-message
//! deletes, so an accidental `UPDATE` fails loudly instead of quietly
//! rewriting history.

pub mod blobs;
pub mod error;
pub mod migrations;
pub mod store;

pub use blobs::BlobStore;
pub use error::{Result, StoreError};
pub use migrations::{latest_version, migrate};
pub use store::{Draft, FileEdit, NewMessage, Store, StoredProfile, StoredProvider};
