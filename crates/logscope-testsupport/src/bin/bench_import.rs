//! Benchmark harness for the v0.0 performance gates.
//!
//! Usage (release build strongly recommended):
//!   bench_import logs 1000000 [seed]
//!   bench_import metrics 1000000 [seed]
//!   bench_import spans 250000 [seed]
//!   bench_import cancel-logs 1000000 [seed]
//!
//! Corpora are generated on demand from a fixed seed into a temp directory
//! and never committed. Results print as human text plus one JSON line
//! prefixed with `RESULT_JSON:` for machine capture.

use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use logscope_app::{run_import, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::JobEvent;
use logscope_model::UnixNanos;
use logscope_otlp::{
    convert_metrics, convert_traces, stream_otlp_jsonl, ConvertContext, EnvelopePayload,
};
use logscope_query::{
    query_log_page, reconstruct_trace, rollup_gauge_or_delta_sum, EngineConnection,
    LogQueryRequest, QueryCancelHandle,
};
use logscope_store::{FtsIndex, MetricSegmentWriter, SpanSegmentWriter};
use logscope_testsupport::{
    peak_working_set_bytes, write_logs_jsonl, write_metrics_otlp_jsonl, write_spans_otlp_jsonl,
};
use logscope_workspace::{
    LedgerCounts, LedgerEntry, PublishVersions, SegmentToPublish, Signal, Workspace,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("logs");
    let count: u64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let seed: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20_260_729);

    let bench_root = std::env::temp_dir().join(format!("logscope-bench-{}", std::process::id()));
    std::fs::create_dir_all(&bench_root).expect("create bench dir");
    println!("mode={mode} count={count} seed={seed}");
    println!("bench dir: {}", bench_root.display());

    let result = match mode {
        "logs" => bench_logs(&bench_root, count, seed, false),
        "cancel-logs" => bench_logs(&bench_root, count, seed, true),
        "metrics" => bench_metrics(&bench_root, count, seed),
        "spans" => bench_spans(&bench_root, count, seed),
        other => {
            eprintln!("unknown mode {other}");
            std::process::exit(2);
        }
    };
    println!("RESULT_JSON:{}", serde_json::to_string(&result).unwrap());
    let _ = std::fs::remove_dir_all(&bench_root);
}

fn dataset_disk_bytes(ws: &Workspace, dataset_id: &str) -> u64 {
    ws.meta
        .segments_for_dataset(dataset_id)
        .map(|segs| segs.iter().map(|s| s.byte_size as u64).sum())
        .unwrap_or(0)
}

fn bench_logs(root: &Path, count: u64, seed: u64, cancel: bool) -> serde_json::Value {
    let corpus = root.join("logs.jsonl");
    let t_gen = Instant::now();
    let shape = {
        let f = std::fs::File::create(&corpus).expect("create corpus");
        write_logs_jsonl(BufWriter::with_capacity(1 << 20, f), count, seed)
            .expect("generate corpus")
    };
    let gen_secs = t_gen.elapsed().as_secs_f64();
    println!(
        "generated {} lines / {:.1} MiB in {gen_secs:.1}s",
        shape.lines,
        shape.bytes as f64 / (1024.0 * 1024.0)
    );

    let ws = Workspace::create(&root.join("ws"), "bench", "0.0.0").expect("workspace");
    let engine = EngineConnection::open_in_memory().expect("engine");
    let request = ImportRequest::new(vec![corpus.clone()], builtin::jsonl_generic(), "bench logs");

    let (tx, rx) = crossbeam_channel::unbounded::<JobEvent>();
    let t_import = Instant::now();
    let mut ws_slot = Some((ws, engine));
    let (ws, engine) = ws_slot.take().unwrap();
    let handle = logscope_jobs::spawn_job("bench-job", "import", tx, {
        let mut ws = ws;
        let engine = engine;
        let request = request.clone();
        move |ctx| {
            let outcome = run_import(&mut ws, &engine, &request, ctx);
            Ok::<_, logscope_jobs::JobError>((outcome, ws, engine))
        }
    });

    let cancel_latency_ms: Option<f64> = if cancel {
        // Wait for progress to start flowing, then cancel and time exit.
        let mut latency = None;
        for event in rx.iter() {
            if matches!(event, JobEvent::Progress { .. }) {
                let t_cancel = Instant::now();
                handle.control.cancel();
                // Drain until Finished arrives.
                for e in rx.iter() {
                    if matches!(e, JobEvent::Finished { .. }) {
                        break;
                    }
                }
                latency = Some(t_cancel.elapsed().as_secs_f64() * 1000.0);
                break;
            }
        }
        latency
    } else {
        None
    };

    let (outcome, ws, engine) = handle.join().expect("job thread");
    let import_secs = t_import.elapsed().as_secs_f64();

    if cancel {
        let err = outcome.expect_err("cancelled import must not succeed");
        assert_eq!(err.code, "job/cancelled");
        let staging_left = std::fs::read_dir(ws.layout.staging_dir())
            .map(|d| d.count())
            .unwrap_or(0);
        let datasets = ws.meta.list_datasets().expect("list").len();
        println!(
            "cancelled after {:.0} ms; staging entries={staging_left}; datasets={datasets}",
            cancel_latency_ms.unwrap_or(-1.0)
        );
        return serde_json::json!({
            "mode": "cancel-logs",
            "count": count,
            "seed": seed,
            "cancel_latency_ms": cancel_latency_ms,
            "workspace_clean": staging_left == 0 && datasets == 0,
            "peak_working_set_mib": peak_working_set_bytes().map(|b| b / (1024*1024)),
        });
    }

    let outcome = outcome.expect("import outcome");
    let throughput = outcome.accepted as f64 / import_secs;
    let disk = dataset_disk_bytes(&ws, &outcome.dataset_id);
    println!(
        "imported {} records in {import_secs:.1}s ({throughput:.0} rec/s), {} segments, {:.1} MiB on disk",
        outcome.accepted,
        outcome.segments.len(),
        disk as f64 / (1024.0 * 1024.0)
    );

    // Query latencies on the fresh workspace.
    let files = ws.segment_paths(&outcome.dataset_id).expect("paths");
    let fts = FtsIndex::open(&ws.layout.fts_logs_path()).expect("fts");
    let cancel_handle = QueryCancelHandle::new(engine.interrupt_handle());
    let base = LogQueryRequest {
        dataset_ids: vec![outcome.dataset_id.clone()],
        limit: 100,
        ..Default::default()
    };

    let t = Instant::now();
    let first = query_log_page(&engine, &files, &base, Some(&fts), &cancel_handle, None)
        .expect("first page");
    let first_page_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let severe = query_log_page(
        &engine,
        &files,
        &LogQueryRequest {
            min_severity: Some(17),
            ..base.clone()
        },
        Some(&fts),
        &cancel_handle,
        None,
    )
    .expect("severity page");
    let severity_ms = t.elapsed().as_secs_f64() * 1000.0;

    let t = Instant::now();
    let fts_page = query_log_page(
        &engine,
        &files,
        &LogQueryRequest {
            contains_text: Some("cascade".into()),
            limit: 1000,
            ..base.clone()
        },
        Some(&fts),
        &cancel_handle,
        None,
    )
    .expect("fts page");
    let fts_ms = t.elapsed().as_secs_f64() * 1000.0;

    println!(
        "first page {first_page_ms:.0} ms ({} rows) | severity page {severity_ms:.0} ms ({} rows) | fts {fts_ms:.0} ms ({} rows, expect {})",
        first.rows.len(),
        severe.rows.len(),
        fts_page.rows.len(),
        shape.searchable_token_lines
    );
    assert_eq!(fts_page.rows.len() as u64, shape.searchable_token_lines);

    serde_json::json!({
        "mode": "logs",
        "count": count,
        "seed": seed,
        "input_bytes": shape.bytes,
        "generate_secs": gen_secs,
        "import_secs": import_secs,
        "accepted": outcome.accepted,
        "throughput_rec_per_s": throughput,
        "segments": outcome.segments.len(),
        "output_disk_bytes": disk,
        "first_page_ms": first_page_ms,
        "severity_page_ms": severity_ms,
        "fts_page_ms": fts_ms,
        "fts_hits": fts_page.rows.len(),
        "peak_working_set_mib": peak_working_set_bytes().map(|b| b / (1024*1024)),
    })
}

/// Streams OTLP JSONL envelopes into rotated segments and publishes them
/// (spike-level metric/span import used only by this harness).
fn import_otlp_stream(
    ws: &mut Workspace,
    corpus: &Path,
    signal: Signal,
    dataset_name: &str,
) -> (String, u64, Option<String>) {
    let job_id = format!("job-{}", uuid::Uuid::new_v4());
    let dataset_id = format!("ds-{}", uuid::Uuid::new_v4());
    ws.meta.insert_job(&job_id, "bench", Some(&dataset_id)).unwrap();
    ws.meta.create_dataset(&dataset_id, dataset_name, signal).unwrap();
    let staging = ws.begin_staging(&job_id).unwrap();

    let mut metric_writer: Option<(String, MetricSegmentWriter, PathBuf)> = None;
    let mut span_writer: Option<(String, SpanSegmentWriter, PathBuf)> = None;
    let mut staged: Vec<(PathBuf, SegmentToPublish)> = Vec::new();
    let mut rows_written = 0u64;
    let mut first_trace: Option<String> = None;
    let segment_max_rows = 500_000u64;

    let ctx_template = ConvertContext {
        dataset_id: dataset_id.clone(),
        logical_source_id: "src-bench".into(),
        origin: logscope_model::PhysicalOrigin::File {
            file_id: "file-bench".into(),
            archive_entry: None,
        },
        protocol: logscope_model::SourceProtocol::OtlpJsonlFile,
        content_type: Some("application/x-ndjson".into()),
        ingest_time: UnixNanos::now(),
        batch_index: 0,
        envelope_hash: String::new(),
        extra_flags: vec![],
    };

    let file = std::fs::File::open(corpus).unwrap();
    let mut batch_index = 0u64;
    stream_otlp_jsonl(
        std::io::BufReader::with_capacity(1 << 20, file),
        |envelope| {
            let mut ctx = ctx_template.clone();
            ctx.batch_index = batch_index;
            ctx.envelope_hash = envelope.meta.raw_hash.clone();
            batch_index += 1;
            match (&envelope.payload, signal) {
                (EnvelopePayload::Metrics(req), Signal::Metrics) => {
                    let batch = convert_metrics(req, &ctx);
                    assert!(batch.rejects.is_empty());
                    if metric_writer.is_none() {
                        let segment_id = format!("seg-{}", uuid::Uuid::new_v4());
                        let path = staging.join(format!("metrics-{segment_id}.parquet"));
                        metric_writer = Some((
                            segment_id,
                            MetricSegmentWriter::create(&path).unwrap(),
                            path,
                        ));
                    }
                    let (_, w, _) = metric_writer.as_mut().unwrap();
                    w.write_batch(&batch.metrics).unwrap();
                    let rows = w.rows_written();
                    if rows >= segment_max_rows {
                        let (segment_id, w, path) = metric_writer.take().unwrap();
                        let stats = w.finish().unwrap();
                        rows_written += stats.rows;
                        staged.push((
                            path,
                            SegmentToPublish {
                                segment_id: segment_id.clone(),
                                signal,
                                file_name: format!("metrics-{segment_id}.parquet"),
                                row_count: stats.rows as i64,
                                byte_size: stats.byte_size as i64,
                                min_event_time: stats.min_event_time,
                                max_event_time: stats.max_event_time,
                            },
                        ));
                    }
                }
                (EnvelopePayload::Traces(req), Signal::Spans) => {
                    let batch = convert_traces(req, &ctx);
                    assert!(batch.rejects.is_empty());
                    if first_trace.is_none() {
                        first_trace =
                            batch.spans.first().map(|s| s.trace_id.as_str().to_string());
                    }
                    if span_writer.is_none() {
                        let segment_id = format!("seg-{}", uuid::Uuid::new_v4());
                        let path = staging.join(format!("spans-{segment_id}.parquet"));
                        span_writer =
                            Some((segment_id, SpanSegmentWriter::create(&path).unwrap(), path));
                    }
                    let (_, w, _) = span_writer.as_mut().unwrap();
                    w.write_batch(&batch.spans).unwrap();
                    if w.rows_written() >= segment_max_rows {
                        let (segment_id, w, path) = span_writer.take().unwrap();
                        let stats = w.finish().unwrap();
                        rows_written += stats.rows;
                        staged.push((
                            path,
                            SegmentToPublish {
                                segment_id: segment_id.clone(),
                                signal,
                                file_name: format!("spans-{segment_id}.parquet"),
                                row_count: stats.rows as i64,
                                byte_size: stats.byte_size as i64,
                                min_event_time: stats.min_event_time,
                                max_event_time: stats.max_event_time,
                            },
                        ));
                    }
                }
                other => panic!("unexpected payload/signal combination: {other:?}"),
            }
            true
        },
        |reject| panic!("unexpected reject in bench corpus: {reject:?}"),
    )
    .unwrap();

    if let Some((segment_id, w, path)) = metric_writer.take() {
        let stats = w.finish().unwrap();
        rows_written += stats.rows;
        staged.push((
            path,
            SegmentToPublish {
                segment_id: segment_id.clone(),
                signal,
                file_name: format!("metrics-{segment_id}.parquet"),
                row_count: stats.rows as i64,
                byte_size: stats.byte_size as i64,
                min_event_time: stats.min_event_time,
                max_event_time: stats.max_event_time,
            },
        ));
    }
    if let Some((segment_id, w, path)) = span_writer.take() {
        let stats = w.finish().unwrap();
        rows_written += stats.rows;
        staged.push((
            path,
            SegmentToPublish {
                segment_id: segment_id.clone(),
                signal,
                file_name: format!("spans-{segment_id}.parquet"),
                row_count: stats.rows as i64,
                byte_size: stats.byte_size as i64,
                min_event_time: stats.min_event_time,
                max_event_time: stats.max_event_time,
            },
        ));
    }

    ws.publish_staged_import(
        &job_id,
        &dataset_id,
        &staged,
        &[LedgerEntry {
            source_id: "src-bench".into(),
            file_id: "file-bench".into(),
            checkpoint_json: "{}".into(),
            counts: LedgerCounts {
                accepted: rows_written,
                ..Default::default()
            },
        }],
        &PublishVersions::default(),
    )
    .unwrap();
    (dataset_id, rows_written, first_trace)
}

fn bench_metrics(root: &Path, count: u64, seed: u64) -> serde_json::Value {
    let corpus = root.join("metrics.jsonl");
    let t_gen = Instant::now();
    let shape = {
        let f = std::fs::File::create(&corpus).unwrap();
        write_metrics_otlp_jsonl(BufWriter::with_capacity(1 << 20, f), count, 10_000, seed)
            .unwrap()
    };
    println!(
        "generated {} envelopes / {} points / {:.1} MiB in {:.1}s",
        shape.envelopes,
        shape.points,
        shape.bytes as f64 / (1024.0 * 1024.0),
        t_gen.elapsed().as_secs_f64()
    );

    let mut ws = Workspace::create(&root.join("ws-metrics"), "bench", "0.0.0").unwrap();
    let t_import = Instant::now();
    let (dataset_id, rows, _) = import_otlp_stream(&mut ws, &corpus, Signal::Metrics, "bench metrics");
    let import_secs = t_import.elapsed().as_secs_f64();
    let disk = dataset_disk_bytes(&ws, &dataset_id);
    println!(
        "imported {rows} points in {import_secs:.1}s ({:.0} points/s), {:.1} MiB on disk",
        rows as f64 / import_secs,
        disk as f64 / (1024.0 * 1024.0)
    );
    assert_eq!(rows, shape.points);

    let engine = EngineConnection::open_in_memory().unwrap();
    let files = ws.segment_paths(&dataset_id).unwrap();
    let t = Instant::now();
    let rollup = rollup_gauge_or_delta_sum(
        &engine,
        &files,
        "bench.requests",
        60 * 1_000_000_000,
        None,
        None,
    )
    .unwrap();
    let rollup_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!("rollup: {} buckets in {rollup_ms:.0} ms", rollup.len());

    serde_json::json!({
        "mode": "metrics",
        "points": shape.points,
        "seed": seed,
        "input_bytes": shape.bytes,
        "series_cardinality": shape.series_cardinality,
        "import_secs": import_secs,
        "throughput_points_per_s": rows as f64 / import_secs,
        "output_disk_bytes": disk,
        "rollup_ms": rollup_ms,
        "rollup_buckets": rollup.len(),
        "peak_working_set_mib": peak_working_set_bytes().map(|b| b / (1024*1024)),
    })
}

fn bench_spans(root: &Path, count: u64, seed: u64) -> serde_json::Value {
    let corpus = root.join("spans.jsonl");
    let shape = {
        let f = std::fs::File::create(&corpus).unwrap();
        write_spans_otlp_jsonl(BufWriter::with_capacity(1 << 20, f), count, seed).unwrap()
    };
    println!(
        "generated {} spans across {} traces ({:.1} MiB)",
        shape.spans,
        shape.traces,
        shape.bytes as f64 / (1024.0 * 1024.0)
    );

    let mut ws = Workspace::create(&root.join("ws-spans"), "bench", "0.0.0").unwrap();
    let t_import = Instant::now();
    let (dataset_id, rows, first_trace) =
        import_otlp_stream(&mut ws, &corpus, Signal::Spans, "bench spans");
    let import_secs = t_import.elapsed().as_secs_f64();
    let disk = dataset_disk_bytes(&ws, &dataset_id);
    println!(
        "imported {rows} spans in {import_secs:.1}s ({:.0} spans/s)",
        rows as f64 / import_secs
    );
    assert_eq!(rows, shape.spans);

    let engine = EngineConnection::open_in_memory().unwrap();
    let files = ws.segment_paths(&dataset_id).unwrap();
    let trace_id = first_trace.expect("at least one trace");
    let t = Instant::now();
    let graph = reconstruct_trace(&engine, &files, &trace_id).unwrap();
    let reconstruct_ms = t.elapsed().as_secs_f64() * 1000.0;
    println!(
        "reconstructed trace {trace_id}: {} spans, {} roots in {reconstruct_ms:.0} ms",
        graph.integrity.span_count, graph.integrity.root_count
    );

    serde_json::json!({
        "mode": "spans",
        "spans": shape.spans,
        "traces": shape.traces,
        "seed": seed,
        "input_bytes": shape.bytes,
        "orphans_generated": shape.orphan_spans,
        "duplicates_generated": shape.duplicate_spans,
        "incomplete_generated": shape.incomplete_spans,
        "import_secs": import_secs,
        "throughput_spans_per_s": rows as f64 / import_secs,
        "output_disk_bytes": disk,
        "reconstruct_ms": reconstruct_ms,
        "reconstructed_span_count": graph.integrity.span_count,
        "peak_working_set_mib": peak_working_set_bytes().map(|b| b / (1024*1024)),
    })
}
