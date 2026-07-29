//! Metadata repositories for the v0.2 Explorer control-plane tables
//! (field catalog, index lifecycle, saved searches, column sets, recent
//! searches, export jobs).

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::WorkspaceError;
use crate::meta::MetaDb;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Bound on retained recent searches.
pub const MAX_RECENT_SEARCHES: usize = 50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldStatRow {
    pub dataset_id: String,
    pub display: String,
    /// JSON array of path segments.
    pub path_json: String,
    /// JSON array of observed type tags (`str`, `int`, …).
    pub types_json: String,
    pub present_count: i64,
    pub distinct_est: Option<i64>,
    pub distinct_is_exact: bool,
    pub examples_json: String,
    pub queryable: bool,
    pub catalog_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStateRow {
    pub kind: String,
    pub dataset_id: String,
    pub version: i64,
    pub status: String,
    pub updated_at: String,
    pub detail_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSearchRow {
    pub saved_search_id: String,
    pub name: String,
    pub query_text: String,
    pub language_version: i64,
    pub fingerprint: String,
    pub dataset_selection_json: String,
    pub time_strategy_json: String,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnSetRow {
    pub column_set_id: String,
    pub name: String,
    pub columns_json: String,
    pub is_default: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentSearchRow {
    pub recent_id: i64,
    pub query_text: String,
    pub language_version: i64,
    pub fingerprint: String,
    pub dataset_selection_json: String,
    pub time_strategy_json: String,
    pub run_count: i64,
    pub last_run_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportJobRow {
    pub export_id: String,
    pub job_id: String,
    pub format: String,
    pub destination: String,
    pub query_text: String,
    pub fingerprint: String,
    pub rows_written: i64,
    pub bytes_written: i64,
    pub truncated: bool,
    pub status: String,
    pub error_json: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

impl MetaDb {
    // ---- field catalog ----------------------------------------------------

    /// Replaces the complete derived catalog of one dataset in a single
    /// transaction (the catalog is rebuilt whole, never patched).
    pub fn replace_field_stats(
        &self,
        dataset_id: &str,
        rows: &[FieldStatRow],
    ) -> Result<(), WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM field_stats WHERE dataset_id = ?1",
            params![dataset_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO field_stats
                   (dataset_id, display, path_json, types_json, present_count,
                    distinct_est, distinct_is_exact, examples_json, queryable,
                    catalog_version, built_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            let ts = now();
            for r in rows {
                stmt.execute(params![
                    dataset_id,
                    r.display,
                    r.path_json,
                    r.types_json,
                    r.present_count,
                    r.distinct_est,
                    r.distinct_is_exact as i64,
                    r.examples_json,
                    r.queryable as i64,
                    r.catalog_version,
                    ts
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn field_stats_for_datasets(
        &self,
        dataset_ids: &[String],
    ) -> Result<Vec<FieldStatRow>, WorkspaceError> {
        if dataset_ids.is_empty() {
            return Ok(vec![]);
        }
        let conn = self.raw();
        let placeholders = (0..dataset_ids.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT dataset_id, display, path_json, types_json, present_count,
                    distinct_est, distinct_is_exact, examples_json, queryable,
                    catalog_version
             FROM field_stats WHERE dataset_id IN ({placeholders})
             ORDER BY display"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params_vec: Vec<&dyn rusqlite::ToSql> = dataset_ids
            .iter()
            .map(|d| d as &dyn rusqlite::ToSql)
            .collect();
        let rows = stmt
            .query_map(params_vec.as_slice(), |r| {
                Ok(FieldStatRow {
                    dataset_id: r.get(0)?,
                    display: r.get(1)?,
                    path_json: r.get(2)?,
                    types_json: r.get(3)?,
                    present_count: r.get(4)?,
                    distinct_est: r.get(5)?,
                    distinct_is_exact: r.get::<_, i64>(6)? != 0,
                    examples_json: r.get(7)?,
                    queryable: r.get::<_, i64>(8)? != 0,
                    catalog_version: r.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- index lifecycle ---------------------------------------------------

    pub fn set_index_state(
        &self,
        kind: &str,
        dataset_id: &str,
        version: i64,
        status: &str,
        detail_json: &str,
    ) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        conn.execute(
            "INSERT INTO index_state (kind, dataset_id, version, status, updated_at, detail_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(kind, dataset_id) DO UPDATE SET
               version = excluded.version, status = excluded.status,
               updated_at = excluded.updated_at, detail_json = excluded.detail_json",
            params![kind, dataset_id, version, status, now(), detail_json],
        )?;
        Ok(())
    }

    pub fn index_states(&self, kind: &str) -> Result<Vec<IndexStateRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT kind, dataset_id, version, status, updated_at, detail_json
             FROM index_state WHERE kind = ?1 ORDER BY dataset_id",
        )?;
        let rows = stmt
            .query_map(params![kind], |r| {
                Ok(IndexStateRow {
                    kind: r.get(0)?,
                    dataset_id: r.get(1)?,
                    version: r.get(2)?,
                    status: r.get(3)?,
                    updated_at: r.get(4)?,
                    detail_json: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// True when every listed dataset has `kind` at `version` in `ready`.
    pub fn indexes_ready(
        &self,
        kind: &str,
        version: i64,
        dataset_ids: &[String],
    ) -> Result<bool, WorkspaceError> {
        let states = self.index_states(kind)?;
        Ok(dataset_ids.iter().all(|d| {
            states
                .iter()
                .any(|s| &s.dataset_id == d && s.version == version && s.status == "ready")
        }))
    }

    // ---- saved searches ----------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn upsert_saved_search(
        &self,
        saved_search_id: &str,
        name: &str,
        query_text: &str,
        language_version: i64,
        fingerprint: &str,
        dataset_selection_json: &str,
        time_strategy_json: &str,
        description: Option<&str>,
    ) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        let ts = now();
        conn.execute(
            "INSERT INTO saved_searches
               (saved_search_id, name, query_text, language_version, fingerprint,
                dataset_selection_json, time_strategy_json, description, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
             ON CONFLICT(saved_search_id) DO UPDATE SET
               name = excluded.name, query_text = excluded.query_text,
               language_version = excluded.language_version,
               fingerprint = excluded.fingerprint,
               dataset_selection_json = excluded.dataset_selection_json,
               time_strategy_json = excluded.time_strategy_json,
               description = excluded.description, updated_at = excluded.updated_at",
            params![
                saved_search_id,
                name,
                query_text,
                language_version,
                fingerprint,
                dataset_selection_json,
                time_strategy_json,
                description,
                ts
            ],
        )?;
        Ok(())
    }

    pub fn list_saved_searches(&self) -> Result<Vec<SavedSearchRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT saved_search_id, name, query_text, language_version, fingerprint,
                    dataset_selection_json, time_strategy_json, description,
                    created_at, updated_at
             FROM saved_searches ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SavedSearchRow {
                    saved_search_id: r.get(0)?,
                    name: r.get(1)?,
                    query_text: r.get(2)?,
                    language_version: r.get(3)?,
                    fingerprint: r.get(4)?,
                    dataset_selection_json: r.get(5)?,
                    time_strategy_json: r.get(6)?,
                    description: r.get(7)?,
                    created_at: r.get(8)?,
                    updated_at: r.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_saved_search(&self, saved_search_id: &str) -> Result<bool, WorkspaceError> {
        let conn = self.raw();
        let n = conn.execute(
            "DELETE FROM saved_searches WHERE saved_search_id = ?1",
            params![saved_search_id],
        )?;
        Ok(n > 0)
    }

    // ---- column sets ---------------------------------------------------------

    pub fn upsert_column_set(
        &self,
        column_set_id: &str,
        name: &str,
        columns_json: &str,
        is_default: bool,
    ) -> Result<(), WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        if is_default {
            tx.execute("UPDATE column_sets SET is_default = 0", [])?;
        }
        let ts = now();
        tx.execute(
            "INSERT INTO column_sets (column_set_id, name, columns_json, is_default, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(column_set_id) DO UPDATE SET
               name = excluded.name, columns_json = excluded.columns_json,
               is_default = excluded.is_default, updated_at = excluded.updated_at",
            params![column_set_id, name, columns_json, is_default as i64, ts],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_column_sets(&self) -> Result<Vec<ColumnSetRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT column_set_id, name, columns_json, is_default, created_at, updated_at
             FROM column_sets ORDER BY name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(ColumnSetRow {
                    column_set_id: r.get(0)?,
                    name: r.get(1)?,
                    columns_json: r.get(2)?,
                    is_default: r.get::<_, i64>(3)? != 0,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_column_set(&self, column_set_id: &str) -> Result<bool, WorkspaceError> {
        let conn = self.raw();
        let n = conn.execute(
            "DELETE FROM column_sets WHERE column_set_id = ?1",
            params![column_set_id],
        )?;
        Ok(n > 0)
    }

    // ---- recent searches -----------------------------------------------------

    /// Records one execution. Re-running an unchanged effective query bumps
    /// its counter instead of inserting a duplicate; the list stays bounded.
    pub fn touch_recent_search(
        &self,
        query_text: &str,
        language_version: i64,
        fingerprint: &str,
        dataset_selection_json: &str,
        time_strategy_json: &str,
    ) -> Result<(), WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let ts = now();
        tx.execute(
            "INSERT INTO recent_searches
               (query_text, language_version, fingerprint, dataset_selection_json,
                time_strategy_json, run_count, last_run_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)
             ON CONFLICT(fingerprint, dataset_selection_json, time_strategy_json)
             DO UPDATE SET run_count = run_count + 1, last_run_at = excluded.last_run_at,
                           query_text = excluded.query_text",
            params![
                query_text,
                language_version,
                fingerprint,
                dataset_selection_json,
                time_strategy_json,
                ts
            ],
        )?;
        tx.execute(
            "DELETE FROM recent_searches WHERE recent_id NOT IN (
                SELECT recent_id FROM recent_searches
                ORDER BY last_run_at DESC, recent_id DESC LIMIT ?1)",
            params![MAX_RECENT_SEARCHES as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_recent_searches(&self) -> Result<Vec<RecentSearchRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT recent_id, query_text, language_version, fingerprint,
                    dataset_selection_json, time_strategy_json, run_count, last_run_at
             FROM recent_searches ORDER BY last_run_at DESC, recent_id DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(RecentSearchRow {
                    recent_id: r.get(0)?,
                    query_text: r.get(1)?,
                    language_version: r.get(2)?,
                    fingerprint: r.get(3)?,
                    dataset_selection_json: r.get(4)?,
                    time_strategy_json: r.get(5)?,
                    run_count: r.get(6)?,
                    last_run_at: r.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn delete_recent_search(&self, recent_id: i64) -> Result<bool, WorkspaceError> {
        let conn = self.raw();
        let n = conn.execute(
            "DELETE FROM recent_searches WHERE recent_id = ?1",
            params![recent_id],
        )?;
        Ok(n > 0)
    }

    pub fn clear_recent_searches(&self) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        conn.execute("DELETE FROM recent_searches", [])?;
        Ok(())
    }

    // ---- export jobs ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn insert_export_job(
        &self,
        export_id: &str,
        job_id: &str,
        format: &str,
        destination: &str,
        query_text: &str,
        fingerprint: &str,
        dataset_selection_json: &str,
        time_strategy_json: &str,
        resolved: (Option<i64>, Option<i64>),
        row_limit: i64,
        byte_limit: i64,
    ) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        conn.execute(
            "INSERT INTO export_jobs
               (export_id, job_id, format, destination, query_text, fingerprint,
                dataset_selection_json, time_strategy_json, resolved_start,
                resolved_end, row_limit, byte_limit, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 'running', ?13)",
            params![
                export_id,
                job_id,
                format,
                destination,
                query_text,
                fingerprint,
                dataset_selection_json,
                time_strategy_json,
                resolved.0,
                resolved.1,
                row_limit,
                byte_limit,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn finish_export_job(
        &self,
        export_id: &str,
        status: &str,
        rows_written: i64,
        bytes_written: i64,
        truncated: bool,
        error_json: Option<&str>,
    ) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        conn.execute(
            "UPDATE export_jobs SET status = ?2, rows_written = ?3, bytes_written = ?4,
                    truncated = ?5, error_json = ?6, finished_at = ?7
             WHERE export_id = ?1",
            params![
                export_id,
                status,
                rows_written,
                bytes_written,
                truncated as i64,
                error_json,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn get_export_job(&self, export_id: &str) -> Result<Option<ExportJobRow>, WorkspaceError> {
        let conn = self.raw();
        Ok(conn
            .query_row(
                "SELECT export_id, job_id, format, destination, query_text, fingerprint,
                        rows_written, bytes_written, truncated, status, error_json,
                        created_at, finished_at
                 FROM export_jobs WHERE export_id = ?1",
                params![export_id],
                |r| {
                    Ok(ExportJobRow {
                        export_id: r.get(0)?,
                        job_id: r.get(1)?,
                        format: r.get(2)?,
                        destination: r.get(3)?,
                        query_text: r.get(4)?,
                        fingerprint: r.get(5)?,
                        rows_written: r.get(6)?,
                        bytes_written: r.get(7)?,
                        truncated: r.get::<_, i64>(8)? != 0,
                        status: r.get(9)?,
                        error_json: r.get(10)?,
                        created_at: r.get(11)?,
                        finished_at: r.get(12)?,
                    })
                },
            )
            .optional()?)
    }
}
