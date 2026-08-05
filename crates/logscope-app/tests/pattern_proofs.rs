//! WP2 pattern-engine proofs over a real imported workspace: exact
//! template families with exact counts, deterministic reruns (same
//! semantic fingerprint, identical summaries), stack fingerprints that
//! ignore volatile line numbers but never merge distinct exception
//! types, honest missing-field/malformed/truncation accounting,
//! deterministic drill-down that refuses stale scopes, and cancellation
//! that can never look like an empty success.

use std::io::Write as _;
use std::path::Path;

use logscope_app::{analysis, patterns, run_import, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::{JobContext, JobControl};
use logscope_query::{EngineConnection, TimeStrategy};
use logscope_workspace::Workspace;

fn fg_ctx(job_id: &str) -> (JobContext, JobControl) {
    let (ctx, control, rx) = JobContext::detached(job_id);
    std::mem::forget(rx);
    (ctx, control)
}

const JAVA_A: &str = "java.lang.IllegalStateException: broken\\n\\tat com.example.Service.handle(Service.java:LINE)\\n\\tat com.example.Loop.run(Loop.java:9)";
const JAVA_B: &str = "java.io.IOException: disk\\n\\tat com.example.Disk.write(Disk.java:7)";

fn write_corpus(path: &Path) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut line = 0usize;
    let ts = |line: usize| format!("2024-06-01T10:{:02}:{:02}Z", (line / 60) % 60, line % 60);
    // Family A: 30 records, volatile number + duration.
    for i in 0..30 {
        writeln!(
            f,
            "{{\"@timestamp\":\"{}\",\"level\":\"INFO\",\"message\":\"handler {} finished in {}ms\",\"service\":\"orders\"}}",
            ts(line), i, i + 3
        )
        .unwrap();
        line += 1;
    }
    // Family B: 20 records, volatile uuid-ish hex id.
    for i in 0..20 {
        writeln!(
            f,
            "{{\"@timestamp\":\"{}\",\"level\":\"ERROR\",\"message\":\"payment failed for order {:032x}\",\"service\":\"payments\"}}",
            ts(line), 0xabc000u128 + i as u128
        )
        .unwrap();
        line += 1;
    }
    // Family C: 10 records.
    for i in 0..10 {
        writeln!(
            f,
            "{{\"@timestamp\":\"{}\",\"level\":\"INFO\",\"message\":\"cache miss key {}\",\"service\":\"cache\"}}",
            ts(line), i * 7
        )
        .unwrap();
        line += 1;
    }
    // Family A without timestamps: counted, not dated.
    for i in 0..3 {
        writeln!(
            f,
            "{{\"level\":\"INFO\",\"message\":\"handler {} finished in {}ms\",\"service\":\"orders\"}}",
            100 + i, 5
        )
        .unwrap();
    }
    // Stack corpus: same Java trace with volatile line numbers (8),
    // a different exception type (4), malformed stacks (3), and
    // records without the field at all (5).
    for i in 0..8 {
        writeln!(
            f,
            "{{\"@timestamp\":\"{}\",\"level\":\"ERROR\",\"message\":\"boom\",\"stack\":\"{}\"}}",
            ts(line),
            JAVA_A.replace("LINE", &(40 + i).to_string())
        )
        .unwrap();
        line += 1;
    }
    for _ in 0..4 {
        writeln!(
            f,
            "{{\"@timestamp\":\"{}\",\"level\":\"ERROR\",\"message\":\"boom\",\"stack\":\"{JAVA_B}\"}}",
            ts(line)
        )
        .unwrap();
        line += 1;
    }
    for _ in 0..3 {
        writeln!(
            f,
            "{{\"@timestamp\":\"{}\",\"level\":\"ERROR\",\"message\":\"boom\",\"stack\":\"not a stack at all\"}}",
            ts(line)
        )
        .unwrap();
        line += 1;
    }
    for _ in 0..5 {
        writeln!(
            f,
            "{{\"@timestamp\":\"{}\",\"level\":\"ERROR\",\"message\":\"no stack field here\"}}",
            ts(line)
        )
        .unwrap();
        line += 1;
    }
}

struct Env {
    _dir: tempfile::TempDir,
    ws: Workspace,
    engine: EngineConnection,
    dataset_id: String,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jsonl");
    write_corpus(&input);
    let engine = EngineConnection::open_in_memory().unwrap();
    let mut ws = Workspace::create(&dir.path().join("ws"), "patterns", "0.4.0-test").unwrap();
    let (ctx, _c) = fg_ctx("job-import");
    let outcome = run_import(
        &mut ws,
        &engine,
        &ImportRequest::new(vec![input], builtin::jsonl_generic(), "logs"),
        &ctx,
    )
    .unwrap();
    Env {
        _dir: dir,
        ws,
        engine,
        dataset_id: outcome.dataset_id,
    }
}

fn message_def(e: &Env, limits_json: &str) -> String {
    analysis::create_definition(
        &e.ws,
        &analysis::NewDefinitionRequest {
            kind: "message_pattern".into(),
            name: "templates".into(),
            description: None,
            dataset_ids: vec![e.dataset_id.clone()],
            query_text: String::new(),
            time_strategy: TimeStrategy::All,
            field_selection_json: "{}".into(),
            algorithm_id: "template.mask".into(),
            algorithm_version: 1,
            config_json: "{\"bucket_seconds\":60}".into(),
            masking_profile_json: "{}".into(),
            thresholds_json: "{}".into(),
            limits_json: limits_json.into(),
        },
    )
    .unwrap()
    .definition_id
}

#[test]
fn template_families_get_exact_counts_and_deterministic_reruns() {
    let e = env();
    let def = message_def(&e, "{}");
    let (ctx, _c) = fg_ctx("job-run-1");
    let run1 = patterns::run_pattern_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run1.state, "completed");
    let counts: serde_json::Value = serde_json::from_str(&run1.counts_json).unwrap();
    assert_eq!(counts["excluded_missing_field"], 0);

    let page = patterns::list_patterns(&e.ws, &e.engine, &run1.run_id, 0, 100).unwrap();
    let handler = page
        .iter()
        .find(|p| p.template == "handler <num> finished in <dur>")
        .expect("family A template");
    assert_eq!(handler.count, 33, "30 dated + 3 untimestamped");
    assert_eq!(handler.untimestamped, 3);
    assert!(handler.first_seen.is_some());
    let payment = page
        .iter()
        .find(|p| p.template == "payment failed for order <id>")
        .expect("family B template");
    assert_eq!(payment.count, 20);
    let cache = page
        .iter()
        .find(|p| p.template == "cache miss key <num>")
        .expect("family C template");
    assert_eq!(cache.count, 10);
    // Ordering: count DESC then pattern id; family A is first.
    assert_eq!(page[0].pattern_id, handler.pattern_id);
    let examples: Vec<patterns::PatternExample> =
        serde_json::from_str(&handler.examples_json).unwrap();
    assert!(examples.iter().any(|x| x.role == "earliest"));
    assert!(examples.iter().any(|x| x.role == "peak"));

    // Rerun: same semantic identity, identical summaries.
    let (ctx, _c) = fg_ctx("job-run-2");
    let run2 = patterns::run_pattern_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run1.semantic_fingerprint, run2.semantic_fingerprint);
    assert_ne!(run1.run_id, run2.run_id);
    let page2 = patterns::list_patterns(&e.ws, &e.engine, &run2.run_id, 0, 100).unwrap();
    let key = |p: &patterns::PatternSummary| {
        (
            p.pattern_id.clone(),
            p.count,
            p.first_seen,
            p.last_seen,
            p.examples_json.clone(),
        )
    };
    assert_eq!(
        page.iter().map(key).collect::<Vec<_>>(),
        page2.iter().map(key).collect::<Vec<_>>(),
        "reruns produce identical summaries"
    );

    // The derived artifact is cataloged with its real checksum.
    let artifacts = e.ws.meta.list_derived_artifacts(&run1.run_id).unwrap();
    assert_eq!(artifacts.len(), 1);
    let path =
        e.ws.layout
            .derived_analysis_dir(&run1.run_id)
            .join(patterns::SUMMARIES_FILE);
    assert!(path.exists());
    assert_eq!(artifacts[0].row_count, page.len() as i64);
}

#[test]
fn stack_fingerprints_ignore_volatile_lines_but_never_merge_types() {
    let e = env();
    let def = analysis::create_definition(
        &e.ws,
        &analysis::NewDefinitionRequest {
            kind: "stack_fingerprint".into(),
            name: "stacks".into(),
            description: None,
            dataset_ids: vec![e.dataset_id.clone()],
            query_text: String::new(),
            time_strategy: TimeStrategy::All,
            field_selection_json: "{\"stack_field\":\"stack\"}".into(),
            algorithm_id: "stack.frames".into(),
            algorithm_version: 1,
            config_json: "{}".into(),
            masking_profile_json: "{}".into(),
            thresholds_json: "{}".into(),
            limits_json: "{}".into(),
        },
    )
    .unwrap()
    .definition_id;
    let (ctx, _c) = fg_ctx("job-stacks");
    let run = patterns::run_pattern_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.state, "completed");
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert_eq!(
        counts["stack_malformed"], 3,
        "malformed stacks counted, not merged"
    );
    assert!(counts["excluded_missing_field"].as_u64().unwrap() >= 5);

    let page = patterns::list_patterns(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    assert_eq!(
        page.len(),
        2,
        "volatile line numbers collapse; distinct types do not"
    );
    let ise = page
        .iter()
        .find(|p| p.exception_type.as_deref() == Some("java.lang.IllegalStateException"))
        .unwrap();
    assert_eq!(ise.count, 8);
    assert_eq!(ise.parse_quality.as_deref(), Some("parsed"));
    let io = page
        .iter()
        .find(|p| p.exception_type.as_deref() == Some("java.io.IOException"))
        .unwrap();
    assert_eq!(io.count, 4);
}

#[test]
fn drill_down_matches_the_summary_and_refuses_stale_scopes() {
    let e = env();
    let def = message_def(&e, "{}");
    let (ctx, _c) = fg_ctx("job-run");
    let run = patterns::run_pattern_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    let page = patterns::list_patterns(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let payment = page
        .iter()
        .find(|p| p.template == "payment failed for order <id>")
        .unwrap();

    let rows = patterns::pattern_records(&e.ws, &e.engine, &run.run_id, &payment.pattern_id, 1000)
        .unwrap();
    assert_eq!(rows.len(), payment.count as usize);
    assert!(rows
        .iter()
        .all(|r| r.display_message.starts_with("payment failed for order")));

    // Bounded drill-down honors the limit deterministically.
    let bounded =
        patterns::pattern_records(&e.ws, &e.engine, &run.run_id, &payment.pattern_id, 5).unwrap();
    assert_eq!(bounded.len(), 5);
    assert_eq!(
        bounded.iter().map(|r| &r.record_id).collect::<Vec<_>>(),
        rows.iter()
            .take(5)
            .map(|r| &r.record_id)
            .collect::<Vec<_>>(),
    );

    // A revised definition makes the run stale; drill-down refuses
    // rather than silently answering from a moved scope.
    let d = e.ws.meta.get_analysis_definition(&def).unwrap().unwrap();
    e.ws.meta
        .update_analysis_definition(&logscope_workspace::AnalysisDefinitionEdit {
            definition_id: def.clone(),
            expected_revision: d.revision,
            name: "revised".into(),
            description: None,
            dataset_selection_json: d.dataset_selection_json.clone(),
            query_text: d.query_text.clone(),
            query_language_version: d.query_language_version,
            query_fingerprint: d.query_fingerprint.clone(),
            time_strategy_json: d.time_strategy_json.clone(),
            field_selection_json: d.field_selection_json.clone(),
            config_json: d.config_json.clone(),
            config_fingerprint: d.config_fingerprint.clone(),
            masking_profile_json: d.masking_profile_json.clone(),
            thresholds_json: d.thresholds_json.clone(),
            limits_json: d.limits_json.clone(),
        })
        .unwrap();
    let err = patterns::pattern_records(&e.ws, &e.engine, &run.run_id, &payment.pattern_id, 10)
        .unwrap_err();
    assert_eq!(err.code, "analysis/stale-run");
}

#[test]
fn pattern_limit_truncates_explicitly_never_silently() {
    let e = env();
    let def = message_def(&e, "{\"max_patterns\":2}");
    let (ctx, _c) = fg_ctx("job-limited");
    let run = patterns::run_pattern_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.state, "completed");
    let manifest: serde_json::Value =
        serde_json::from_str(run.manifest_json.as_deref().unwrap()).unwrap();
    assert_eq!(manifest["patterns_truncated"], true);
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert!(counts["excluded_over_pattern_limit"].as_u64().unwrap() > 0);
    let page = patterns::list_patterns(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    assert_eq!(page.len(), 2);
}

#[test]
fn cancellation_is_terminal_and_leaves_no_derived_artifact() {
    let e = env();
    let def = message_def(&e, "{}");
    let (ctx, control) = fg_ctx("job-cancelled");
    control.cancel();
    let run = patterns::run_pattern_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.state, "cancelled");
    assert!(
        run.manifest_json.is_none(),
        "no result manifest on cancellation"
    );
    assert!(e
        .ws
        .meta
        .list_derived_artifacts(&run.run_id)
        .unwrap()
        .is_empty());
    let err = patterns::list_patterns(&e.ws, &e.engine, &run.run_id, 0, 10).unwrap_err();
    assert_eq!(err.code, "analysis/invalid-definition");
}
