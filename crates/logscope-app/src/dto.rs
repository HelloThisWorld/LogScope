//! Typed command-boundary DTOs shared between Rust and the TypeScript UI.
//!
//! Every type here derives `TS`. Two generated copies are kept in the
//! tree and must stay identical: the ts-rs default `crates/logscope-app/
//! bindings/` and the copy the UI imports, `apps/desktop/src/bindings/`.
//! Regenerate both with `cargo test -p logscope-app --lib export`, once
//! plainly and once with `TS_RS_EXPORT_DIR=<abs path to
//! apps/desktop/src/bindings>`.
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

// ---- v0.3 Case boundary -----------------------------------------------

/// Investigation row as stored; `tags` is the decoded form of the
/// repository's `tags_json`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InvestigationDto {
    pub investigation_id: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub severity: Option<String>,
    pub owner_text: Option<String>,
    pub tags: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    pub status_changed_at: Option<String>,
    #[ts(type = "number | null")]
    pub incident_started_at: Option<i64>,
    #[ts(type = "number | null")]
    pub mitigated_at: Option<i64>,
    #[ts(type = "number | null")]
    pub resolved_at: Option<i64>,
    #[ts(type = "number | null")]
    pub window_start: Option<i64>,
    #[ts(type = "number | null")]
    pub window_end: Option<i64>,
    /// Optimistic-concurrency token: every mutation must send it back.
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NewInvestigationDto {
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<String>,
    pub owner_text: Option<String>,
    pub tags: Vec<String>,
    #[ts(type = "number | null")]
    pub incident_started_at: Option<i64>,
    #[ts(type = "number | null")]
    pub window_start: Option<i64>,
    #[ts(type = "number | null")]
    pub window_end: Option<i64>,
}

/// Full editable-field update (status changes use their own command so
/// the transition is auditable as such).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InvestigationEditDto {
    pub investigation_id: String,
    #[ts(type = "number")]
    pub expected_revision: i64,
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<String>,
    pub owner_text: Option<String>,
    pub tags: Vec<String>,
    #[ts(type = "number | null")]
    pub incident_started_at: Option<i64>,
    #[ts(type = "number | null")]
    pub mitigated_at: Option<i64>,
    #[ts(type = "number | null")]
    pub resolved_at: Option<i64>,
    #[ts(type = "number | null")]
    pub window_start: Option<i64>,
    #[ts(type = "number | null")]
    pub window_end: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HypothesisDto {
    pub hypothesis_id: String,
    pub investigation_id: String,
    pub statement: String,
    pub rationale: Option<String>,
    pub state: String,
    #[ts(type = "number")]
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
    #[ts(type = "number")]
    pub revision: i64,
    /// Evidence linked to this hypothesis (ids into the bundle's list).
    pub linked_evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ItemDto {
    pub item_id: String,
    pub investigation_id: String,
    pub kind: String,
    pub content: String,
    pub task_status: Option<String>,
    pub question_status: Option<String>,
    pub authored_by_user: bool,
    #[ts(type = "number")]
    pub position: i64,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NewItemDto {
    pub investigation_id: String,
    /// `note` | `task` | `finding` | `question`.
    pub kind: String,
    pub content: String,
    pub task_status: Option<String>,
    pub question_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EvidenceDto {
    pub evidence_id: String,
    pub investigation_id: String,
    pub kind: String,
    pub signal: String,
    pub title: String,
    pub annotation: Option<String>,
    pub relevance: Option<String>,
    pub group_id: Option<String>,
    #[ts(type = "number")]
    pub position: i64,
    pub supersedes_evidence_id: Option<String>,
    pub archived: bool,
    /// Latest resolver integrity state (`unverified` until first verify).
    pub resolver_state: String,
    /// Structured detail for the state, exactly as the resolver wrote it.
    pub resolver_detail_json: String,
    pub last_verified_at: Option<String>,
    /// Versioned envelope payloads; the UI renders snapshots read-only
    /// and never re-interprets references (jump-back goes through
    /// `evidence_restore_context`).
    #[ts(type = "number")]
    pub envelope_version: i64,
    pub snapshot_json: String,
    pub created_at: String,
    pub updated_at: String,
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EvidenceGroupDto {
    pub group_id: String,
    pub investigation_id: String,
    pub name: String,
    #[ts(type = "number")]
    pub position: i64,
    #[ts(type = "number")]
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct HistoryDto {
    #[ts(type = "number")]
    pub history_id: i64,
    pub entity_kind: String,
    pub entity_id: String,
    #[ts(type = "number")]
    pub revision: i64,
    pub action: String,
    pub detail_json: String,
    pub created_at: String,
}

/// Everything the investigation workspace view needs in one fetch.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InvestigationBundleDto {
    pub investigation: InvestigationDto,
    pub hypotheses: Vec<HypothesisDto>,
    pub items: Vec<ItemDto>,
    pub evidence: Vec<EvidenceDto>,
    pub groups: Vec<EvidenceGroupDto>,
}

// ---- pin requests (mirror logscope_app::case, ids minted server-side) --

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PinCommonDto {
    pub investigation_id: String,
    pub title: String,
    pub annotation: Option<String>,
    pub relevance: Option<String>,
    pub group_id: Option<String>,
}

/// The exact Explorer scope a pin is made from.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QueryScopeDto {
    pub query_text: String,
    pub dataset_ids: Vec<String>,
    pub time_strategy: TimeStrategyDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PinEventDto {
    pub common: PinCommonDto,
    pub dataset_id: String,
    pub record_id: String,
    pub display_fields: Vec<String>,
    pub include_raw_excerpt: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PinSelectionDto {
    pub common: PinCommonDto,
    pub record_ids: Vec<String>,
    pub scope: QueryScopeDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PinQueryDto {
    pub common: PinCommonDto,
    pub scope: QueryScopeDto,
    pub saved_search_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PinGroupDto {
    pub common: PinCommonDto,
    pub scope: QueryScopeDto,
    pub field: String,
    /// JSON-encoded scalar; `null` selects the missing-value group.
    pub value_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PinIntervalDto {
    pub common: PinCommonDto,
    pub scope: QueryScopeDto,
    #[ts(type = "number")]
    pub start: i64,
    #[ts(type = "number")]
    pub end: i64,
    #[ts(type = "number")]
    pub bucket_width_nanos: i64,
    pub display_timezone: String,
    /// Visible neighbor buckets as (bucket_start, count) pairs.
    #[ts(type = "Array<[number, number]>")]
    pub neighbor_buckets: Vec<(i64, i64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PinItemDto {
    pub common: PinCommonDto,
    pub item_id: String,
}

// ---- verification -----------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VerifyStartedDto {
    pub job_id: String,
    /// Number of evidence items the run will consider.
    #[ts(type = "number")]
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EvidenceOutcomeDto {
    pub evidence_id: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VerificationReportDto {
    #[ts(type = "number")]
    pub total: i64,
    #[ts(type = "number")]
    pub updated: i64,
    pub cancelled: bool,
    #[ts(type = "number")]
    pub dataset_lookups: i64,
    /// State name → count.
    #[ts(type = "Record<string, number>")]
    pub states: std::collections::BTreeMap<String, i64>,
    pub outcomes: Vec<EvidenceOutcomeDto>,
    #[ts(type = "number")]
    pub duration_ms: i64,
}

/// Terminal payload of a verification job (emitted as `verify-finished`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct VerifyFinishedDto {
    pub job_id: String,
    pub investigation_id: String,
    pub report: Option<VerificationReportDto>,
    pub error: Option<ErrorDto>,
}

// ---- jump-back --------------------------------------------------------

/// Decoded restore instructions for one evidence item: exactly what was
/// captured, never broadened. `kind` mirrors the evidence kind; unused
/// fields stay `None` for kinds that do not carry them.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RestoreContextDto {
    pub kind: String,
    /// Authoritative query text to restore (for group pins this is the
    /// captured base query composed with the group predicate).
    pub query_text: Option<String>,
    pub dataset_ids: Vec<String>,
    pub time_strategy: Option<TimeStrategyDto>,
    /// Concrete half-open bounds resolved at pin time.
    #[ts(type = "number | null")]
    pub resolved_start: Option<i64>,
    #[ts(type = "number | null")]
    pub resolved_end: Option<i64>,
    /// Event pins: the record to focus.
    pub record_id: Option<String>,
    pub dataset_id: Option<String>,
    /// Selection pins: the captured ids, in order.
    pub record_ids: Vec<String>,
    /// Interval pins: the exact half-open interval.
    #[ts(type = "number | null")]
    pub interval_start: Option<i64>,
    #[ts(type = "number | null")]
    pub interval_end: Option<i64>,
    /// Item pins: the referenced workspace item.
    pub item_id: Option<String>,
}
