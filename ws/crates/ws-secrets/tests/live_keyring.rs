//! Round-trip against the real OS credential store (checkpoint C1).
//!
//! Gated behind `live-tests` because it writes to Windows Credential Manager.
//! Run with: cargo test -p ws-secrets --features live-tests
#![cfg(feature = "live-tests")]
// Tests may unwrap and expect: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ws_secrets::{Secret, SecretStore};

/// A distinctive account name so a failed run is obvious in Credential
/// Manager and cannot collide with a real provider id.
const ACCOUNT: &str = "__ws_test_account__";

#[test]
fn a_key_can_be_set_read_replaced_and_deleted() {
    let store = SecretStore::new();

    // Start clean, in case an earlier run died partway.
    store.delete_key(ACCOUNT).unwrap();
    assert!(!store.has_key(ACCOUNT).unwrap());
    assert!(store.get_key(ACCOUNT).unwrap().is_none());

    store.set_key(ACCOUNT, &Secret::new("first-value")).unwrap();
    assert!(store.has_key(ACCOUNT).unwrap());
    assert_eq!(
        store.get_key(ACCOUNT).unwrap().unwrap().expose(),
        "first-value"
    );

    // Replacing overwrites rather than adding a second credential.
    store
        .set_key(ACCOUNT, &Secret::new("second-value"))
        .unwrap();
    assert_eq!(
        store.get_key(ACCOUNT).unwrap().unwrap().expose(),
        "second-value"
    );

    store.delete_key(ACCOUNT).unwrap();
    assert!(!store.has_key(ACCOUNT).unwrap());

    // Deleting again is not an error, so "delete all data" is idempotent.
    store.delete_key(ACCOUNT).unwrap();
}
