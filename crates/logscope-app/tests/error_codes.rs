//! W9 stable error-code regression sweep — the one place that asserts
//! the complete registry.
//!
//! Three layers of protection:
//! 1. `REGISTRY` is the documented set of stable codes. It must stay
//!    sorted, unique, and well-formed.
//! 2. A source scan over every crate and the Tauri boundary extracts
//!    each `"layer/kebab-code"` literal and asserts exact equality with
//!    the registry — adding, removing, or typo-ing a code anywhere
//!    without updating this file fails the suite.
//! 3. Every code cheaply reachable from the service layer is produced
//!    through its REAL path below and asserted by code string. Deep
//!    codes (store/otlp/lang/io families…) are produced in their owning
//!    crates' suites; the registry still pins their names here.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use logscope_app::case::{self, PinCommon, QueryScope};
use logscope_app::redact::Projection;
use logscope_app::{bundle, explorer, report, run_import, timeline, ImportRequest};
use logscope_case::vocab::ItemKind;
use logscope_ingest::builtin;
use logscope_jobs::{JobContext, JobControl, JobError};
use logscope_query::{query_page, EngineConnection, PageRequest, QueryCancelHandle, TimeStrategy};
use logscope_workspace::{NewHypothesis, NewInvestigation, NewMarker, Workspace, WorkspaceError};

/// Every stable error code, sorted. This list IS the contract.
const REGISTRY: &[&str] = &[
    "analysis/derived",
    "analysis/invalid-definition",
    "analysis/stale-run",
    "analysis/unsupported-version",
    "bundle/data",
    "bundle/data-too-large",
    "bundle/destination-exists",
    "bundle/invalid",
    "bundle/io",
    "bundle/report-modified",
    "bundle/serialize",
    "bundle/unsupported-version",
    "case/empty-scope",
    "case/invalid",
    "case/invalid-timestamp",
    "case/invalid-value",
    "case/investigation-archived",
    "case/undecodable-reference",
    "case/unsupported-envelope",
    "export/destination-exists",
    "export/failed",
    "export/invalid-destination",
    "export/invalid-format",
    "export/io",
    "export/not-found",
    "export/publish",
    "export/serialize",
    "import/busy",
    "import/invalid-argument",
    "import/invalid-profile",
    "import/io",
    "import/unsupported-format",
    "ingest/archive-limit",
    "ingest/invalid-profile",
    "ingest/io",
    "ingest/json",
    "ingest/unsupported-format",
    "job/cancelled",
    "job/interrupted",
    "job/panic",
    "lang/alias-shadows-attribute",
    "lang/always-present",
    "lang/ambiguous-field",
    "lang/broad-regex",
    "lang/empty-phrase",
    "lang/empty-regex",
    "lang/empty-term",
    "lang/exists-op",
    "lang/group-op",
    "lang/invalid-escape",
    "lang/invalid-severity",
    "lang/invalid-timestamp",
    "lang/leading-wildcard",
    "lang/missing-value",
    "lang/query-too-long",
    "lang/regex-needs-field",
    "lang/regex-op",
    "lang/regex-too-long",
    "lang/regex-unsupported",
    "lang/too-deep",
    "lang/too-many-clauses",
    "lang/too-many-tokens",
    "lang/trailing-backslash",
    "lang/type-conflict",
    "lang/type-mismatch",
    "lang/unbalanced-paren",
    "lang/unexpected-char",
    "lang/unexpected-end",
    "lang/unexpected-token",
    "lang/unknown-field",
    "lang/unsupported-regex-flag",
    "lang/unsupported-type",
    "lang/unterminated-regex",
    "lang/unterminated-string",
    "lang/wildcard-in-text",
    "lang/wildcard-too-long",
    "otlp/invalid-envelope",
    "otlp/invalid-span-id",
    "otlp/invalid-trace-id",
    "otlp/missing-point-value",
    "otlp/timestamp-out-of-range",
    "otlp/unknown-metric-type",
    "parse/invalid-csv",
    "parse/invalid-json",
    "parse/record-too-large",
    "parse/truncated-record",
    "query/busy",
    "query/cancelled",
    "query/engine",
    "query/fts",
    "query/invalid",
    "query/invalid-parameter",
    "query/invalid-time-strategy",
    "query/io",
    "query/no-datasets",
    "query/record-not-found",
    "query/store",
    "query/timeout",
    "query/unknown-field",
    "redaction/invalid-profile",
    "report/destination-exists",
    "report/invalid-definition",
    "report/invalid-destination",
    "report/invalid-format",
    "report/io",
    "report/publish",
    "store/arrow",
    "store/fts",
    "store/io",
    "store/json",
    "store/parquet",
    "workspace/already-exists",
    "workspace/busy",
    "workspace/db",
    "workspace/importing",
    "workspace/invalid-argument",
    "workspace/io",
    "workspace/manifest",
    "workspace/manifest-too-new",
    "workspace/missing-entity",
    "workspace/none",
    "workspace/not-a-workspace",
    "workspace/schema-too-new",
    "workspace/stale-revision",
];

#[test]
fn registry_is_sorted_unique_and_well_formed() {
    let shape = regex::Regex::new(r"^[a-z]+/[a-z0-9]+(-[a-z0-9]+)*$").unwrap();
    for pair in REGISTRY.windows(2) {
        assert!(
            pair[0] < pair[1],
            "registry must stay sorted+unique: {pair:?}"
        );
    }
    for code in REGISTRY {
        assert!(shape.is_match(code), "malformed code: {code}");
    }
}

/// Recursively collects `.rs` files under `dir`, skipping build output.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.is_dir() {
            if name != "target" && name != "node_modules" {
                rs_files(&path, out);
            }
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn every_code_in_source_is_registered_and_every_registered_code_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let src = entry.unwrap().path().join("src");
        if src.is_dir() {
            rs_files(&src, &mut files);
        }
    }
    let tauri_src = root
        .join("apps")
        .join("desktop")
        .join("src-tauri")
        .join("src");
    assert!(tauri_src.is_dir(), "Tauri boundary sources must be scanned");
    rs_files(&tauri_src, &mut files);
    assert!(files.len() > 40, "the scan must actually find the sources");

    let extract = regex::Regex::new(
        r#""((?:analysis|workspace|case|report|bundle|redaction|job|import|export|query|parse|ingest|otlp|lang|store)/[a-z0-9-]+)""#,
    )
    .unwrap();
    let mut observed: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for cap in extract.captures_iter(&text) {
            observed.insert(cap[1].to_string());
        }
    }

    let registered: std::collections::BTreeSet<String> =
        REGISTRY.iter().map(|s| s.to_string()).collect();
    let unregistered: Vec<&String> = observed.difference(&registered).collect();
    assert!(
        unregistered.is_empty(),
        "codes in source but not in the registry (register them here): {unregistered:?}"
    );
    let vanished: Vec<&String> = registered.difference(&observed).collect();
    assert!(
        vanished.is_empty(),
        "registered codes no longer produced anywhere (remove or restore): {vanished:?}"
    );
}

// ---- real-path production ----------------------------------------------------

fn fg_ctx(job_id: &str) -> (JobContext, JobControl) {
    let (ctx, control, rx) = JobContext::detached(job_id);
    std::mem::forget(rx);
    (ctx, control)
}

fn ws_code(e: WorkspaceError) -> &'static str {
    e.code()
}

#[test]
fn workspace_and_pure_service_codes_come_from_their_real_paths() {
    let dir = tempfile::tempdir().unwrap();

    // workspace/not-a-workspace — opening a plain directory.
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        ws_code(Workspace::open(&empty, "t").err().unwrap()),
        "workspace/not-a-workspace"
    );

    // workspace/already-exists — creating on top of a workspace.
    let root = dir.path().join("ws");
    let ws = Workspace::create(&root, "t", "0.3.0-test").unwrap();
    assert_eq!(
        ws_code(Workspace::create(&root, "t", "t").err().unwrap()),
        "workspace/already-exists"
    );

    // workspace/db — the UNIQUE key on a duplicate insert surfaces as a
    // database error, not a silent overwrite.
    let new_inv = NewInvestigation {
        investigation_id: "inv-1".into(),
        title: "t".into(),
        description: None,
        severity: None,
        owner_text: None,
        tags_json: "[]".into(),
        incident_started_at: None,
        window_start: None,
        window_end: None,
    };
    let inv = ws.meta.create_investigation(&new_inv).unwrap();
    assert_eq!(
        ws_code(ws.meta.create_investigation(&new_inv).unwrap_err()),
        "workspace/db"
    );

    // workspace/invalid-argument — a marker window end without a start.
    let bad_marker = NewMarker {
        marker_id: "mark-bad".into(),
        investigation_id: inv.investigation_id.clone(),
        kind: "deployment".into(),
        label: "x".into(),
        description: None,
        at_nanos: None,
        end_nanos: Some(5),
        original_tz_offset_min: None,
        original_time_text: None,
    };
    assert_eq!(
        ws_code(ws.meta.create_marker(&bad_marker).unwrap_err()),
        "workspace/invalid-argument"
    );

    // workspace/stale-revision vs workspace/missing-entity — the
    // optimistic guard distinguishes the two honestly.
    let hyp = ws
        .meta
        .create_hypothesis(&NewHypothesis {
            hypothesis_id: "hyp-1".into(),
            investigation_id: inv.investigation_id.clone(),
            statement: "s".into(),
            rationale: None,
        })
        .unwrap();
    assert_eq!(
        ws_code(
            ws.meta
                .update_hypothesis(&hyp.hypothesis_id, 999, "s2", None)
                .unwrap_err()
        ),
        "workspace/stale-revision"
    );
    assert_eq!(
        ws_code(
            ws.meta
                .update_hypothesis("hyp-none", 1, "s2", None)
                .unwrap_err()
        ),
        "workspace/missing-entity"
    );

    // workspace/missing-entity at the service boundary too.
    assert_eq!(
        timeline::timeline(&ws, "inv-none").unwrap_err().code,
        "workspace/missing-entity"
    );

    // case/empty-scope — pinning a query with no published log dataset.
    let engine = EngineConnection::open_in_memory().unwrap();
    let err = case::pin_query(
        &ws,
        &engine,
        &case::PinQueryRequest {
            common: PinCommon {
                investigation_id: inv.investigation_id.clone(),
                title: "q".into(),
                annotation: None,
                relevance: None,
                group_id: None,
            },
            scope: QueryScope {
                query_text: String::new(),
                dataset_ids: vec![],
                time_strategy: TimeStrategy::All,
            },
            saved_search_id: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, "case/empty-scope");

    // workspace/manifest-too-new — a manifest written by a newer build.
    let manifest_file = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_file).unwrap()).unwrap();
    manifest["manifest_version"] = serde_json::json!(9999);
    drop(ws);
    std::fs::write(&manifest_file, serde_json::to_string(&manifest).unwrap()).unwrap();
    assert_eq!(
        ws_code(Workspace::open(&root, "t").err().unwrap()),
        "workspace/manifest-too-new"
    );

    // workspace/schema-too-new — a metadata schema from the future. The
    // migration row shape matches db.rs's own migration test.
    let root2 = dir.path().join("ws2");
    let ws2 = Workspace::create(&root2, "t", "0.3.0-test").unwrap();
    drop(ws2);
    let conn = rusqlite::Connection::open(root2.join("workspace.db")).unwrap();
    conn.execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (999999, 'future', 'now')",
        [],
    )
    .unwrap();
    drop(conn);
    assert_eq!(
        ws_code(Workspace::open(&root2, "t").err().unwrap()),
        "workspace/schema-too-new"
    );

    // Pure validators and their stable codes.
    assert_eq!(
        timeline::parse_marker_time("nope").unwrap_err().code,
        "case/invalid-timestamp"
    );
    assert_eq!(
        logscope_case::validate_item_shape(ItemKind::Task, Some("bogus"), None)
            .unwrap_err()
            .code(),
        "case/invalid-value"
    );
    assert_eq!(
        logscope_case::validate_tags_json("\"x\"")
            .unwrap_err()
            .code(),
        "case/invalid"
    );
    assert_eq!(
        report::parse_sections("[{\"kind\":\"bogus\"}]")
            .unwrap_err()
            .code,
        "report/invalid-definition"
    );
    assert_eq!(
        Projection::compile("nope", "").err().unwrap().code,
        "redaction/invalid-profile"
    );
    assert_eq!(
        bundle::check_entry_path("../up").unwrap_err().code,
        "bundle/invalid"
    );
    // job/cancelled is minted by exactly one constructor: the Cancelled →
    // JobError conversion every checkpoint uses.
    assert_eq!(
        JobError::from(logscope_jobs::Cancelled).code,
        "job/cancelled"
    );

    // bundle/io — a bundle path that cannot be read.
    let err = bundle::import_bundle(
        &dir.path().join("missing.logscope-case"),
        &dir.path().join("fresh-root"),
        "x",
        "t",
    )
    .unwrap_err();
    assert_eq!(err.code, "bundle/io");

    // bundle/unsupported-version — a crafted bundle from a newer build.
    let manifest_entry = manifest_for_version(99);
    let zip_bytes = zip_of(&[("manifest.json", manifest_entry.as_slice())]);
    let bundle_path = dir.path().join("future.logscope-case");
    std::fs::write(&bundle_path, &zip_bytes).unwrap();
    let err = bundle::import_bundle(&bundle_path, &dir.path().join("fresh-root-2"), "x", "t")
        .unwrap_err();
    assert_eq!(err.code, "bundle/unsupported-version");
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

fn manifest_for_version(version: u32) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "bundle_schema_version": version,
        "min_compatible_version": version,
        "investigation_id": "inv-x",
        "investigation_title": "x",
        "investigation_revision": 1,
        "reproduction_scope": "snapshot_only",
        "entries": [],
        "envelope_version": 1,
        "app_version": "test",
        "disclosure": null,
        "inclusions": {},
    }))
    .unwrap()
}

#[test]
fn import_query_report_and_bundle_codes_come_from_their_real_paths() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jsonl");
    std::fs::write(
        &input,
        "{\"@timestamp\":\"2024-06-01T00:00:00Z\",\"level\":\"INFO\",\"message\":\"one\"}\n\
         not json at all\n\
         {\"@timestamp\":\"2024-06-01T00:00:01Z\",\"level\":\"INFO\",\"message\":\"two\"}\n\
         {\"broken\": \"trunc",
    )
    .unwrap();

    let mut ws = Workspace::create(&dir.path().join("ws"), "t", "0.3.0-test").unwrap();
    let engine = EngineConnection::open_in_memory().unwrap();

    // import/invalid-argument — an import with no files.
    let (ctx, _c) = fg_ctx("job-e1");
    let err = run_import(
        &mut ws,
        &engine,
        &ImportRequest::new(vec![], builtin::jsonl_generic(), "none"),
        &ctx,
    )
    .unwrap_err();
    assert_eq!(err.code, "import/invalid-argument");

    // import/invalid-profile — a profile that fails its own validation.
    let mut broken_profile = builtin::jsonl_generic();
    broken_profile.profile_id = String::new();
    let ok = dir.path().join("ok.jsonl");
    std::fs::write(&ok, "{}\n").unwrap();
    let (ctx, _c) = fg_ctx("job-e2");
    let err = run_import(
        &mut ws,
        &engine,
        &ImportRequest::new(vec![ok.clone()], broken_profile, "broken"),
        &ctx,
    )
    .unwrap_err();
    assert_eq!(err.code, "import/invalid-profile");

    // import/unsupported-format — a CSV header row that is not UTF-8
    // cannot be read as CSV at all.
    let bad_csv = dir.path().join("bad.csv");
    std::fs::write(&bad_csv, [0xff_u8, 0xfe, 0x00, 0x67, 0x61, 0x72]).unwrap();
    let (ctx, _c) = fg_ctx("job-e2b");
    let err = run_import(
        &mut ws,
        &engine,
        &ImportRequest::new(vec![bad_csv], builtin::csv_basic(), "badcsv"),
        &ctx,
    )
    .unwrap_err();
    assert_eq!(err.code, "import/unsupported-format");

    // ingest/io — a source file that does not exist fails at
    // registration/hashing, which runs before the reader opens (the
    // reader's own `import/io` covers the between-steps race).
    let (ctx, _c) = fg_ctx("job-e3");
    let err = run_import(
        &mut ws,
        &engine,
        &ImportRequest::new(
            vec![dir.path().join("gone.jsonl")],
            builtin::jsonl_generic(),
            "gone",
        ),
        &ctx,
    )
    .unwrap_err();
    assert_eq!(err.code, "ingest/io");

    // A real import whose reject ledger carries the parse/* codes.
    let (ctx, _c) = fg_ctx("job-e4");
    let outcome = run_import(
        &mut ws,
        &engine,
        &ImportRequest::new(vec![input], builtin::jsonl_generic(), "mixed"),
        &ctx,
    )
    .unwrap();
    assert_eq!(outcome.accepted, 2);
    let rejects = ws
        .meta
        .rejected_for_dataset(&outcome.dataset_id, 100, 0)
        .unwrap();
    assert!(rejects
        .iter()
        .any(|r| r.reason_code == "parse/invalid-json"));
    assert!(rejects
        .iter()
        .any(|r| r.reason_code == "parse/truncated-record"));

    let inv = ws
        .meta
        .create_investigation(&NewInvestigation {
            investigation_id: "inv-e".into(),
            title: "t".into(),
            description: None,
            severity: None,
            owner_text: None,
            tags_json: "[]".into(),
            incident_started_at: None,
            window_start: None,
            window_end: None,
        })
        .unwrap();

    // query/invalid — a pinned query that does not validate.
    let err = case::pin_query(
        &ws,
        &engine,
        &case::PinQueryRequest {
            common: PinCommon {
                investigation_id: inv.investigation_id.clone(),
                title: "bad".into(),
                annotation: None,
                relevance: None,
                group_id: None,
            },
            scope: QueryScope {
                query_text: "(((".into(),
                dataset_ids: vec![outcome.dataset_id.clone()],
                time_strategy: TimeStrategy::All,
            },
            saved_search_id: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, "query/invalid");

    // query/cancelled — a cancellation requested before execution must
    // still cancel (the F-1 regression: pre-execution interrupts were
    // dropped by the engine).
    let selection = explorer::resolve_dataset_selection(&ws, &[]).unwrap();
    let files = explorer::segment_files_for(&ws, &selection).unwrap();
    let analysis = explorer::analyze_query(&ws, &selection, "");
    let filter = explorer::compile_for_execution(&ws, &selection, &analysis).unwrap();
    let window = logscope_query::resolve_window(&TimeStrategy::All, None);
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    cancel.cancel();
    let err = query_page(
        &engine,
        &files,
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: 10,
        },
        &cancel,
        None,
    )
    .unwrap_err();
    assert_eq!(err.code(), "query/cancelled");

    // case/investigation-archived — pinning into an archived case.
    ws.meta
        .set_investigation_status(&inv.investigation_id, inv.revision, "archived", "archived")
        .unwrap();
    let err = case::pin_event(
        &ws,
        &engine,
        &case::PinEventRequest {
            common: PinCommon {
                investigation_id: inv.investigation_id.clone(),
                title: "x".into(),
                annotation: None,
                relevance: None,
                group_id: None,
            },
            dataset_id: outcome.dataset_id.clone(),
            record_id: "log-0".into(),
            display_fields: vec![],
            include_raw_excerpt: false,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, "case/investigation-archived");
    let inv = ws
        .meta
        .get_investigation(&inv.investigation_id)
        .unwrap()
        .unwrap();
    ws.meta
        .set_investigation_status(&inv.investigation_id, inv.revision, "open", "restored")
        .unwrap();

    // report/destination-exists + report/invalid-destination.
    let def = ws
        .meta
        .create_report_def(&logscope_workspace::NewReportDef {
            report_def_id: "rep-e".into(),
            investigation_id: inv.investigation_id.clone(),
            title: "r".into(),
            subtitle: None,
            sections_json: "[{\"kind\":\"summary\",\"content\":\"s\"}]".into(),
            selected_evidence_json: "[]".into(),
            selected_markers_json: "[]".into(),
            options_json: "{}".into(),
        })
        .unwrap();
    let out = dir.path().join("r.md");
    report::generate_report(
        &ws,
        &def.report_def_id,
        report::ReportFormat::Markdown,
        &out,
    )
    .unwrap();
    let err = report::generate_report(
        &ws,
        &def.report_def_id,
        report::ReportFormat::Markdown,
        &out,
    )
    .unwrap_err();
    assert_eq!(err.code, "report/destination-exists");
    let err = report::generate_report(
        &ws,
        &def.report_def_id,
        report::ReportFormat::Markdown,
        Path::new("bare-name.md"),
    )
    .unwrap_err();
    assert_eq!(err.code, "report/invalid-destination");

    // bundle/destination-exists.
    let bundle_out = dir.path().join("case.logscope-case");
    std::fs::write(&bundle_out, "occupied").unwrap();
    let err = bundle::export_bundle(
        &ws,
        &engine,
        &inv.investigation_id,
        &bundle_out,
        &bundle::BundleOptions::default(),
    )
    .unwrap_err();
    assert_eq!(err.code, "bundle/destination-exists");

    // bundle/report-modified — an artifact tampered after generation is
    // refused instead of silently bundling the modified bytes.
    let artifact_path = dir.path().join("tampered.md");
    let artifact = report::generate_report(
        &ws,
        &def.report_def_id,
        report::ReportFormat::Markdown,
        &artifact_path,
    )
    .unwrap();
    assert_eq!(artifact.status, "completed");
    let mut bytes = std::fs::read(&artifact_path).unwrap();
    bytes.extend_from_slice(b"\ntampered");
    std::fs::write(&artifact_path, &bytes).unwrap();
    let err = bundle::export_bundle(
        &ws,
        &engine,
        &inv.investigation_id,
        &dir.path().join("case-2.logscope-case"),
        &bundle::BundleOptions {
            redaction_profile_id: None,
            include_reports: true,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, "bundle/report-modified");
}
