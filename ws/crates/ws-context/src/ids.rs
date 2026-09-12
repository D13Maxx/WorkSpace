//! Identifier newtypes.
//!
//! Message ids are ULIDs: lexicographically sortable by creation time, which
//! gives sibling branches a stable order without a separate sequence column.

use serde::{Deserialize, Serialize};
use std::fmt;
use ulid::Ulid;

macro_rules! ulid_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Ulid);

        impl $name {
            /// Generate a fresh id from the current time.
            #[must_use]
            pub fn new() -> Self {
                Self(Ulid::generate())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = ulid::DecodeError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ulid::from_string(s).map(Self)
            }
        }
    };
}

ulid_id!(ConvId, "Identifies a conversation.");
ulid_id!(MsgId, "Identifies a message. Immutable once persisted.");
ulid_id!(SummaryId, "Identifies a cached context summary.");
ulid_id!(
    EventId,
    "Identifies a conversation event (e.g. a model switch)."
);
ulid_id!(
    BlobId,
    "Identifies a stored blob by row, distinct from its hash."
);

/// Names a backend: one of the API adapters, or the local engine.
///
/// Free-form rather than an enum because users can add any number of
/// OpenAI-compatible endpoints, each of which is its own backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BackendId(pub String);

impl BackendId {
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for BackendId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for BackendId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Content-addressed reference to a blob on disk (`blobs/{sha[0..2]}/{sha256}`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobRef(pub String);

impl BlobRef {
    #[must_use]
    pub fn new(sha256_hex: impl Into<String>) -> Self {
        Self(sha256_hex.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlobRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
