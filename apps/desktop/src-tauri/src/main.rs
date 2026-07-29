//! LogScope desktop shell (v0.0 architecture proof).
//!
//! Thin typed command boundary over `logscope-app` services. React owns
//! presentation only; every operation here delegates to shared services so
//! the future CLI/Agent API keep identical semantics.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use logscope_app::dto::*;
use logscope_app::{run_import, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::{JobControl, JobEvent};
use logscope_query::{query_log_page, EngineConnection, LogQueryRequest, QueryCancelHandle};
use logscope_store::FtsIndex;
use logscope_workspace::Workspace;
use parking_lot::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

struct AppState {
    workspace: Mutex<Option<Workspace>>,
    engine: Mutex<EngineConnection>,
    jobs: Mutex<HashMap<String, JobControl>>,
    import_running: AtomicBool,
}

type CmdResult<T> = Result<T, ErrorDto>;

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
    *slot = Some(ws);
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
    *slot = Some(ws);
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
    let Some(mut ws) = slot.take() else {
        state.import_running.store(false, Ordering::SeqCst);
        return Err(ErrorDto::new("workspace/none", "no workspace is open"));
    };
    drop(slot);

    let import_request = ImportRequest::new(
        request.paths.iter().map(PathBuf::from).collect(),
        profile,
        &request.dataset_name,
    );
    let job_id = format!("job-{}", uuid::Uuid::new_v4());
    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();

    // Forward job events onto the UI event bus.
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

    // Watcher restores the workspace when the job finishes.
    let watcher_app = app.clone();
    let watch_job_id = job_id.clone();
    std::thread::spawn(move || {
        let result = handle.join();
        let state = watcher_app.state::<AppState>();
        match result {
            Ok((_outcome, ws)) => {
                *state.workspace.lock() = Some(ws);
            }
            Err(e) => {
                // The job thread itself failed (panic isolation already
                // reported it); the workspace instance is lost with the
                // thread, so the UI must reopen from disk. Recovery on the
                // next open discards any staged leftovers.
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

#[tauri::command]
fn query_logs(state: State<'_, AppState>, request: LogQueryDto) -> CmdResult<LogPageDto> {
    let slot = state.workspace.lock();
    let ws = slot
        .as_ref()
        .ok_or_else(|| ErrorDto::new("workspace/none", "no workspace is open"))?;

    let dataset_ids = if request.dataset_ids.is_empty() {
        ws.meta
            .list_datasets()
            .map_err(ws_err)?
            .into_iter()
            .filter(|d| d.status == "published")
            .map(|d| d.dataset_id)
            .collect()
    } else {
        request.dataset_ids.clone()
    };
    let mut files = Vec::new();
    for id in &dataset_ids {
        files.extend(ws.segment_paths(id).map_err(ws_err)?);
    }
    let fts = FtsIndex::open(&ws.layout.fts_logs_path()).map_err(|e| ErrorDto::new(e.code(), e))?;

    let engine = state.engine.lock();
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let query = LogQueryRequest {
        dataset_ids,
        time_start: request.time_start,
        time_end: request.time_end,
        min_severity: request.min_severity,
        contains_text: request.contains_text.clone(),
        attr_equals: vec![],
        resource_id: None,
        trace_id: None,
        limit: request.limit,
        offset: request.offset,
    };
    let page = query_log_page(&engine, &files, &query, Some(&fts), &cancel, None)
        .map_err(|e| ErrorDto::new(e.code(), e))?;
    Ok(LogPageDto {
        rows: page
            .rows
            .into_iter()
            .map(|r| LogRowDto {
                event_time_text: r
                    .event_time
                    .map(|t| logscope_model::UnixNanos(t).to_rfc3339()),
                record_id: r.record_id,
                event_time: r.event_time,
                severity_text: r.severity_text,
                severity_number: r.severity_number,
                display_message: r.display_message,
                dataset_id: r.dataset_id,
                record_number: r.record_number,
                line_start: r.line_start,
                attributes_json: r.attributes_json,
            })
            .collect(),
        has_more: page.has_more,
        limit: page.limit,
    })
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

    let engine = EngineConnection::open_in_memory().expect("query engine init");
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            workspace: Mutex::new(None),
            engine: Mutex::new(engine),
            jobs: Mutex::new(HashMap::new()),
            import_running: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            recent_workspaces,
            create_workspace,
            open_workspace,
            close_workspace,
            overview,
            start_import,
            cancel_job,
            query_logs
        ])
        .run(tauri::generate_context!())
        .expect("error while running LogScope");
}
