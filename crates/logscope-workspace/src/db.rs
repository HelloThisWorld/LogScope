//! SQLite metadata database: open, configure, and forward-only migrations.

use std::path::Path;

use rusqlite::Connection;

use crate::error::WorkspaceError;

/// Embedded, ordered, forward-only migrations. Each runs in one transaction
/// and is recorded in `schema_migrations`. Never edit a shipped migration;
/// add a new one.
const MIGRATIONS: &[(i64, &str, &str)] =
    &[(1, "0001_init", include_str!("migrations/0001_init.sql"))];

/// Highest schema version this build can open.
pub fn supported_schema_version() -> i64 {
    MIGRATIONS.last().map(|(v, _, _)| *v).unwrap_or(0)
}

pub fn open_connection(path: &Path) -> Result<Connection, WorkspaceError> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

/// Applies pending migrations. Errors if the database is newer than this
/// build (forward-only: downgrades are not supported).
pub fn migrate(conn: &mut Connection) -> Result<i64, WorkspaceError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    INTEGER PRIMARY KEY,
            name       TEXT NOT NULL,
            applied_at TEXT NOT NULL
        ) STRICT;",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |r| r.get(0),
    )?;
    let supported = supported_schema_version();
    if current > supported {
        return Err(WorkspaceError::SchemaTooNew {
            found: current,
            supported,
        });
    }

    for (version, name, sql) in MIGRATIONS {
        if *version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![version, name, chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        tracing::info!(version, name, "applied workspace migration");
    }
    Ok(supported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let mut conn = open_connection(&path).unwrap();
        let v1 = migrate(&mut conn).unwrap();
        let v2 = migrate(&mut conn).unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1, supported_schema_version());

        // The schema exists.
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='datasets'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn newer_database_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta.db");
        let mut conn = open_connection(&path).unwrap();
        migrate(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (9999, 'future', 'now')",
            [],
        )
        .unwrap();
        let err = migrate(&mut conn).unwrap_err();
        assert!(matches!(err, WorkspaceError::SchemaTooNew { .. }));
    }
}
