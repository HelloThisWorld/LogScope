//! v0.2 application-flow proofs: import → analyze → query through the
//! Explorer services; pre-0.2 (schema v1) workspace migration without
//! source re-import; saved searches / column sets / recent searches across
//! reopen; bounded atomic export with truncation and cancellation.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use logscope_app::explorer;
use logscope_app::{run_export, run_import, ExportFormat, ExportSpec, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::{JobContext, JobControl};
use logscope_query::{
    query_counts, query_page, resolve_window, EngineConnection, PageRequest, QueryCancelHandle,
    TimeStrategy,
};
use logscope_query_lang::LANGUAGE_VERSION;
use logscope_workspace::Workspace;

fn write_es_jsonl(path: &Path, records: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    for i in 0..records {
        let level = match i % 5 {
            0 => "ERROR",
            1 => "WARN",
            _ => "INFO",
        };
        let outcome = if i % 5 == 0 { "failure" } else { "success" };
        writeln!(
            f,
            "{{\"@timestamp\":\"2024-06-01T10:{:02}:{:02}Z\",\"log.level\":\"{level}\",\
             \"message\":\"handler {} finished{}\",\"service.name\":\"orders\",\
             \"event.outcome\":\"{outcome}\",\"retry\":{{\"count\":{}}},\"idx\":{i}}}",
            (i / 60) % 60,
            i % 60,
            i,
            if i % 7 == 0 { " with timeout" } else { "" },
            i % 3,
        )
        .unwrap();
    }
}

/// Runs a job body on a plain context (foreground, no channel consumers).
fn fg_ctx(job_id: &str) -> (JobContext, JobControl) {
    let (ctx, control, rx) = JobContext::detached(job_id);
    std::mem::forget(rx); // progress events are irrelevant here
    (ctx, control)
}

fn import_file(ws: &mut Workspace, engine: &EngineConnection, file: &Path) -> String {
    let request = ImportRequest::new(
        vec![file.to_path_buf()],
        builtin::elasticsearch_export(),
        "es export",
    );
    let (ctx, _control) = fg_ctx(&format!("job-{}", uuid::Uuid::new_v4()));
    let outcome = run_import(ws, engine, &request, &ctx).expect("import succeeds");
    outcome.dataset_id
}

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    input: PathBuf,
}

fn env(records: usize) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    let input = dir.path().join("input.jsonl");
    write_es_jsonl(&input, records);
    Env {
        root,
        input,
        _dir: dir,
    }
}

#[test]
fn explorer_flow_import_query_save_reopen() {
    let e = env(300);
    let engine = EngineConnection::open_in_memory().unwrap();
    let mut ws = Workspace::create(&e.root, "case", "0.2.0-test").unwrap();
    let dataset_id = import_file(&mut ws, &engine, &e.input);

    // Selection, catalog, and index states are ready right after import.
    let selection = explorer::resolve_dataset_selection(&ws, &[]).unwrap();
    assert_eq!(selection, vec![dataset_id.clone()]);
    assert!(explorer::fts_ready(&ws, &selection).unwrap());
    let catalog = explorer::load_catalog(&ws, &selection).unwrap();
    assert!(catalog.complete);
    let fields: Vec<String> = catalog
        .field_entries()
        .into_iter()
        .map(|(d, ..)| d)
        .collect();
    assert!(fields.contains(&"service.name".to_string()));
    assert!(fields.contains(&"retry.count".to_string()), "{fields:?}");

    // Analyze + execute the documented first-query shape.
    let text =
        r#"service.name:orders AND severity:(ERROR OR WARN) AND "timeout" AND NOT retry.count:0"#;
    let analysis = explorer::analyze_query(&ws, &selection, text);
    assert!(
        analysis.resolved.is_some(),
        "diagnostics: {:?}",
        analysis.diagnostics
    );
    let filter = explorer::compile_for_execution(&ws, &selection, &analysis).unwrap();
    assert!(filter.used_fts(), "index is ready, so FTS path applies");
    let files = explorer::segment_files_for(&ws, &selection).unwrap();
    let latest = explorer::latest_event_time(&ws, &selection).unwrap();
    assert!(latest.is_some());
    let window = resolve_window(&TimeStrategy::All, latest);
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let page = query_page(
        &engine,
        &files,
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 100,
        },
        &cancel,
        None,
    )
    .unwrap();
    assert!(!page.rows.is_empty());
    // ERROR/WARN ∧ "timeout" ∧ retry ≠ 0: i%5∈{0,1} ∧ i%7==0 ∧ i%3≠0.
    let expected = (0..300)
        .filter(|i| (i % 5 == 0 || i % 5 == 1) && i % 7 == 0 && i % 3 != 0)
        .count();
    assert_eq!(page.rows.len(), expected);

    // Saved state: search, column set, recents.
    let resolved = analysis.resolved.as_ref().unwrap();
    ws.meta
        .upsert_saved_search(
            "ss-1",
            "workflow timeouts",
            text,
            LANGUAGE_VERSION as i64,
            &resolved.fingerprint,
            r#"{"kind":"all"}"#,
            r#"{"kind":"all"}"#,
            None,
        )
        .unwrap();
    ws.meta
        .upsert_column_set(
            "cs-1",
            "triage",
            r#"[{"field":"timestamp"},{"field":"severity"},{"field":"message"},{"field":"service.name"}]"#,
            true,
        )
        .unwrap();
    for _ in 0..3 {
        ws.meta
            .touch_recent_search(
                text,
                LANGUAGE_VERSION as i64,
                &resolved.fingerprint,
                r#"{"kind":"all"}"#,
                r#"{"kind":"all"}"#,
            )
            .unwrap();
    }
    assert_eq!(ws.meta.list_recent_searches().unwrap().len(), 1);
    assert_eq!(ws.meta.list_recent_searches().unwrap()[0].run_count, 3);

    // Close and reopen: everything survives; the query runs again without
    // re-importing anything.
    drop(ws);
    let ws2 = Workspace::open(&e.root, "0.2.0-test").unwrap();
    let saved = ws2.meta.list_saved_searches().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].query_text, text);
    assert_eq!(saved[0].language_version, LANGUAGE_VERSION as i64);
    let cols = ws2.meta.list_column_sets().unwrap();
    assert_eq!(cols.len(), 1);
    assert!(cols[0].is_default);
    assert_eq!(ws2.meta.list_recent_searches().unwrap().len(), 1);

    let selection2 = explorer::resolve_dataset_selection(&ws2, &[]).unwrap();
    let analysis2 = explorer::analyze_query(&ws2, &selection2, &saved[0].query_text);
    let filter2 = explorer::compile_for_execution(&ws2, &selection2, &analysis2).unwrap();
    assert_eq!(
        analysis2.resolved.unwrap().fingerprint,
        saved[0].fingerprint,
        "reloaded saved search keeps its meaning"
    );
    let files2 = explorer::segment_files_for(&ws2, &selection2).unwrap();
    let page2 = query_page(
        &engine,
        &files2,
        &filter2,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 100,
        },
        &cancel,
        None,
    )
    .unwrap();
    assert_eq!(page2.rows.len(), expected);

    // Recent-search local delete works.
    let recents = ws2.meta.list_recent_searches().unwrap();
    assert!(ws2.meta.delete_recent_search(recents[0].recent_id).unwrap());
    assert!(ws2.meta.list_recent_searches().unwrap().is_empty());
}

/// Builds a schema-v1 (pre-0.2) workspace exactly as the 0.0 build laid it
/// out: manifest, 0001 schema only, published dataset + parquet segment,
/// v1-tokenizer FTS file. Then proves 0.2 migrates and queries it without
/// re-importing the source.
#[test]
fn pre_v02_workspace_migrates_and_stays_searchable() {
    let e = env(120);
    let engine = EngineConnection::open_in_memory().unwrap();

    // Step 1: produce real published data with the current pipeline in a
    // scratch workspace.
    let scratch_root = e.root.with_file_name("scratch-ws");
    let mut scratch = Workspace::create(&scratch_root, "scratch", "0.0.0").unwrap();
    let dataset_id = import_file(&mut scratch, &engine, &e.input);
    let seg_files = scratch.segment_paths(&dataset_id).unwrap();
    let seg_rows = scratch.meta.segments_for_dataset(&dataset_id).unwrap();
    let dataset_row = scratch.meta.get_dataset(&dataset_id).unwrap().unwrap();
    drop(scratch);

    // Step 2: assemble the v1 workspace by hand (as a 0.0 build would have
    // left it): 0001 schema only, v1 FTS tokenizer, manifest v1.
    let v1_root = e.root.with_file_name("v1-ws");
    std::fs::create_dir_all(v1_root.join("data").join(&dataset_id)).unwrap();
    std::fs::create_dir_all(v1_root.join("indexes")).unwrap();
    std::fs::create_dir_all(v1_root.join("staging")).unwrap();
    for f in &seg_files {
        let dest = v1_root
            .join("data")
            .join(&dataset_id)
            .join(f.file_name().unwrap());
        std::fs::copy(f, dest).unwrap();
    }
    {
        let conn = rusqlite::Connection::open(v1_root.join("workspace.db")).unwrap();
        conn.execute_batch(include_str!(
            "../../logscope-workspace/src/migrations/0001_init.sql"
        ))
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL) STRICT;
             INSERT INTO schema_migrations VALUES (1, '0001_init', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO datasets (dataset_id, name, signal, status, created_at, published_at)
             VALUES (?1, ?2, 'logs', 'published', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![dataset_id, dataset_row.name],
        )
        .unwrap();
        for s in &seg_rows {
            conn.execute(
                "INSERT INTO segments (segment_id, dataset_id, signal, file_name, row_count,
                        byte_size, min_event_time, max_event_time, created_at, fts_indexed)
                 VALUES (?1, ?2, 'logs', ?3, ?4, ?5, ?6, ?7, '2026-01-01T00:00:00Z', 1)",
                rusqlite::params![
                    s.segment_id,
                    dataset_id,
                    s.file_name,
                    s.row_count,
                    s.byte_size,
                    s.min_event_time,
                    s.max_event_time
                ],
            )
            .unwrap();
        }
        // Marker row in an unrelated table that migration must preserve.
        conn.execute(
            "INSERT INTO workspace_info (key, value) VALUES ('v1-marker', 'kept')",
            [],
        )
        .unwrap();
    }
    {
        // v0.0 FTS index: old tokenizer, no user_version stamp.
        let conn = rusqlite::Connection::open(v1_root.join("indexes").join("fts-logs.db")).unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE fts_logs USING fts5(
                message, record_id UNINDEXED, dataset_id UNINDEXED, segment_id UNINDEXED,
                tokenize = 'unicode61');",
        )
        .unwrap();
    }
    std::fs::write(
        v1_root.join("manifest.json"),
        serde_json::json!({
            "manifest_version": 1,
            "workspace_id": "ws-v1-fixture",
            "name": "pre-0.2 case",
            "product_version": "0.0.0",
            "schema_version": 1,
            "created_at": "2026-01-01T00:00:00Z",
            "available_signals": ["logs"],
        })
        .to_string(),
    )
    .unwrap();

    // Step 3: open with the 0.2 build → transactional migration to v2.
    let ws = Workspace::open(&v1_root, "0.2.0-test").unwrap();
    assert_eq!(ws.manifest.schema_version, 2);
    assert_eq!(
        ws.meta.get_info("v1-marker").unwrap().as_deref(),
        Some("kept"),
        "unrelated workspace data preserved"
    );
    // Migration seeded the derived-index lifecycle as pending.
    let fts_states = ws.meta.index_states("fts").unwrap();
    assert_eq!(fts_states.len(), 1);
    assert_eq!(fts_states[0].status, "pending");
    assert!(!explorer::fts_ready(&ws, std::slice::from_ref(&dataset_id)).unwrap());

    // Step 4: the data is searchable immediately (exact fallback path),
    // with no source re-import.
    let selection = explorer::resolve_dataset_selection(&ws, &[]).unwrap();
    assert_eq!(selection, vec![dataset_id.clone()]);
    let analysis = explorer::analyze_query(&ws, &selection, r#""timeout" severity:ERROR"#);
    // Catalog is still pending → attr fields unknown, but canonical + text
    // queries resolve.
    assert!(
        analysis.resolved.is_some(),
        "diagnostics: {:?}",
        analysis.diagnostics
    );
    let filter = explorer::compile_for_execution(&ws, &selection, &analysis).unwrap();
    assert!(filter.used_fallback_scan());
    assert!(!filter.used_fts());
    let files = explorer::segment_files_for(&ws, &selection).unwrap();
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let window = resolve_window(&TimeStrategy::All, None);
    let fallback_counts = query_counts(&engine, &files, &filter, &window, &cancel, None).unwrap();
    assert!(fallback_counts.matching > 0);

    // Step 5: rebuild derived indexes (cancellable jobs), then the indexed
    // path returns identical results.
    let (ctx, _c) = fg_ctx("job-rebuild");
    explorer::rebuild_fts_to_current(&ws, &engine, &ctx).unwrap();
    explorer::build_field_catalog(&ws, &engine, &dataset_id, &ctx).unwrap();
    assert!(explorer::fts_ready(&ws, &selection).unwrap());
    let analysis2 = explorer::analyze_query(&ws, &selection, r#""timeout" severity:ERROR"#);
    let filter2 = explorer::compile_for_execution(&ws, &selection, &analysis2).unwrap();
    assert!(filter2.used_fts());
    let indexed_counts = query_counts(&engine, &files, &filter2, &window, &cancel, None).unwrap();
    assert_eq!(indexed_counts.matching, fallback_counts.matching);

    // The attribute catalog now resolves dynamic fields too.
    let analysis3 = explorer::analyze_query(&ws, &selection, "service.name:orders");
    assert!(analysis3.resolved.is_some());

    // Interrupted rebuild recovery: flip one state back to pending and run
    // the rebuild again — idempotent re-index, still consistent.
    ws.meta
        .set_index_state("fts", &dataset_id, 1, "pending", "{}")
        .unwrap();
    let (ctx2, _c2) = fg_ctx("job-rebuild-2");
    explorer::rebuild_fts_to_current(&ws, &engine, &ctx2).unwrap();
    let counts3 = query_counts(&engine, &files, &filter2, &window, &cancel, None).unwrap();
    assert_eq!(counts3.matching, fallback_counts.matching);
}

#[test]
fn export_is_bounded_streamed_atomic_and_cancellable() {
    let e = env(200);
    let engine = EngineConnection::open_in_memory().unwrap();
    let mut ws = Workspace::create(&e.root, "case", "0.2.0-test").unwrap();
    let _dataset_id = import_file(&mut ws, &engine, &e.input);
    let selection = explorer::resolve_dataset_selection(&ws, &[]).unwrap();
    let files = explorer::segment_files_for(&ws, &selection).unwrap();
    let analysis = explorer::analyze_query(&ws, &selection, "severity:ERROR");
    let filter = explorer::compile_for_execution(&ws, &selection, &analysis).unwrap();
    let window = resolve_window(&TimeStrategy::All, None);
    let out_dir = e.root.parent().unwrap().join("exports");

    // JSONL export: complete, one JSON object per line, page-order stable.
    let dest_jsonl = out_dir.join("errors.jsonl");
    let spec = ExportSpec {
        format: ExportFormat::Jsonl,
        destination: dest_jsonl.clone(),
        row_limit: 10_000,
        byte_limit: 50 * 1024 * 1024,
        csv_columns: vec![],
        csv_formula_guard: true,
    };
    let (ctx, _c) = fg_ctx("job-export-1");
    let outcome = run_export(&engine, &files, &filter, &window, &spec, &ctx).unwrap();
    assert!(!outcome.truncated);
    assert_eq!(outcome.rows_written, 40, "200 records, i%5==0 are ERROR");
    let body = std::fs::read_to_string(&dest_jsonl).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 40);
    for line in &lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("complete JSON per line");
        assert_eq!(v["severity"], "ERROR");
        assert!(v["attributes"].is_object(), "typed attributes preserved");
    }
    // Overwrite protection.
    let (ctx2, _c2) = fg_ctx("job-export-2");
    let err = run_export(&engine, &files, &filter, &window, &spec, &ctx2).unwrap_err();
    assert_eq!(err.code, "export/destination-exists");

    // CSV export with row-limit truncation + formula guard.
    let dest_csv = out_dir.join("errors.csv");
    let spec_csv = ExportSpec {
        format: ExportFormat::Csv,
        destination: dest_csv.clone(),
        row_limit: 10,
        byte_limit: 50 * 1024 * 1024,
        csv_columns: vec![
            "timestamp".into(),
            "severity".into(),
            "message".into(),
            "retry.count".into(),
        ],
        csv_formula_guard: true,
    };
    let (ctx3, _c3) = fg_ctx("job-export-3");
    let outcome_csv = run_export(&engine, &files, &filter, &window, &spec_csv, &ctx3).unwrap();
    assert!(outcome_csv.truncated, "row limit reached must be visible");
    assert_eq!(outcome_csv.rows_written, 10);
    let csv_body = std::fs::read_to_string(&dest_csv).unwrap();
    let csv_lines: Vec<&str> = csv_body.lines().collect();
    assert_eq!(csv_lines.len(), 11, "header + 10 rows");
    assert_eq!(csv_lines[0], "timestamp,severity,message,retry.count");
    assert!(csv_lines[1].contains("ERROR"));

    // Byte-limit truncation never writes a partial record.
    let dest_small = out_dir.join("small.jsonl");
    let spec_small = ExportSpec {
        format: ExportFormat::Jsonl,
        destination: dest_small.clone(),
        row_limit: 10_000,
        byte_limit: 2_048,
        csv_columns: vec![],
        csv_formula_guard: true,
    };
    let (ctx4, _c4) = fg_ctx("job-export-4");
    let out_small = run_export(&engine, &files, &filter, &window, &spec_small, &ctx4).unwrap();
    assert!(out_small.truncated);
    assert!(out_small.bytes_written <= 2_048);
    let small_body = std::fs::read_to_string(&dest_small).unwrap();
    for line in small_body.lines() {
        serde_json::from_str::<serde_json::Value>(line).expect("no partial final record");
    }

    // Cancellation: no destination file, no partial leftovers.
    let dest_cancel = out_dir.join("cancelled.jsonl");
    let (ctx5, control5) = fg_ctx("job-export-5");
    control5.cancel();
    let spec_cancel = ExportSpec {
        format: ExportFormat::Jsonl,
        destination: dest_cancel.clone(),
        row_limit: 10_000,
        byte_limit: 50 * 1024 * 1024,
        csv_columns: vec![],
        csv_formula_guard: true,
    };
    let err = run_export(&engine, &files, &filter, &window, &spec_cancel, &ctx5).unwrap_err();
    assert_eq!(err.code, "job/cancelled");
    assert!(!dest_cancel.exists());
    let leftovers: Vec<_> = std::fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("partial"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "partial files cleaned up: {leftovers:?}"
    );

    // CSV formula-injection guard on a hostile message.
    let hostile = out_dir.join("hostile.csv");
    let es = e.input.with_file_name("hostile.jsonl");
    std::fs::write(
        &es,
        "{\"@timestamp\":\"2024-06-01T10:00:00Z\",\"log.level\":\"ERROR\",\
         \"message\":\"=cmd|' /C calc'!A0\",\"service.name\":\"orders\"}\n",
    )
    .unwrap();
    let ds2 = import_file(&mut ws, &engine, &es);
    let sel2 = vec![ds2];
    let files2 = explorer::segment_files_for(&ws, &sel2).unwrap();
    let analysis2 = explorer::analyze_query(&ws, &sel2, "");
    let filter2 = explorer::compile_for_execution(&ws, &sel2, &analysis2).unwrap();
    let (ctx6, _c6) = fg_ctx("job-export-6");
    let spec_h = ExportSpec {
        format: ExportFormat::Csv,
        destination: hostile.clone(),
        row_limit: 10,
        byte_limit: 1024 * 1024,
        csv_columns: vec!["message".into()],
        csv_formula_guard: true,
    };
    run_export(&engine, &files2, &filter2, &window, &spec_h, &ctx6).unwrap();
    let hostile_body = std::fs::read_to_string(&hostile).unwrap();
    assert!(
        hostile_body.lines().any(|l| l.starts_with("'=cmd")),
        "formula guard prefixes the cell: {hostile_body}"
    );
}
