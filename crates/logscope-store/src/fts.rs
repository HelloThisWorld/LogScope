//! SQLite FTS5 full-text index over published log segments.
//!
//! DuckDB's fts extension is not statically available offline (proved by
//! `logscope-query/tests/offline_probe.rs`), so full-text search runs on
//! SQLite FTS5, which is compiled into the bundled SQLite. Segments are
//! immutable: each is indexed exactly once at publish time, which sidesteps
//! FTS incremental-update pitfalls entirely (ADR-0006).

use std::path::Path;

use rusqlite::{params, Connection};

use crate::error::StoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FtsHit {
    pub record_id: String,
    pub dataset_id: String,
    pub segment_id: String,
}

/// Tokenizer/semantics version of the FTS index. v2 (LogScope 0.2) uses
/// `unicode61 remove_diacritics 0` so that indexed text search and the
/// bounded regex fallback scan share one documented token definition
/// (case-insensitive, diacritics NOT folded). v1 databases (0.0, diacritic
/// folding on) are detected via `PRAGMA user_version` and rebuilt.
pub const FTS_INDEX_VERSION: i64 = 2;

const FTS_V2_SCHEMA: &str = "CREATE VIRTUAL TABLE IF NOT EXISTS fts_logs USING fts5(
        message,
        record_id UNINDEXED,
        dataset_id UNINDEXED,
        segment_id UNINDEXED,
        tokenize = 'unicode61 remove_diacritics 0'
     );";

pub struct FtsIndex {
    conn: Connection,
}

impl FtsIndex {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let existed = path.exists();
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let table_exists: i64 = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'fts_logs'",
            [],
            |r| r.get(0),
        )?;
        if table_exists == 0 {
            conn.execute_batch(FTS_V2_SCHEMA)?;
            conn.pragma_update(None, "user_version", FTS_INDEX_VERSION)?;
        } else if !existed {
            // Cannot happen (fresh file has no table), but keep the invariant.
            conn.pragma_update(None, "user_version", FTS_INDEX_VERSION)?;
        }
        Ok(FtsIndex { conn })
    }

    /// Tokenizer version of this database (0 = pre-versioned v0.0 index).
    pub fn version(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// True when this database predates [`FTS_INDEX_VERSION`] and must be
    /// rebuilt before indexed text search may be used.
    pub fn needs_rebuild(&self) -> Result<bool, StoreError> {
        Ok(self.version()? < FTS_INDEX_VERSION)
    }

    /// Drops all indexed data and re-creates the table with the current
    /// tokenizer. Used by the rebuild job; the index is derived state, so
    /// losing it is always recoverable.
    pub fn reset_to_current_version(&mut self) -> Result<(), StoreError> {
        self.conn.execute_batch("DROP TABLE IF EXISTS fts_logs;")?;
        self.conn.execute_batch(FTS_V2_SCHEMA)?;
        self.conn
            .pragma_update(None, "user_version", FTS_INDEX_VERSION)?;
        Ok(())
    }

    /// Indexes one immutable segment's display messages in a single
    /// transaction. Re-indexing the same segment first removes its rows,
    /// so the operation is idempotent.
    pub fn index_log_segment<'a>(
        &mut self,
        dataset_id: &str,
        segment_id: &str,
        entries: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Result<u64, StoreError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM fts_logs WHERE segment_id = ?1",
            params![segment_id],
        )?;
        let mut n = 0u64;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO fts_logs (message, record_id, dataset_id, segment_id)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (record_id, message) in entries {
                stmt.execute(params![message, record_id, dataset_id, segment_id])?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn remove_dataset(&mut self, dataset_id: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM fts_logs WHERE dataset_id = ?1",
            params![dataset_id],
        )?;
        Ok(())
    }

    /// Bounded search across the given datasets, best matches first.
    pub fn search_logs(
        &self,
        dataset_ids: &[String],
        user_query: &str,
        limit: usize,
    ) -> Result<Vec<FtsHit>, StoreError> {
        self.search_logs_expr(dataset_ids, &escape_match_query(user_query), limit)
    }

    /// Bounded search with a pre-built MATCH expression. The expression must
    /// come from trusted code (the query compiler), never raw user text.
    pub fn search_logs_expr(
        &self,
        dataset_ids: &[String],
        match_expr: &str,
        limit: usize,
    ) -> Result<Vec<FtsHit>, StoreError> {
        if dataset_ids.is_empty() || match_expr.is_empty() {
            return Ok(vec![]);
        }
        let placeholders = (0..dataset_ids.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT record_id, dataset_id, segment_id FROM fts_logs
             WHERE fts_logs MATCH ?1 AND dataset_id IN ({placeholders})
             ORDER BY rank LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let limit = limit as i64;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = vec![&match_expr, &limit];
        for d in dataset_ids {
            params_vec.push(d);
        }
        let rows = stmt
            .query_map(params_vec.as_slice(), |r| {
                Ok(FtsHit {
                    record_id: r.get(0)?,
                    dataset_id: r.get(1)?,
                    segment_id: r.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Number of hits for a trusted MATCH expression across datasets,
    /// counted up to `cap` (the caller only needs "fits / does not fit").
    pub fn count_logs_expr(
        &self,
        dataset_ids: &[String],
        match_expr: &str,
        cap: usize,
    ) -> Result<usize, StoreError> {
        if dataset_ids.is_empty() || match_expr.is_empty() {
            return Ok(0);
        }
        let placeholders = (0..dataset_ids.len())
            .map(|i| format!("?{}", i + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT count(*) FROM (
                SELECT 1 FROM fts_logs
                WHERE fts_logs MATCH ?1 AND dataset_id IN ({placeholders})
                LIMIT ?2)"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let cap_param = cap as i64 + 1;
        let mut params_vec: Vec<&dyn rusqlite::ToSql> = vec![&match_expr, &cap_param];
        for d in dataset_ids {
            params_vec.push(d);
        }
        let n: i64 = stmt.query_row(params_vec.as_slice(), |r| r.get(0))?;
        Ok(n as usize)
    }
}

/// Converts free text into a safe FTS5 MATCH expression: every whitespace
/// token becomes a quoted phrase (AND semantics), so user input can never
/// inject FTS5 operators or produce syntax errors.
pub fn escape_match_query(text: &str) -> String {
    text.split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_search_and_reindex_are_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = FtsIndex::open(&dir.path().join("fts.db")).unwrap();

        let entries = [
            ("log-1", "connection timeout to upstream"),
            ("log-2", "user login ok"),
            ("log-3", "upstream returned 502"),
        ];
        let n = idx
            .index_log_segment("ds-1", "seg-1", entries.iter().map(|(a, b)| (*a, *b)))
            .unwrap();
        assert_eq!(n, 3);

        let hits = idx
            .search_logs(&["ds-1".to_string()], "upstream", 10)
            .unwrap();
        assert_eq!(hits.len(), 2);

        // Idempotent re-index of the same segment: no duplicates.
        idx.index_log_segment("ds-1", "seg-1", entries.iter().map(|(a, b)| (*a, *b)))
            .unwrap();
        let hits = idx
            .search_logs(&["ds-1".to_string()], "upstream", 10)
            .unwrap();
        assert_eq!(hits.len(), 2);

        // Other datasets are not searched.
        let none = idx
            .search_logs(&["ds-other".to_string()], "upstream", 10)
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn hostile_query_text_cannot_break_match_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = FtsIndex::open(&dir.path().join("fts.db")).unwrap();
        idx.index_log_segment("ds-1", "seg-1", [("log-1", "a AND b OR c")].into_iter())
            .unwrap();
        for hostile in ["AND", "OR NOT (", "\"unbalanced", "a* NEAR/3 b", "-excl"] {
            // Must not error, regardless of hits.
            idx.search_logs(&["ds-1".to_string()], hostile, 10).unwrap();
        }
        // Operator words are matched literally, not interpreted.
        let hits = idx.search_logs(&["ds-1".to_string()], "AND", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
