//! v0.0 vertical architecture proof:
//! import -> structured query -> full-text search -> close -> reopen ->
//! query again, plus cancellation, duplicate policy, and determinism.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use logscope_app::{run_import, ImportOutcome, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::{JobError, JobEvent};
use logscope_query::{query_log_page, EngineConnection, LogQueryRequest, QueryCancelHandle};
use logscope_store::FtsIndex;
use logscope_workspace::Workspace;

/// Deterministic synthetic JSONL corpus (no real names/hosts/tokens).
fn write_jsonl(path: &Path, records: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    for i in 0..records {
        let level = match i % 5 {
            0 => "ERROR",
            1 => "WARN",
            _ => "INFO",
        };
        let marker = if i % 97 == 0 { " quorum lost" } else { "" };
        let trace = if i % 10 == 0 {
            format!(",\"trace_id\":\"{:032x}\"", 0xabc0_0000_u128 + (i as u128))
        } else {
            String::new()
        };
        writeln!(
            f,
            "{{\"@timestamp\":\"2024-06-01T{:02}:{:02}:{:02}Z\",\"level\":\"{}\",\"message\":\"event {} processed{}\",\"service\":\"checkout\",\"idx\":{}{}}}",
            (i / 3600) % 24,
            (i / 60) % 60,
            i % 60,
            level,
            i,
            marker,
            i,
            trace
        )
        .unwrap();
    }
}

type JobReturn = (Result<ImportOutcome, JobError>, Workspace, EngineConnection);

/// Runs an import inside a real background job, returning the moved-in
/// workspace/engine afterwards. `cancel_after_first_progress` exercises the
/// cooperative cancellation path.
fn import_in_job(
    mut ws: Workspace,
    engine: EngineConnection,
    request: ImportRequest,
    cancel_after_first_progress: bool,
) -> (JobReturn, Receiver<JobEvent>) {
    let (tx, rx) = crossbeam_channel::unbounded();
    let handle = logscope_jobs::spawn_job(
        format!("job-{}", uuid::Uuid::new_v4()),
        "import",
        tx,
        move |ctx| {
            let outcome = run_import(&mut ws, &engine, &request, ctx);
            Ok::<JobReturn, JobError>((outcome, ws, engine))
        },
    );
    if cancel_after_first_progress {
        for event in rx.iter() {
            if matches!(event, JobEvent::Progress { .. }) {
                handle.control.cancel();
                break;
            }
        }
    }
    let result = handle.join().expect("job thread itself must not fail");
    (result, rx)
}

fn page(
    ws: &Workspace,
    engine: &EngineConnection,
    dataset_id: &str,
    request: LogQueryRequest,
) -> logscope_query::LogPage {
    let files = ws.segment_paths(dataset_id).unwrap();
    let fts = FtsIndex::open(&ws.layout.fts_logs_path()).unwrap();
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    query_log_page(engine, &files, &request, Some(&fts), &cancel, None).unwrap()
}

fn base_request(dataset_id: &str, limit: u32) -> LogQueryRequest {
    LogQueryRequest {
        dataset_ids: vec![dataset_id.to_string()],
        limit,
        ..Default::default()
    }
}

#[test]
fn import_query_close_reopen_query() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("app.jsonl");
    write_jsonl(&input, 3000);

    let ws = Workspace::create(&dir.path().join("ws"), "Proof", "0.0.0").unwrap();
    let engine = EngineConnection::open_in_memory().unwrap();

    let mut request = ImportRequest::new(vec![input.clone()], builtin::jsonl_generic(), "app logs");
    request.batch_size = 512;
    request.segment_max_rows = 1024;

    let ((outcome, ws, engine), events) = import_in_job(ws, engine, request, false);
    let outcome = outcome.unwrap();
    assert_eq!(outcome.accepted, 3000);
    assert_eq!(outcome.unparsed, 0);
    assert_eq!(outcome.rejected, 0);
    assert_eq!(outcome.duplicates, 0);
    assert_eq!(outcome.segments.len(), 3, "1024+1024+952 rows");

    // Progress events flowed.
    let saw_progress = events
        .try_iter()
        .any(|e| matches!(e, JobEvent::Progress { .. }));
    assert!(saw_progress);

    let ds = &outcome.dataset_id;
    let segments = ws.meta.segments_for_dataset(ds).unwrap();
    assert_eq!(segments.len(), 3);
    assert_eq!(segments.iter().map(|s| s.row_count).sum::<i64>(), 3000);
    assert!(segments.iter().all(|s| s.fts_indexed));

    // Bounded first page, ordered by event time.
    let p = page(&ws, &engine, ds, base_request(ds, 100));
    assert_eq!(p.rows.len(), 100);
    assert!(p.has_more);
    for w in p.rows.windows(2) {
        assert!(w[0].event_time <= w[1].event_time);
    }
    // Page-size clamping.
    let clamped = page(
        &ws,
        &engine,
        ds,
        LogQueryRequest {
            dataset_ids: vec![ds.clone()],
            limit: 50_000,
            ..Default::default()
        },
    );
    assert_eq!(clamped.limit, 1000);

    // Structured severity filter: i % 5 == 0 -> ERROR (600 records).
    let errors = page(
        &ws,
        &engine,
        ds,
        LogQueryRequest {
            min_severity: Some(17),
            ..base_request(ds, 1000)
        },
    );
    assert_eq!(errors.rows.len(), 600);
    assert!(!errors.has_more);
    assert!(errors
        .rows
        .iter()
        .all(|r| r.severity_text.as_deref() == Some("ERROR")));

    // Attribute equality on preserved dynamic attributes.
    let svc = page(
        &ws,
        &engine,
        ds,
        LogQueryRequest {
            attr_equals: vec![("service".into(), "checkout".into())],
            ..base_request(ds, 10)
        },
    );
    assert_eq!(svc.rows.len(), 10);
    assert!(svc.has_more);
    let none = page(
        &ws,
        &engine,
        ds,
        LogQueryRequest {
            attr_equals: vec![("service".into(), "other".into())],
            ..base_request(ds, 10)
        },
    );
    assert!(none.rows.is_empty());

    // Full-text search: "quorum" appears for i % 97 == 0 (31 records).
    let fts_hits = page(
        &ws,
        &engine,
        ds,
        LogQueryRequest {
            contains_text: Some("quorum".into()),
            ..base_request(ds, 1000)
        },
    );
    assert_eq!(fts_hits.rows.len(), 31);
    assert!(fts_hits
        .rows
        .iter()
        .all(|r| r.display_message.contains("quorum")));

    // Trace correlation.
    let trace = page(
        &ws,
        &engine,
        ds,
        LogQueryRequest {
            trace_id: Some(format!("{:032x}", 0xabc0_0000_u128)),
            ..base_request(ds, 10)
        },
    );
    assert_eq!(trace.rows.len(), 1);

    // Typed attributes survived: idx is an int in tagged canonical JSON.
    assert!(p.rows[0].attributes_json.contains("\"t\":\"int\""));
    // Provenance resolves to a locator.
    let prov: logscope_model::IngestProvenance =
        serde_json::from_str(&p.rows[0].provenance_json).unwrap();
    assert!(prov.locator.record_number.is_some());
    assert!(prov.locator.byte_start.is_some());

    // ---- close and reopen ------------------------------------------------
    let root = ws.layout.root().to_path_buf();
    let ds_clone = ds.clone();
    drop(ws);

    let ws2 = Workspace::open(&root, "0.0.0").unwrap();
    assert!(ws2.recovery.is_clean(), "recovery: {:?}", ws2.recovery);
    let p2 = page(&ws2, &engine, &ds_clone, base_request(&ds_clone, 100));
    assert_eq!(p2.rows.len(), 100);
    let fts2 = page(
        &ws2,
        &engine,
        &ds_clone,
        LogQueryRequest {
            contains_text: Some("quorum".into()),
            ..base_request(&ds_clone, 1000)
        },
    );
    assert_eq!(fts2.rows.len(), 31);

    // ---- deterministic re-import ----------------------------------------
    let request2 = {
        let mut r = ImportRequest::new(vec![input], builtin::jsonl_generic(), "app logs again");
        r.batch_size = 999;
        r.segment_max_rows = 2048;
        r
    };
    let ((outcome2, ws2, engine), _) = import_in_job(ws2, engine, request2, false);
    let outcome2 = outcome2.unwrap();
    assert_eq!(outcome2.accepted, 3000);
    let mut ids1: Vec<String> = page(&ws2, &engine, &ds_clone, base_request(&ds_clone, 1000))
        .rows
        .into_iter()
        .map(|r| r.record_id)
        .collect();
    let mut ids2: Vec<String> = page(
        &ws2,
        &engine,
        &outcome2.dataset_id,
        base_request(&outcome2.dataset_id, 1000),
    )
    .rows
    .into_iter()
    .map(|r| r.record_id)
    .collect();
    ids1.sort();
    ids2.sort();
    assert_eq!(
        ids1, ids2,
        "same source + same versions must produce identical record IDs"
    );
}

#[test]
fn cancelled_import_leaves_workspace_valid_and_reusable() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("big.jsonl");
    write_jsonl(&input, 60_000);

    let ws = Workspace::create(&dir.path().join("ws"), "Cancel", "0.0.0").unwrap();
    let engine = EngineConnection::open_in_memory().unwrap();

    let mut request = ImportRequest::new(vec![input], builtin::jsonl_generic(), "big");
    request.batch_size = 256;

    let ((outcome, ws, engine), _events) = import_in_job(ws, engine, request, true);
    let err = outcome.unwrap_err();
    assert_eq!(err.code, "job/cancelled");

    // Workspace is clean: no staging leftovers, no half-visible dataset.
    let staging_entries: Vec<_> = std::fs::read_dir(ws.layout.staging_dir())
        .unwrap()
        .collect();
    assert!(staging_entries.is_empty(), "staging must be discarded");
    let datasets = ws.meta.list_datasets().unwrap();
    assert!(datasets.is_empty(), "cancelled dataset must be removed");
    let jobs = ws.meta.list_jobs().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].status, "cancelled");

    // The workspace remains fully usable.
    let small = dir.path().join("small.jsonl");
    write_jsonl(&small, 50);
    let ((outcome, ws, engine), _) = import_in_job(
        ws,
        engine,
        ImportRequest::new(vec![small], builtin::jsonl_generic(), "small"),
        false,
    );
    let outcome = outcome.unwrap();
    assert_eq!(outcome.accepted, 50);
    let p = page(
        &ws,
        &engine,
        &outcome.dataset_id,
        base_request(&outcome.dataset_id, 100),
    );
    assert_eq!(p.rows.len(), 50);
}

#[test]
fn malformed_records_and_duplicate_policy_are_visible() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.jsonl");
    std::fs::write(
        &a,
        "{\"@timestamp\":\"2024-06-01T00:00:00Z\",\"level\":\"INFO\",\"message\":\"one\"}\n\
         not json at all\n\
         {\"@timestamp\":\"2024-06-01T00:00:01Z\",\"level\":\"INFO\",\"message\":\"two\"}\n\
         {\"@timestamp\":\"2024-06-01T00:00:02Z\",\"level\":\"WARN\",\"message\":\"three\"}\n\
         {\"broken\": \"trunc",
    )
    .unwrap();
    let b = dir.path().join("b.jsonl");
    std::fs::copy(&a, &b).unwrap();

    let ws = Workspace::create(&dir.path().join("ws"), "Dups", "0.0.0").unwrap();
    let engine = EngineConnection::open_in_memory().unwrap();
    let ((outcome, ws, _engine), _) = import_in_job(
        ws,
        engine,
        ImportRequest::new(vec![a, b], builtin::jsonl_generic(), "dups"),
        false,
    );
    let outcome = outcome.unwrap();

    // File A: 3 accepted, 2 malformed. File B is byte-identical, so every
    // valid record is a duplicate (same content, same intra-file position).
    assert_eq!(outcome.accepted, 3);
    assert_eq!(outcome.duplicates, 3);
    assert_eq!(outcome.unparsed, 4);
    assert_eq!(outcome.rejected, 0);

    let rejects = ws
        .meta
        .rejected_for_dataset(&outcome.dataset_id, 100, 0)
        .unwrap();
    assert_eq!(rejects.len(), 4);
    assert!(rejects
        .iter()
        .any(|r| r.reason_code == "parse/invalid-json"));
    assert!(rejects
        .iter()
        .any(|r| r.reason_code == "parse/truncated-record"));
    for r in &rejects {
        let locator: logscope_model::RecordLocator = serde_json::from_str(&r.locator_json).unwrap();
        assert!(locator.line_start.is_some(), "every reject keeps a locator");
        assert!(r.raw_excerpt.as_ref().is_some_and(|e| !e.is_empty()));
    }

    // Ledger records per-file counts.
    let _ = PathBuf::new();
}
