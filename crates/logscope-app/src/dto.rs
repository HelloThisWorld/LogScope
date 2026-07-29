//! Typed command-boundary DTOs shared between Rust and the TypeScript UI.
//!
//! Every type here derives `TS` and is exported to
//! `apps/desktop/src/bindings/` by `cargo test -p logscope-app export_bindings`.
//! The desktop shell must only speak these shapes.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct WorkspaceInfoDto {
    pub root: String,
    pub workspace_id: String,
    pub name: String,
    pub schema_version: i64,
    pub product_version: String,
    pub available_signals: Vec<String>,
    /// Present when opening performed crash recovery.
    pub recovery: Option<RecoveryDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RecoveryDto {
    pub discarded_staging_dirs: Vec<String>,
    pub removed_orphan_files: Vec<String>,
    pub interrupted_jobs: Vec<String>,
    pub discarded_staging_datasets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DatasetDto {
    pub dataset_id: String,
    pub name: String,
    pub signal: String,
    pub status: String,
    pub created_at: String,
    pub row_count: i64,
    pub byte_size: i64,
    pub segment_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JobDto {
    pub job_id: String,
    pub kind: String,
    pub status: String,
    pub dataset_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub error_json: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OverviewDto {
    pub workspace: WorkspaceInfoDto,
    pub datasets: Vec<DatasetDto>,
    pub jobs: Vec<JobDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StartImportDto {
    pub paths: Vec<String>,
    pub dataset_name: String,
    /// Built-in profile selector for the v0.0 proof UI: `jsonl` or `csv`.
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LogQueryDto {
    pub dataset_ids: Vec<String>,
    pub time_start: Option<i64>,
    pub time_end: Option<i64>,
    pub min_severity: Option<i32>,
    pub contains_text: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LogRowDto {
    pub record_id: String,
    pub event_time: Option<i64>,
    pub event_time_text: Option<String>,
    pub severity_text: Option<String>,
    pub severity_number: Option<i32>,
    pub display_message: String,
    pub dataset_id: String,
    pub record_number: Option<u64>,
    pub line_start: Option<u64>,
    pub attributes_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LogPageDto {
    pub rows: Vec<LogRowDto>,
    pub has_more: bool,
    pub limit: u32,
}

/// Structured error envelope for every command (stable codes).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ErrorDto {
    pub code: String,
    pub message: String,
}

impl ErrorDto {
    pub fn new(code: impl Into<String>, message: impl std::fmt::Display) -> Self {
        ErrorDto {
            code: code.into(),
            message: message.to_string(),
        }
    }
}
