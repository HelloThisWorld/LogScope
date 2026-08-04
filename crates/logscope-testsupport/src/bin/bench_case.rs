//! v0.3 case benchmark: measured, reproducible evidence for the W3–W7
//! surfaces over the deterministic corpus (ADR-0010). Times pinning at
//! scale, batched verification (and proves the no-N+1 property by
//! asserting the dataset-lookup count), the timeline read model, report
//! generation, bundle export, and mid-run cancellation latency.
//!
//! Usage: bench_case <record-count> [--evidence <n>] [--workspace <dir>] [--fresh]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use logscope_app::case::{self, PinCommon, QueryScope};
use logscope_app::{bundle, explorer, report, run_import, timeline, ImportRequest};
use logscope_jobs::JobContext;
use logscope_query::{
    query_page, resolve_window, EngineConnection, PageRequest, QueryCancelHandle, TimeStrategy,
};
use logscope_testsupport::{peak_working_set_bytes, write_logs_jsonl};
use logscope_workspace::{NewInvestigation, NewReportDef, Workspace};

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let count: u64 = args
        .first()
        .and_then(|a| a.parse().ok())
        .expect("usage: bench_case <record-count> [--evidence <n>] [--workspace <dir>] [--fresh]");
    let evidence_n: usize = args
        .iter()
        .position(|a| a == "--evidence")
        .and_then(|i| args.get(i + 1))
        .and_then(|a| a.parse().ok())
        .unwrap_or(1000);
    let ws_dir = args
        .iter()
        .position(|a| a == "--workspace")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("target/bench-case-ws-{count}")));
    let fresh = args.iter().any(|a| a == "--fresh");

    println!("LogScope v0.3 case benchmark — {count} records, {evidence_n} evidence");
    println!(
        "machine: {} · build: {} · seed 20260729",
        std::env::consts::OS,
        if cfg!(debug_assertions) {
            "debug (NOT valid for acceptance)"
        } else {
            "release"
        }
    );

    if fresh && ws_dir.exists() {
        std::fs::remove_dir_all(&ws_dir).expect("clear bench workspace");
    }
    let import_ms = if !ws_dir.exists() {
        let t = Instant::now();
        build_workspace(&ws_dir, count);
        let e = ms(t.elapsed());
        println!(
            "import+index build: {e:.0} ms ({:.0} rec/s)",
            count as f64 / (e / 1000.0)
        );
        Some(e)
    } else {
        println!("reusing bench workspace {}", ws_dir.display());
        None
    };

    let ws = Workspace::open(&ws_dir, "bench").expect("open bench workspace");
    let engine = EngineConnection::open_in_memory().expect("engine");
    let selection = explorer::resolve_dataset_selection(&ws, &[]).expect("selection");
    let files = explorer::segment_files_for(&ws, &selection).expect("files");

    // Fresh investigation per run (id embeds the timestamp so reruns on a
    // reused workspace do not collide).
    let inv_id = format!("inv-bench-{}", uuid::Uuid::new_v4());
    ws.meta
        .create_investigation(&NewInvestigation {
            investigation_id: inv_id.clone(),
            title: "bench investigation".into(),
            description: None,
            severity: Some("sev2".into()),
            owner_text: None,
            tags_json: "[]".into(),
            incident_started_at: None,
            window_start: None,
            window_end: None,
        })
        .expect("investigation");

    // Collect record ids through the real paging path.
    let analysis = explorer::analyze_query(&ws, &selection, "");
    let filter = explorer::compile_for_execution(&ws, &selection, &analysis).expect("compile");
    let window = resolve_window(&TimeStrategy::All, None);
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let mut ids: Vec<(String, String)> = Vec::with_capacity(evidence_n);
    let mut cursor = None;
    while ids.len() < evidence_n {
        let page = query_page(
            &engine,
            &files,
            &filter,
            &window,
            &PageRequest {
                cursor: cursor.clone(),
                backward: false,
                limit: 200,
            },
            &cancel,
            Some(Duration::from_secs(120)),
        )
        .expect("page");
        for r in &page.rows {
            if ids.len() < evidence_n {
                ids.push((r.dataset_id.clone(), r.record_id.clone()));
            }
        }
        if !page.has_more || page.next_cursor.is_none() {
            break;
        }
        cursor = page.next_cursor;
    }
    assert!(
        ids.len() >= evidence_n.min(count as usize),
        "corpus too small for the requested evidence count"
    );

    // ---- pin at scale.
    let t = Instant::now();
    for (i, (dataset_id, record_id)) in ids.iter().enumerate() {
        case::pin_event(
            &ws,
            &engine,
            &case::PinEventRequest {
                common: PinCommon {
                    investigation_id: inv_id.clone(),
                    title: format!("event {i}"),
                    annotation: None,
                    relevance: None,
                    group_id: None,
                },
                dataset_id: dataset_id.clone(),
                record_id: record_id.clone(),
                display_fields: vec!["service".into()],
                include_raw_excerpt: false,
            },
        )
        .expect("pin");
    }
    let pin_ms = ms(t.elapsed());
    println!(
        "pin {} events: {pin_ms:.0} ms ({:.2} ms/pin)",
        ids.len(),
        pin_ms / ids.len() as f64
    );

    // One selection pin over the first 500 ids exercises the multi-row shape.
    let sel_ids: Vec<String> = ids.iter().take(500).map(|(_, r)| r.clone()).collect();
    let t = Instant::now();
    case::pin_selection(
        &ws,
        &engine,
        &case::PinSelectionRequest {
            common: PinCommon {
                investigation_id: inv_id.clone(),
                title: "bench selection".into(),
                annotation: None,
                relevance: None,
                group_id: None,
            },
            record_ids: sel_ids,
            scope: QueryScope {
                query_text: String::new(),
                dataset_ids: vec![],
                time_strategy: TimeStrategy::All,
            },
        },
    )
    .expect("selection pin");
    println!("pin 500-id selection: {:.0} ms", ms(t.elapsed()));

    // ---- batched verification (the no-N+1 proof at scale).
    let (ctx, _control, rx) = JobContext::detached("bench-verify");
    std::mem::forget(rx);
    let t = Instant::now();
    let verify = case::verify_evidence(&ws, &engine, &inv_id, None, &ctx).expect("verify");
    let verify_ms = ms(t.elapsed());
    println!(
        "verify {} evidence: {verify_ms:.0} ms · dataset_lookups {} · states {:?}",
        verify.total, verify.dataset_lookups, verify.states
    );
    assert_eq!(
        verify.dataset_lookups, 1,
        "one dataset must mean ONE batched lookup — anything else is the N+1 regression"
    );

    // ---- cancellation latency: cancel mid-verify, measure time to
    // return. The job thread owns its engine, exactly as production
    // verification does.
    let mut cancel_lat = Vec::new();
    for _ in 0..5 {
        let (ctx, control, rx) = JobContext::detached("bench-verify-cancel");
        std::mem::forget(rx);
        let job_engine = EngineConnection::open_in_memory().expect("engine");
        let (ws_ref, inv_ref, ctx_ref) = (&ws, &inv_id, &ctx);
        std::thread::scope(|s| {
            let h = s.spawn(move || {
                let _ = case::verify_evidence(ws_ref, &job_engine, inv_ref, None, ctx_ref);
            });
            std::thread::sleep(Duration::from_millis(20));
            let t_cancel = Instant::now();
            control.cancel();
            h.join().expect("join");
            cancel_lat.push(ms(t_cancel.elapsed()));
        });
    }
    cancel_lat.sort_by(f64::total_cmp);
    println!(
        "verify cancellation latency (cancel→return): p50 {:.1} ms · max {:.1} ms",
        cancel_lat[2], cancel_lat[4]
    );

    // ---- timeline read model over the full evidence set.
    let t = Instant::now();
    let tl = timeline::timeline(&ws, &inv_id).expect("timeline");
    println!(
        "timeline ({} dated / {} undated): {:.0} ms",
        tl.dated.len(),
        tl.undated.len(),
        ms(t.elapsed())
    );

    // ---- report generation over every pinned item.
    let evidence_rows = ws.meta.list_evidence(&inv_id, true).expect("list");
    let selected: Vec<serde_json::Value> = evidence_rows
        .iter()
        .map(|e| serde_json::json!({"id": e.evidence_id, "revision": e.revision}))
        .collect();
    let def = ws
        .meta
        .create_report_def(&NewReportDef {
            report_def_id: format!("rep-bench-{}", uuid::Uuid::new_v4()),
            investigation_id: inv_id.clone(),
            title: "bench report".into(),
            subtitle: None,
            sections_json: serde_json::json!([
                {"kind": "summary", "content": "bench"},
                {"kind": "timeline"},
                {"kind": "evidence"},
            ])
            .to_string(),
            selected_evidence_json: serde_json::to_string(&selected).unwrap(),
            selected_markers_json: "[]".into(),
            options_json: "{}".into(),
        })
        .expect("def");
    let report_dir = tempfile::tempdir().expect("report dir");
    let t = Instant::now();
    let art = report::generate_report(
        &ws,
        &def.report_def_id,
        report::ReportFormat::Markdown,
        &report_dir.path().join("bench.md"),
    )
    .expect("report");
    println!(
        "report (markdown, {} evidence): {:.0} ms · {} bytes",
        evidence_rows.len(),
        ms(t.elapsed()),
        art.byte_size.unwrap_or(0)
    );

    // ---- bundle export (includes the referenced-record parquet subset).
    let t = Instant::now();
    let exp = bundle::export_bundle(
        &ws,
        &engine,
        &inv_id,
        &report_dir.path().join("bench.logscope-case"),
        &bundle::BundleOptions::default(),
    )
    .expect("bundle");
    println!(
        "bundle export ({} evidence, data subset): {:.0} ms · {} bytes",
        evidence_rows.len(),
        ms(t.elapsed()),
        exp.byte_size.unwrap_or(0)
    );

    let peak = peak_working_set_bytes().unwrap_or(0);
    println!("peak working set: {:.0} MiB", peak as f64 / 1048576.0);
    let json = serde_json::json!({
        "records": count,
        "evidence": evidence_rows.len(),
        "import_ms": import_ms,
        "pin_total_ms": pin_ms,
        "verify_ms": verify_ms,
        "verify_dataset_lookups": verify.dataset_lookups,
        "cancel_latency_p50_ms": cancel_lat[2],
        "cancel_latency_max_ms": cancel_lat[4],
        "peak_working_set_bytes": peak,
        "debug_build": cfg!(debug_assertions),
    });
    println!("JSON: {json}");
}

fn build_workspace(dir: &Path, count: u64) {
    let corpus_dir = tempfile::tempdir().expect("tempdir");
    let corpus = corpus_dir.path().join("bench-logs.jsonl");
    let file = std::fs::File::create(&corpus).expect("corpus file");
    let shape = write_logs_jsonl(std::io::BufWriter::new(file), count, 20260729).expect("generate");
    println!(
        "corpus: {} lines, {:.0} MiB",
        shape.lines,
        shape.bytes as f64 / 1048576.0
    );
    let mut ws = Workspace::create(dir, "bench", "0.3.0-bench").expect("create ws");
    let engine = EngineConnection::open_in_memory().expect("engine");
    let request = ImportRequest::new(
        vec![corpus],
        logscope_ingest::builtin::jsonl_generic(),
        "bench logs",
    );
    let (ctx, _control, rx) = JobContext::detached("bench-import");
    std::mem::forget(rx);
    run_import(&mut ws, &engine, &request, &ctx).expect("import");
}
