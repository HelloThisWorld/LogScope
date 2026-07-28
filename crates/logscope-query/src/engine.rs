//! Hardened DuckDB connection management.
//!
//! Every connection is configured to never install or load extensions on
//! demand: `autoinstall_known_extensions` and `autoload_known_extensions`
//! are disabled at open. The `parquet` and `json` extensions are statically
//! linked into the bundled build, so all required functionality works with
//! networking disabled. See ADR-0006.

use std::path::Path;
use std::sync::Arc;

use duckdb::{Config, Connection, InterruptHandle};

use crate::error::QueryError;

/// SQL applied to every new connection before any user query runs.
const HARDEN_SQL: &str = "SET autoinstall_known_extensions = false;\
     SET autoload_known_extensions = false;";

/// A DuckDB connection with offline hardening applied.
pub struct EngineConnection {
    conn: Connection,
}

impl EngineConnection {
    pub fn open_in_memory() -> Result<Self, QueryError> {
        let conn = Connection::open_in_memory_with_flags(Self::config()?)?;
        conn.execute_batch(HARDEN_SQL)?;
        Ok(EngineConnection { conn })
    }

    /// Opens a persistent DuckDB database file (used for scratch/analysis
    /// state; canonical data lives in Parquet segments).
    pub fn open_file(path: &Path) -> Result<Self, QueryError> {
        let conn = Connection::open_with_flags(path, Self::config()?)?;
        conn.execute_batch(HARDEN_SQL)?;
        Ok(EngineConnection { conn })
    }

    fn config() -> Result<Config, QueryError> {
        // enable_external_access stays on: reading our own Parquet segment
        // files from the workspace directory is the entire storage design.
        // Network access is impossible because httpfs is not compiled in and
        // extension auto-install/auto-load are disabled.
        Ok(Config::default())
    }

    pub fn raw(&self) -> &Connection {
        &self.conn
    }

    /// Handle for cancelling in-flight queries from another thread.
    pub fn interrupt_handle(&self) -> Arc<InterruptHandle> {
        self.conn.interrupt_handle()
    }

    /// Returns `(name, value)` pairs of the offline-hardening settings, so
    /// callers and tests can assert the connection state.
    pub fn hardening_state(&self) -> Result<Vec<(String, String)>, QueryError> {
        let mut stmt = self.conn.prepare(
            "SELECT name, value FROM duckdb_settings() \
             WHERE name IN ('autoinstall_known_extensions','autoload_known_extensions') \
             ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
