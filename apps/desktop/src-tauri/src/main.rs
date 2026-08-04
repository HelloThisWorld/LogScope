//! LogScope desktop shell.
//!
//! Thin typed command boundary over `logscope-app` services. React owns
//! presentation only; every operation delegates to shared services so any
//! future caller keeps identical semantics. The workspace is shared as an
//! `Arc` so queries, exports, and index jobs can run while the UI reads
//! metadata; imports still take exclusive ownership (v0.0 model).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod case_cmds;
mod explorer_cmds;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use logscope_app::dto::*;
use logscope_app::{run_import, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::{JobControl, JobEvent};
use logscope_query::{EngineConnection, QueryCancelHandle};
use logscope_workspace::Workspace;
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

/// Number of pooled query connections = maximum concurrent queries.
const QUERY_POOL_SIZE: usize = 3;

pub(crate) struct AppState {
    pub(crate) workspace: Mutex<Option<Arc<Workspace>>>,
    pub(crate) engines: Vec<Mutex<EngineConnection>>,
    pub(crate) jobs: Mutex<HashMap<String, JobControl>>,
    pub(crate) import_running: AtomicBool,
    pub(crate) query_cancels: Mutex<HashMap<String, QueryCancelHandle>>,
}

pub(crate) type CmdResult<T> = Result<T, ErrorDto>;

fn ws_err(e: logscope_workspace::WorkspaceError) -> ErrorDto {
    ErrorDto::new(e.code(), e)
}

fn workspace_info(ws: &Workspace, recovery: bool) -> WorkspaceInfoDto {
    WorkspaceInfoDto {
        root: ws.layout.root().display().to_string(),
        workspace_id: ws.manifest.workspace_id.clone(),
        name: ws.manifest.name.clone(),
        schema_version: ws.manifest.schema_version,
        product_version: ws.manifest.product_version.clone(),
        available_signals: ws.manifest.available_signals.clone(),
        recovery: (recovery && !ws.recovery.is_clean()).then(|| RecoveryDto {
            discarded_staging_dirs: ws.recovery.discarded_staging_dirs.clone(),
            removed_orphan_files: ws.recovery.removed_orphan_files.clone(),
            interrupted_jobs: ws.recovery.interrupted_jobs.clone(),
            discarded_staging_datasets: ws.recovery.discarded_staging_datasets.clone(),
        }),
    }
}

fn remember_recent(app: &AppHandle, root: &str) {
    let Ok(dir) = app.path().app_config_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let file = dir.join("recent-workspaces.json");
    let mut list: Vec<String> = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    list.retain(|p| p != root);
    list.insert(0, root.to_string());
    list.truncate(10);
    if let Ok(text) = serde_json::to_string_pretty(&list) {
        let _ = std::fs::write(&file, text);
    }
}

#[tauri::command]
fn recent_workspaces(app: AppHandle) -> Vec<String> {
    app.path()
        .app_config_dir()
        .ok()
        .and_then(|dir| std::fs::read_to_string(dir.join("recent-workspaces.json")).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

#[tauri::command]
fn create_workspace(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    name: String,
) -> CmdResult<WorkspaceInfoDto> {
    let mut slot = state.workspace.lock();
    if slot.is_some() || state.import_running.load(Ordering::SeqCst) {
        return Err(ErrorDto::new(
            "workspace/busy",
            "close the current workspace first",
        ));
    }
    let ws = Workspace::create(&PathBuf::from(&path), &name, logscope_app::PRODUCT_VERSION)
        .map_err(ws_err)?;
    let info = workspace_info(&ws, false);
    remember_recent(&app, &info.root);
    *slot = Some(Arc::new(ws));
    Ok(info)
}

#[tauri::command]
fn open_workspace(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> CmdResult<WorkspaceInfoDto> {
    let mut slot = state.workspace.lock();
    if slot.is_some() || state.import_running.load(Ordering::SeqCst) {
        return Err(ErrorDto::new(
            "workspace/busy",
            "close the current workspace first",
        ));
    }
    let ws =
        Workspace::open(&PathBuf::from(&path), logscope_app::PRODUCT_VERSION).map_err(ws_err)?;
    let info = workspace_info(&ws, true);
    remember_recent(&app, &info.root);
    *slot = Some(Arc::new(ws));
    Ok(info)
}

#[tauri::command]
fn close_workspace(state: State<'_, AppState>) -> CmdResult<bool> {
    if state.import_running.load(Ordering::SeqCst) {
        return Err(ErrorDto::new(
            "workspace/busy",
            "an import is running; cancel it before closing",
        ));
    }
    if !state.jobs.lock().is_empty() {
        return Err(ErrorDto::new(
            "workspace/busy",
            "background work is running; cancel it before closing",
        ));
    }
    Ok(state.workspace.lock().take().is_some())
}

#[tauri::command]
fn overview(state: State<'_, AppState>) -> CmdResult<OverviewDto> {
    let slot = state.workspace.lock();
    let ws = slot.as_ref().ok_or_else(|| {
        if state.import_running.load(Ordering::SeqCst) {
            ErrorDto::new("workspace/importing", "import in progress")
        } else {
            ErrorDto::new("workspace/none", "no workspace is open")
        }
    })?;
    let mut datasets = Vec::new();
    for d in ws.meta.list_datasets().map_err(ws_err)? {
        let segments = ws
            .meta
            .segments_for_dataset(&d.dataset_id)
            .map_err(ws_err)?;
        datasets.push(DatasetDto {
            dataset_id: d.dataset_id,
            name: d.name,
            signal: d.signal,
            status: d.status,
            created_at: d.created_at,
            row_count: segments.iter().map(|s| s.row_count).sum(),
            byte_size: segments.iter().map(|s| s.byte_size).sum(),
            segment_count: segments.len() as i64,
        });
    }
    let jobs = ws
        .meta
        .list_jobs()
        .map_err(ws_err)?
        .into_iter()
        .map(|j| JobDto {
            job_id: j.job_id,
            kind: j.kind,
            status: j.status,
            dataset_id: j.dataset_id,
            created_at: j.created_at,
            updated_at: j.updated_at,
            error_json: j.error_json,
        })
        .collect();
    Ok(OverviewDto {
        workspace: workspace_info(ws, false),
        datasets,
        jobs,
    })
}

#[tauri::command]
fn start_import(
    app: AppHandle,
    state: State<'_, AppState>,
    request: StartImportDto,
) -> CmdResult<String> {
    if request.paths.is_empty() {
        return Err(ErrorDto::new(
            "import/invalid-argument",
            "no files selected",
        ));
    }
    let profile = match request.format.as_str() {
        "jsonl" => builtin::jsonl_generic(),
        "csv" => builtin::csv_basic(),
        "elasticsearch" => builtin::elasticsearch_export(),
        other => {
            return Err(ErrorDto::new(
                "import/unsupported-format",
                format!("unknown format selector {other:?}"),
            ))
        }
    };
    let mut slot = state.workspace.lock();
    if state.import_running.swap(true, Ordering::SeqCst) {
        return Err(ErrorDto::new("import/busy", "another import is running"));
    }
    let Some(arc) = slot.take() else {
        state.import_running.store(false, Ordering::SeqCst);
        return Err(ErrorDto::new("workspace/none", "no workspace is open"));
    };
    // Imports need exclusive ownership; short-lived query handles may still
    // hold clones for a moment.
    let mut ws = match Arc::try_unwrap(arc) {
        Ok(ws) => ws,
        Err(arc) => {
            *slot = Some(arc);
            state.import_running.store(false, Ordering::SeqCst);
            return Err(ErrorDto::new(
                "workspace/busy",
                "queries are still running; retry in a moment",
            ));
        }
    };
    drop(slot);

    let import_request = ImportRequest::new(
        request.paths.iter().map(PathBuf::from).collect(),
        profile,
        &request.dataset_name,
    );
    let job_id = format!("job-{}", uuid::Uuid::new_v4());
    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();

    let event_app = app.clone();
    std::thread::spawn(move || {
        for event in rx.iter() {
            let _ = event_app.emit("job-event", &event);
        }
    });

    let engine = EngineConnection::open_in_memory().map_err(|e| {
        state.import_running.store(false, Ordering::SeqCst);
        ErrorDto::new(e.code(), e)
    })?;
    let handle = logscope_jobs::spawn_job(job_id.clone(), "import", tx, move |ctx| {
        let outcome = run_import(&mut ws, &engine, &import_request, ctx);
        Ok::<_, logscope_jobs::JobError>((outcome, ws))
    });
    state
        .jobs
        .lock()
        .insert(job_id.clone(), handle.control.clone());

    let watcher_app = app.clone();
    let watch_job_id = job_id.clone();
    std::thread::spawn(move || {
        let result = handle.join();
        let state = watcher_app.state::<AppState>();
        match result {
            Ok((_outcome, ws)) => {
                *state.workspace.lock() = Some(Arc::new(ws));
            }
            Err(e) => {
                tracing::error!(job = %watch_job_id, error = %e, "import job thread failed");
            }
        }
        state.jobs.lock().remove(&watch_job_id);
        state.import_running.store(false, Ordering::SeqCst);
        let _ = watcher_app.emit("import-finished", &watch_job_id);
    });

    Ok(job_id)
}

#[tauri::command]
fn cancel_job(state: State<'_, AppState>, job_id: String) -> bool {
    if let Some(control) = state.jobs.lock().get(&job_id) {
        control.cancel();
        true
    } else {
        false
    }
}

/// Portable-mode fixed WebView2: when a `webview2/` folder ships next to the
/// executable, point the WebView2 loader at it so first launch neither
/// installs nor downloads anything (ADR-0002).
#[cfg(windows)]
fn use_bundled_webview2_if_present() {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let fixed = dir.join("webview2");
            if fixed.join("msedgewebview2.exe").exists() {
                // Called before any threads are spawned (edition 2021).
                std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", &fixed);
            }
        }
    }
}

fn main() {
    #[cfg(windows)]
    use_bundled_webview2_if_present();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let engines: Vec<Mutex<EngineConnection>> = (0..QUERY_POOL_SIZE)
        .map(|_| Mutex::new(EngineConnection::open_in_memory().expect("query engine init")))
        .collect();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            workspace: Mutex::new(None),
            engines,
            jobs: Mutex::new(HashMap::new()),
            import_running: AtomicBool::new(false),
            query_cancels: Mutex::new(HashMap::new()),
        })
        .invoke_handler(tauri::generate_handler![
            recent_workspaces,
            create_workspace,
            open_workspace,
            close_workspace,
            overview,
            start_import,
            cancel_job,
            explorer_cmds::validate_query,
            explorer_cmds::field_catalog,
            explorer_cmds::run_query,
            explorer_cmds::run_histogram,
            explorer_cmds::run_facets,
            explorer_cmds::field_summary,
            explorer_cmds::cancel_query,
            explorer_cmds::get_record,
            explorer_cmds::source_context,
            explorer_cmds::build_predicate,
            explorer_cmds::build_missing_predicate,
            explorer_cmds::quote_value,
            explorer_cmds::saved_searches,
            explorer_cmds::save_search,
            explorer_cmds::delete_saved_search,
            explorer_cmds::column_sets,
            explorer_cmds::save_column_set,
            explorer_cmds::delete_column_set,
            explorer_cmds::recent_searches,
            explorer_cmds::delete_recent_search,
            explorer_cmds::clear_recent_searches,
            explorer_cmds::start_export,
            explorer_cmds::export_status,
            explorer_cmds::index_status,
            explorer_cmds::rebuild_indexes,
            explorer_cmds::list_import_profiles,
            case_cmds::list_investigations,
            case_cmds::create_investigation,
            case_cmds::update_investigation,
            case_cmds::set_investigation_status,
            case_cmds::investigation_bundle,
            case_cmds::investigation_activity,
            case_cmds::create_hypothesis,
            case_cmds::update_hypothesis,
            case_cmds::set_hypothesis_state,
            case_cmds::link_hypothesis_evidence,
            case_cmds::unlink_hypothesis_evidence,
            case_cmds::create_item,
            case_cmds::update_item_content,
            case_cmds::set_item_status,
            case_cmds::set_item_archived,
            case_cmds::reorder_case_children,
            case_cmds::create_evidence_group,
            case_cmds::rename_evidence_group,
            case_cmds::delete_evidence_group,
            case_cmds::update_evidence_annotation,
            case_cmds::set_evidence_group,
            case_cmds::set_evidence_archived,
            case_cmds::evidence_history,
            case_cmds::pin_event,
            case_cmds::pin_selection,
            case_cmds::pin_query,
            case_cmds::pin_group,
            case_cmds::pin_interval,
            case_cmds::pin_item,
            case_cmds::start_verify_evidence,
            case_cmds::evidence_restore_context
        ])
        .run(tauri::generate_context!())
        .expect("error while running LogScope");
}
