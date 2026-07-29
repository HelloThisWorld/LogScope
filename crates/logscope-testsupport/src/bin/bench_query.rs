//! v0.2 Explorer query benchmark: measured, reproducible evidence over the
//! deterministic corpus (ADR-0010). Builds (or reuses) a bench workspace by
//! importing the generated JSONL through the real pipeline, then times the
//! Explorer query surface end to end (service level, below Tauri).
//!
//! Usage: bench_query <record-count> [--workspace <dir>] [--fresh]
//!
//! "process-cold" = first execution in this process (OS page cache state is
//! whatever the machine has; documented, not falsified). "warm" = median of
//! the following 5 runs; "max" = the slowest of those 5.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use logscope_app::explorer;
use logscope_app::{run_export, run_import, ExportFormat, ExportSpec, ImportRequest};
use logscope_jobs::JobContext;
use logscope_query::{
    query_counts, query_facets, query_field_summary, query_histogram, query_page, resolve_window,
    CompiledFilter, EngineConnection, FieldTarget, PageRequest, QueryCancelHandle, ResolvedWindow,
    TimeStrategy,
};
use logscope_testsupport::{peak_working_set_bytes, write_logs_jsonl};
use logscope_workspace::Workspace;

struct Bench {
    engine: EngineConnection,
    ws: Workspace,
    files: Vec<PathBuf>,
    selection: Vec<String>,
    results: Vec<(String, f64, f64, f64, String)>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

impl Bench {
    fn prepare(&self, text: &str, window: &ResolvedWindow) -> (CompiledFilter, ResolvedWindow) {
        let analysis = explorer::analyze_query(&self.ws, &self.selection, text);
        assert!(
            analysis.resolved.is_some(),
            "bench query must be valid: {text} → {:?}",
            analysis.diagnostics
        );
        let filter =
            explorer::compile_for_execution(&self.ws, &self.selection, &analysis).expect("compile");
        (filter, window.clone())
    }

    /// Times one operation: 1 cold + 5 warm runs.
    fn timed(&mut self, name: &str, detail: &str, mut op: impl FnMut(&Self) -> String) {
        let cold_start = Instant::now();
        let note = op(self);
        let cold = ms(cold_start.elapsed());
        let mut warm = Vec::with_capacity(5);
        for _ in 0..5 {
            let s = Instant::now();
            op(self);
            warm.push(ms(s.elapsed()));
        }
        warm.sort_by(f64::total_cmp);
        let p50 = warm[2];
        let max = warm[4];
        println!(
            "  {name:<38} cold {cold:>9.1} ms   p50 {p50:>9.1} ms   max {max:>9.1} ms   {note}"
        );
        self.results.push((
            name.to_string(),
            cold,
            p50,
            max,
            format!("{detail}{}{note}", if note.is_empty() { "" } else { " · " }),
        ));
    }

    fn run_page_once(
        &self,
        filter: &CompiledFilter,
        window: &ResolvedWindow,
        cursor: Option<String>,
        backward: bool,
    ) -> logscope_query::QueryPage {
        let cancel = QueryCancelHandle::new(self.engine.interrupt_handle());
        query_page(
            &self.engine,
            &self.files,
            filter,
            window,
            &PageRequest {
                cursor,
                backward,
                limit: 200,
            },
            &cancel,
            Some(Duration::from_secs(120)),
        )
        .expect("page")
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let count: u64 = args
        .first()
        .and_then(|a| a.parse().ok())
        .expect("usage: bench_query <record-count> [--workspace <dir>] [--fresh]");
    let ws_dir = args
        .iter()
        .position(|a| a == "--workspace")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("target/bench-ws-{count}")));
    let fresh = args.iter().any(|a| a == "--fresh");

    println!("LogScope v0.2 query benchmark — {count} records");
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

    let open_start = Instant::now();
    let ws = Workspace::open(&ws_dir, "bench").expect("open bench workspace");
    let open_ms = ms(open_start.elapsed());
    println!("workspace open: {open_ms:.1} ms");

    let engine = EngineConnection::open_in_memory().expect("engine");
    let selection = explorer::resolve_dataset_selection(&ws, &[]).expect("selection");
    let files = explorer::segment_files_for(&ws, &selection).expect("files");
    let latest = explorer::latest_event_time(&ws, &selection)
        .expect("latest")
        .expect("timestamped corpus");
    assert!(
        explorer::fts_ready(&ws, &selection).expect("fts state"),
        "bench workspace must have FTS ready"
    );

    let all = resolve_window(&TimeStrategy::All, Some(latest));
    let mut b = Bench {
        engine,
        ws,
        files,
        selection,
        results: Vec::new(),
    };

    // First-page structured query.
    let (f_sev, w) = b.prepare("severity:(ERROR OR WARN)", &all);
    b.timed(
        "structured severity page",
        "severity:(ERROR OR WARN)",
        |b| {
            let p = b.run_page_once(&f_sev, &w, None, false);
            format!("{} rows", p.rows.len())
        },
    );

    // Full-text term (rare) — FTS path.
    b.timed("full-text rare term (indexed)", "\"cascade\"", |b| {
        let (f, w) = b.prepare("cascade", &all);
        let p = b.run_page_once(&f, &w, None, false);
        format!(
            "{} rows{}",
            p.rows.len(),
            if f.used_fts() { " · fts" } else { " · scan" }
        )
    });

    // Full-text moderately common term.
    b.timed("full-text common term", "\"upstream\"", |b| {
        let (f, w) = b.prepare("upstream", &all);
        let p = b.run_page_once(&f, &w, None, false);
        format!(
            "{} rows{}",
            p.rows.len(),
            if f.used_fts() { " · fts" } else { " · scan" }
        )
    });

    // Exact fallback scan for the same term (index bypassed).
    b.timed("full-text fallback scan", "regex token scan", |b| {
        let analysis = explorer::analyze_query(&b.ws, &b.selection, "upstream");
        let resolved = analysis.resolved.expect("valid");
        let ctx = logscope_query::FtsContext {
            index: None,
            dataset_ids: &b.selection,
        };
        let f = logscope_query::compile_filter(resolved.expr.as_ref(), &ctx).expect("compile");
        let p = b.run_page_once(&f, &all, None, false);
        format!("{} rows", p.rows.len())
    });

    // Combined field + phrase + time (middle half of the corpus).
    let mid_window = resolve_window(
        &TimeStrategy::Absolute {
            start: latest - (count as i64 * 3_000_000) * 3 / 4,
            end: latest - (count as i64 * 3_000_000) / 4,
        },
        Some(latest),
    );
    let combined = r#"service:checkout-svc AND severity:(ERROR OR WARN) AND "upstream""#;
    let (f_comb, w_comb) = b.prepare(combined, &mid_window);
    b.timed("combined field+phrase+time", combined, |b| {
        let p = b.run_page_once(&f_comb, &w_comb, None, false);
        format!("{} rows", p.rows.len())
    });

    // Exact counts under the same filter.
    b.timed("count(structured)", "count over severity filter", |b| {
        let cancel = QueryCancelHandle::new(b.engine.interrupt_handle());
        let c = query_counts(
            &b.engine,
            &b.files,
            &f_sev,
            &w,
            &cancel,
            Some(Duration::from_secs(120)),
        )
        .expect("counts");
        format!("{} matching", c.matching)
    });

    // Histogram over all data and over a narrow hour.
    b.timed("histogram (all data)", "120 bins", |b| {
        let cancel = QueryCancelHandle::new(b.engine.interrupt_handle());
        let h = query_histogram(
            &b.engine,
            &b.files,
            &f_sev,
            &w,
            120,
            &cancel,
            Some(Duration::from_secs(120)),
        )
        .expect("histogram");
        format!("{} bins", h.bins.len())
    });
    let hour = resolve_window(
        &TimeStrategy::Absolute {
            start: latest - 3_600_000_000_000,
            end: latest + 1,
        },
        Some(latest),
    );
    b.timed("histogram (last hour)", "narrow range", |b| {
        let cancel = QueryCancelHandle::new(b.engine.interrupt_handle());
        let h = query_histogram(
            &b.engine,
            &b.files,
            &f_sev,
            &hour,
            120,
            &cancel,
            Some(Duration::from_secs(120)),
        )
        .expect("histogram");
        format!("{} bins", h.bins.len())
    });

    // Facets: severity + service + region.
    b.timed(
        "facets (3 fields, top 10)",
        "severity, service, region",
        |b| {
            let cancel = QueryCancelHandle::new(b.engine.interrupt_handle());
            let targets = vec![
                (
                    "severity".to_string(),
                    FieldTarget::Canonical {
                        field: logscope_query_lang::CanonicalField::Severity,
                    },
                ),
                (
                    "service".to_string(),
                    FieldTarget::Attr {
                        path: vec!["service".into()],
                    },
                ),
                (
                    "region".to_string(),
                    FieldTarget::Attr {
                        path: vec!["region".into()],
                    },
                ),
            ];
            let f = query_facets(
                &b.engine,
                &b.files,
                &f_sev,
                &w,
                &targets,
                10,
                &cancel,
                Some(Duration::from_secs(120)),
            )
            .expect("facets");
            format!("{} facets", f.len())
        },
    );

    // High-cardinality field summary (seq is unique per record).
    b.timed("field summary (high-cardinality)", "attr seq", |b| {
        let cancel = QueryCancelHandle::new(b.engine.interrupt_handle());
        let s = query_field_summary(
            &b.engine,
            &b.files,
            &f_sev,
            &w,
            "seq",
            &FieldTarget::Attr {
                path: vec!["seq".into()],
            },
            true,
            vec![logscope_query_lang::AttrType::Int],
            &cancel,
            Some(Duration::from_secs(120)),
        )
        .expect("summary");
        format!(
            "~{} distinct{}",
            s.distinct_count,
            if s.high_cardinality { " (high)" } else { "" }
        )
    });

    // Paging: next and previous.
    let first_page = b.run_page_once(&f_sev, &w, None, false);
    let next_cursor = first_page.next_cursor.clone().expect("cursor");
    b.timed("next page (cursor)", "page 2 fetch", |b| {
        let p = b.run_page_once(&f_sev, &w, Some(next_cursor.clone()), false);
        format!("{} rows", p.rows.len())
    });
    b.timed("previous page (cursor)", "backward fetch", |b| {
        let p = b.run_page_once(&f_sev, &w, Some(next_cursor.clone()), true);
        format!("{} rows", p.rows.len())
    });

    // Scroll memory: 50 sequential pages, peak process memory recorded.
    {
        let before = peak_working_set_bytes().unwrap_or(0);
        let t = Instant::now();
        let mut cursor = None;
        let mut pages = 0;
        for _ in 0..50 {
            let p = b.run_page_once(&f_sev, &w, cursor.clone(), false);
            pages += 1;
            cursor = p.next_cursor.clone();
            if !p.has_more {
                break;
            }
        }
        let after = peak_working_set_bytes().unwrap_or(0);
        println!(
            "  {:<38} {:>9.1} ms for {pages} pages · peak WS {:.0} MiB (Δ {:.0} MiB)",
            "scroll 50 pages",
            ms(t.elapsed()),
            after as f64 / 1048576.0,
            (after - before) as f64 / 1048576.0
        );
        b.results.push((
            "scroll 50 pages".into(),
            ms(t.elapsed()),
            f64::NAN,
            f64::NAN,
            format!("peak working set {:.0} MiB", after as f64 / 1048576.0),
        ));
    }

    // Cancellation latency on a deliberately expensive bounded query
    // (three regex predicates over 1M+ rows). Retries with shorter delays
    // if the machine finishes the query before the cancel fires.
    {
        let expensive = "message:/([a-z]+ ){1,6}[a-z]+/ AND k8s.pod:/([a-z]+-)+[0-9a-f]{4}/ AND NOT message:/cascade|refreshed|dump/";
        let analysis = explorer::analyze_query(&b.ws, &b.selection, expensive);
        let resolved = analysis.resolved.expect("valid regex query");
        let ctx = logscope_query::FtsContext {
            index: None,
            dataset_ids: &b.selection,
        };
        let f = logscope_query::compile_filter(resolved.expr.as_ref(), &ctx).expect("compile");
        let mut recorded = false;
        for delay_ms in [30u64, 10, 2] {
            let cancel = QueryCancelHandle::new(b.engine.interrupt_handle());
            let c2 = cancel.clone();
            let killer = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(delay_ms));
                let t = Instant::now();
                c2.cancel();
                t
            });
            let t0 = Instant::now();
            let result = query_counts(
                &b.engine,
                &b.files,
                &f,
                &all,
                &cancel,
                Some(Duration::from_secs(120)),
            );
            let total = t0.elapsed();
            let cancel_at = killer.join().expect("join");
            if let Err(e) = result {
                let ack = cancel_at.elapsed();
                println!(
                    "  {:<38} ack {:>7.1} ms after cancel at {delay_ms} ms (query total {:.1} ms, {e:?})",
                    "cancellation latency", ms(ack), ms(total)
                );
                b.results.push((
                    "cancellation ack".into(),
                    ms(ack),
                    f64::NAN,
                    f64::NAN,
                    format!("{e:?}"),
                ));
                recorded = true;
                break;
            }
        }
        if !recorded {
            println!(
                "  {:<38} query completed before every cancel attempt (2 ms) — bounded query too fast to cancel here",
                "cancellation latency"
            );
            b.results.push((
                "cancellation ack".into(),
                f64::NAN,
                f64::NAN,
                f64::NAN,
                "query finished before cancel could land (fast machine)".into(),
            ));
        }
    }

    // Export throughput (bounded 100k JSONL).
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("bench-export.jsonl");
        let spec = ExportSpec {
            format: ExportFormat::Jsonl,
            destination: dest,
            row_limit: 100_000,
            byte_limit: u64::MAX / 2,
            csv_columns: vec![],
            csv_formula_guard: true,
        };
        let (ctx, _c, _rx) = JobContext::detached("bench-export");
        let t = Instant::now();
        let out = run_export(&b.engine, &b.files, &f_sev, &all, &spec, &ctx).expect("export");
        let secs = t.elapsed().as_secs_f64();
        println!(
            "  {:<38} {:>9.1} ms · {} rows · {:.1} MiB · {:.0} rows/s",
            "export JSONL (bounded 100k)",
            secs * 1000.0,
            out.rows_written,
            out.bytes_written as f64 / 1048576.0,
            out.rows_written as f64 / secs
        );
        b.results.push((
            "export jsonl 100k".into(),
            secs * 1000.0,
            f64::NAN,
            f64::NAN,
            format!(
                "{} rows, {:.1} MiB, truncated={}",
                out.rows_written,
                out.bytes_written as f64 / 1048576.0,
                out.truncated
            ),
        ));
    }

    // Index rebuild (FTS to current version, forced) + interrupted recovery.
    {
        for d in &b.selection {
            b.ws.meta
                .set_index_state("fts", d, 1, "pending", "{}")
                .expect("state");
        }
        let (ctx, control, _rx) = JobContext::detached("bench-rebuild-interrupt");
        control.cancel();
        let interrupted = explorer::rebuild_fts_to_current(&b.ws, &b.engine, &ctx);
        assert!(interrupted.is_err(), "pre-cancelled rebuild reports cancel");
        let (ctx2, _c2, _rx2) = JobContext::detached("bench-rebuild");
        let t = Instant::now();
        let n = explorer::rebuild_fts_to_current(&b.ws, &b.engine, &ctx2).expect("rebuild");
        println!(
            "  {:<38} {:>9.1} ms · {n} rows reindexed (after interrupted attempt)",
            "FTS rebuild + recovery",
            ms(t.elapsed())
        );
        b.results.push((
            "fts rebuild".into(),
            ms(t.elapsed()),
            f64::NAN,
            f64::NAN,
            format!("{n} rows"),
        ));
    }

    // Machine-readable summary.
    let peak = peak_working_set_bytes().unwrap_or(0);
    println!("\npeak working set: {:.0} MiB", peak as f64 / 1048576.0);
    let json = serde_json::json!({
        "records": count,
        "workspace_open_ms": open_ms,
        "import_ms": import_ms,
        "peak_working_set_bytes": peak,
        "debug_build": cfg!(debug_assertions),
        "results": b.results.iter().map(|(name, cold, p50, max, note)| serde_json::json!({
            "name": name, "cold_ms": cold, "warm_p50_ms": p50, "warm_max_ms": max, "note": note,
        })).collect::<Vec<_>>(),
    });
    println!("JSON: {json}");
}

fn build_workspace(dir: &Path, count: u64) {
    let corpus_dir = tempfile::tempdir().expect("tempdir");
    let corpus = corpus_dir.path().join("bench-logs.jsonl");
    let file = std::fs::File::create(&corpus).expect("corpus file");
    let shape = write_logs_jsonl(std::io::BufWriter::new(file), count, 20260729).expect("generate");
    println!(
        "corpus: {} lines, {:.0} MiB, {} cascade lines",
        shape.lines,
        shape.bytes as f64 / 1048576.0,
        shape.searchable_token_lines
    );
    let mut ws = Workspace::create(dir, "bench", "0.2.0-bench").expect("create ws");
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
