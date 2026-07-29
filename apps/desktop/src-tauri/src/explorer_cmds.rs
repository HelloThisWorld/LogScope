//! v0.2 Explorer commands: a thin typed layer over `logscope_app` services.
//! No query semantics live here — analysis, compilation, and execution all
//! run through the shared application services.

use std::path::PathBuf;
use std::time::Instant;

use logscope_app::dto::*;
use logscope_app::explorer;
use logscope_app::{run_export, ExportFormat, ExportSpec};
use logscope_jobs::JobEvent;
use logscope_query::{
    fetch_record, query_counts, query_facets, query_field_summary, query_histogram, query_page,
    query_source_context, resolve_window, CompiledFilter, FieldTarget, LogRow, PageRequest,
    QueryCancelHandle, ResolvedWindow, TimeStrategy,
};
use logscope_query_lang::{
    builtin_field, format_predicate, format_value, Analysis, AttrType, DiagSeverity,
    FieldResolution, LANGUAGE_VERSION,
};
use logscope_workspace::Workspace;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::{AppState, CmdResult};

fn err(code: &str, msg: impl std::fmt::Display) -> ErrorDto {
    ErrorDto::new(code, msg)
}

fn ws_handle(state: &AppState) -> CmdResult<std::sync::Arc<Workspace>> {
    state
        .workspace
        .lock()
        .as_ref()
        .cloned()
        .ok_or_else(|| err("workspace/none", "no workspace is open"))
}

fn span_dto(s: logscope_query_lang::Span) -> SpanDto {
    SpanDto {
        start: s.start,
        end: s.end,
        start_utf16: s.start_utf16,
        end_utf16: s.end_utf16,
    }
}

fn diag_dto(d: &logscope_query_lang::Diagnostic) -> DiagnosticDto {
    DiagnosticDto {
        code: d.code.clone(),
        severity: match d.severity {
            DiagSeverity::Error => "error".into(),
            DiagSeverity::Warning => "warning".into(),
        },
        message: d.message.clone(),
        span: span_dto(d.span),
        expected: d.expected.clone(),
        hint: d.hint.clone(),
    }
}

fn strategy_from_dto(dto: &TimeStrategyDto) -> CmdResult<TimeStrategy> {
    match dto.kind.as_str() {
        "all" => Ok(TimeStrategy::All),
        "absolute" => match (dto.start, dto.end) {
            (Some(start), Some(end)) if end > start => Ok(TimeStrategy::Absolute { start, end }),
            _ => Err(err(
                "query/invalid-time-strategy",
                "absolute strategy needs start < end",
            )),
        },
        "relative_to_latest" => match dto.duration_nanos {
            Some(d) if d > 0 => Ok(TimeStrategy::RelativeToLatest { duration_nanos: d }),
            _ => Err(err(
                "query/invalid-time-strategy",
                "relative strategy needs a positive duration",
            )),
        },
        other => Err(err(
            "query/invalid-time-strategy",
            format!("unknown time strategy kind {other:?}"),
        )),
    }
}

fn strategy_to_json(dto: &TimeStrategyDto) -> String {
    serde_json::json!({
        "kind": dto.kind,
        "start": dto.start,
        "end": dto.end,
        "duration_nanos": dto.duration_nanos,
    })
    .to_string()
}

fn strategy_from_json(json: &str) -> TimeStrategyDto {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    TimeStrategyDto {
        kind: v["kind"].as_str().unwrap_or("all").to_string(),
        start: v["start"].as_i64(),
        end: v["end"].as_i64(),
        duration_nanos: v["duration_nanos"].as_i64(),
    }
}

fn selection_to_json(dataset_ids: &[String]) -> String {
    if dataset_ids.is_empty() {
        r#"{"kind":"all"}"#.to_string()
    } else {
        serde_json::json!({"kind": "explicit", "dataset_ids": dataset_ids}).to_string()
    }
}

fn selection_from_json(json: &str) -> (Vec<String>, bool) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    if v["kind"] == "explicit" {
        let ids = v["dataset_ids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        (ids, false)
    } else {
        (vec![], true)
    }
}

fn window_dto(w: &ResolvedWindow) -> ResolvedWindowDto {
    let text = |n: Option<i64>| n.map(|t| logscope_model::UnixNanos(t).to_rfc3339());
    ResolvedWindowDto {
        start: w.start,
        end: w.end,
        start_text: text(w.start),
        end_text: text(w.end),
        empty_anchor: w.empty_anchor,
    }
}

pub(crate) fn severity_band_name(number: Option<i32>, text: Option<&str>) -> Option<String> {
    match number {
        Some(n @ 1..=24) => Some(
            match (n - 1) / 4 {
                0 => "TRACE",
                1 => "DEBUG",
                2 => "INFO",
                3 => "WARN",
                4 => "ERROR",
                _ => "FATAL",
            }
            .to_string(),
        ),
        _ => text.map(|t| t.to_uppercase()),
    }
}

fn row_dto(r: &LogRow) -> LogRowV2Dto {
    LogRowV2Dto {
        record_id: r.record_id.clone(),
        event_time: r.event_time,
        event_time_text: r
            .event_time
            .map(|t| logscope_model::UnixNanos(t).to_rfc3339()),
        severity: severity_band_name(r.severity_number, r.severity_text.as_deref()),
        severity_text: r.severity_text.clone(),
        severity_number: r.severity_number,
        message: r.display_message.clone(),
        trace_id: r.trace_id.clone(),
        span_id: r.span_id.clone(),
        dataset_id: r.dataset_id.clone(),
        source_id: r.source_id.clone(),
        record_number: r.record_number,
        line_start: r.line_start,
        attributes_json: r.attributes_json.clone(),
    }
}

struct Prepared {
    selection: Vec<String>,
    files: Vec<PathBuf>,
    filter: CompiledFilter,
    window: ResolvedWindow,
    warnings: Vec<DiagnosticDto>,
    analysis: Analysis,
}

fn prepare(
    ws: &Workspace,
    dataset_ids: &[String],
    query_text: &str,
    strategy: &TimeStrategyDto,
) -> CmdResult<Prepared> {
    let selection =
        explorer::resolve_dataset_selection(ws, dataset_ids).map_err(|e| err(e.code(), e))?;
    if selection.is_empty() {
        return Err(err("query/no-datasets", "no published log datasets"));
    }
    let analysis = explorer::analyze_query(ws, &selection, query_text);
    if analysis.resolved.is_none() {
        let first = analysis
            .diagnostics
            .iter()
            .find(|d| d.is_error())
            .map(|d| d.message.clone())
            .unwrap_or_else(|| "invalid query".into());
        return Err(err("query/invalid", first));
    }
    let filter = explorer::compile_for_execution(ws, &selection, &analysis)
        .map_err(|e| err(&e.code, e.message))?;
    let strategy = strategy_from_dto(strategy)?;
    let latest = explorer::latest_event_time(ws, &selection).map_err(|e| err(e.code(), e))?;
    let window = resolve_window(&strategy, latest);
    let files = explorer::segment_files_for(ws, &selection).map_err(|e| err(e.code(), e))?;
    let warnings = analysis
        .diagnostics
        .iter()
        .filter(|d| !d.is_error())
        .map(diag_dto)
        .collect();
    Ok(Prepared {
        selection,
        files,
        filter,
        window,
        warnings,
        analysis,
    })
}

/// Runs `f` on a pooled engine with a registered cancel handle.
fn with_engine<T>(
    state: &AppState,
    request_id: Option<&str>,
    f: impl FnOnce(&logscope_query::EngineConnection, &QueryCancelHandle) -> CmdResult<T>,
) -> CmdResult<T> {
    for slot in &state.engines {
        if let Some(engine) = slot.try_lock() {
            let cancel = QueryCancelHandle::new(engine.interrupt_handle());
            if let Some(id) = request_id {
                state
                    .query_cancels
                    .lock()
                    .insert(id.to_string(), cancel.clone());
            }
            let result = f(&engine, &cancel);
            if let Some(id) = request_id {
                state.query_cancels.lock().remove(id);
            }
            return result;
        }
    }
    Err(err(
        "query/busy",
        "all query slots are busy; cancel a running query or retry",
    ))
}

fn qerr(e: logscope_query::QueryError) -> ErrorDto {
    ErrorDto::new(e.code(), e)
}

// ---- commands -----------------------------------------------------------

#[tauri::command]
pub fn validate_query(
    state: State<'_, AppState>,
    dataset_ids: Vec<String>,
    query_text: String,
) -> CmdResult<QueryAnalysisDto> {
    let ws = ws_handle(&state)?;
    let selection =
        explorer::resolve_dataset_selection(&ws, &dataset_ids).map_err(|e| err(e.code(), e))?;
    let analysis = explorer::analyze_query(&ws, &selection, &query_text);
    let catalog_complete = explorer::load_catalog(&ws, &selection)
        .map(|c| c.complete)
        .unwrap_or(false);
    Ok(QueryAnalysisDto {
        valid: analysis.resolved.is_some(),
        diagnostics: analysis.diagnostics.iter().map(diag_dto).collect(),
        highlights: analysis
            .highlights
            .iter()
            .map(|h| HighlightDto {
                kind: format!("{:?}", h.kind).to_lowercase(),
                span: span_dto(h.span),
            })
            .collect(),
        fingerprint: analysis.resolved.as_ref().map(|r| r.fingerprint.clone()),
        language_version: LANGUAGE_VERSION,
        catalog_complete,
    })
}

#[tauri::command]
pub fn field_catalog(
    state: State<'_, AppState>,
    dataset_ids: Vec<String>,
) -> CmdResult<FieldCatalogDto> {
    let ws = ws_handle(&state)?;
    let selection =
        explorer::resolve_dataset_selection(&ws, &dataset_ids).map_err(|e| err(e.code(), e))?;
    let catalog = explorer::load_catalog(&ws, &selection).map_err(|e| err(e.code(), e))?;
    let fts = explorer::fts_ready(&ws, &selection).map_err(|e| err(e.code(), e))?;
    let mut fields: Vec<FieldInfoDto> = logscope_query_lang::builtin_field_names()
        .iter()
        .filter(|n| !matches!(**n, "observed_timestamp" | "record_id"))
        .map(|n| FieldInfoDto {
            display: n.to_string(),
            origin: "canonical".into(),
            types: vec!["canonical".into()],
            present_count: -1,
            distinct_est: -1,
            distinct_is_exact: false,
            examples: vec![],
            queryable: true,
            facetable: true,
        })
        .collect();
    for (display, types, present, distinct, exact, examples, queryable) in catalog.field_entries() {
        fields.push(FieldInfoDto {
            display,
            origin: "attribute".into(),
            types: types.iter().map(|t| t.describe().to_string()).collect(),
            present_count: present,
            distinct_est: distinct,
            distinct_is_exact: exact,
            examples,
            queryable,
            facetable: types.iter().all(|t| t.is_scalar()),
        });
    }
    Ok(FieldCatalogDto {
        fields,
        complete: catalog.complete,
        fts_ready: fts,
    })
}

#[tauri::command]
pub fn run_query(state: State<'_, AppState>, request: RunQueryDto) -> CmdResult<QueryPageV2Dto> {
    let started = Instant::now();
    let ws = ws_handle(&state)?;
    let prepared = prepare(
        &ws,
        &request.dataset_ids,
        &request.query_text,
        &request.time_strategy,
    )?;
    let (page, counts) = with_engine(&state, Some(&request.request_id), |engine, cancel| {
        let page = query_page(
            engine,
            &prepared.files,
            &prepared.filter,
            &prepared.window,
            &PageRequest {
                cursor: request.cursor.clone(),
                backward: request.backward,
                limit: request.limit,
            },
            cancel,
            None,
        )
        .map_err(qerr)?;
        let counts = query_counts(
            engine,
            &prepared.files,
            &prepared.filter,
            &prepared.window,
            cancel,
            None,
        )
        .map_err(qerr)?;
        Ok((page, counts))
    })?;
    if request.record_recent && request.cursor.is_none() {
        if let Some(resolved) = prepared.analysis.resolved.as_ref() {
            let _ = ws.meta.touch_recent_search(
                &request.query_text,
                LANGUAGE_VERSION as i64,
                &resolved.fingerprint,
                &selection_to_json(&request.dataset_ids),
                &strategy_to_json(&request.time_strategy),
            );
        }
    }
    Ok(QueryPageV2Dto {
        request_id: request.request_id,
        rows: page.rows.iter().map(row_dto).collect(),
        next_cursor: page.next_cursor,
        prev_cursor: page.prev_cursor,
        has_more: page.has_more,
        matching: counts.matching,
        omitted_untimestamped: counts.omitted_untimestamped,
        resolved_window: window_dto(&prepared.window),
        elapsed_ms: started.elapsed().as_millis() as u64,
        used_fts: prepared.filter.used_fts(),
        used_fallback_text_scan: prepared.filter.used_fallback_scan(),
        warnings: prepared.warnings,
    })
}

#[tauri::command]
pub fn run_histogram(
    state: State<'_, AppState>,
    request: HistogramRequestDto,
) -> CmdResult<HistogramDto> {
    let ws = ws_handle(&state)?;
    let prepared = prepare(
        &ws,
        &request.dataset_ids,
        &request.query_text,
        &request.time_strategy,
    )?;
    let histogram = with_engine(&state, Some(&request.request_id), |engine, cancel| {
        query_histogram(
            engine,
            &prepared.files,
            &prepared.filter,
            &prepared.window,
            request.max_bins,
            cancel,
            None,
        )
        .map_err(qerr)
    })?;
    Ok(HistogramDto {
        request_id: request.request_id,
        bins: histogram
            .bins
            .iter()
            .map(|b| HistogramBinDto {
                start: b.start,
                count: b.count,
            })
            .collect(),
        bin_width_nanos: histogram.bin_width_nanos,
        start: histogram.start,
        end: histogram.end,
        total_in_range: histogram.total_in_range,
        untimestamped_count: histogram.untimestamped_count,
        empty: histogram.empty,
        timezone: "UTC".into(),
    })
}

/// Facet/summary field resolution through the catalog only.
fn facet_target(ws: &Workspace, selection: &[String], field: &str) -> Result<FieldTarget, String> {
    if let Some(canon) = builtin_field(field) {
        return Ok(FieldTarget::Canonical { field: canon });
    }
    let catalog = explorer::load_catalog(ws, selection).map_err(|e| e.to_string())?;
    match logscope_query_lang::CatalogView::resolve_attr(&catalog, field) {
        FieldResolution::Attr(info) => {
            if info.types.iter().all(|t| t.is_scalar()) {
                Ok(FieldTarget::Attr { path: info.path })
            } else {
                Err(format!("`{field}` has non-scalar values"))
            }
        }
        FieldResolution::Ambiguous { candidates } => {
            Err(format!("`{field}` is ambiguous: {}", candidates.join(", ")))
        }
        _ => Err(format!("unknown field `{field}`")),
    }
}

#[tauri::command]
pub fn run_facets(
    state: State<'_, AppState>,
    request: FacetsRequestDto,
) -> CmdResult<Vec<FacetDto>> {
    let ws = ws_handle(&state)?;
    let prepared = prepare(
        &ws,
        &request.dataset_ids,
        &request.query_text,
        &request.time_strategy,
    )?;
    let mut targets = Vec::new();
    let mut results: Vec<FacetDto> = Vec::new();
    for field in &request.fields {
        match facet_target(&ws, &prepared.selection, field) {
            Ok(t) => targets.push((field.clone(), t)),
            Err(message) => results.push(FacetDto {
                display: field.clone(),
                values: vec![],
                missing_count: 0,
                truncated: false,
                error: Some(message),
            }),
        }
    }
    let computed = with_engine(&state, Some(&request.request_id), |engine, cancel| {
        query_facets(
            engine,
            &prepared.files,
            &prepared.filter,
            &prepared.window,
            &targets,
            request.top_k,
            cancel,
            None,
        )
        .map_err(qerr)
    })?;
    for f in computed {
        results.push(FacetDto {
            display: f.display,
            values: f
                .values
                .into_iter()
                .map(|v| FacetValueDto {
                    value: v.value,
                    count: v.count,
                })
                .collect(),
            missing_count: f.missing_count,
            truncated: f.truncated,
            error: None,
        });
    }
    Ok(results)
}

#[tauri::command]
pub fn field_summary(
    state: State<'_, AppState>,
    request: FieldSummaryRequestDto,
) -> CmdResult<FieldSummaryDto> {
    let ws = ws_handle(&state)?;
    let prepared = prepare(
        &ws,
        &request.dataset_ids,
        &request.query_text,
        &request.time_strategy,
    )?;
    let target = facet_target(&ws, &prepared.selection, &request.field)
        .map_err(|m| err("query/unknown-field", m))?;
    let types: Vec<AttrType> = match &target {
        FieldTarget::Attr { .. } => explorer::load_catalog(&ws, &prepared.selection)
            .ok()
            .and_then(|c| {
                match logscope_query_lang::CatalogView::resolve_attr(&c, &request.field) {
                    FieldResolution::Attr(info) => Some(info.types),
                    _ => None,
                }
            })
            .unwrap_or_default(),
        FieldTarget::Canonical { .. } => vec![],
    };
    let numeric = matches!(&target, FieldTarget::Attr { .. })
        && types
            .iter()
            .all(|t| matches!(t, AttrType::Int | AttrType::Double));
    let summary = with_engine(&state, Some(&request.request_id), |engine, cancel| {
        query_field_summary(
            engine,
            &prepared.files,
            &prepared.filter,
            &prepared.window,
            &request.field,
            &target,
            numeric,
            types.clone(),
            cancel,
            None,
        )
        .map_err(qerr)
    })?;
    Ok(FieldSummaryDto {
        display: summary.display,
        present_count: summary.present_count,
        missing_count: summary.missing_count,
        distinct_count: summary.distinct_count,
        distinct_is_exact: summary.distinct_is_exact,
        top_values: summary
            .top_values
            .into_iter()
            .map(|v| FacetValueDto {
                value: v.value,
                count: v.count,
            })
            .collect(),
        min_numeric: summary.min_numeric,
        max_numeric: summary.max_numeric,
        high_cardinality: summary.high_cardinality,
        types: summary.types.iter().map(|t| t.describe().into()).collect(),
    })
}

#[tauri::command]
pub fn cancel_query(state: State<'_, AppState>, request_id: String) -> bool {
    if let Some(handle) = state.query_cancels.lock().get(&request_id) {
        handle.cancel();
        true
    } else {
        false
    }
}

#[tauri::command]
pub fn get_record(
    state: State<'_, AppState>,
    dataset_id: String,
    record_id: String,
) -> CmdResult<RecordDetailDto> {
    let ws = ws_handle(&state)?;
    let files = explorer::segment_files_for(&ws, std::slice::from_ref(&dataset_id))
        .map_err(|e| err(e.code(), e))?;
    let detail = with_engine(&state, None, |engine, cancel| {
        logscope_query::fetch_record_detail(engine, &files, &dataset_id, &record_id, cancel, None)
            .map_err(qerr)
    })?
    .ok_or_else(|| err("query/record-not-found", "record not found"))?;
    let row = &detail.row;

    let prov: serde_json::Value =
        serde_json::from_str(&row.provenance_json).unwrap_or(serde_json::Value::Null);
    let dataset = ws
        .meta
        .get_dataset(&dataset_id)
        .map_err(|e| err(e.code(), e))?;
    // Timestamp quality from provenance flags plus structural facts.
    let mut quality: Vec<String> = prov["flags"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["flag"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if row.event_time.is_none() && !quality.iter().any(|q| q.starts_with("timestamp")) {
        quality.push("timestamp_missing".into());
    }
    if detail.timezone_assumption_json.is_some() && !quality.contains(&"timezone_assumed".into()) {
        quality.push("timezone_assumed".into());
    }

    let resource_json = ws
        .meta
        .get_resource_json(&row.resource_id)
        .map_err(|e| err(e.code(), e))?;
    let scope_json = ws
        .meta
        .get_scope_json(&detail.scope_id)
        .map_err(|e| err(e.code(), e))?;

    Ok(RecordDetailDto {
        body_json: detail.body_json.clone(),
        event_name: detail.event_name.clone(),
        resource_json,
        scope_json,
        provenance_json: row.provenance_json.clone(),
        timestamp_quality: quality,
        original_timestamp_text: detail.original_timestamp_text.clone(),
        profile_id: dataset.as_ref().and_then(|d| d.profile_id.clone()),
        profile_version: dataset.as_ref().and_then(|d| d.profile_version.clone()),
        parser_id: dataset.as_ref().and_then(|d| d.parser_id.clone()),
        parser_version: dataset.as_ref().and_then(|d| d.parser_version.clone()),
        normalizer_version: dataset.as_ref().and_then(|d| d.normalizer_version.clone()),
        row: row_dto(row),
    })
}

#[tauri::command]
pub fn source_context(
    state: State<'_, AppState>,
    request: SourceContextRequestDto,
) -> CmdResult<SourceContextDto> {
    let ws = ws_handle(&state)?;
    let files = explorer::segment_files_for(&ws, std::slice::from_ref(&request.dataset_id))
        .map_err(|e| err(e.code(), e))?;
    let anchor = with_engine(&state, None, |engine, cancel| {
        fetch_record(
            engine,
            &files,
            &request.dataset_id,
            &request.record_id,
            cancel,
            None,
        )
        .map_err(qerr)
    })?
    .ok_or_else(|| err("query/record-not-found", "record not found"))?;

    let prov: serde_json::Value =
        serde_json::from_str(&anchor.provenance_json).unwrap_or(serde_json::Value::Null);
    let origin_id = prov["origin"]["file_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let record_number = anchor.record_number.unwrap_or(0);
    let context = with_engine(&state, None, |engine, cancel| {
        query_source_context(
            engine,
            &files,
            &request.dataset_id,
            &origin_id,
            &request.record_id,
            record_number,
            request.before,
            request.after,
            cancel,
            None,
        )
        .map_err(qerr)
    })?;

    let byte_start = prov["locator"]["byte_start"].as_u64();
    let byte_end = prov["locator"]["byte_end"].as_u64();
    let (status, raw_excerpt, source_path) = if origin_id.is_empty() {
        (explorer::SourceStatus::Unsupported, None, None)
    } else {
        explorer::read_raw_excerpt(&ws, &origin_id, byte_start, byte_end)
    };

    Ok(SourceContextDto {
        records: context.records.iter().map(row_dto).collect(),
        anchor_record_id: context.anchor_record_id,
        source_status: status.as_str().to_string(),
        source_path,
        raw_excerpt,
        range_low: context.range_low,
        range_high: context.range_high,
    })
}

/// Authoritative predicate text for facet clicks / detail-panel actions.
#[tauri::command]
pub fn build_predicate(field: String, value: String, negate: bool) -> String {
    let base = format_predicate(&field, &value);
    if negate {
        format!("NOT {base}")
    } else {
        base
    }
}

/// Authoritative "missing value" predicate builder.
#[tauri::command]
pub fn build_missing_predicate(field: String) -> String {
    format!("NOT {field}:*")
}

/// Escapes a raw value for query text (used when copying values).
#[tauri::command]
pub fn quote_value(value: String) -> String {
    format_value(&value)
}

// ---- saved state ----------------------------------------------------------

#[tauri::command]
pub fn saved_searches(state: State<'_, AppState>) -> CmdResult<Vec<SavedSearchDto>> {
    let ws = ws_handle(&state)?;
    Ok(ws
        .meta
        .list_saved_searches()
        .map_err(|e| err(e.code(), e))?
        .into_iter()
        .map(|r| {
            let (dataset_ids, all_datasets) = selection_from_json(&r.dataset_selection_json);
            SavedSearchDto {
                saved_search_id: r.saved_search_id,
                name: r.name,
                query_text: r.query_text,
                language_version: r.language_version,
                fingerprint: r.fingerprint,
                dataset_ids,
                all_datasets,
                time_strategy: strategy_from_json(&r.time_strategy_json),
                created_at: r.created_at,
                updated_at: r.updated_at,
            }
        })
        .collect())
}

#[tauri::command]
pub fn save_search(
    state: State<'_, AppState>,
    saved_search_id: Option<String>,
    name: String,
    query_text: String,
    dataset_ids: Vec<String>,
    time_strategy: TimeStrategyDto,
) -> CmdResult<String> {
    let ws = ws_handle(&state)?;
    if name.trim().is_empty() {
        return Err(err("saved-search/invalid", "name must not be empty"));
    }
    let selection =
        explorer::resolve_dataset_selection(&ws, &dataset_ids).map_err(|e| err(e.code(), e))?;
    let analysis = explorer::analyze_query(&ws, &selection, &query_text);
    let Some(resolved) = analysis.resolved else {
        return Err(err("query/invalid", "cannot save an invalid query"));
    };
    strategy_from_dto(&time_strategy)?;
    let id = saved_search_id.unwrap_or_else(|| format!("ss-{}", uuid::Uuid::new_v4()));
    ws.meta
        .upsert_saved_search(
            &id,
            name.trim(),
            &query_text,
            LANGUAGE_VERSION as i64,
            &resolved.fingerprint,
            &selection_to_json(&dataset_ids),
            &strategy_to_json(&time_strategy),
            None,
        )
        .map_err(|e| err(e.code(), e))?;
    Ok(id)
}

#[tauri::command]
pub fn delete_saved_search(state: State<'_, AppState>, saved_search_id: String) -> CmdResult<bool> {
    let ws = ws_handle(&state)?;
    ws.meta
        .delete_saved_search(&saved_search_id)
        .map_err(|e| err(e.code(), e))
}

#[tauri::command]
pub fn column_sets(state: State<'_, AppState>) -> CmdResult<Vec<ColumnSetDto>> {
    let ws = ws_handle(&state)?;
    Ok(ws
        .meta
        .list_column_sets()
        .map_err(|e| err(e.code(), e))?
        .into_iter()
        .map(|r| ColumnSetDto {
            column_set_id: r.column_set_id,
            name: r.name,
            columns: serde_json::from_str::<Vec<serde_json::Value>>(&r.columns_json)
                .unwrap_or_default()
                .iter()
                .filter_map(|c| {
                    c.as_str()
                        .map(str::to_string)
                        .or_else(|| c["field"].as_str().map(str::to_string))
                })
                .collect(),
            is_default: r.is_default,
        })
        .collect())
}

#[tauri::command]
pub fn save_column_set(
    state: State<'_, AppState>,
    column_set_id: Option<String>,
    name: String,
    columns: Vec<String>,
    is_default: bool,
) -> CmdResult<String> {
    let ws = ws_handle(&state)?;
    if columns.is_empty() {
        return Err(err("column-set/invalid", "at least one column"));
    }
    let id = column_set_id.unwrap_or_else(|| format!("cs-{}", uuid::Uuid::new_v4()));
    ws.meta
        .upsert_column_set(
            &id,
            name.trim(),
            &serde_json::to_string(&columns).unwrap_or_else(|_| "[]".into()),
            is_default,
        )
        .map_err(|e| err(e.code(), e))?;
    Ok(id)
}

#[tauri::command]
pub fn delete_column_set(state: State<'_, AppState>, column_set_id: String) -> CmdResult<bool> {
    let ws = ws_handle(&state)?;
    ws.meta
        .delete_column_set(&column_set_id)
        .map_err(|e| err(e.code(), e))
}

#[tauri::command]
pub fn recent_searches(state: State<'_, AppState>) -> CmdResult<Vec<RecentSearchDto>> {
    let ws = ws_handle(&state)?;
    Ok(ws
        .meta
        .list_recent_searches()
        .map_err(|e| err(e.code(), e))?
        .into_iter()
        .map(|r| {
            let (dataset_ids, all_datasets) = selection_from_json(&r.dataset_selection_json);
            RecentSearchDto {
                recent_id: r.recent_id,
                query_text: r.query_text,
                language_version: r.language_version,
                dataset_ids,
                all_datasets,
                time_strategy: strategy_from_json(&r.time_strategy_json),
                run_count: r.run_count,
                last_run_at: r.last_run_at,
            }
        })
        .collect())
}

#[tauri::command]
pub fn delete_recent_search(state: State<'_, AppState>, recent_id: i64) -> CmdResult<bool> {
    let ws = ws_handle(&state)?;
    ws.meta
        .delete_recent_search(recent_id)
        .map_err(|e| err(e.code(), e))
}

#[tauri::command]
pub fn clear_recent_searches(state: State<'_, AppState>) -> CmdResult<bool> {
    let ws = ws_handle(&state)?;
    ws.meta
        .clear_recent_searches()
        .map_err(|e| err(e.code(), e))?;
    Ok(true)
}

// ---- export ----------------------------------------------------------------

#[tauri::command]
pub fn start_export(
    app: AppHandle,
    state: State<'_, AppState>,
    request: StartExportDto,
) -> CmdResult<ExportStatusDto> {
    let ws = ws_handle(&state)?;
    let prepared = prepare(
        &ws,
        &request.dataset_ids,
        &request.query_text,
        &request.time_strategy,
    )?;
    let format = match request.format.as_str() {
        "csv" => ExportFormat::Csv,
        "jsonl" => ExportFormat::Jsonl,
        other => {
            return Err(err(
                "export/invalid-format",
                format!("unknown format {other:?}"),
            ))
        }
    };
    let export_id = format!("exp-{}", uuid::Uuid::new_v4());
    let job_id = format!("job-{}", uuid::Uuid::new_v4());
    let spec = ExportSpec {
        format,
        destination: PathBuf::from(&request.destination),
        row_limit: request
            .row_limit
            .unwrap_or(logscope_app::DEFAULT_EXPORT_ROWS),
        byte_limit: request
            .byte_limit
            .unwrap_or(logscope_app::DEFAULT_EXPORT_BYTES),
        csv_columns: request.csv_columns.clone(),
        csv_formula_guard: true,
    };
    let fingerprint = prepared
        .analysis
        .resolved
        .as_ref()
        .map(|r| r.fingerprint.clone())
        .unwrap_or_default();
    ws.meta
        .insert_export_job(
            &export_id,
            &job_id,
            format.as_str(),
            &request.destination,
            &request.query_text,
            &fingerprint,
            &selection_to_json(&request.dataset_ids),
            &strategy_to_json(&request.time_strategy),
            (prepared.window.start, prepared.window.end),
            spec.row_limit as i64,
            spec.byte_limit as i64,
        )
        .map_err(|e| err(e.code(), e))?;

    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();
    let event_app = app.clone();
    std::thread::spawn(move || {
        for event in rx.iter() {
            let _ = event_app.emit("job-event", &event);
        }
    });

    // The export job owns its own engine connection; the workspace handle
    // is shared read-only.
    let engine =
        logscope_query::EngineConnection::open_in_memory().map_err(|e| err(e.code(), e))?;
    let ws_job = ws.clone();
    let export_id_job = export_id.clone();
    let files = prepared.files.clone();
    let filter = prepared.filter;
    let window = prepared.window.clone();
    let spec_job = spec;
    let handle = logscope_jobs::spawn_job(job_id.clone(), "export", tx, move |ctx| {
        let result = run_export(&engine, &files, &filter, &window, &spec_job, ctx);
        match &result {
            Ok(outcome) => {
                let _ = ws_job.meta.finish_export_job(
                    &export_id_job,
                    "completed",
                    outcome.rows_written as i64,
                    outcome.bytes_written as i64,
                    outcome.truncated,
                    None,
                );
            }
            Err(e) => {
                let status = if e.code == "job/cancelled" {
                    "cancelled"
                } else {
                    "failed"
                };
                let _ = ws_job.meta.finish_export_job(
                    &export_id_job,
                    status,
                    0,
                    0,
                    false,
                    serde_json::to_string(e).ok().as_deref(),
                );
            }
        }
        result
    });
    state
        .jobs
        .lock()
        .insert(job_id.clone(), handle.control.clone());
    let watcher_app = app.clone();
    let watch_job = job_id.clone();
    std::thread::spawn(move || {
        let _ = handle.join();
        let state = watcher_app.state::<AppState>();
        state.jobs.lock().remove(&watch_job);
        let _ = watcher_app.emit("export-finished", &watch_job);
    });

    Ok(ExportStatusDto {
        export_id,
        job_id,
        status: "running".into(),
        rows_written: 0,
        bytes_written: 0,
        truncated: false,
        destination: request.destination,
        error: None,
    })
}

#[tauri::command]
pub fn export_status(state: State<'_, AppState>, export_id: String) -> CmdResult<ExportStatusDto> {
    let ws = ws_handle(&state)?;
    let row = ws
        .meta
        .get_export_job(&export_id)
        .map_err(|e| err(e.code(), e))?
        .ok_or_else(|| err("export/not-found", "unknown export"))?;
    Ok(ExportStatusDto {
        export_id: row.export_id,
        job_id: row.job_id,
        status: row.status,
        rows_written: row.rows_written,
        bytes_written: row.bytes_written,
        truncated: row.truncated,
        destination: row.destination,
        error: row
            .error_json
            .as_deref()
            .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
            .map(|v| ErrorDto {
                code: v["code"].as_str().unwrap_or("export/failed").into(),
                message: v["message"].as_str().unwrap_or("export failed").into(),
            }),
    })
}

// ---- index maintenance -------------------------------------------------------

#[tauri::command]
pub fn index_status(state: State<'_, AppState>) -> CmdResult<Vec<IndexStateDto>> {
    let ws = ws_handle(&state)?;
    let mut out = Vec::new();
    for kind in ["fts", "field_catalog"] {
        for s in ws.meta.index_states(kind).map_err(|e| err(e.code(), e))? {
            out.push(IndexStateDto {
                kind: s.kind,
                dataset_id: s.dataset_id,
                version: s.version,
                status: s.status,
            });
        }
    }
    Ok(out)
}

/// Rebuilds pending derived indexes (FTS tokenizer upgrades after
/// migration, field catalogs) as one cancellable background job.
#[tauri::command]
pub fn rebuild_indexes(app: AppHandle, state: State<'_, AppState>) -> CmdResult<String> {
    let ws = ws_handle(&state)?;
    let job_id = format!("job-{}", uuid::Uuid::new_v4());
    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();
    let event_app = app.clone();
    std::thread::spawn(move || {
        for event in rx.iter() {
            let _ = event_app.emit("job-event", &event);
        }
    });
    let engine =
        logscope_query::EngineConnection::open_in_memory().map_err(|e| err(e.code(), e))?;
    let ws_job = ws.clone();
    let handle = logscope_jobs::spawn_job(job_id.clone(), "rebuild-indexes", tx, move |ctx| {
        explorer::rebuild_fts_to_current(&ws_job, &engine, ctx)?;
        let pending: Vec<String> = ws_job
            .meta
            .index_states("field_catalog")
            .map_err(explorer::ws_code_err)?
            .into_iter()
            .filter(|s| s.status != "ready")
            .map(|s| s.dataset_id)
            .collect();
        for dataset_id in pending {
            ctx.control.checkpoint()?;
            if let Err(e) = explorer::build_field_catalog(&ws_job, &engine, &dataset_id, ctx) {
                if ctx.control.is_cancel_requested() {
                    return Err(logscope_jobs::JobError::new(
                        "job/cancelled",
                        "index rebuild cancelled",
                    ));
                }
                return Err(e);
            }
        }
        Ok::<_, logscope_jobs::JobError>(())
    });
    state
        .jobs
        .lock()
        .insert(job_id.clone(), handle.control.clone());
    let watcher_app = app.clone();
    let watch_job = job_id.clone();
    std::thread::spawn(move || {
        let _ = handle.join();
        let state = watcher_app.state::<AppState>();
        state.jobs.lock().remove(&watch_job);
        let _ = watcher_app.emit("indexes-rebuilt", &watch_job);
    });
    Ok(job_id)
}

#[tauri::command]
pub fn list_import_profiles() -> Vec<(String, String)> {
    use logscope_ingest::builtin;
    vec![
        ("jsonl".into(), builtin::jsonl_generic().display_name),
        ("csv".into(), builtin::csv_basic().display_name),
        (
            "elasticsearch".into(),
            builtin::elasticsearch_export().display_name,
        ),
    ]
}
