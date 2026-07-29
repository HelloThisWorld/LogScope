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

pub struct FtsIndex {
    conn: Connection,
}

impl FtsIndex {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS fts_logs USING fts5(
                message,
                record_id UNINDEXED,
                dataset_id UNINDEXED,
                segment_id UNINDEXED,
                tokenize = 'unicode61'
             );",
        )?;
        Ok(FtsIndex { conn })
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
        if dataset_ids.is_empty() {
            return Ok(vec![]);
        }
        let match_expr = escape_match_query(user_query);
        if match_expr.is_empty() {
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
