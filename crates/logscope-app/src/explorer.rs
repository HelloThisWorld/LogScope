//! Explorer application service: the one place that wires workspace
//! metadata, the trusted field catalog, the authoritative language
//! pipeline, and the query engine together. The desktop shell (and any
//! future caller) goes through these functions only.

use std::path::PathBuf;
use std::time::Duration;

use logscope_jobs::{JobContext, JobError};
use logscope_query::{
    compile_filter, compute_field_stats, index_segment_into_fts, CompiledFilter, EngineConnection,
    FtsContext, LoadedCatalog, QueryCancelHandle, StoredFieldStat, CATALOG_VERSION,
};
use logscope_query_lang::{analyze, Analysis, AttrType, LangLimits};
use logscope_store::{FtsIndex, FTS_INDEX_VERSION};
use logscope_workspace::{FieldStatRow, Workspace, WorkspaceError};

/// Stable structured error for explorer operations.
pub fn ws_code_err(e: WorkspaceError) -> JobError {
    JobError::new(e.code(), e.to_string())
}

/// Validated dataset selection: only published log datasets are queryable;
/// an empty request selects all of them.
pub fn resolve_dataset_selection(
    ws: &Workspace,
    requested: &[String],
) -> Result<Vec<String>, WorkspaceError> {
    let published: Vec<String> = ws
        .meta
        .list_datasets()?
        .into_iter()
        .filter(|d| d.status == "published" && d.signal == "logs")
        .map(|d| d.dataset_id)
        .collect();
    if requested.is_empty() {
        return Ok(published);
    }
    let mut selected = Vec::with_capacity(requested.len());
    for id in requested {
        if published.contains(id) {
            if !selected.contains(id) {
                selected.push(id.clone());
            }
        } else {
            return Err(WorkspaceError::Invalid(format!(
                "dataset {id} is not a published log dataset"
            )));
        }
    }
    Ok(selected)
}

pub fn segment_files_for(
    ws: &Workspace,
    dataset_ids: &[String],
) -> Result<Vec<PathBuf>, WorkspaceError> {
    let mut files = Vec::new();
    for id in dataset_ids {
        files.extend(ws.segment_paths(id)?);
    }
    Ok(files)
}

/// Newest event timestamp across the selected datasets (segment metadata).
pub fn latest_event_time(
    ws: &Workspace,
    dataset_ids: &[String],
) -> Result<Option<i64>, WorkspaceError> {
    let mut latest = None;
    for id in dataset_ids {
        for seg in ws.meta.segments_for_dataset(id)? {
            latest = latest.max(seg.max_event_time);
        }
    }
    Ok(latest)
}

fn stat_row_to_stored(row: FieldStatRow) -> StoredFieldStat {
    let path: Vec<String> = serde_json::from_str(&row.path_json).unwrap_or_default();
    let types: Vec<AttrType> = serde_json::from_str(&row.types_json).unwrap_or_default();
    let examples: Vec<String> = serde_json::from_str(&row.examples_json).unwrap_or_default();
    StoredFieldStat {
        dataset_id: row.dataset_id,
        display: row.display,
        path,
        types,
        present_count: row.present_count,
        distinct_est: row.distinct_est,
        distinct_is_exact: row.distinct_is_exact,
        examples,
        queryable: row.queryable,
    }
}

/// Loads the trusted catalog for a selection. `complete == false` while
/// some selected dataset's catalog is still pending/building.
pub fn load_catalog(
    ws: &Workspace,
    dataset_ids: &[String],
) -> Result<LoadedCatalog, WorkspaceError> {
    let rows = ws.meta.field_stats_for_datasets(dataset_ids)?;
    let ready: Vec<String> = ws
        .meta
        .index_states("field_catalog")?
        .into_iter()
        .filter(|s| s.status == "ready" && s.version == CATALOG_VERSION)
        .map(|s| s.dataset_id)
        .collect();
    Ok(LoadedCatalog::build(
        dataset_ids.to_vec(),
        rows.into_iter().map(stat_row_to_stored).collect(),
        &ready,
    ))
}

/// True when indexed text search may be used for this selection: every
/// selected dataset is FTS-ready at the current tokenizer version AND the
/// index file itself is at the current version.
pub fn fts_ready(ws: &Workspace, dataset_ids: &[String]) -> Result<bool, WorkspaceError> {
    if !ws
        .meta
        .indexes_ready("fts", FTS_INDEX_VERSION, dataset_ids)?
    {
        return Ok(false);
    }
    match FtsIndex::open(&ws.layout.fts_logs_path()) {
        Ok(fts) => Ok(!fts.needs_rebuild().unwrap_or(true)),
        Err(_) => Ok(false),
    }
}

/// Authoritative analysis of query text against the selection's catalog.
pub fn analyze_query(ws: &Workspace, dataset_ids: &[String], text: &str) -> Analysis {
    let catalog = load_catalog(ws, dataset_ids).unwrap_or_default();
    analyze(text, &catalog, &LangLimits::default())
}

/// Compiles an analyzed query for execution, choosing the text path.
pub fn compile_for_execution(
    ws: &Workspace,
    dataset_ids: &[String],
    analysis: &Analysis,
) -> Result<CompiledFilter, JobError> {
    let resolved = analysis
        .resolved
        .as_ref()
        .ok_or_else(|| JobError::new("query/invalid", "query has errors"))?;
    let fts_ok = fts_ready(ws, dataset_ids).map_err(ws_code_err)?;
    let fts_index = if fts_ok {
        Some(
            FtsIndex::open(&ws.layout.fts_logs_path())
                .map_err(|e| JobError::new(e.code(), e.to_string()))?,
        )
    } else {
        None
    };
    let ctx = FtsContext {
        index: fts_index.as_ref(),
        dataset_ids,
    };
    compile_filter(resolved.expr.as_ref(), &ctx).map_err(|e| JobError::new(e.code(), e.to_string()))
}

/// Builds (or rebuilds) one dataset's derived field catalog. Cancellable
/// through the job control; state transitions land in `index_state`.
pub fn build_field_catalog(
    ws: &Workspace,
    engine: &EngineConnection,
    dataset_id: &str,
    ctx: &JobContext,
) -> Result<usize, JobError> {
    let meta_fail = |e: WorkspaceError| JobError::new(e.code(), e.to_string());
    ws.meta
        .set_index_state(
            "field_catalog",
            dataset_id,
            CATALOG_VERSION,
            "building",
            "{}",
        )
        .map_err(meta_fail)?;
    let files = ws.segment_paths(dataset_id).map_err(meta_fail)?;

    // Bridge job cancellation into the engine interrupt.
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let control = ctx.control.clone();
        let cancel = cancel.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if control.is_cancel_requested() {
                    cancel.cancel();
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let result = compute_field_stats(engine, &files, &cancel, Duration::from_secs(600));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = watcher.join();

    match result {
        Ok(stats) => {
            let rows: Vec<FieldStatRow> = stats
                .iter()
                .map(|s| FieldStatRow {
                    dataset_id: dataset_id.to_string(),
                    display: s.display.clone(),
                    path_json: serde_json::to_string(&s.path).unwrap_or_else(|_| "[]".into()),
                    types_json: serde_json::to_string(&s.types).unwrap_or_else(|_| "[]".into()),
                    present_count: s.present_count,
                    distinct_est: s.distinct_est,
                    distinct_is_exact: s.distinct_is_exact,
                    examples_json: serde_json::to_string(&s.examples)
                        .unwrap_or_else(|_| "[]".into()),
                    queryable: s.queryable,
                    catalog_version: CATALOG_VERSION,
                })
                .collect();
            ws.meta
                .replace_field_stats(dataset_id, &rows)
                .map_err(meta_fail)?;
            ws.meta
                .set_index_state("field_catalog", dataset_id, CATALOG_VERSION, "ready", "{}")
                .map_err(meta_fail)?;
            Ok(rows.len())
        }
        Err(e) => {
            let status = if ctx.control.is_cancel_requested() {
                "pending"
            } else {
                "failed"
            };
            let detail = serde_json::json!({ "error": e.to_string() }).to_string();
            let _ = ws.meta.set_index_state(
                "field_catalog",
                dataset_id,
                CATALOG_VERSION,
                status,
                &detail,
            );
            Err(JobError::new(e.code(), e.to_string()))
        }
    }
}

/// Rebuilds the FTS index to the current tokenizer version for every log
/// dataset (used after the 0002 migration flags a v1 index). Cancellable
/// between segments; interrupted rebuilds stay `pending` and resume later.
pub fn rebuild_fts_to_current(
    ws: &Workspace,
    engine: &EngineConnection,
    ctx: &JobContext,
) -> Result<u64, JobError> {
    let meta_fail = |e: WorkspaceError| JobError::new(e.code(), e.to_string());
    let mut fts = FtsIndex::open(&ws.layout.fts_logs_path())
        .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    if fts
        .needs_rebuild()
        .map_err(|e| JobError::new(e.code(), e.to_string()))?
    {
        // Derived state: dropping it is always safe; every dataset flips to
        // pending until its segments are re-indexed below.
        fts.reset_to_current_version()
            .map_err(|e| JobError::new(e.code(), e.to_string()))?;
        for d in ws.meta.list_datasets().map_err(meta_fail)? {
            if d.signal == "logs" && d.status == "published" {
                ws.meta
                    .set_index_state("fts", &d.dataset_id, FTS_INDEX_VERSION, "pending", "{}")
                    .map_err(meta_fail)?;
            }
        }
    }
    let states = ws.meta.index_states("fts").map_err(meta_fail)?;
    let mut indexed = 0u64;
    for state in states {
        if state.status == "ready" && state.version == FTS_INDEX_VERSION {
            continue;
        }
        ctx.control.checkpoint().map_err(JobError::from)?;
        ws.meta
            .set_index_state(
                "fts",
                &state.dataset_id,
                FTS_INDEX_VERSION,
                "building",
                "{}",
            )
            .map_err(meta_fail)?;
        let dir = ws.layout.dataset_dir(&state.dataset_id);
        for seg in ws
            .meta
            .segments_for_dataset(&state.dataset_id)
            .map_err(meta_fail)?
        {
            ctx.control.checkpoint().map_err(JobError::from)?;
            let path = dir.join(&seg.file_name);
            indexed +=
                index_segment_into_fts(engine, &mut fts, &state.dataset_id, &seg.segment_id, &path)
                    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
            ws.meta
                .mark_segment_fts_indexed(&seg.segment_id)
                .map_err(meta_fail)?;
        }
        ws.meta
            .set_index_state("fts", &state.dataset_id, FTS_INDEX_VERSION, "ready", "{}")
            .map_err(meta_fail)?;
    }
    Ok(indexed)
}

/// Marks the derived-index states for a freshly imported dataset (called
/// after atomic publication inside the import job).
pub fn note_new_dataset_indexes(ws: &Workspace, dataset_id: &str) -> Result<(), WorkspaceError> {
    // The import path indexes segments into the current-version FTS file.
    ws.meta
        .set_index_state("fts", dataset_id, FTS_INDEX_VERSION, "ready", "{}")?;
    ws.meta.set_index_state(
        "field_catalog",
        dataset_id,
        CATALOG_VERSION,
        "pending",
        "{}",
    )?;
    Ok(())
}
