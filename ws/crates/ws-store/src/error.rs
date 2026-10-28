//! Storage failures.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("could not serialise stored value: {0}")]
    Json(#[from] serde_json::Error),

    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    #[error("malformed id in database: {0}")]
    BadId(String),

    #[error("unknown {kind} in database: {value}")]
    BadEnum { kind: &'static str, value: String },

    #[error("conversation {0} not found")]
    ConversationNotFound(String),

    #[error(
        "this database was written by a newer version of the app \
         (schema {found}, this build understands {supported})"
    )]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("blob {expected} does not match its contents (got {actual})")]
    BlobCorrupt { expected: String, actual: String },
}

pub type Result<T> = std::result::Result<T, StoreError>;

impl StoreError {
    /// True when SQLite refused the write because of an immutability trigger,
    /// rather than for an ordinary reason like a constraint violation.
    ///
    /// Callers use this to turn a low-level abort into a clear message: an
    /// attempt to edit history is a bug, not a user error.
    #[must_use]
    pub fn is_immutability_violation(&self) -> bool {
        match self {
            Self::Sqlite(rusqlite::Error::SqliteFailure(_, Some(msg))) => {
                msg.contains("messages are immutable")
                    || msg.contains("messages cannot be deleted individually")
            }
            _ => false,
        }
    }
}
