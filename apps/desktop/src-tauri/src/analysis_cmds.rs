//! v0.4 pattern-analysis commands: a thin typed layer over
//! `logscope_app::{analysis, patterns}`. No analysis semantics live
//! here — identity, masking, execution, and staleness all run through
//! the shared services; the UI never computes a pattern itself.

use std::sync::Arc;

use logscope_app::dto::*;
use logscope_app::{analysis, comparison, correlation, patterns};
use logscope_jobs::JobEvent;
use logscope_query::TimeStrategy;
use logscope_workspace::{AnalysisDefinitionRow, AnalysisRunRow, Workspace};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::explorer_cmds::{err, with_engine, ws_handle};
use crate::{AppState, CmdResult};

fn ws_err(e: logscope_workspace::WorkspaceError) -> ErrorDto {
    ErrorDto::new(e.code(), e)
}

fn jerr(e: &logscope_jobs::JobError) -> ErrorDto {
    ErrorDto::new(&e.code, &e.message)
}

fn def_dto(row: AnalysisDefinitionRow) -> AnalysisDefinitionDto {
    AnalysisDefinitionDto {
        dataset_ids: serde_json::from_str(&row.dataset_selection_json).unwrap_or_default(),
        definition_id: row.definition_id,
        kind: row.kind,
        name: row.name,
        description: row.description,
        query_text: row.query_text,
        algorithm_id: row.algorithm_id,
        algorithm_version: row.algorithm_version,
        field_selection_json: row.field_selection_json,
        config_json: row.config_json,
        masking_profile_json: row.masking_profile_json,
        limits_json: row.limits_json,
        created_at: row.created_at,
        updated_at: row.updated_at,
        revision: row.revision,
    }
}

fn run_dto(row: AnalysisRunRow) -> AnalysisRunDto {
    AnalysisRunDto {
        run_id: row.run_id,
        definition_id: row.definition_id,
        definition_revision: row.definition_revision,
        semantic_fingerprint: row.semantic_fingerprint,
        state: row.state,
        dataset_revs_json: row.dataset_revs_json,
        bounds_json: row.bounds_json,
        counts_json: row.counts_json,
        warnings_json: row.warnings_json,
        manifest_json: row.manifest_json,
        error_json: row.error_json,
        invalidation_reason: row.invalidation_reason,
        started_at: row.started_at,
        finished_at: row.finished_at,
    }
}

fn summary_dto(s: patterns::PatternSummary) -> PatternSummaryDto {
    PatternSummaryDto {
        pattern_id: s.pattern_id,
        kind: s.kind,
        template: s.template,
        exception_type: s.exception_type,
        count: s.count as i64,
        untimestamped: s.untimestamped as i64,
        first_seen: s.first_seen,
        last_seen: s.last_seen,
        peak_bucket_start: s.peak_bucket_start,
        peak_bucket_count: s.peak_bucket_count as i64,
        buckets_truncated: s.buckets_truncated,
        services_truncated: s.services_truncated,
        parse_quality: s.parse_quality,
        services_json: s.services_json,
        examples_json: s.examples_json,
    }
}

fn result_dto(r: comparison::ComparisonResult) -> ComparisonResultDto {
    ComparisonResultDto {
        result_id: r.result_id,
        dimension: r.dimension,
        key: r.key,
        classification: r.classification,
        baseline_count: r.baseline_count as i64,
        suspect_count: r.suspect_count as i64,
        count_change: r.count_change,
        rate_change_bp: r.rate_change_bp,
        rule_id: r.rule_id,
        rule_version: r.rule_version,
        calculation_json: r.calculation_json,
    }
}

#[tauri::command]
pub fn list_analysis_definitions(
    state: State<'_, AppState>,
) -> CmdResult<Vec<AnalysisDefinitionDto>> {
    let ws = ws_handle(&state)?;
    Ok(ws
        .meta
        .list_analysis_definitions()
        .map_err(ws_err)?
        .into_iter()
        .map(def_dto)
        .collect())
}

/// Creates a validated pattern-analysis definition. The two supported
/// kinds carry their algorithm identity implicitly; an invalid kind,
/// masking profile, config, or query is a structured refusal.
#[tauri::command]
pub fn create_pattern_definition(
    state: State<'_, AppState>,
    new: NewPatternDefinitionDto,
) -> CmdResult<AnalysisDefinitionDto> {
    let ws = ws_handle(&state)?;
    let (algorithm_id, field_selection_json) = match new.kind.as_str() {
        "message_pattern" => ("template.mask", "{}".to_string()),
        "stack_fingerprint" => {
            let field = new.stack_field.as_deref().unwrap_or("").trim().to_string();
            if field.is_empty() {
                return Err(err(
                    "analysis/invalid-definition",
                    "stack_fingerprint requires the attribute holding the stack text",
                ));
            }
            (
                "stack.frames",
                serde_json::json!({ "stack_field": field }).to_string(),
            )
        }
        other => {
            return Err(err(
                "analysis/invalid-definition",
                format!("unsupported pattern kind {other:?}"),
            ))
        }
    };
    analysis::create_definition(
        &ws,
        &analysis::NewDefinitionRequest {
            kind: new.kind,
            name: new.name,
            description: new.description,
            dataset_ids: new.dataset_ids,
            query_text: new.query_text,
            time_strategy: TimeStrategy::All,
            field_selection_json,
            algorithm_id: algorithm_id.into(),
            algorithm_version: 1,
            config_json: if new.config_json.trim().is_empty() {
                "{}".into()
            } else {
                new.config_json
            },
            masking_profile_json: if new.masking_profile_json.trim().is_empty() {
                "{}".into()
            } else {
                new.masking_profile_json
            },
            thresholds_json: "{}".into(),
            limits_json: if new.limits_json.trim().is_empty() {
                "{}".into()
            } else {
                new.limits_json
            },
        },
    )
    .map(def_dto)
    .map_err(|e| jerr(&e))
}

#[tauri::command]
pub fn list_analysis_runs(
    state: State<'_, AppState>,
    definition_id: Option<String>,
) -> CmdResult<Vec<AnalysisRunDto>> {
    let ws = ws_handle(&state)?;
    Ok(ws
        .meta
        .list_analysis_runs(definition_id.as_deref())
        .map_err(ws_err)?
        .into_iter()
        .map(run_dto)
        .collect())
}

/// Live staleness check for a completed run: `null` = still current,
/// otherwise the human-readable reason (dataset moved / definition
/// revised). The run row itself is only marked stale explicitly.
#[tauri::command]
pub fn check_analysis_run(state: State<'_, AppState>, run_id: String) -> CmdResult<Option<String>> {
    let ws = ws_handle(&state)?;
    let run = ws
        .meta
        .get_analysis_run(&run_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            err(
                "workspace/missing-entity",
                format!("run {run_id} not found"),
            )
        })?;
    if run.state != "completed" {
        return Ok(run.invalidation_reason);
    }
    analysis::check_run_current(&ws, &run).map_err(|e| jerr(&e))
}

/// Starts a pattern-analysis job. Progress arrives on the shared
/// `job-event` channel, cancellation goes through `cancel_job`, and the
/// terminal run row is emitted as `analysis-finished` — cancelled and
/// failed runs included, never as an empty success.
#[tauri::command]
pub fn start_pattern_analysis(
    app: AppHandle,
    state: State<'_, AppState>,
    definition_id: String,
) -> CmdResult<AnalysisStartedDto> {
    let ws = ws_handle(&state)?;
    let job_id = format!("job-{}", uuid::Uuid::new_v4());
    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();
    let event_app = app.clone();
    std::thread::spawn(move || {
        for event in rx.iter() {
            let _ = event_app.emit("job-event", &event);
        }
    });

    // The analysis job owns its own engine connection; run/derived
    // writes go through `ws.meta` and the derived directory.
    let engine =
        logscope_query::EngineConnection::open_in_memory().map_err(|e| err(e.code(), e))?;
    let ws_job: Arc<Workspace> = ws.clone();
    let def_job = definition_id.clone();
    let handle = logscope_jobs::spawn_job(job_id.clone(), "pattern-analysis", tx, move |ctx| {
        patterns::run_pattern_analysis(&ws_job, &engine, &def_job, ctx)
    });
    state
        .jobs
        .lock()
        .insert(job_id.clone(), handle.control.clone());

    let watcher_app = app.clone();
    let watch_job = job_id.clone();
    let def_done = definition_id.clone();
    std::thread::spawn(move || {
        let result = handle.join();
        let state = watcher_app.state::<AppState>();
        state.jobs.lock().remove(&watch_job);
        let payload = match result {
            Ok(run) => AnalysisFinishedDto {
                job_id: watch_job.clone(),
                definition_id: def_done,
                run: Some(run_dto(run)),
                error: None,
            },
            Err(e) => AnalysisFinishedDto {
                job_id: watch_job.clone(),
                definition_id: def_done,
                run: None,
                error: Some(jerr(&e)),
            },
        };
        let _ = watcher_app.emit("analysis-finished", &payload);
    });

    Ok(AnalysisStartedDto {
        job_id,
        definition_id,
    })
}

/// Creates a validated comparison definition. The typed window bounds
/// and dimension are composed into the config here — the UI never hand
/// writes analysis JSON — and the config is parsed immediately, so a
/// reversed, empty, or overlapping pair of windows is refused before
/// any definition exists rather than at the first run.
#[tauri::command]
pub fn create_comparison_definition(
    state: State<'_, AppState>,
    new: NewComparisonDefinitionDto,
) -> CmdResult<AnalysisDefinitionDto> {
    let ws = ws_handle(&state)?;
    let mut config = serde_json::json!({
        "dimension": new.dimension,
        "baseline_start": new.baseline_start,
        "baseline_end": new.baseline_end,
        "suspect_start": new.suspect_start,
        "suspect_end": new.suspect_end,
    });
    if let Some(attribute) = new
        .attribute
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        config["attribute"] = attribute.into();
    }
    if let Some(k) = new.top_k {
        config["top_k"] = k.into();
    }
    let config_json = config.to_string();
    // Structured refusal now, not after a run row exists.
    comparison::ComparisonConfig::parse(&config_json).map_err(|e| jerr(&e))?;

    let field_selection_json = match new.dimension.as_str() {
        "stack_fingerprint" => {
            let field = new.stack_field.as_deref().unwrap_or("").trim().to_string();
            if field.is_empty() {
                return Err(err(
                    "analysis/invalid-definition",
                    "the stack_fingerprint dimension requires the attribute holding the stack text",
                ));
            }
            serde_json::json!({ "stack_field": field }).to_string()
        }
        _ => "{}".to_string(),
    };
    analysis::create_definition(
        &ws,
        &analysis::NewDefinitionRequest {
            kind: "comparison".into(),
            name: new.name,
            description: new.description,
            dataset_ids: new.dataset_ids,
            query_text: new.query_text,
            time_strategy: TimeStrategy::All,
            field_selection_json,
            algorithm_id: logscope_case::comparison::COMPARISON_RULE_ID.into(),
            algorithm_version: logscope_case::comparison::COMPARISON_RULE_VERSION,
            config_json,
            masking_profile_json: if new.masking_profile_json.trim().is_empty() {
                "{}".into()
            } else {
                new.masking_profile_json
            },
            thresholds_json: if new.thresholds_json.trim().is_empty() {
                "{}".into()
            } else {
                new.thresholds_json
            },
            limits_json: "{}".into(),
        },
    )
    .map(def_dto)
    .map_err(|e| jerr(&e))
}

/// Starts a comparison job on the same job/cancel/terminal-event
/// machinery as pattern analysis.
#[tauri::command]
pub fn start_comparison_analysis(
    app: AppHandle,
    state: State<'_, AppState>,
    definition_id: String,
) -> CmdResult<AnalysisStartedDto> {
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
    let ws_job: Arc<Workspace> = ws.clone();
    let def_job = definition_id.clone();
    let handle = logscope_jobs::spawn_job(job_id.clone(), "comparison-analysis", tx, move |ctx| {
        comparison::run_comparison_analysis(&ws_job, &engine, &def_job, ctx)
    });
    state
        .jobs
        .lock()
        .insert(job_id.clone(), handle.control.clone());

    let watcher_app = app.clone();
    let watch_job = job_id.clone();
    let def_done = definition_id.clone();
    std::thread::spawn(move || {
        let result = handle.join();
        let state = watcher_app.state::<AppState>();
        state.jobs.lock().remove(&watch_job);
        let payload = match result {
            Ok(run) => AnalysisFinishedDto {
                job_id: watch_job.clone(),
                definition_id: def_done,
                run: Some(run_dto(run)),
                error: None,
            },
            Err(e) => AnalysisFinishedDto {
                job_id: watch_job.clone(),
                definition_id: def_done,
                run: None,
                error: Some(jerr(&e)),
            },
        };
        let _ = watcher_app.emit("analysis-finished", &payload);
    });

    Ok(AnalysisStartedDto {
        job_id,
        definition_id,
    })
}

/// One page of classified comparison rows from a completed run.
#[tauri::command]
pub fn list_comparison_results(
    state: State<'_, AppState>,
    run_id: String,
    offset: u32,
    limit: u32,
) -> CmdResult<Vec<ComparisonResultDto>> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        comparison::list_comparison_results(&ws, engine, &run_id, offset as u64, limit as u64)
            .map(|rows| rows.into_iter().map(result_dto).collect())
            .map_err(|e| jerr(&e))
    })
}

/// Drill-down to the records one key contributed on one side
/// (`baseline` or `suspect`), re-derived from the frozen windows.
/// Refused for stale runs — the answer would silently change.
#[tauri::command]
pub fn comparison_records(
    state: State<'_, AppState>,
    run_id: String,
    key: String,
    side: String,
    limit: u32,
) -> CmdResult<Vec<LogRowV2Dto>> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        comparison::comparison_records(&ws, engine, &run_id, &key, &side, limit as usize)
            .map(|rows| rows.iter().map(crate::explorer_cmds::row_dto).collect())
            .map_err(|e| jerr(&e))
    })
}

/// One page of pattern summaries from a completed run.
#[tauri::command]
pub fn list_patterns(
    state: State<'_, AppState>,
    run_id: String,
    offset: u32,
    limit: u32,
) -> CmdResult<Vec<PatternSummaryDto>> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        patterns::list_patterns(&ws, engine, &run_id, offset as u64, limit as u64)
            .map(|rows| rows.into_iter().map(summary_dto).collect())
            .map_err(|e| jerr(&e))
    })
}

/// Deterministic drill-down to the contributing canonical records
/// (bounded). Refused for stale runs — the answer would silently change.
#[tauri::command]
pub fn pattern_records(
    state: State<'_, AppState>,
    run_id: String,
    pattern_id: String,
    limit: u32,
) -> CmdResult<Vec<LogRowV2Dto>> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        patterns::pattern_records(&ws, engine, &run_id, &pattern_id, limit as usize)
            .map(|rows| rows.iter().map(crate::explorer_cmds::row_dto).collect())
            .map_err(|e| jerr(&e))
    })
}

fn group_dto(row: correlation::CorrelationGroup) -> CorrelationGroupDto {
    CorrelationGroupDto {
        group_id: row.group_id,
        key_selector: row.key_selector,
        key_value: row.key_value,
        confidence: row.confidence,
        event_count: row.event_count as i64,
        undated_count: row.undated_count as i64,
        truncated_count: row.truncated_count as i64,
        first_event_time: row.first_event_time,
        last_event_time: row.last_event_time,
        resources_json: row.resources_json,
        edge_count: row.edge_count as i64,
        rule_id: row.rule_id,
        rule_version: row.rule_version,
        reason: row.reason,
    }
}

fn edge_dto(row: correlation::CorrelationEdge) -> CorrelationEdgeDto {
    CorrelationEdgeDto {
        edge_id: row.edge_id,
        group_id: row.group_id,
        from_record_id: row.from_record_id,
        to_record_id: row.to_record_id,
        from_event_time: row.from_event_time,
        to_event_time: row.to_event_time,
        delta_nanos: row.delta_nanos,
        confidence: row.confidence,
        reason: row.reason,
    }
}

fn signal_dto(row: correlation::CorrelationSignal) -> CorrelationSignalDto {
    CorrelationSignalDto {
        signal_id: row.signal_id,
        group_id: row.group_id,
        kind: row.kind,
        rule_id: row.rule_id,
        rule_version: row.rule_version,
        strength: row.strength,
        investigative_lead: row.investigative_lead,
        from_record_id: row.from_record_id,
        to_record_id: row.to_record_id,
        from_event_time: row.from_event_time,
        to_event_time: row.to_event_time,
        delta_nanos: row.delta_nanos,
        tolerance_nanos: row.tolerance_nanos,
        matched_json: row.matched_json,
        missing_json: row.missing_json,
        reason: row.reason,
    }
}

/// Composes `config_json` from typed fields and parses it immediately,
/// so an impossible combination (normalization on a canonical
/// identifier, `span_id` as a key, a zero gap threshold) is refused
/// before a definition row exists.
#[tauri::command]
pub fn create_correlation_definition(
    state: State<'_, AppState>,
    new: NewCorrelationDefinitionDto,
) -> CmdResult<AnalysisDefinitionDto> {
    let ws = ws_handle(&state)?;
    let mut normalization = serde_json::json!({
        "trim": new.trim,
        "case_fold": new.case_fold,
    });
    if let Some(prefix) = new.strip_prefix.as_deref().filter(|s| !s.is_empty()) {
        normalization["strip_prefix"] = prefix.into();
    }
    let mut config = serde_json::json!({
        "key": new.key,
        "normalization": normalization,
        "signals": new.signals,
        "thresholds": {
            "clock_skew_tolerance_nanos": new.clock_skew_tolerance_nanos,
            "gap_threshold_nanos": new.gap_threshold_nanos,
        },
    });
    if let Some(attribute) = new
        .attribute
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        config["attribute"] = attribute.into();
    }
    if let Some(field) = new
        .attempt_attribute
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        config["attempt_attribute"] = field.into();
    }
    let config_json = config.to_string();
    // Structured refusal now, not after a run row exists.
    correlation::CorrelationConfig::parse(&config_json, "{}").map_err(|e| jerr(&e))?;

    analysis::create_definition(
        &ws,
        &analysis::NewDefinitionRequest {
            kind: "correlation".into(),
            name: new.name,
            description: new.description,
            dataset_ids: new.dataset_ids,
            query_text: new.query_text,
            time_strategy: TimeStrategy::All,
            field_selection_json: "{}".into(),
            algorithm_id: logscope_case::correlation::CORRELATION_RULE_ID.into(),
            algorithm_version: logscope_case::correlation::CORRELATION_RULE_VERSION,
            config_json,
            masking_profile_json: "{}".into(),
            thresholds_json: "{}".into(),
            limits_json: "{}".into(),
        },
    )
    .map(def_dto)
    .map_err(|e| jerr(&e))
}

/// Starts a correlation job on the same job/cancel/terminal-event
/// machinery as the other analyses.
#[tauri::command]
pub fn start_correlation_analysis(
    app: AppHandle,
    state: State<'_, AppState>,
    definition_id: String,
) -> CmdResult<AnalysisStartedDto> {
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
    let ws_job: Arc<Workspace> = ws.clone();
    let def_job = definition_id.clone();
    let handle = logscope_jobs::spawn_job(job_id.clone(), "correlation-analysis", tx, move |ctx| {
        correlation::run_correlation_analysis(&ws_job, &engine, &def_job, ctx)
    });
    state
        .jobs
        .lock()
        .insert(job_id.clone(), handle.control.clone());

    let watcher_app = app.clone();
    let watch_job = job_id.clone();
    let def_done = definition_id.clone();
    std::thread::spawn(move || {
        let result = handle.join();
        let state = watcher_app.state::<AppState>();
        state.jobs.lock().remove(&watch_job);
        let payload = match result {
            Ok(run) => AnalysisFinishedDto {
                job_id: watch_job.clone(),
                definition_id: def_done,
                run: Some(run_dto(run)),
                error: None,
            },
            Err(e) => AnalysisFinishedDto {
                job_id: watch_job.clone(),
                definition_id: def_done,
                run: None,
                error: Some(jerr(&e)),
            },
        };
        let _ = watcher_app.emit("analysis-finished", &payload);
    });

    Ok(AnalysisStartedDto {
        job_id,
        definition_id,
    })
}

/// One page of correlation groups in the stored deterministic order.
#[tauri::command]
pub fn list_correlation_groups(
    state: State<'_, AppState>,
    run_id: String,
    offset: u32,
    limit: u32,
) -> CmdResult<Vec<CorrelationGroupDto>> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        correlation::list_correlation_groups(&ws, engine, &run_id, offset as u64, limit as u64)
            .map(|rows| rows.into_iter().map(group_dto).collect())
            .map_err(|e| jerr(&e))
    })
}

/// The bounded ordered edges of one group (previous/next only).
#[tauri::command]
pub fn list_correlation_edges(
    state: State<'_, AppState>,
    run_id: String,
    group_id: String,
    limit: u32,
) -> CmdResult<Vec<CorrelationEdgeDto>> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        correlation::list_correlation_edges(&ws, engine, &run_id, &group_id, limit as u64)
            .map(|rows| rows.into_iter().map(edge_dto).collect())
            .map_err(|e| jerr(&e))
    })
}

/// The behavioural signals observed inside one group. A run produced
/// before signals existed is refused with the re-run instruction rather
/// than returning an empty page.
#[tauri::command]
pub fn list_correlation_signals(
    state: State<'_, AppState>,
    run_id: String,
    group_id: String,
    limit: u32,
) -> CmdResult<Vec<CorrelationSignalDto>> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        correlation::list_correlation_signals(&ws, engine, &run_id, &group_id, limit as u64)
            .map(|rows| rows.into_iter().map(signal_dto).collect())
            .map_err(|e| jerr(&e))
    })
}

/// Deterministic drill-down to a group's member records. Refused for
/// stale runs: the answer would silently change.
#[tauri::command]
pub fn correlation_records(
    state: State<'_, AppState>,
    run_id: String,
    group_id: String,
    limit: u32,
) -> CmdResult<Vec<LogRowV2Dto>> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        correlation::correlation_records(&ws, engine, &run_id, &group_id, limit as usize)
            .map(|rows| rows.iter().map(crate::explorer_cmds::row_dto).collect())
            .map_err(|e| jerr(&e))
    })
}

/// A probable neighborhood around one selected record, inside the run's
/// frozen scope. Always `probable`: proximity is evidence that two
/// things were near each other, and nothing more.
#[tauri::command]
pub fn probable_neighborhood(
    state: State<'_, AppState>,
    run_id: String,
    anchor_record_id: String,
    compatible_fields: Vec<String>,
    tolerance_nanos: i64,
    max_neighbors: u32,
) -> CmdResult<ProbableNeighborhoodDto> {
    let ws = ws_handle(&state)?;
    with_engine(&state, None, |engine, _| {
        correlation::probable_neighborhood(
            &ws,
            engine,
            &run_id,
            &anchor_record_id,
            &compatible_fields,
            tolerance_nanos,
            max_neighbors as u64,
        )
        .map(|h| ProbableNeighborhoodDto {
            anchor_record_id: h.anchor_record_id,
            anchor_event_time: h.anchor_event_time,
            anchor_time_quality: h.anchor_time_quality,
            rule_id: h.rule_id,
            rule_version: h.rule_version,
            confidence: h.confidence,
            compatible_fields: h.compatible_fields,
            constraints: h.constraints,
            tolerance_nanos: h.tolerance_nanos,
            neighbors: h
                .neighbors
                .into_iter()
                .map(|n| ProbableNeighborDto {
                    record_id: n.record_id,
                    event_time: n.event_time,
                    delta_nanos: n.delta_nanos,
                    matched_fields: n.matched_fields,
                    time_quality: n.time_quality,
                })
                .collect(),
            admitted: h.admitted as i64,
            truncated: h.truncated as i64,
            scanned: h.scanned as i64,
            reason: h.reason,
        })
        .map_err(|e| jerr(&e))
    })
}
