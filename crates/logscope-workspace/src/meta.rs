//! Metadata repositories over `workspace.db`.

use std::path::Path;

use logscope_model::{RecordLocator, ResourceDescriptor, ScopeDescriptor};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db;
use crate::error::WorkspaceError;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Maximum bytes of raw excerpt stored per rejected record.
pub const MAX_REJECT_EXCERPT_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Logs,
    Metrics,
    Spans,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Logs => "logs",
            Signal::Metrics => "metrics",
            Signal::Spans => "spans",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "logs" => Some(Signal::Logs),
            "metrics" => Some(Signal::Metrics),
            "spans" => Some(Signal::Spans),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRow {
    pub source_id: String,
    pub kind: String,
    pub display_name: String,
    pub retention_mode: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceFileRow {
    pub file_id: String,
    pub source_id: String,
    pub path: String,
    pub archive_entry: Option<String>,
    pub size_bytes: i64,
    pub modified_at: Option<String>,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetRow {
    pub dataset_id: String,
    pub name: String,
    pub signal: String,
    pub status: String,
    pub created_at: String,
    pub published_at: Option<String>,
    pub profile_id: Option<String>,
    pub profile_version: Option<String>,
    pub parser_id: Option<String>,
    pub parser_version: Option<String>,
    pub normalizer_version: Option<String>,
    pub model_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentRow {
    pub segment_id: String,
    pub dataset_id: String,
    pub signal: String,
    pub file_name: String,
    pub row_count: i64,
    pub byte_size: i64,
    pub min_event_time: Option<i64>,
    pub max_event_time: Option<i64>,
    pub fts_indexed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRow {
    pub job_id: String,
    pub kind: String,
    pub status: String,
    pub dataset_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub error_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedRecordRow {
    pub reject_id: i64,
    pub dataset_id: String,
    pub source_id: String,
    pub file_id: String,
    pub locator_json: String,
    pub reason_code: String,
    pub message: String,
    pub raw_excerpt: Option<Vec<u8>>,
    pub retryable: bool,
}

/// A staged segment ready for atomic publication.
#[derive(Debug, Clone)]
pub struct SegmentToPublish {
    pub segment_id: String,
    pub signal: Signal,
    pub file_name: String,
    pub row_count: i64,
    pub byte_size: i64,
    pub min_event_time: Option<i64>,
    pub max_event_time: Option<i64>,
}

/// Per-file ledger counts recorded at publish time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LedgerCounts {
    pub accepted: u64,
    pub rejected: u64,
    pub unparsed: u64,
    pub duplicate: u64,
}

#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub source_id: String,
    pub file_id: String,
    pub checkpoint_json: String,
    pub counts: LedgerCounts,
}

/// Thread-safe metadata store. All methods take `&self`.
pub struct MetaDb {
    conn: Mutex<Connection>,
}

impl MetaDb {
    /// Crate-internal access to the underlying connection (repositories in
    /// sibling modules).
    pub(crate) fn raw(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    pub fn open(path: &Path) -> Result<Self, WorkspaceError> {
        let mut conn = db::open_connection(path)?;
        db::migrate(&mut conn)?;
        Ok(MetaDb {
            conn: Mutex::new(conn),
        })
    }

    pub fn schema_version(&self) -> Result<i64, WorkspaceError> {
        let conn = self.conn.lock();
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn set_info(&self, key: &str, value: &str) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO workspace_info (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_info(&self, key: &str) -> Result<Option<String>, WorkspaceError> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT value FROM workspace_info WHERE key = ?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    // ---- sources ---------------------------------------------------------

    pub fn insert_source(
        &self,
        source_id: &str,
        kind: &str,
        display_name: &str,
        retention_mode: &str,
        detail_json: &str,
    ) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO sources (source_id, kind, display_name, retention_mode, created_at, detail_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![source_id, kind, display_name, retention_mode, now(), detail_json],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_source_file(
        &self,
        file_id: &str,
        source_id: &str,
        path: &str,
        archive_parent_file_id: Option<&str>,
        archive_entry: Option<&str>,
        size_bytes: i64,
        modified_at: Option<&str>,
        content_hash: &str,
    ) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO source_files
               (file_id, source_id, path, managed_rel_path, archive_parent_file_id,
                archive_entry, size_bytes, modified_at, content_hash, created_at)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                file_id,
                source_id,
                path,
                archive_parent_file_id,
                archive_entry,
                size_bytes,
                modified_at,
                content_hash,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn list_sources(&self) -> Result<Vec<SourceRow>, WorkspaceError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT source_id, kind, display_name, retention_mode, created_at
             FROM sources ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(SourceRow {
                    source_id: r.get(0)?,
                    kind: r.get(1)?,
                    display_name: r.get(2)?,
                    retention_mode: r.get(3)?,
                    created_at: r.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn find_source_files_by_hash(
        &self,
        content_hash: &str,
    ) -> Result<Vec<SourceFileRow>, WorkspaceError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT file_id, source_id, path, archive_entry, size_bytes, modified_at, content_hash
             FROM source_files WHERE content_hash = ?1",
        )?;
        let rows = stmt
            .query_map(params![content_hash], |r| {
                Ok(SourceFileRow {
                    file_id: r.get(0)?,
                    source_id: r.get(1)?,
                    path: r.get(2)?,
                    archive_entry: r.get(3)?,
                    size_bytes: r.get(4)?,
                    modified_at: r.get(5)?,
                    content_hash: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- datasets --------------------------------------------------------

    pub fn create_dataset(
        &self,
        dataset_id: &str,
        name: &str,
        signal: Signal,
    ) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO datasets (dataset_id, name, signal, status, created_at)
             VALUES (?1, ?2, ?3, 'staging', ?4)",
            params![dataset_id, name, signal.as_str(), now()],
        )?;
        Ok(())
    }

    pub fn link_dataset_source(
        &self,
        dataset_id: &str,
        source_id: &str,
    ) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO dataset_sources (dataset_id, source_id) VALUES (?1, ?2)",
            params![dataset_id, source_id],
        )?;
        Ok(())
    }

    pub fn get_dataset(&self, dataset_id: &str) -> Result<Option<DatasetRow>, WorkspaceError> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT dataset_id, name, signal, status, created_at, published_at,
                        profile_id, profile_version, parser_id, parser_version,
                        normalizer_version, model_version
                 FROM datasets WHERE dataset_id = ?1",
                params![dataset_id],
                Self::map_dataset,
            )
            .optional()?)
    }

    pub fn list_datasets(&self) -> Result<Vec<DatasetRow>, WorkspaceError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT dataset_id, name, signal, status, created_at, published_at,
                    profile_id, profile_version, parser_id, parser_version,
                    normalizer_version, model_version
             FROM datasets ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map([], Self::map_dataset)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn map_dataset(r: &rusqlite::Row<'_>) -> rusqlite::Result<DatasetRow> {
        Ok(DatasetRow {
            dataset_id: r.get(0)?,
            name: r.get(1)?,
            signal: r.get(2)?,
            status: r.get(3)?,
            created_at: r.get(4)?,
            published_at: r.get(5)?,
            profile_id: r.get(6)?,
            profile_version: r.get(7)?,
            parser_id: r.get(8)?,
            parser_version: r.get(9)?,
            normalizer_version: r.get(10)?,
            model_version: r.get(11)?,
        })
    }

    pub fn delete_dataset(&self, dataset_id: &str) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM datasets WHERE dataset_id = ?1",
            params![dataset_id],
        )?;
        Ok(())
    }

    // ---- segments --------------------------------------------------------

    pub fn segments_for_dataset(
        &self,
        dataset_id: &str,
    ) -> Result<Vec<SegmentRow>, WorkspaceError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT segment_id, dataset_id, signal, file_name, row_count, byte_size,
                    min_event_time, max_event_time, fts_indexed
             FROM segments WHERE dataset_id = ?1 ORDER BY file_name",
        )?;
        let rows = stmt
            .query_map(params![dataset_id], |r| {
                Ok(SegmentRow {
                    segment_id: r.get(0)?,
                    dataset_id: r.get(1)?,
                    signal: r.get(2)?,
                    file_name: r.get(3)?,
                    row_count: r.get(4)?,
                    byte_size: r.get(5)?,
                    min_event_time: r.get(6)?,
                    max_event_time: r.get(7)?,
                    fts_indexed: r.get::<_, i64>(8)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn all_segment_file_names(&self, dataset_id: &str) -> Result<Vec<String>, WorkspaceError> {
        Ok(self
            .segments_for_dataset(dataset_id)?
            .into_iter()
            .map(|s| s.file_name)
            .collect())
    }

    pub fn mark_segment_fts_indexed(&self, segment_id: &str) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE segments SET fts_indexed = 1 WHERE segment_id = ?1",
            params![segment_id],
        )?;
        Ok(())
    }

    // ---- resources / scopes ---------------------------------------------

    pub fn upsert_resource(&self, r: &ResourceDescriptor) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO resources
               (resource_id, canonical_json, service_name, service_namespace,
                service_instance_id, deployment_environment, schema_url, first_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                r.resource_id,
                serde_json::to_string(r).map_err(WorkspaceError::Manifest)?,
                r.derived.service_name,
                r.derived.service_namespace,
                r.derived.service_instance_id,
                r.derived.deployment_environment,
                r.schema_url,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn upsert_scope(&self, s: &ScopeDescriptor) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO scopes
               (scope_id, canonical_json, name, version, schema_url, first_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                s.scope_id,
                serde_json::to_string(s).map_err(WorkspaceError::Manifest)?,
                s.name,
                s.version,
                s.schema_url,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn get_resource_json(&self, resource_id: &str) -> Result<Option<String>, WorkspaceError> {
        let conn = self.conn.lock();
        Ok(conn
            .query_row(
                "SELECT canonical_json FROM resources WHERE resource_id = ?1",
                params![resource_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    // ---- jobs ------------------------------------------------------------

    pub fn insert_job(
        &self,
        job_id: &str,
        kind: &str,
        dataset_id: Option<&str>,
    ) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO jobs (job_id, kind, status, dataset_id, created_at, updated_at)
             VALUES (?1, ?2, 'running', ?3, ?4, ?4)",
            params![job_id, kind, dataset_id, now()],
        )?;
        Ok(())
    }

    pub fn update_job_status(
        &self,
        job_id: &str,
        status: &str,
        error_json: Option<&str>,
    ) -> Result<(), WorkspaceError> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE jobs SET status = ?2, error_json = ?3, updated_at = ?4 WHERE job_id = ?1",
            params![job_id, status, error_json, now()],
        )?;
        Ok(())
    }

    pub fn list_jobs(&self) -> Result<Vec<JobRow>, WorkspaceError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT job_id, kind, status, dataset_id, created_at, updated_at, error_json
             FROM jobs ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(JobRow {
                    job_id: r.get(0)?,
                    kind: r.get(1)?,
                    status: r.get(2)?,
                    dataset_id: r.get(3)?,
                    created_at: r.get(4)?,
                    updated_at: r.get(5)?,
                    error_json: r.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- rejected records ------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn insert_rejected(
        &self,
        dataset_id: &str,
        source_id: &str,
        file_id: &str,
        locator: &RecordLocator,
        reason_code: &str,
        message: &str,
        raw_excerpt: Option<&[u8]>,
        parser: (&str, &str),
        profile: Option<(&str, &str)>,
        retryable: bool,
    ) -> Result<(), WorkspaceError> {
        let excerpt = raw_excerpt.map(|b| &b[..b.len().min(MAX_REJECT_EXCERPT_BYTES)]);
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO rejected_records
               (dataset_id, source_id, file_id, locator_json, reason_code, message,
                raw_excerpt, parser_id, parser_version, profile_id, profile_version,
                retryable, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                dataset_id,
                source_id,
                file_id,
                serde_json::to_string(locator).map_err(WorkspaceError::Manifest)?,
                reason_code,
                message,
                excerpt,
                parser.0,
                parser.1,
                profile.map(|p| p.0),
                profile.map(|p| p.1),
                retryable as i64,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn rejected_for_dataset(
        &self,
        dataset_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<RejectedRecordRow>, WorkspaceError> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT reject_id, dataset_id, source_id, file_id, locator_json, reason_code,
                    message, raw_excerpt, retryable
             FROM rejected_records WHERE dataset_id = ?1
             ORDER BY reject_id LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt
            .query_map(params![dataset_id, limit, offset], |r| {
                Ok(RejectedRecordRow {
                    reject_id: r.get(0)?,
                    dataset_id: r.get(1)?,
                    source_id: r.get(2)?,
                    file_id: r.get(3)?,
                    locator_json: r.get(4)?,
                    reason_code: r.get(5)?,
                    message: r.get(6)?,
                    raw_excerpt: r.get(7)?,
                    retryable: r.get::<_, i64>(8)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- atomic publication ---------------------------------------------

    /// Publishes an import in one transaction: segment rows become visible,
    /// the dataset flips to `published` with its version stamps, ledger
    /// entries are recorded, and the job completes. The segment files must
    /// already be in place (moved before this call); the recovery sweep
    /// removes any moved file whose transaction never committed.
    #[allow(clippy::too_many_arguments)]
    pub fn publish_import(
        &self,
        job_id: &str,
        dataset_id: &str,
        segments: &[SegmentToPublish],
        ledger: &[LedgerEntry],
        versions: &PublishVersions,
    ) -> Result<(), WorkspaceError> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let ts = now();
        for s in segments {
            tx.execute(
                "INSERT INTO segments
                   (segment_id, dataset_id, signal, file_name, row_count, byte_size,
                    min_event_time, max_event_time, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    s.segment_id,
                    dataset_id,
                    s.signal.as_str(),
                    s.file_name,
                    s.row_count,
                    s.byte_size,
                    s.min_event_time,
                    s.max_event_time,
                    ts
                ],
            )?;
        }
        for e in ledger {
            tx.execute(
                "INSERT INTO ingest_ledger
                   (job_id, source_id, file_id, checkpoint_json, records_accepted,
                    records_rejected, records_unparsed, records_duplicate, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    job_id,
                    e.source_id,
                    e.file_id,
                    e.checkpoint_json,
                    e.counts.accepted as i64,
                    e.counts.rejected as i64,
                    e.counts.unparsed as i64,
                    e.counts.duplicate as i64,
                    ts
                ],
            )?;
        }
        tx.execute(
            "UPDATE datasets SET status = 'published', published_at = ?2,
                    profile_id = ?3, profile_version = ?4, parser_id = ?5,
                    parser_version = ?6, normalizer_version = ?7, model_version = ?8
             WHERE dataset_id = ?1",
            params![
                dataset_id,
                ts,
                versions.profile_id,
                versions.profile_version,
                versions.parser_id,
                versions.parser_version,
                versions.normalizer_version,
                versions.model_version
            ],
        )?;
        tx.execute(
            "UPDATE jobs SET status = 'completed', updated_at = ?2 WHERE job_id = ?1",
            params![job_id, ts],
        )?;
        tx.commit()?;
        Ok(())
    }
}

/// Version stamps recorded on the dataset at publish time.
#[derive(Debug, Clone, Default)]
pub struct PublishVersions {
    pub profile_id: Option<String>,
    pub profile_version: Option<String>,
    pub parser_id: Option<String>,
    pub parser_version: Option<String>,
    pub normalizer_version: Option<String>,
    pub model_version: Option<String>,
}
