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

// ---- v0.2 Explorer boundary -------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SpanDto {
    pub start: u32,
    pub end: u32,
    pub start_utf16: u32,
    pub end_utf16: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DiagnosticDto {
    pub code: String,
    /// `error` or `warning`.
    pub severity: String,
    pub message: String,
    pub span: SpanDto,
    pub expected: Option<String>,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HighlightDto {
    /// field | term | keyword | string | regex | operator | paren.
    pub kind: String,
    pub span: SpanDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QueryAnalysisDto {
    pub valid: bool,
    pub diagnostics: Vec<DiagnosticDto>,
    pub highlights: Vec<HighlightDto>,
    pub fingerprint: Option<String>,
    pub language_version: u32,
    /// Catalog completeness for the selection (suggestions may be partial
    /// while a catalog build is pending).
    pub catalog_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FieldInfoDto {
    pub display: String,
    /// `canonical` or `attribute`.
    pub origin: String,
    pub types: Vec<String>,
    pub present_count: i64,
    pub distinct_est: i64,
    pub distinct_is_exact: bool,
    pub examples: Vec<String>,
    pub queryable: bool,
    pub facetable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FieldCatalogDto {
    pub fields: Vec<FieldInfoDto>,
    pub complete: bool,
    pub fts_ready: bool,
}

/// Persistable time strategy (`kind`: all | absolute | relative_to_latest).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TimeStrategyDto {
    pub kind: String,
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub duration_nanos: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ResolvedWindowDto {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub start_text: Option<String>,
    pub end_text: Option<String>,
    pub empty_anchor: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RunQueryDto {
    /// Client-generated identity used for cancellation and stale-response
    /// protection.
    pub request_id: String,
    pub dataset_ids: Vec<String>,
    pub query_text: String,
    pub time_strategy: TimeStrategyDto,
    pub cursor: Option<String>,
    pub backward: bool,
    pub limit: u32,
    /// Also record this run in recent searches (first page only).
    pub record_recent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LogRowV2Dto {
    pub record_id: String,
    pub event_time: Option<i64>,
    pub event_time_text: Option<String>,
    pub severity: Option<String>,
    pub severity_text: Option<String>,
    pub severity_number: Option<i32>,
    pub message: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub dataset_id: String,
    pub source_id: String,
    pub record_number: Option<u64>,
    pub line_start: Option<u64>,
    pub attributes_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QueryPageV2Dto {
    pub request_id: String,
    pub rows: Vec<LogRowV2Dto>,
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub has_more: bool,
    pub matching: i64,
    pub omitted_untimestamped: i64,
    pub resolved_window: ResolvedWindowDto,
    pub elapsed_ms: u64,
    pub used_fts: bool,
    pub used_fallback_text_scan: bool,
    pub warnings: Vec<DiagnosticDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistogramRequestDto {
    pub request_id: String,
    pub dataset_ids: Vec<String>,
    pub query_text: String,
    pub time_strategy: TimeStrategyDto,
    pub max_bins: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistogramBinDto {
    pub start: i64,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistogramDto {
    pub request_id: String,
    pub bins: Vec<HistogramBinDto>,
    pub bin_width_nanos: i64,
    pub start: i64,
    pub end: i64,
    pub total_in_range: i64,
    pub untimestamped_count: i64,
    pub empty: bool,
    /// Display timezone of the axis (always UTC in v0.2).
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FacetsRequestDto {
    pub request_id: String,
    pub dataset_ids: Vec<String>,
    pub query_text: String,
    pub time_strategy: TimeStrategyDto,
    pub fields: Vec<String>,
    pub top_k: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FacetValueDto {
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FacetDto {
    pub display: String,
    pub values: Vec<FacetValueDto>,
    pub missing_count: i64,
    pub truncated: bool,
    /// Set when the field could not be faceted (unknown/conflicting type).
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FieldSummaryRequestDto {
    pub request_id: String,
    pub dataset_ids: Vec<String>,
    pub query_text: String,
    pub time_strategy: TimeStrategyDto,
    pub field: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FieldSummaryDto {
    pub display: String,
    pub present_count: i64,
    pub missing_count: i64,
    pub distinct_count: i64,
    pub distinct_is_exact: bool,
    pub top_values: Vec<FacetValueDto>,
    pub min_numeric: Option<f64>,
    pub max_numeric: Option<f64>,
    pub high_cardinality: bool,
    pub types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RecordDetailDto {
    pub row: LogRowV2Dto,
    pub body_json: Option<String>,
    pub event_name: Option<String>,
    pub resource_json: Option<String>,
    pub scope_json: Option<String>,
    pub provenance_json: String,
    pub timestamp_quality: Vec<String>,
    pub original_timestamp_text: Option<String>,
    pub profile_id: Option<String>,
    pub profile_version: Option<String>,
    pub parser_id: Option<String>,
    pub parser_version: Option<String>,
    pub normalizer_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceContextRequestDto {
    pub dataset_id: String,
    pub record_id: String,
    pub before: u32,
    pub after: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SourceContextDto {
    pub records: Vec<LogRowV2Dto>,
    pub anchor_record_id: String,
    /// available | changed | missing | unsupported.
    pub source_status: String,
    pub source_path: Option<String>,
    /// Bounded raw bytes of the anchor record from the original file, when
    /// available and unchanged (lossless view of multiline framing).
    pub raw_excerpt: Option<String>,
    pub range_low: u64,
    pub range_high: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SavedSearchDto {
    pub saved_search_id: String,
    pub name: String,
    pub query_text: String,
    pub language_version: i64,
    pub fingerprint: String,
    pub dataset_ids: Vec<String>,
    /// Empty vec + `all_datasets == true` = all compatible datasets.
    pub all_datasets: bool,
    pub time_strategy: TimeStrategyDto,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ColumnSetDto {
    pub column_set_id: String,
    pub name: String,
    pub columns: Vec<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RecentSearchDto {
    pub recent_id: i64,
    pub query_text: String,
    pub language_version: i64,
    pub dataset_ids: Vec<String>,
    pub all_datasets: bool,
    pub time_strategy: TimeStrategyDto,
    pub run_count: i64,
    pub last_run_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StartExportDto {
    pub dataset_ids: Vec<String>,
    pub query_text: String,
    pub time_strategy: TimeStrategyDto,
    /// csv | jsonl.
    pub format: String,
    pub destination: String,
    pub row_limit: Option<u64>,
    pub byte_limit: Option<u64>,
    pub csv_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ExportStatusDto {
    pub export_id: String,
    pub job_id: String,
    pub status: String,
    pub rows_written: i64,
    pub bytes_written: i64,
    pub truncated: bool,
    pub destination: String,
    pub error: Option<ErrorDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct IndexStateDto {
    pub kind: String,
    pub dataset_id: String,
    pub version: i64,
    pub status: String,
}

impl ErrorDto {
    pub fn new(code: impl Into<String>, message: impl std::fmt::Display) -> Self {
        ErrorDto {
            code: code.into(),
            message: message.to_string(),
        }
    }
}
