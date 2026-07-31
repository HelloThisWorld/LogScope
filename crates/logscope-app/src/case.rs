//! Investigation case services: dataset revision fingerprints, the six
//! evidence pin services, and the batched, cancellable evidence resolver.
//!
//! Pinning captures a typed live reference plus a bounded snapshot through
//! the authoritative Explorer pipeline (`logscope-query` + the trusted
//! catalog) — never a second filter implementation. Verification is
//! read-only over evidence payloads: it updates only the resolver columns
//! and never rewrites a captured snapshot or reference (updating a
//! reference is an explicit user action producing a new revision or a
//! superseding item).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use serde::Serialize;

use logscope_case::envelope::{
    self, CountState, DatasetRevRef, DecodeOutcome, EventRef, EventSnapshot, EvidenceReference,
    EvidenceSnapshot, GroupRef, GroupSnapshot, IntervalRef, IntervalSnapshot, ItemReference,
    ItemSnapshot, QueryContext, QueryRef, QuerySummarySnapshot, SelectionRef, SelectionSnapshot,
    SnapshotField, SnapshotRow, MAX_REPRESENTATIVE_IDS, MAX_SELECTION_IDS, MAX_SNAPSHOT_ROWS,
};
use logscope_case::ids::PREFIX_EVIDENCE;
use logscope_case::{
    new_id, CaseError, EvidenceKind, ResolverState, SignalKind, EVIDENCE_ENVELOPE_VERSION,
};
use logscope_jobs::{JobContext, JobError};
use logscope_model::hashing::stable_id;
use logscope_model::provenance::{IngestProvenance, PhysicalOrigin, QualityFlag};
use logscope_query::{
    query_counts, query_page, resolve_window, CompiledFilter, EngineConnection, PageRequest,
    QueryCancelHandle, ResolvedWindow, TimeStrategy,
};
use logscope_query_lang::{CatalogView, FieldResolution, LANGUAGE_VERSION};
use logscope_store::STORAGE_SCHEMA_VERSION;
use logscope_workspace::{
    DatasetRow, EvidenceRow, InvestigationRow, ItemRow, NewEvidence, SourceFileRow, Workspace,
    WorkspaceError,
};

use crate::explorer;

/// Bound on captured neighbor buckets for interval evidence.
pub const MAX_NEIGHBOR_BUCKETS: usize = 16;
/// Rows captured into query/group/interval snapshots (kept below the
/// selection snapshot bound so several pins stay cheap to render).
const QUERY_SNAPSHOT_ROWS: usize = 20;

fn ws_err(e: WorkspaceError) -> JobError {
    JobError::new(e.code(), e.to_string())
}

fn case_err(e: CaseError) -> JobError {
    JobError::new(e.code(), e.to_string())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---- dataset revision fingerprint ---------------------------------------------

/// Computes the `dsrev-<hex>` fingerprint of a dataset's published segment
/// set: a deterministic digest over the sorted (segment_id, row_count,
/// byte_size) triples, the dataset id, and the storage schema version.
/// Recomputable on demand; identical segments always yield the same value.
pub fn dataset_revision(ws: &Workspace, dataset_id: &str) -> Result<String, WorkspaceError> {
    let mut segments = ws.meta.segments_for_dataset(dataset_id)?;
    segments.sort_by(|a, b| a.segment_id.cmp(&b.segment_id));
    Ok(stable_id("dsrev", |d| {
        d.str("dsrev.v1");
        d.str(dataset_id);
        d.u32(STORAGE_SCHEMA_VERSION);
        for s in &segments {
            d.str(&s.segment_id);
            d.i64(s.row_count);
            d.i64(s.byte_size);
        }
    }))
}

// ---- pin requests --------------------------------------------------------------

/// Fields shared by every pin request.
#[derive(Debug, Clone)]
pub struct PinCommon {
    pub investigation_id: String,
    /// Concise display label; must not be blank.
    pub title: String,
    pub annotation: Option<String>,
    /// Explicit relevance explanation.
    pub relevance: Option<String>,
    pub group_id: Option<String>,
}

/// The effective query scope a pin was made from: the exact text, dataset
/// selection, and time strategy in effect in the Explorer.
#[derive(Debug, Clone)]
pub struct QueryScope {
    pub query_text: String,
    /// Empty = all published log datasets (same rule as the Explorer).
    pub dataset_ids: Vec<String>,
    pub time_strategy: TimeStrategy,
}

#[derive(Debug, Clone)]
pub struct PinEventRequest {
    pub common: PinCommon,
    pub dataset_id: String,
    pub record_id: String,
    /// Display fields (attribute names) visible at pin time; resolved
    /// through the trusted catalog. Names that do not resolve to an
    /// attribute are omitted from the snapshot (canonical columns are
    /// always captured).
    pub display_fields: Vec<String>,
    /// Capture the bounded raw source excerpt when the locator allows it.
    pub include_raw_excerpt: bool,
}

#[derive(Debug, Clone)]
pub struct PinSelectionRequest {
    pub common: PinCommon,
    /// Ordered stable canonical ids exactly as selected.
    pub record_ids: Vec<String>,
    pub scope: QueryScope,
}

#[derive(Debug, Clone)]
pub struct PinQueryRequest {
    pub common: PinCommon,
    pub scope: QueryScope,
    /// Present when pinned from a saved search (captured, never
    /// substituted later).
    pub saved_search_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PinGroupRequest {
    pub common: PinCommon,
    /// Base query the facet/distribution was computed over.
    pub scope: QueryScope,
    /// Grouping field display name.
    pub field: String,
    /// JSON-encoded scalar group value; `null` = the missing-value group.
    pub value_json: String,
}

#[derive(Debug, Clone)]
pub struct PinIntervalRequest {
    pub common: PinCommon,
    pub scope: QueryScope,
    /// Half-open interval [start, end) in UTC nanos.
    pub start: i64,
    pub end: i64,
    pub bucket_width_nanos: i64,
    /// IANA timezone the histogram was displayed in.
    pub display_timezone: String,
    /// Visible neighbor buckets (bucket_start, count), bounded.
    pub neighbor_buckets: Vec<(i64, i64)>,
}

#[derive(Debug, Clone)]
pub struct PinItemRequest {
    pub common: PinCommon,
    pub item_id: String,
}

// ---- shared pin plumbing -------------------------------------------------------

/// Loads the target investigation and refuses pins into archived ones.
fn active_investigation(ws: &Workspace, id: &str) -> Result<InvestigationRow, JobError> {
    let row = ws
        .meta
        .get_investigation(id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("investigation {id} does not exist"),
            )
        })?;
    if row.status == "archived" {
        return Err(JobError::new(
            "case/investigation-archived",
            "restore the investigation before pinning evidence",
        ));
    }
    Ok(row)
}

fn all_window() -> ResolvedWindow {
    ResolvedWindow {
        strategy: TimeStrategy::All,
        start: None,
        end: None,
        empty_anchor: false,
    }
}

/// A validated, compiled query scope ready for capture or verification.
struct PreparedScope {
    context: QueryContext,
    dataset_refs: Vec<DatasetRevRef>,
    filter: CompiledFilter,
    window: ResolvedWindow,
    files: Vec<PathBuf>,
}

/// Runs the authoritative pipeline over a pin scope: dataset validation,
/// analysis against the trusted catalog, compilation, and concrete window
/// resolution. Analysis errors become structured failures — an invalid
/// scope is never captured as evidence.
fn prepare_scope(ws: &Workspace, scope: &QueryScope) -> Result<PreparedScope, JobError> {
    let selection = explorer::resolve_dataset_selection(ws, &scope.dataset_ids).map_err(ws_err)?;
    if selection.is_empty() {
        return Err(JobError::new(
            "case/empty-scope",
            "no published log dataset is selected",
        ));
    }
    let analysis = explorer::analyze_query(ws, &selection, &scope.query_text);
    let Some(resolved) = analysis.resolved.as_ref() else {
        let messages: Vec<String> = analysis
            .diagnostics
            .iter()
            .take(5)
            .map(|d| d.message.clone())
            .collect();
        let mut err = JobError::new("query/invalid", "the pinned query does not validate");
        err.detail = Some(serde_json::json!({ "diagnostics": messages }));
        return Err(err);
    };
    let filter = explorer::compile_for_execution(ws, &selection, &analysis)?;
    let latest = explorer::latest_event_time(ws, &selection).map_err(ws_err)?;
    let window = resolve_window(&scope.time_strategy, latest);
    let files = explorer::segment_files_for(ws, &selection).map_err(ws_err)?;
    let mut dataset_refs = Vec::with_capacity(selection.len());
    for id in &selection {
        dataset_refs.push(DatasetRevRef {
            dataset_id: id.clone(),
            dataset_revision: dataset_revision(ws, id).map_err(ws_err)?,
        });
    }
    let context = QueryContext {
        query_text: scope.query_text.clone(),
        language_version: LANGUAGE_VERSION as i64,
        fingerprint: Some(resolved.fingerprint.clone()),
        dataset_ids: selection,
        time_strategy_json: serde_json::to_string(&scope.time_strategy)
            .unwrap_or_else(|_| "{\"kind\":\"all\"}".into()),
        resolved_start: window.start,
        resolved_end: window.end,
        omitted_untimestamped: None,
    };
    Ok(PreparedScope {
        context,
        dataset_refs,
        filter,
        window,
        files,
    })
}

/// Temp-table id-set filter: one bounded lookup for a whole id batch (the
/// same pattern the FTS path uses) instead of one query per id.
fn id_set_filter(ids: &[String]) -> CompiledFilter {
    CompiledFilter {
        where_sql: "record_id IN (SELECT record_id FROM __ls_case_ids)".into(),
        params: vec![],
        temp_tables: vec![("__ls_case_ids".to_string(), ids.to_vec())],
        text_modes: vec![],
    }
}

/// Fetches up to `limit` rows for an id set in one bounded query.
fn fetch_rows_by_ids(
    engine: &EngineConnection,
    files: &[PathBuf],
    ids: &[String],
    limit: usize,
) -> Result<Vec<logscope_query::LogRow>, JobError> {
    if ids.is_empty() || files.is_empty() {
        return Ok(vec![]);
    }
    let filter = id_set_filter(ids);
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let page = query_page(
        engine,
        files,
        &filter,
        &all_window(),
        &PageRequest {
            cursor: None,
            backward: false,
            limit: limit as u32,
        },
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    Ok(page.rows)
}

/// Which of `ids` exist in the given segment files — one id-set query for
/// the whole batch.
fn lookup_existing_ids(
    engine: &EngineConnection,
    files: &[PathBuf],
    ids: &[String],
) -> Result<HashSet<String>, JobError> {
    if ids.is_empty() || files.is_empty() {
        return Ok(HashSet::new());
    }
    let filter = id_set_filter(ids);
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let mut found: HashSet<String> = HashSet::with_capacity(ids.len());
    let want = ids.len();
    logscope_query::stream_query(
        engine,
        files,
        &filter,
        &all_window(),
        (want as u64).saturating_mul(4),
        &cancel,
        std::time::Duration::from_millis(logscope_query::explore::DEFAULT_BUDGET_MS),
        |row| {
            found.insert(row.record_id);
            Ok(found.len() < want)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    Ok(found)
}

/// Walks the typed attribute tree (`{"seg": {... {"v": value, "t": tag}}}`)
/// and renders the leaf value for display: scalars verbatim, composites as
/// compact JSON.
fn attr_value_by_path(attrs: &serde_json::Value, path: &[String]) -> Option<String> {
    let mut node = attrs;
    for seg in path {
        node = node.get(seg)?;
    }
    let value = node.get("v")?;
    Some(match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// Builds one bounded snapshot row from a canonical row plus the resolved
/// display fields (display name, attribute path).
fn snapshot_row(row: &logscope_query::LogRow, extras: &[(String, Vec<String>)]) -> SnapshotRow {
    let (message, message_truncated) = envelope::bound_field(&row.display_message);
    let mut fields = Vec::new();
    if !extras.is_empty() {
        if let Ok(attrs) = serde_json::from_str::<serde_json::Value>(&row.attributes_json) {
            for (display, path) in extras {
                if let Some(raw) = attr_value_by_path(&attrs, path) {
                    let (value, truncated) = envelope::bound_field(&raw);
                    fields.push(SnapshotField {
                        name: display.clone(),
                        value,
                        truncated,
                    });
                }
            }
        }
    }
    SnapshotRow {
        record_id: row.record_id.clone(),
        event_time: row.event_time,
        severity_text: row.severity_text.clone(),
        severity_number: row.severity_number,
        display_message: message,
        display_message_truncated: message_truncated,
        fields,
    }
}

/// Validates, encodes, and stores one pinned evidence item.
fn store_pin(
    ws: &Workspace,
    common: &PinCommon,
    investigation: &InvestigationRow,
    kind: EvidenceKind,
    signal: SignalKind,
    reference: EvidenceReference,
    snapshot: EvidenceSnapshot,
) -> Result<EvidenceRow, JobError> {
    if common.title.trim().is_empty() {
        return Err(JobError::new(
            "case/invalid",
            "evidence needs a non-empty title",
        ));
    }
    envelope::validate_reference(&reference).map_err(case_err)?;
    let reference_json = envelope::encode_reference(&reference).map_err(case_err)?;
    let snapshot_json = envelope::encode_snapshot(&snapshot).map_err(case_err)?;
    let new = NewEvidence {
        evidence_id: new_id(PREFIX_EVIDENCE),
        investigation_id: common.investigation_id.clone(),
        envelope_version: EVIDENCE_ENVELOPE_VERSION,
        kind: kind.as_str().to_string(),
        signal: signal.as_str().to_string(),
        title: common.title.trim().to_string(),
        annotation: common.annotation.clone(),
        relevance: common.relevance.clone(),
        captured_investigation_revision: investigation.revision,
        group_id: common.group_id.clone(),
        supersedes_evidence_id: None,
        reference_json,
        snapshot_json,
    };
    ws.meta.insert_evidence(&new).map_err(ws_err)
}

/// Timestamp-quality flag names carried onto event evidence.
fn timestamp_quality_names(provenance: &IngestProvenance) -> Vec<String> {
    provenance
        .flags
        .iter()
        .filter_map(|f| match f {
            QualityFlag::TimestampMissing => Some("timestamp_missing"),
            QualityFlag::TimestampUnparsed => Some("timestamp_unparsed"),
            QualityFlag::TimezoneAssumed => Some("timezone_assumed"),
            _ => None,
        })
        .map(str::to_string)
        .collect()
}

// ---- pin services --------------------------------------------------------------

/// Pins one canonical log event.
pub fn pin_event(
    ws: &Workspace,
    engine: &EngineConnection,
    req: &PinEventRequest,
) -> Result<EvidenceRow, JobError> {
    let investigation = active_investigation(ws, &req.common.investigation_id)?;
    let selection = explorer::resolve_dataset_selection(ws, std::slice::from_ref(&req.dataset_id))
        .map_err(ws_err)?;
    let files = explorer::segment_files_for(ws, &selection).map_err(ws_err)?;
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let detail = logscope_query::fetch_record_detail(
        engine,
        &files,
        &req.dataset_id,
        &req.record_id,
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?
    .ok_or_else(|| {
        JobError::new(
            "workspace/missing-entity",
            format!(
                "record {} is not in dataset {}",
                req.record_id, req.dataset_id
            ),
        )
    })?;

    let provenance: IngestProvenance = serde_json::from_str(&detail.row.provenance_json)
        .map_err(|e| JobError::new("case/invalid", format!("provenance unreadable: {e}")))?;
    let (source_file_id, source_row): (Option<String>, Option<SourceFileRow>) =
        match &provenance.origin {
            PhysicalOrigin::File { file_id, .. } => {
                let row = ws.meta.get_source_file(file_id).map_err(ws_err)?;
                (Some(file_id.clone()), row)
            }
            PhysicalOrigin::OtlpSession { .. } => (None, None),
        };

    let reference = EvidenceReference::Event(EventRef {
        record_id: req.record_id.clone(),
        dataset_id: req.dataset_id.clone(),
        dataset_revision: dataset_revision(ws, &req.dataset_id).map_err(ws_err)?,
        segment_id: None,
        source_file_id: source_file_id.clone(),
        source_content_hash: source_row.as_ref().map(|r| r.content_hash.clone()),
        source_locator_json: serde_json::to_string(&provenance.locator).ok(),
        profile_id: provenance.profile_id.clone(),
        profile_version: provenance.profile_version.clone(),
        parser_id: provenance.parser_id.clone(),
        parser_version: provenance.parser_version.clone(),
        event_time: detail.row.event_time,
        timestamp_quality: timestamp_quality_names(&provenance),
    });

    // Resolve requested display fields through the trusted catalog.
    let catalog = explorer::load_catalog(ws, &selection).map_err(ws_err)?;
    let mut extras: Vec<(String, Vec<String>)> = Vec::new();
    for name in &req.display_fields {
        if let FieldResolution::Attr(info) = catalog.resolve_attr(name) {
            extras.push((info.display, info.path));
        }
    }
    let row_snapshot = snapshot_row(&detail.row, &extras);

    let (raw_excerpt, raw_excerpt_truncated) = if req.include_raw_excerpt {
        match (
            source_file_id.as_deref(),
            provenance.locator.byte_start,
            provenance.locator.byte_end,
        ) {
            (Some(file_id), Some(start), Some(end)) => {
                let (status, text, _path) =
                    explorer::read_raw_excerpt(ws, file_id, Some(start), Some(end));
                match (status, text) {
                    (explorer::SourceStatus::Available, Some(t)) => {
                        let (bounded, truncated) = envelope::bound_field(&t);
                        (Some(bounded), truncated)
                    }
                    _ => (None, false),
                }
            }
            _ => (None, false),
        }
    } else {
        (None, false)
    };

    let snapshot = EvidenceSnapshot::Event(EventSnapshot {
        row: row_snapshot,
        raw_excerpt,
        raw_excerpt_truncated,
    });
    store_pin(
        ws,
        &req.common,
        &investigation,
        EvidenceKind::Event,
        SignalKind::Log,
        reference,
        snapshot,
    )
}

/// Pins a bounded, ordered multi-row selection.
pub fn pin_selection(
    ws: &Workspace,
    engine: &EngineConnection,
    req: &PinSelectionRequest,
) -> Result<EvidenceRow, JobError> {
    let investigation = active_investigation(ws, &req.common.investigation_id)?;
    if req.record_ids.is_empty() {
        return Err(JobError::new("case/invalid", "the selection is empty"));
    }
    let mut seen = HashSet::new();
    let ordered: Vec<String> = req
        .record_ids
        .iter()
        .filter(|id| seen.insert((*id).clone()))
        .cloned()
        .collect();
    let selected_count = ordered.len() as u32;
    let truncated = ordered.len() > MAX_SELECTION_IDS;
    let captured: Vec<String> = ordered.into_iter().take(MAX_SELECTION_IDS).collect();

    let prepared = prepare_scope(ws, &req.scope)?;
    let rows = fetch_rows_by_ids(engine, &prepared.files, &captured, MAX_SNAPSHOT_ROWS)?;
    let rows_truncated = rows.len() < captured.len();
    let snapshot_rows: Vec<SnapshotRow> = rows.iter().map(|r| snapshot_row(r, &[])).collect();

    let reference = EvidenceReference::Selection(SelectionRef {
        record_ids: captured,
        datasets: prepared.dataset_refs.clone(),
        context: prepared.context,
        selected_count,
        max_allowed: MAX_SELECTION_IDS as u32,
        truncated,
    });
    let snapshot = EvidenceSnapshot::Selection(SelectionSnapshot {
        rows: snapshot_rows,
        rows_truncated,
    });
    store_pin(
        ws,
        &req.common,
        &investigation,
        EvidenceKind::Selection,
        SignalKind::Log,
        reference,
        snapshot,
    )
}

/// Pins a saved or ad hoc query result.
pub fn pin_query(
    ws: &Workspace,
    engine: &EngineConnection,
    req: &PinQueryRequest,
) -> Result<EvidenceRow, JobError> {
    let investigation = active_investigation(ws, &req.common.investigation_id)?;
    let mut prepared = prepare_scope(ws, &req.scope)?;

    // A saved-search pin captures the definition's identity; it must exist
    // now — a same-name substitute is never accepted later.
    let saved = match &req.saved_search_id {
        None => None,
        Some(id) => {
            let row = ws
                .meta
                .list_saved_searches()
                .map_err(ws_err)?
                .into_iter()
                .find(|s| &s.saved_search_id == id)
                .ok_or_else(|| {
                    JobError::new(
                        "workspace/missing-entity",
                        format!("saved search {id} does not exist"),
                    )
                })?;
            Some(row)
        }
    };

    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let started = Instant::now();
    let counts = query_counts(
        engine,
        &prepared.files,
        &prepared.filter,
        &prepared.window,
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    let duration_ms = started.elapsed().as_millis() as i64;
    let bounded = prepared.window.start.is_some() || prepared.window.end.is_some();
    if bounded {
        prepared.context.omitted_untimestamped = Some(counts.omitted_untimestamped);
    }

    let page = query_page(
        engine,
        &prepared.files,
        &prepared.filter,
        &prepared.window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: QUERY_SNAPSHOT_ROWS as u32,
        },
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    let representative_ids: Vec<String> = page
        .rows
        .iter()
        .take(MAX_REPRESENTATIVE_IDS)
        .map(|r| r.record_id.clone())
        .collect();
    let snapshot_rows: Vec<SnapshotRow> = page.rows.iter().map(|r| snapshot_row(r, &[])).collect();

    let count = CountState::Exact {
        count: counts.matching,
    };
    let reference = EvidenceReference::Query(QueryRef {
        context: prepared.context,
        datasets: prepared.dataset_refs,
        saved_search_id: saved.as_ref().map(|s| s.saved_search_id.clone()),
        saved_search_fingerprint: saved.as_ref().map(|s| s.fingerprint.clone()),
        sort: "event_time DESC NULLS LAST, record_id DESC, dataset_id DESC".into(),
        count: count.clone(),
        representative_ids,
    });
    let snapshot = EvidenceSnapshot::Query(QuerySummarySnapshot {
        count,
        duration_ms: Some(duration_ms),
        rows: snapshot_rows,
        rows_truncated: page.has_more,
    });
    store_pin(
        ws,
        &req.common,
        &investigation,
        EvidenceKind::Query,
        SignalKind::Log,
        reference,
        snapshot,
    )
}

/// Renders the query-language predicate selecting one group value. The
/// missing-value group (`null`) uses the documented missing test.
pub fn group_predicate(field: &str, value_json: &str) -> Result<String, JobError> {
    let value: serde_json::Value = serde_json::from_str(value_json)
        .map_err(|e| JobError::new("case/invalid", format!("group value is not JSON: {e}")))?;
    Ok(match value {
        serde_json::Value::Null => format!("NOT {field}:*"),
        serde_json::Value::String(s) => format!("{field}:{}", quote_value(&s)),
        serde_json::Value::Bool(b) => format!("{field}:{b}"),
        serde_json::Value::Number(n) => format!("{field}:{n}"),
        other => {
            return Err(JobError::new(
                "case/invalid",
                format!("group value must be a scalar, got {other}"),
            ))
        }
    })
}

/// Quotes a string value for the query language (`\"`, `\\`, `\n`, `\t`,
/// `\r` are the supported escapes).
fn quote_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Composes the effective drill-down query for a group: the base text (if
/// any) parenthesized and conjoined with the group predicate. Used by both
/// pinning and verification so the two can never drift apart.
pub fn compose_group_query(base: &str, predicate: &str) -> String {
    let base = base.trim();
    if base.is_empty() {
        predicate.to_string()
    } else {
        format!("({base}) AND {predicate}")
    }
}

/// Pins a visible facet / field-distribution group.
pub fn pin_group(
    ws: &Workspace,
    engine: &EngineConnection,
    req: &PinGroupRequest,
) -> Result<EvidenceRow, JobError> {
    let investigation = active_investigation(ws, &req.common.investigation_id)?;
    let predicate = group_predicate(&req.field, &req.value_json)?;
    let effective_text = compose_group_query(&req.scope.query_text, &predicate);

    // Base context (what the facet was computed over) is captured; the
    // effective drill-down is validated and executed for the counts.
    let base = prepare_scope(ws, &req.scope)?;
    let effective_scope = QueryScope {
        query_text: effective_text,
        dataset_ids: req.scope.dataset_ids.clone(),
        time_strategy: req.scope.time_strategy.clone(),
    };
    let effective = prepare_scope(ws, &effective_scope)?;

    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let group_counts = query_counts(
        engine,
        &effective.files,
        &effective.filter,
        &effective.window,
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    let base_counts = query_counts(
        engine,
        &base.files,
        &base.filter,
        &base.window,
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    let share_bp = if base_counts.matching > 0 {
        Some(((group_counts.matching as i128 * 10_000) / base_counts.matching as i128) as i32)
    } else {
        None
    };

    let page = query_page(
        engine,
        &effective.files,
        &effective.filter,
        &effective.window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: QUERY_SNAPSHOT_ROWS as u32,
        },
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    let representative_ids: Vec<String> = page
        .rows
        .iter()
        .take(MAX_REPRESENTATIVE_IDS)
        .map(|r| r.record_id.clone())
        .collect();
    let snapshot_rows: Vec<SnapshotRow> = page.rows.iter().map(|r| snapshot_row(r, &[])).collect();

    let count = CountState::Exact {
        count: group_counts.matching,
    };
    let reference = EvidenceReference::ExplorerGroup(GroupRef {
        context: base.context,
        datasets: base.dataset_refs,
        field: req.field.clone(),
        value_json: req.value_json.clone(),
        predicate_text: predicate,
        count: count.clone(),
        representative_ids,
    });
    let snapshot = EvidenceSnapshot::ExplorerGroup(GroupSnapshot {
        field: req.field.clone(),
        value_json: req.value_json.clone(),
        count,
        share_bp,
        rows: snapshot_rows,
        rows_truncated: page.has_more,
    });
    store_pin(
        ws,
        &req.common,
        &investigation,
        EvidenceKind::ExplorerGroup,
        SignalKind::Log,
        reference,
        snapshot,
    )
}

/// Pins one histogram interval.
pub fn pin_interval(
    ws: &Workspace,
    engine: &EngineConnection,
    req: &PinIntervalRequest,
) -> Result<EvidenceRow, JobError> {
    let investigation = active_investigation(ws, &req.common.investigation_id)?;
    if req.end <= req.start {
        return Err(JobError::new(
            "case/invalid",
            "interval must be half-open with end > start",
        ));
    }
    let base = prepare_scope(ws, &req.scope)?;
    let interval_window = ResolvedWindow {
        strategy: TimeStrategy::Absolute {
            start: req.start,
            end: req.end,
        },
        start: Some(req.start),
        end: Some(req.end),
        empty_anchor: false,
    };

    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let counts = query_counts(
        engine,
        &base.files,
        &base.filter,
        &interval_window,
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    let page = query_page(
        engine,
        &base.files,
        &base.filter,
        &interval_window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: QUERY_SNAPSHOT_ROWS as u32,
        },
        &cancel,
        None,
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    let representative_ids: Vec<String> = page
        .rows
        .iter()
        .take(MAX_REPRESENTATIVE_IDS)
        .map(|r| r.record_id.clone())
        .collect();
    let snapshot_rows: Vec<SnapshotRow> = page.rows.iter().map(|r| snapshot_row(r, &[])).collect();

    let mut neighbors = req.neighbor_buckets.clone();
    neighbors.truncate(MAX_NEIGHBOR_BUCKETS);

    let count = CountState::Exact {
        count: counts.matching,
    };
    let reference = EvidenceReference::HistogramInterval(IntervalRef {
        context: base.context,
        datasets: base.dataset_refs,
        start: req.start,
        end: req.end,
        bucket_width_nanos: req.bucket_width_nanos,
        display_timezone: req.display_timezone.clone(),
        count: count.clone(),
        representative_ids,
    });
    let snapshot = EvidenceSnapshot::HistogramInterval(IntervalSnapshot {
        count,
        neighbor_buckets: neighbors,
        rows: snapshot_rows,
        rows_truncated: page.has_more,
    });
    store_pin(
        ws,
        &req.common,
        &investigation,
        EvidenceKind::HistogramInterval,
        SignalKind::Log,
        reference,
        snapshot,
    )
}

/// Pins a manual note/finding/task/question item as evidence.
pub fn pin_item(ws: &Workspace, req: &PinItemRequest) -> Result<EvidenceRow, JobError> {
    let investigation = active_investigation(ws, &req.common.investigation_id)?;
    let item = ws
        .meta
        .list_items(&req.common.investigation_id, true)
        .map_err(ws_err)?
        .into_iter()
        .find(|i| i.item_id == req.item_id)
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!(
                    "item {} is not part of investigation {}",
                    req.item_id, req.common.investigation_id
                ),
            )
        })?;
    let (content, content_truncated) = envelope::bound_field(&item.content);
    let reference = EvidenceReference::ItemRef(ItemReference {
        item_id: item.item_id.clone(),
        item_revision: item.revision,
    });
    let snapshot = EvidenceSnapshot::ItemRef(ItemSnapshot {
        item_kind: item.kind.clone(),
        content,
        content_truncated,
    });
    store_pin(
        ws,
        &req.common,
        &investigation,
        EvidenceKind::ItemRef,
        SignalKind::Manual,
        reference,
        snapshot,
    )
}

// ---- batched, cancellable verification ----------------------------------------

/// Per-evidence verification outcome.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceOutcome {
    pub evidence_id: String,
    pub state: String,
}

/// Result of one (batch) verification run.
#[derive(Debug, Clone, Serialize)]
pub struct VerificationReport {
    /// Evidence items considered by this run.
    pub total: usize,
    /// Items whose resolver columns were written.
    pub updated: usize,
    pub cancelled: bool,
    /// Canonical id-set lookups issued (one per dataset — proof there is
    /// no per-evidence query).
    pub dataset_lookups: usize,
    pub states: BTreeMap<String, i64>,
    pub outcomes: Vec<EvidenceOutcome>,
    pub duration_ms: u64,
}

/// Everything known about one dataset during a verification run.
struct DatasetFact {
    exists: bool,
    current_dsrev: Option<String>,
    files: Vec<PathBuf>,
    parser_version: Option<String>,
    profile_version: Option<String>,
}

/// Lazily probed source-file state, cached per file id for the run.
struct SourceProbe {
    exists: bool,
    size_matches: bool,
    /// Present only when the size fast-path passed and hashing ran.
    current_hash: Option<String>,
}

/// Decoded verification work for one evidence row.
enum Pending {
    /// State decided during decode (version/corruption cases).
    Final(ResolverState, serde_json::Value),
    Event(EventRef),
    Selection(SelectionRef),
    Query(QueryRef),
    Group(GroupRef),
    Interval(IntervalRef),
    Item(ItemReference),
}

/// Verifies evidence for one investigation: batched canonical id lookups
/// (one per dataset), cached source-fingerprint checks (BLAKE3 with a size
/// fast-path), and query/group/interval re-validation through the
/// authoritative query service. Writes only the resolver columns; snapshots
/// and references are never touched. Cancellation between items leaves
/// already-verified items at their fresh state and unreached items exactly
/// as they were.
pub fn verify_evidence(
    ws: &Workspace,
    engine: &EngineConnection,
    investigation_id: &str,
    only: Option<&[String]>,
    ctx: &JobContext,
) -> Result<VerificationReport, JobError> {
    let started = Instant::now();
    let investigation = active_investigation(ws, investigation_id)?;

    let mut rows = ws
        .meta
        .list_evidence(investigation_id, true)
        .map_err(ws_err)?;
    if let Some(ids) = only {
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        rows.retain(|r| wanted.contains(r.evidence_id.as_str()));
        if rows.len() != wanted.len() {
            let have: HashSet<&str> = rows.iter().map(|r| r.evidence_id.as_str()).collect();
            let missing: Vec<&&str> = wanted.iter().filter(|id| !have.contains(**id)).collect();
            return Err(JobError::new(
                "workspace/missing-entity",
                format!("evidence not in this investigation: {missing:?}"),
            ));
        }
    } else {
        rows.retain(|r| !r.archived);
    }

    // Phase 1: decode every reference (version-gated).
    let mut pending: Vec<(EvidenceRow, Pending)> = Vec::with_capacity(rows.len());
    for row in rows {
        let work = match envelope::decode_reference(row.envelope_version, &row.reference_json) {
            DecodeOutcome::UnsupportedVersion { stored, supported } => Pending::Final(
                ResolverState::UnsupportedReferenceVersion,
                serde_json::json!({
                    "cause": "envelope_version",
                    "stored": stored,
                    "supported": supported,
                }),
            ),
            DecodeOutcome::Undecodable { error } => {
                let snapshot_readable = matches!(
                    envelope::decode_snapshot(row.envelope_version, &row.snapshot_json),
                    DecodeOutcome::Decoded(_)
                );
                if snapshot_readable {
                    // The reference cannot be interpreted safely, but the
                    // captured snapshot keeps the evidence readable.
                    Pending::Final(
                        ResolverState::UnsupportedReferenceVersion,
                        serde_json::json!({
                            "cause": "undecodable",
                            "error": error,
                            "snapshot_readable": true,
                        }),
                    )
                } else {
                    Pending::Final(
                        ResolverState::Broken,
                        serde_json::json!({
                            "cause": "undecodable",
                            "error": error,
                            "snapshot_readable": false,
                        }),
                    )
                }
            }
            DecodeOutcome::Decoded(reference) => match reference {
                EvidenceReference::Event(e) => Pending::Event(e),
                EvidenceReference::Selection(s) => Pending::Selection(s),
                EvidenceReference::Query(q) => Pending::Query(q),
                EvidenceReference::ExplorerGroup(g) => Pending::Group(g),
                EvidenceReference::HistogramInterval(i) => Pending::Interval(i),
                EvidenceReference::ItemRef(i) => Pending::Item(i),
            },
        };
        pending.push((row, work));
    }

    // Phase 2: dataset facts for every referenced dataset.
    let datasets: HashMap<String, DatasetRow> = ws
        .meta
        .list_datasets()
        .map_err(ws_err)?
        .into_iter()
        .filter(|d| d.signal == "logs" && d.status == "published")
        .map(|d| (d.dataset_id.clone(), d))
        .collect();
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut wanted_ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (_, work) in &pending {
        match work {
            Pending::Event(e) => {
                referenced.insert(e.dataset_id.clone());
                wanted_ids
                    .entry(e.dataset_id.clone())
                    .or_default()
                    .insert(e.record_id.clone());
            }
            Pending::Selection(s) => {
                for d in &s.datasets {
                    referenced.insert(d.dataset_id.clone());
                    wanted_ids
                        .entry(d.dataset_id.clone())
                        .or_default()
                        .extend(s.record_ids.iter().cloned());
                }
            }
            Pending::Query(q) => referenced.extend(q.context.dataset_ids.iter().cloned()),
            Pending::Group(g) => referenced.extend(g.context.dataset_ids.iter().cloned()),
            Pending::Interval(i) => referenced.extend(i.context.dataset_ids.iter().cloned()),
            Pending::Final(..) | Pending::Item(_) => {}
        }
    }
    let mut facts: BTreeMap<String, DatasetFact> = BTreeMap::new();
    for id in &referenced {
        let fact = match datasets.get(id) {
            None => DatasetFact {
                exists: false,
                current_dsrev: None,
                files: vec![],
                parser_version: None,
                profile_version: None,
            },
            Some(row) => {
                let segments = ws.meta.segments_for_dataset(id).map_err(ws_err)?;
                let current_dsrev = if segments.is_empty() {
                    None
                } else {
                    Some(dataset_revision(ws, id).map_err(ws_err)?)
                };
                DatasetFact {
                    exists: true,
                    current_dsrev,
                    files: ws.segment_paths(id).map_err(ws_err)?,
                    parser_version: row.parser_version.clone(),
                    profile_version: row.profile_version.clone(),
                }
            }
        };
        facts.insert(id.clone(), fact);
    }

    // Phase 3: one canonical id-set lookup per dataset (never per item).
    let mut found: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    let mut dataset_lookups = 0usize;
    let mut cancelled = false;
    for (dataset_id, ids) in &wanted_ids {
        if ctx.control.is_cancel_requested() {
            cancelled = true;
            break;
        }
        let fact = &facts[dataset_id];
        if !fact.exists || fact.files.is_empty() {
            found.insert(dataset_id.clone(), HashSet::new());
            continue;
        }
        let id_vec: Vec<String> = ids.iter().cloned().collect();
        let set = lookup_existing_ids(engine, &fact.files, &id_vec)?;
        dataset_lookups += 1;
        found.insert(dataset_id.clone(), set);
    }

    // Items and saved searches, loaded once when referenced.
    let needs_items = pending.iter().any(|(_, w)| matches!(w, Pending::Item(_)));
    let items: HashMap<String, ItemRow> = if needs_items {
        ws.meta
            .list_items(investigation_id, true)
            .map_err(ws_err)?
            .into_iter()
            .map(|i| (i.item_id.clone(), i))
            .collect()
    } else {
        HashMap::new()
    };
    let needs_saved = pending
        .iter()
        .any(|(_, w)| matches!(w, Pending::Query(q) if q.saved_search_id.is_some()));
    let saved: HashMap<String, String> = if needs_saved {
        ws.meta
            .list_saved_searches()
            .map_err(ws_err)?
            .into_iter()
            .map(|s| (s.saved_search_id, s.fingerprint))
            .collect()
    } else {
        HashMap::new()
    };

    // Phase 4: per-item resolution; query-shaped evidence re-validates and
    // re-runs through the authoritative service (one bounded count each).
    let mut probes = probe_cache(ws);
    let mut outcomes: Vec<EvidenceOutcome> = Vec::with_capacity(pending.len());
    let mut states: BTreeMap<String, i64> = BTreeMap::new();
    let mut updated = 0usize;
    let total = pending.len();
    for (row, work) in pending {
        if cancelled || ctx.control.is_cancel_requested() {
            cancelled = true;
            break;
        }
        let (state, detail) = match work {
            Pending::Final(state, detail) => (state, detail),
            Pending::Event(e) => resolve_event(&facts, &found, &mut probes, &e),
            Pending::Selection(s) => resolve_selection(&facts, &found, &s),
            Pending::Query(q) => {
                resolve_query_like(ws, engine, &facts, QueryLike::Query(&q), &saved)
            }
            Pending::Group(g) => {
                resolve_query_like(ws, engine, &facts, QueryLike::Group(&g), &saved)
            }
            Pending::Interval(i) => {
                resolve_query_like(ws, engine, &facts, QueryLike::Interval(&i), &saved)
            }
            Pending::Item(i) => resolve_item(&items, &i),
        };
        ws.meta
            .update_evidence_resolution(
                &row.evidence_id,
                state.as_str(),
                &detail.to_string(),
                &now_rfc3339(),
            )
            .map_err(ws_err)?;
        updated += 1;
        *states.entry(state.as_str().to_string()).or_insert(0) += 1;
        outcomes.push(EvidenceOutcome {
            evidence_id: row.evidence_id,
            state: state.as_str().to_string(),
        });
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let summary = serde_json::json!({
        "total": total,
        "updated": updated,
        "cancelled": cancelled,
        "duration_ms": duration_ms,
        "states": states,
    });
    ws.meta
        .record_verification_run(&investigation.investigation_id, &summary.to_string())
        .map_err(ws_err)?;

    Ok(VerificationReport {
        total,
        updated,
        cancelled,
        dataset_lookups,
        states,
        outcomes,
        duration_ms,
    })
}

fn resolve_event(
    facts: &BTreeMap<String, DatasetFact>,
    found: &BTreeMap<String, HashSet<String>>,
    probes: &mut ProbeCache<'_>,
    e: &EventRef,
) -> (ResolverState, serde_json::Value) {
    let Some(fact) = facts.get(&e.dataset_id) else {
        return (
            ResolverState::DatasetRevisionUnavailable,
            serde_json::json!({ "dataset_id": e.dataset_id, "dataset_present": false }),
        );
    };
    if !fact.exists || fact.current_dsrev.is_none() {
        return (
            ResolverState::DatasetRevisionUnavailable,
            serde_json::json!({
                "dataset_id": e.dataset_id,
                "dataset_present": fact.exists,
                "captured_dataset_revision": e.dataset_revision,
            }),
        );
    }
    let current_dsrev = fact.current_dsrev.as_deref().unwrap_or_default();
    let record_found = found
        .get(&e.dataset_id)
        .is_some_and(|s| s.contains(&e.record_id));
    if !record_found {
        return (
            ResolverState::DatasetRevisionUnavailable,
            serde_json::json!({
                "record_found": false,
                "captured_dataset_revision": e.dataset_revision,
                "current_dataset_revision": current_dsrev,
            }),
        );
    }

    let mut secondary = serde_json::Map::new();
    if current_dsrev != e.dataset_revision {
        secondary.insert(
            "dataset_revision_advanced".into(),
            serde_json::json!(current_dsrev),
        );
    }
    if let Some(current) = &fact.parser_version {
        if *current != e.parser_version {
            secondary.insert("parser_version_current".into(), serde_json::json!(current));
        }
    }
    if let (Some(captured), Some(current)) = (&e.profile_version, &fact.profile_version) {
        if captured != current {
            secondary.insert("profile_version_current".into(), serde_json::json!(current));
        }
    }

    let (state, mut detail) = match (&e.source_file_id, &e.source_content_hash) {
        (None, _) => (
            ResolverState::CanonicalAvailableSourceUnavailable,
            serde_json::json!({ "cause": "no_source_reference" }),
        ),
        (Some(file_id), captured_hash) => {
            let probe = probes.probe(file_id);
            match probe {
                None => (
                    ResolverState::CanonicalAvailableSourceUnavailable,
                    serde_json::json!({ "cause": "source_registration_missing", "file_id": file_id }),
                ),
                Some(p) if !p.exists => (
                    ResolverState::SourceMissing,
                    serde_json::json!({ "file_id": file_id }),
                ),
                Some(p) if !p.size_matches => (
                    ResolverState::SourceChanged,
                    serde_json::json!({ "cause": "size", "file_id": file_id }),
                ),
                Some(p) => match (captured_hash, &p.current_hash) {
                    (None, _) => (
                        ResolverState::CanonicalAvailableSourceUnavailable,
                        serde_json::json!({ "cause": "no_captured_hash", "file_id": file_id }),
                    ),
                    (Some(expected), Some(current)) if expected != current => (
                        ResolverState::SourceChanged,
                        serde_json::json!({ "cause": "content_hash", "file_id": file_id }),
                    ),
                    (Some(_), None) => (
                        ResolverState::CanonicalAvailableSourceUnavailable,
                        serde_json::json!({ "cause": "hash_unavailable", "file_id": file_id }),
                    ),
                    (Some(_), Some(_)) => (ResolverState::Verified, serde_json::json!({})),
                },
            }
        }
    };
    if !secondary.is_empty() {
        if let serde_json::Value::Object(map) = &mut detail {
            map.insert("secondary".into(), serde_json::Value::Object(secondary));
        }
    }
    (state, detail)
}

fn resolve_selection(
    facts: &BTreeMap<String, DatasetFact>,
    found: &BTreeMap<String, HashSet<String>>,
    s: &SelectionRef,
) -> (ResolverState, serde_json::Value) {
    let mut missing_datasets: Vec<&str> = Vec::new();
    for d in &s.datasets {
        let ok = facts
            .get(&d.dataset_id)
            .map(|f| f.exists && f.current_dsrev.is_some())
            .unwrap_or(false);
        if !ok {
            missing_datasets.push(&d.dataset_id);
        }
    }
    if !missing_datasets.is_empty() {
        return (
            ResolverState::DatasetRevisionUnavailable,
            serde_json::json!({ "missing_datasets": missing_datasets }),
        );
    }
    let mut missing: Vec<&str> = Vec::new();
    for id in &s.record_ids {
        let present = s
            .datasets
            .iter()
            .any(|d| found.get(&d.dataset_id).is_some_and(|set| set.contains(id)));
        if !present {
            missing.push(id);
        }
    }
    let mut secondary = serde_json::Map::new();
    let advanced: Vec<&str> = s
        .datasets
        .iter()
        .filter(|d| {
            facts
                .get(&d.dataset_id)
                .and_then(|f| f.current_dsrev.as_deref())
                .is_some_and(|c| c != d.dataset_revision)
        })
        .map(|d| d.dataset_id.as_str())
        .collect();
    if !advanced.is_empty() {
        secondary.insert(
            "dataset_revision_advanced".into(),
            serde_json::json!(advanced),
        );
    }
    if missing.is_empty() {
        let mut detail = serde_json::json!({ "resolved_count": s.record_ids.len() });
        if !secondary.is_empty() {
            detail["secondary"] = serde_json::Value::Object(secondary);
        }
        (ResolverState::Verified, detail)
    } else {
        let sample: Vec<&&str> = missing.iter().take(20).collect();
        let mut detail = serde_json::json!({
            "resolved_count": s.record_ids.len() - missing.len(),
            "missing_count": missing.len(),
            "missing_sample": sample,
        });
        if !secondary.is_empty() {
            detail["secondary"] = serde_json::Value::Object(secondary);
        }
        (ResolverState::PartiallyResolved, detail)
    }
}

enum QueryLike<'a> {
    Query(&'a QueryRef),
    Group(&'a GroupRef),
    Interval(&'a IntervalRef),
}

impl QueryLike<'_> {
    fn context(&self) -> &QueryContext {
        match self {
            QueryLike::Query(q) => &q.context,
            QueryLike::Group(g) => &g.context,
            QueryLike::Interval(i) => &i.context,
        }
    }
    fn datasets(&self) -> &[DatasetRevRef] {
        match self {
            QueryLike::Query(q) => &q.datasets,
            QueryLike::Group(g) => &g.datasets,
            QueryLike::Interval(i) => &i.datasets,
        }
    }
    fn captured_count(&self) -> &CountState {
        match self {
            QueryLike::Query(q) => &q.count,
            QueryLike::Group(g) => &g.count,
            QueryLike::Interval(i) => &i.count,
        }
    }
}

fn resolve_query_like(
    ws: &Workspace,
    engine: &EngineConnection,
    facts: &BTreeMap<String, DatasetFact>,
    like: QueryLike<'_>,
    saved: &HashMap<String, String>,
) -> (ResolverState, serde_json::Value) {
    let ctx = like.context();
    if ctx.language_version > LANGUAGE_VERSION as i64 {
        return (
            ResolverState::UnsupportedReferenceVersion,
            serde_json::json!({
                "cause": "language_version",
                "stored": ctx.language_version,
                "supported": LANGUAGE_VERSION,
            }),
        );
    }
    let missing: Vec<&str> = like
        .datasets()
        .iter()
        .filter(|d| {
            !facts
                .get(&d.dataset_id)
                .map(|f| f.exists && f.current_dsrev.is_some())
                .unwrap_or(false)
        })
        .map(|d| d.dataset_id.as_str())
        .collect();
    if !missing.is_empty() {
        return (
            ResolverState::DatasetRevisionUnavailable,
            serde_json::json!({ "missing_datasets": missing }),
        );
    }

    // Re-validate through the authoritative pipeline. Group evidence
    // recomposes the effective drill-down exactly as pinning did.
    let effective_text = match &like {
        QueryLike::Group(g) => compose_group_query(&ctx.query_text, &g.predicate_text),
        _ => ctx.query_text.clone(),
    };
    let analysis = explorer::analyze_query(ws, &ctx.dataset_ids, &effective_text);
    let Some(resolved) = analysis.resolved.as_ref() else {
        let messages: Vec<String> = analysis
            .diagnostics
            .iter()
            .take(3)
            .map(|d| d.message.clone())
            .collect();
        return (
            ResolverState::QueryDrift,
            serde_json::json!({ "validates": false, "diagnostics": messages }),
        );
    };
    // Fingerprint drift: for plain query/interval evidence the captured
    // fingerprint is over the same text that was just re-analyzed; for
    // group evidence it is over the base text, so re-analyze the base.
    let current_fingerprint = match &like {
        QueryLike::Group(_) => {
            let base = explorer::analyze_query(ws, &ctx.dataset_ids, &ctx.query_text);
            base.resolved.as_ref().map(|r| r.fingerprint.clone())
        }
        _ => Some(resolved.fingerprint.clone()),
    };
    if let (Some(captured), Some(current)) = (&ctx.fingerprint, &current_fingerprint) {
        if captured != current {
            return (
                ResolverState::QueryDrift,
                serde_json::json!({
                    "cause": "resolution_changed",
                    "captured_fingerprint": captured,
                    "current_fingerprint": current,
                }),
            );
        }
    }

    let filter = match explorer::compile_for_execution(ws, &ctx.dataset_ids, &analysis) {
        Ok(f) => f,
        Err(e) => {
            return (
                ResolverState::QueryDrift,
                serde_json::json!({ "validates": false, "error": e.message }),
            )
        }
    };
    let strategy: TimeStrategy =
        serde_json::from_str(&ctx.time_strategy_json).unwrap_or(TimeStrategy::All);
    let window = match &like {
        QueryLike::Interval(i) => ResolvedWindow {
            strategy: TimeStrategy::Absolute {
                start: i.start,
                end: i.end,
            },
            start: Some(i.start),
            end: Some(i.end),
            empty_anchor: false,
        },
        _ => ResolvedWindow {
            strategy,
            start: ctx.resolved_start,
            end: ctx.resolved_end,
            empty_anchor: false,
        },
    };
    let mut files: Vec<PathBuf> = Vec::new();
    for d in like.datasets() {
        files.extend(facts[&d.dataset_id].files.iter().cloned());
    }
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let counts = match query_counts(engine, &files, &filter, &window, &cancel, None) {
        Ok(c) => c,
        Err(e) => {
            return (
                ResolverState::QueryDrift,
                serde_json::json!({ "runs": false, "error": e.to_string() }),
            )
        }
    };

    let mut secondary = serde_json::Map::new();
    let advanced: Vec<&str> = like
        .datasets()
        .iter()
        .filter(|d| {
            facts
                .get(&d.dataset_id)
                .and_then(|f| f.current_dsrev.as_deref())
                .is_some_and(|c| c != d.dataset_revision)
        })
        .map(|d| d.dataset_id.as_str())
        .collect();
    if !advanced.is_empty() {
        secondary.insert(
            "dataset_revision_advanced".into(),
            serde_json::json!(advanced),
        );
    }
    if let QueryLike::Query(q) = &like {
        if let Some(id) = &q.saved_search_id {
            match saved.get(id) {
                None => {
                    secondary.insert("saved_search".into(), serde_json::json!("missing"));
                }
                Some(fp) => {
                    if Some(fp) != q.saved_search_fingerprint.as_ref() {
                        secondary.insert("saved_search".into(), serde_json::json!("changed"));
                    }
                }
            }
        }
    }

    match like.captured_count() {
        CountState::Exact { count } => {
            if counts.matching != *count {
                return (
                    ResolverState::QueryDrift,
                    serde_json::json!({
                        "cause": "count",
                        "expected": count,
                        "actual": counts.matching,
                    }),
                );
            }
        }
        _ => {
            secondary.insert("count_unchecked".into(), serde_json::json!(true));
        }
    }
    if let Some(expected_omitted) = ctx.omitted_untimestamped {
        if !matches!(like, QueryLike::Interval(_))
            && counts.omitted_untimestamped != expected_omitted
        {
            return (
                ResolverState::QueryDrift,
                serde_json::json!({
                    "cause": "omitted_untimestamped",
                    "expected": expected_omitted,
                    "actual": counts.omitted_untimestamped,
                }),
            );
        }
    }

    let mut detail = serde_json::json!({});
    if !secondary.is_empty() {
        detail["secondary"] = serde_json::Value::Object(secondary);
    }
    (ResolverState::Verified, detail)
}

fn resolve_item(
    items: &HashMap<String, ItemRow>,
    reference: &ItemReference,
) -> (ResolverState, serde_json::Value) {
    match items.get(&reference.item_id) {
        None => (
            ResolverState::SourceMissing,
            serde_json::json!({ "entity": "item", "item_id": reference.item_id }),
        ),
        Some(item) if item.revision != reference.item_revision => (
            ResolverState::SourceChanged,
            serde_json::json!({
                "cause": "item_revision",
                "captured_revision": reference.item_revision,
                "current_revision": item.revision,
            }),
        ),
        Some(item) => {
            let mut detail = serde_json::json!({});
            if item.archived {
                detail["secondary"] = serde_json::json!({ "item_archived": true });
            }
            (ResolverState::Verified, detail)
        }
    }
}

/// Per-run cache of source-file probes so a file shared by many evidence
/// items is stat'ed and hashed at most once.
struct ProbeCache<'a> {
    ws: &'a Workspace,
    cache: HashMap<String, Option<SourceProbe>>,
}

impl<'a> ProbeCache<'a> {
    fn probe(&mut self, file_id: &str) -> Option<&SourceProbe> {
        if !self.cache.contains_key(file_id) {
            let value = build_probe(self.ws, file_id);
            self.cache.insert(file_id.to_string(), value);
        }
        self.cache.get(file_id).and_then(|p| p.as_ref())
    }
}

fn probe_cache(ws: &Workspace) -> ProbeCache<'_> {
    ProbeCache {
        ws,
        cache: HashMap::new(),
    }
}

fn build_probe(ws: &Workspace, file_id: &str) -> Option<SourceProbe> {
    let row = ws.meta.get_source_file(file_id).ok().flatten()?;
    let path = std::path::Path::new(&row.path);
    let Ok(meta) = std::fs::metadata(path) else {
        return Some(SourceProbe {
            exists: false,
            size_matches: false,
            current_hash: None,
        });
    };
    let size_matches = meta.len() as i64 == row.size_bytes;
    // Full-file BLAKE3 only behind the size fast-path: a size mismatch is
    // already a definitive change.
    let current_hash = if size_matches {
        logscope_ingest::fingerprint_file(path)
            .ok()
            .map(|f| f.content_hash)
    } else {
        None
    };
    Some(SourceProbe {
        exists: true,
        size_matches,
        current_hash,
    })
}
