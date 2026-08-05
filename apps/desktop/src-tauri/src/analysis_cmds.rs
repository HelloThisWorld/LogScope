//! v0.4 pattern-analysis commands: a thin typed layer over
//! `logscope_app::{analysis, patterns}`. No analysis semantics live
//! here — identity, masking, execution, and staleness all run through
//! the shared services; the UI never computes a pattern itself.

use std::sync::Arc;

use logscope_app::dto::*;
use logscope_app::{analysis, patterns};
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
