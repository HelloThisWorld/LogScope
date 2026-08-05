//! Analysis definition and run lifecycle services (v0.4 WP1).
//!
//! WP1 delivers the durable contracts, not algorithms: definitions are
//! validated against the real vocabulary/query/config rules, and a run's
//! scope is frozen here — dataset revisions, query fingerprint, and
//! concrete UTC bounds — before its semantic fingerprint is computed.
//! Execution bodies (template extraction, comparison, correlation)
//! arrive in WP2–WP4 and plug into this lifecycle; nothing in WP1 can
//! complete a run with fabricated results.

use logscope_case::analysis::{
    config_fingerprint, semantic_fingerprint, SemanticIdentity, ANALYSIS_DEFINITION_SCHEMA_VERSION,
};
use logscope_case::{new_id, AnalysisKind};
use logscope_jobs::JobError;
use logscope_query::{resolve_window, TimeStrategy};
use logscope_query_lang::LANGUAGE_VERSION;
use logscope_workspace::{
    AnalysisDefinitionRow, AnalysisRunRow, NewAnalysisDefinition, NewAnalysisRun, Workspace,
};

use crate::explorer;

fn ws_err(e: logscope_workspace::WorkspaceError) -> JobError {
    JobError::new(e.code(), e.to_string())
}

fn invalid(msg: impl std::fmt::Display) -> JobError {
    JobError::new("analysis/invalid-definition", msg.to_string())
}

/// Everything needed to create a validated analysis definition.
#[derive(Debug, Clone)]
pub struct NewDefinitionRequest {
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub dataset_ids: Vec<String>,
    pub query_text: String,
    pub time_strategy: TimeStrategy,
    pub field_selection_json: String,
    pub algorithm_id: String,
    pub algorithm_version: i64,
    pub config_json: String,
    pub masking_profile_json: String,
    pub thresholds_json: String,
    pub limits_json: String,
}

fn require_json_object(what: &str, json: &str) -> Result<(), JobError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| invalid(format!("{what} is not valid JSON: {e}")))?;
    if !value.is_object() {
        return Err(invalid(format!("{what} must be a JSON object")));
    }
    Ok(())
}

/// Validates and stores an analysis definition. An invalid kind, config,
/// or query is a structured refusal — never stored to fail later.
pub fn create_definition(
    ws: &Workspace,
    req: &NewDefinitionRequest,
) -> Result<AnalysisDefinitionRow, JobError> {
    if AnalysisKind::parse(&req.kind).is_none() {
        return Err(invalid(format!(
            "unknown analysis kind {:?} (expected one of {})",
            req.kind,
            AnalysisKind::EXPECTED
        )));
    }
    if req.name.trim().is_empty() {
        return Err(invalid("name must not be empty"));
    }
    if req.algorithm_id.trim().is_empty() || req.algorithm_version < 1 {
        return Err(invalid("algorithm id/version are required"));
    }
    require_json_object("masking profile", &req.masking_profile_json)?;
    require_json_object("thresholds", &req.thresholds_json)?;
    require_json_object("limits", &req.limits_json)?;
    require_json_object("field selection", &req.field_selection_json)?;
    // Configuration identity: object-only, float-free, canonicalized.
    let config_fp = config_fingerprint(&req.config_json).map_err(invalid)?;

    // Datasets and query are validated through the authoritative
    // services — an invalid scope is never captured into a definition.
    let selection = explorer::resolve_dataset_selection(ws, &req.dataset_ids).map_err(ws_err)?;
    let query_fingerprint = if req.query_text.trim().is_empty() {
        None
    } else {
        let analysis = explorer::analyze_query(ws, &selection, &req.query_text);
        let Some(resolved) = analysis.resolved.as_ref() else {
            let messages: Vec<String> = analysis
                .diagnostics
                .iter()
                .take(5)
                .map(|d| d.message.clone())
                .collect();
            let mut err = JobError::new("query/invalid", "the analysis query does not validate");
            err.detail = Some(serde_json::json!({ "diagnostics": messages }));
            return Err(err);
        };
        Some(resolved.fingerprint.clone())
    };

    ws.meta
        .create_analysis_definition(&NewAnalysisDefinition {
            definition_id: new_id("adef"),
            definition_schema_version: ANALYSIS_DEFINITION_SCHEMA_VERSION,
            kind: req.kind.clone(),
            name: req.name.clone(),
            description: req.description.clone(),
            dataset_selection_json: serde_json::to_string(&req.dataset_ids)
                .unwrap_or_else(|_| "[]".into()),
            query_text: req.query_text.clone(),
            query_language_version: LANGUAGE_VERSION as i64,
            query_fingerprint,
            time_strategy_json: serde_json::to_string(&req.time_strategy)
                .unwrap_or_else(|_| "{\"kind\":\"all\"}".into()),
            field_selection_json: req.field_selection_json.clone(),
            algorithm_id: req.algorithm_id.clone(),
            algorithm_version: req.algorithm_version,
            config_json: req.config_json.clone(),
            config_fingerprint: config_fp,
            masking_profile_json: req.masking_profile_json.clone(),
            thresholds_json: req.thresholds_json.clone(),
            limits_json: req.limits_json.clone(),
        })
        .map_err(ws_err)
}

fn load_definition(ws: &Workspace, definition_id: &str) -> Result<AnalysisDefinitionRow, JobError> {
    let def = ws
        .meta
        .get_analysis_definition(definition_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("analysis definition {definition_id} does not exist"),
            )
        })?;
    if def.definition_schema_version > ANALYSIS_DEFINITION_SCHEMA_VERSION {
        return Err(JobError::new(
            "analysis/unsupported-version",
            format!(
                "definition schema {} is newer than supported {} — update LogScope",
                def.definition_schema_version, ANALYSIS_DEFINITION_SCHEMA_VERSION
            ),
        ));
    }
    Ok(def)
}

/// Freezes a run's scope from its definition — exact dataset revisions,
/// the validated query fingerprint, and concrete UTC bounds — computes
/// the deterministic semantic fingerprint, and inserts the `pending`
/// two-phase run record. No derived byte exists before this row does.
pub fn begin_run(ws: &Workspace, definition_id: &str) -> Result<AnalysisRunRow, JobError> {
    let def = load_definition(ws, definition_id)?;
    let dataset_ids: Vec<String> =
        serde_json::from_str(&def.dataset_selection_json).unwrap_or_default();
    let selection = explorer::resolve_dataset_selection(ws, &dataset_ids).map_err(ws_err)?;
    if selection.is_empty() {
        return Err(JobError::new(
            "case/empty-scope",
            "no published log dataset is selected",
        ));
    }

    // Re-validate the query against the current catalog; a definition
    // whose query no longer validates must fail here, not mid-run.
    let query_fingerprint = if def.query_text.trim().is_empty() {
        None
    } else {
        let analysis = explorer::analyze_query(ws, &selection, &def.query_text);
        let Some(resolved) = analysis.resolved.as_ref() else {
            return Err(JobError::new(
                "query/invalid",
                "the definition's query no longer validates against the catalog",
            ));
        };
        Some(resolved.fingerprint.clone())
    };

    let strategy: TimeStrategy = serde_json::from_str(&def.time_strategy_json)
        .map_err(|e| invalid(format!("stored time strategy does not parse: {e}")))?;
    let latest = explorer::latest_event_time(ws, &selection).map_err(ws_err)?;
    let window = resolve_window(&strategy, latest);
    let bounds = serde_json::json!({ "start": window.start, "end": window.end });

    let mut dataset_revs: Vec<(String, String)> = Vec::with_capacity(selection.len());
    for id in &selection {
        dataset_revs.push((
            id.clone(),
            crate::case::dataset_revision(ws, id).map_err(ws_err)?,
        ));
    }
    dataset_revs.sort();

    let semantic = semantic_fingerprint(&SemanticIdentity {
        dataset_revs: &dataset_revs,
        query_fingerprint: query_fingerprint.as_deref(),
        query_language_version: def.query_language_version,
        bounds: &bounds,
        algorithm_id: &def.algorithm_id,
        algorithm_version: def.algorithm_version,
        config_fingerprint: &def.config_fingerprint,
    })
    .map_err(invalid)?;

    let revs_json: Vec<serde_json::Value> = dataset_revs
        .iter()
        .map(|(d, r)| serde_json::json!({"dataset_id": d, "dataset_revision": r}))
        .collect();
    ws.meta
        .start_analysis_run(&NewAnalysisRun {
            run_id: new_id("arun"),
            definition_id: def.definition_id.clone(),
            definition_revision: def.revision,
            semantic_fingerprint: semantic,
            dataset_revs_json: serde_json::to_string(&revs_json).unwrap_or_else(|_| "[]".into()),
            query_fingerprint,
            query_language_version: def.query_language_version,
            bounds_json: bounds.to_string(),
            algorithm_id: def.algorithm_id.clone(),
            algorithm_version: def.algorithm_version,
            config_fingerprint: def.config_fingerprint.clone(),
        })
        .map_err(ws_err)
}

/// pending → running: the job body has picked the run up.
pub fn mark_running(ws: &Workspace, run_id: &str) -> Result<(), JobError> {
    ws.meta.mark_analysis_run_running(run_id).map_err(ws_err)
}

/// Publishes a completed run: counts plus the result manifest, exactly
/// once, only from an unfinished state.
pub fn complete_run(
    ws: &Workspace,
    run_id: &str,
    counts_json: &str,
    manifest_json: &str,
) -> Result<AnalysisRunRow, JobError> {
    ws.meta
        .finish_analysis_run(run_id, "completed", counts_json, Some(manifest_json), None)
        .map_err(ws_err)
}

/// Finishes an unfinished run as cancelled or failed, recording the
/// structured error. Never representable as an empty success.
pub fn abort_run(
    ws: &Workspace,
    run_id: &str,
    cancelled: bool,
    error: &JobError,
) -> Result<AnalysisRunRow, JobError> {
    let state = if cancelled { "cancelled" } else { "failed" };
    let error_json = serde_json::to_string(error).unwrap_or_else(|_| "{}".into());
    ws.meta
        .finish_analysis_run(run_id, state, "{}", None, Some(&error_json))
        .map_err(ws_err)
}

/// Compares a completed run's frozen inputs against the current
/// workspace: `Some(reason)` when a referenced dataset revision moved or
/// disappeared, or the definition has a newer revision. Pure check — the
/// caller decides whether to mark the run stale.
pub fn check_run_current(ws: &Workspace, run: &AnalysisRunRow) -> Result<Option<String>, JobError> {
    #[derive(serde::Deserialize)]
    struct Rev {
        dataset_id: String,
        dataset_revision: String,
    }
    let revs: Vec<Rev> = serde_json::from_str(&run.dataset_revs_json)
        .map_err(|e| invalid(format!("stored dataset revisions do not parse: {e}")))?;
    for rev in &revs {
        // A deleted dataset and a moved revision are different honest
        // states; an unknown id also yields a fingerprint (over an empty
        // segment set), so existence is checked first.
        if ws
            .meta
            .get_dataset(&rev.dataset_id)
            .map_err(ws_err)?
            .is_none()
        {
            return Ok(Some(format!(
                "dataset {} is no longer resolvable",
                rev.dataset_id
            )));
        }
        match crate::case::dataset_revision(ws, &rev.dataset_id) {
            Ok(current) if current == rev.dataset_revision => {}
            Ok(current) => {
                return Ok(Some(format!(
                    "dataset {} moved from {} to {current}",
                    rev.dataset_id, rev.dataset_revision
                )))
            }
            Err(_) => {
                return Ok(Some(format!(
                    "dataset {} is no longer resolvable",
                    rev.dataset_id
                )))
            }
        }
    }
    let def = load_definition(ws, &run.definition_id)?;
    if def.revision != run.definition_revision {
        return Ok(Some(format!(
            "definition revised from {} to {}",
            run.definition_revision, def.revision
        )));
    }
    Ok(None)
}

/// Marks a completed run stale with the recorded reason.
pub fn mark_stale(ws: &Workspace, run_id: &str, reason: &str) -> Result<AnalysisRunRow, JobError> {
    ws.meta
        .mark_analysis_run_stale(run_id, reason)
        .map_err(ws_err)
}
