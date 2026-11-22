//! Chats encrypted on this computer: on unless the user turns it off, with
//! the key in the operating system's credential store, beside the API keys.
//! When the setting changes, the database and blobs are converted at the
//! next start, the database through a checked copy, so a failure changes nothing.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::error::{Result, StoreError};
use crate::{BlobStore, DataKey, Store};

/// The credential holding the key. The app's key commands refuse this name.
pub const KEY_ACCOUNT: &str = "chat-encryption-key";
/// `{"encrypt": false}` turns encryption off; missing means on.
pub const SETTING_FILE: &str = "chat-encryption.json";

const HEADER: &[u8] = b"SQLite format 3\0";
const COPY: &str = "converting";
const BACKUP: &str = "before-conversion";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Missing,
    Plain,
    Encrypted,
}

/// Whether the database at `path` is there, and encrypted. A plain SQLite
/// file starts with its header; an encrypted one is random from byte one.
///
/// # Errors
/// The file is there but cannot be read.
pub fn file_state(path: &Path) -> Result<FileState> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileState::Missing),
        Err(e) => return Err(e.into()),
    };
    let mut head = Vec::with_capacity(HEADER.len());
    file.take(HEADER.len() as u64).read_to_end(&mut head)?;
    Ok(match head.as_slice() {
        [] => FileState::Missing,
        h if h == HEADER => FileState::Plain,
        _ => FileState::Encrypted,
    })
}

/// Whether the user wants chats encrypted: yes unless they turned it off.
#[must_use]
pub fn wanted(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(SETTING_FILE))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["encrypt"].as_bool())
        .unwrap_or(true)
}

/// Save the setting; it takes effect at the next start.
///
/// # Errors
/// The file cannot be written.
pub fn set_wanted(dir: &Path, encrypt: bool) -> Result<()> {
    std::fs::write(
        dir.join(SETTING_FILE),
        serde_json::json!({ "encrypt": encrypt }).to_string(),
    )?;
    Ok(())
}

/// `ws.db` beside `ws.db.converting` and the like.
fn sibling(db: &Path, tag: &str) -> PathBuf {
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{tag}"));
    db.with_file_name(name)
}

/// A database file and the journal files SQLite keeps beside it.
fn remove_db(path: &Path) -> Result<()> {
    for tag in ["", "-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(tag);
        match std::fs::remove_file(PathBuf::from(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn sql_text(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Rewrite the database at `path`, read with `from`, as encrypted with `to`
/// (or plain, for none). The copy must have every row before it replaces the
/// original, and must open before the original is deleted.
///
/// # Errors
/// Anything fails; the original is then where it was.
pub fn convert(path: &Path, from: Option<&DataKey>, to: Option<&DataKey>) -> Result<()> {
    let (copy, backup) = (sibling(path, COPY), sibling(path, BACKUP));
    remove_db(&copy)?;
    {
        let conn = Connection::open(path)?;
        if let Some(key) = from {
            conn.execute_batch(&format!("PRAGMA key = {};", key.sql()))?;
        }
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        let target = to.map_or_else(|| "''".to_owned(), DataKey::sql);
        let copy_path = copy.to_string_lossy();
        conn.execute_batch(&format!(
            "ATTACH DATABASE {} AS target KEY {target};",
            sql_text(&copy_path)
        ))?;
        conn.query_row("SELECT sqlcipher_export('target')", [], |_| Ok(()))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        conn.execute_batch(&format!("PRAGMA target.user_version = {version};"))?;
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM main.sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for table in &tables {
            let count = |db: &str| -> rusqlite::Result<i64> {
                let name = table.replace('"', "\"\"");
                conn.query_row(&format!("SELECT count(*) FROM {db}.\"{name}\""), [], |r| {
                    r.get(0)
                })
            };
            let (before, after) = (count("main")?, count("target")?);
            if before != after {
                return Err(StoreError::Encryption(format!(
                    "the converted copy of {table} has {after} rows, not {before}; nothing was changed"
                )));
            }
        }
        conn.execute_batch("DETACH DATABASE target;")?;
    }
    std::fs::rename(path, &backup)?;
    std::fs::rename(&copy, path)?;
    let opened = match to {
        Some(key) => Store::open_encrypted(path, key),
        None => Store::open(path),
    };
    if let Err(e) = opened {
        remove_db(path)?;
        std::fs::rename(&backup, path)?;
        return Err(e);
    }
    drop(opened);
    remove_db(&backup)
}

/// A new key, saved and read back before anything is encrypted with it.
fn new_key(secrets: &ws_secrets::SecretStore) -> Result<DataKey> {
    let key = DataKey::generate();
    let fail =
        |e: ws_secrets::SecretError| StoreError::Encryption(format!("cannot save the key: {e}"));
    secrets
        .set_key(KEY_ACCOUNT, &ws_secrets::Secret::new(key.to_hex()))
        .map_err(fail)?;
    match secrets.get_key(KEY_ACCOUNT).map_err(fail)? {
        Some(s) if s.expose() == key.to_hex() => Ok(key),
        _ => Err(StoreError::Encryption(
            "the key did not read back as saved".to_owned(),
        )),
    }
}

/// Open the app's chats in `dir` as the setting says, converting the
/// database and blobs first if it changed. Without a credential store (a
/// headless Linux box), chats that are not encrypted yet stay plain.
///
/// # Errors
/// The chats are encrypted and the key is missing or unreadable, or a
/// conversion or open fails.
pub fn open_data(dir: &Path) -> Result<(Store, BlobStore)> {
    let db = dir.join("ws.db");
    let backup = sibling(&db, BACKUP);
    // A conversion cut short between its two renames: the original is the backup.
    if !db.exists() && backup.exists() {
        std::fs::rename(&backup, &db)?;
    }
    let state = file_state(&db)?;
    let secrets = ws_secrets::SecretStore::new();
    let stored = match secrets.get_key(KEY_ACCOUNT) {
        Ok(Some(s)) => Some(
            DataKey::from_hex(s.expose())
                .ok_or_else(|| StoreError::Encryption("the saved key is malformed".to_owned()))?,
        ),
        Ok(None) => None,
        Err(e) if state == FileState::Encrypted => {
            return Err(StoreError::Encryption(format!("cannot read the key: {e}")));
        }
        Err(e) => {
            tracing::warn!(error = %e, "no credential store; chats stay unencrypted");
            None
        }
    };
    let key = match stored {
        Some(key) => Some(key),
        None if state == FileState::Encrypted => {
            return Err(StoreError::Encryption(
                "your chats are encrypted, and their key is missing from the credential store"
                    .to_owned(),
            ));
        }
        None if wanted(dir) => new_key(&secrets)
            .inspect_err(|e| tracing::warn!(error = %e, "chats stay unencrypted"))
            .ok(),
        None => None,
    };
    let encrypt = wanted(dir) && key.is_some();
    match (state, encrypt) {
        (FileState::Plain, true) => {
            convert(&db, None, key.as_ref())?;
            tracing::info!("chats encrypted");
        }
        (FileState::Encrypted, false) => {
            convert(&db, key.as_ref(), None)?;
            tracing::info!("chats decrypted");
        }
        _ => {}
    }
    let store = match &key {
        Some(k) if encrypt => Store::open_encrypted(&db, k)?,
        _ => Store::open(&db)?,
    };
    // Opened as intended, so a copy or backup left by a crash is not needed.
    remove_db(&sibling(&db, COPY))?;
    remove_db(&backup)?;
    let blobs = BlobStore::new(dir.join("blobs"))?.with_key(key, encrypt);
    let converted = blobs.convert_all()?;
    if converted > 0 {
        tracing::info!(converted, "blobs converted");
    }
    Ok((store, blobs))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use ws_context::{ConvId, Conversation, ModelRef};

    fn a_chat(store: &Store) -> ConvId {
        let conv = Conversation {
            id: ConvId::new(),
            title: "secret plans".to_owned(),
            system_prompt: None,
            active_leaf_id: None,
            current_model: ModelRef::new("fake", "m"),
            created_at: chrono::Utc::now(),
        };
        store.create_conversation(&conv).unwrap();
        conv.id
    }

    #[test]
    fn a_plain_database_converts_to_encrypted_and_back_with_every_row() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ws.db");
        let id = a_chat(&Store::open(&db).unwrap());
        assert_eq!(file_state(&db).unwrap(), FileState::Plain);

        let key = DataKey::generate();
        convert(&db, None, Some(&key)).unwrap();
        assert_eq!(file_state(&db).unwrap(), FileState::Encrypted);
        let raw = std::fs::read(&db).unwrap();
        assert!(
            !raw.windows(13).any(|w| w == b"conversations"),
            "no schema in clear"
        );
        assert!(Store::open(&db).is_err(), "unreadable without the key");
        assert!(
            Store::open_encrypted(&db, &DataKey::generate()).is_err(),
            "or with another"
        );
        let store = Store::open_encrypted(&db, &key).unwrap();
        assert!(
            store.get_conversation(id).unwrap().is_some(),
            "the chat came across"
        );
        drop(store);
        assert!(!sibling(&db, BACKUP).exists() && !sibling(&db, COPY).exists());

        convert(&db, Some(&key), None).unwrap();
        assert_eq!(file_state(&db).unwrap(), FileState::Plain);
        assert!(
            Store::open(&db)
                .unwrap()
                .get_conversation(id)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn a_failed_conversion_leaves_the_original_where_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("ws.db");
        a_chat(&Store::open(&db).unwrap());
        let before = std::fs::read(&db).unwrap();
        // Read with a key it was never encrypted with: the export cannot start.
        assert!(convert(&db, Some(&DataKey::generate()), None).is_err());
        assert_eq!(std::fs::read(&db).unwrap(), before);
    }

    #[test]
    fn the_setting_is_on_unless_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        assert!(wanted(dir.path()));
        set_wanted(dir.path(), false).unwrap();
        assert!(!wanted(dir.path()));
        set_wanted(dir.path(), true).unwrap();
        assert!(wanted(dir.path()));
    }
}
