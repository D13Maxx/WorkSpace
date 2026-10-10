//! Schema migrations, tracked with `PRAGMA user_version` (master spec s10).
//!
//! Each migration is an embedded, numbered SQL file applied in order inside a
//! transaction. Applied migrations are never edited; changes are new files.

use crate::error::{Result, StoreError};
use rusqlite::Connection;

/// Embedded migrations, in application order.
const MIGRATIONS: &[(u32, &str)] = &[
    (1, include_str!("../migrations/001_initial.sql")),
    (2, include_str!("../migrations/002_file_edits.sql")),
];

/// The newest schema version this build understands.
#[must_use]
pub fn latest_version() -> u32 {
    MIGRATIONS.last().map_or(0, |(v, _)| *v)
}

fn user_version(conn: &Connection) -> Result<u32> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(u32::try_from(v).unwrap_or(0))
}

/// Bring `conn` up to [`latest_version`], returning the version it now holds.
///
/// # Errors
/// Fails if the database is newer than this build understands, which would
/// otherwise mean writing rows an older schema cannot represent.
pub fn migrate(conn: &Connection) -> Result<u32> {
    let current = user_version(conn)?;
    let latest = latest_version();

    if current > latest {
        return Err(StoreError::SchemaTooNew {
            found: current,
            supported: latest,
        });
    }

    for (version, sql) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        tracing::info!(version, "applying migration");
        conn.execute_batch("BEGIN")?;
        match conn.execute_batch(sql) {
            Ok(()) => {
                // PRAGMA does not accept bound parameters, and `version` comes
                // from the constant table above, never from user input.
                conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
                conn.execute_batch("COMMIT")?;
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e.into());
            }
        }
    }

    user_version(conn)
}

#[cfg(test)]
mod tests {
    // Spec s14 permits unwrap/expect in tests.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn migrating_a_fresh_database_reaches_the_latest_version() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(migrate(&conn).unwrap(), latest_version());
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let conn = Connection::open_in_memory().unwrap();
        let first = migrate(&conn).unwrap();
        let second = migrate(&conn).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_database_from_the_future_is_refused_rather_than_written_to() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute_batch("PRAGMA user_version = 9999").unwrap();

        let err = migrate(&conn).unwrap_err();
        assert!(matches!(err, StoreError::SchemaTooNew { found: 9999, .. }));
    }
}
