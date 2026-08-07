//! WP4a correlation proofs over a real imported workspace: exact
//! relationships only from canonical telemetry identity, span-alone
//! refused, invalid identifiers visible but never grouped, correlated
//! groups only under a visible typed rule, deterministic ordering with
//! stable tie-breakers, undated records counted but never sequenced,
//! bounded groups/events/edges with honest truncation counts, and
//! cancellation that is never an empty success.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;

use logscope_app::{analysis, correlation, run_import, ImportRequest};
use logscope_ingest::{builtin, FieldRef, ImportProfile};
use logscope_jobs::{JobContext, JobControl};
use logscope_query::{EngineConnection, TimeStrategy};
use logscope_workspace::Workspace;

fn fg_ctx(job_id: &str) -> (JobContext, JobControl) {
    let (ctx, control, rx) = JobContext::detached(job_id);
    std::mem::forget(rx);
    (ctx, control)
}

// Canonical 32-hex trace IDs and 16-hex span IDs.
const TRACE_A: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90";
const TRACE_B: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";
const TRACE_LONELY: &str = "1111222233334444555566667777888a";
const SPAN_1: &str = "0011223344556677";
const SPAN_2: &str = "8899aabbccddeeff";

/// Corpus notes (all synthetic):
/// - trace A: four dated records (two per span) plus one undated one;
/// - trace B: three records, two sharing an event time so the
///   record-ID tie-break is exercised;
/// - one record whose trace ID is malformed — ingestion keeps the
///   original visible as an attribute and leaves the canonical column
///   empty, so it must never join an exact group;
/// - one record with a span but no trace;
/// - one lonely trace, which is not a relationship;
/// - request IDs for the correlated-key case.
fn write_corpus(path: &Path) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut line = |seconds: Option<u32>, extra: &str| {
        let ts = match seconds {
            Some(s) => format!("\"@timestamp\":\"2024-06-01T10:00:{s:02}Z\","),
            None => String::new(),
        };
        writeln!(
            f,
            "{{{ts}\"level\":\"INFO\",\"message\":\"handled\"{extra}}}"
        )
        .unwrap();
    };
    // Trace A, span 1 and span 2.
    line(
        Some(1),
        &format!(",\"trace_id\":\"{TRACE_A}\",\"span_id\":\"{SPAN_1}\",\"request_id\":\"req-1\""),
    );
    line(
        Some(2),
        &format!(",\"trace_id\":\"{TRACE_A}\",\"span_id\":\"{SPAN_1}\",\"request_id\":\"req-1\""),
    );
    line(
        Some(3),
        &format!(",\"trace_id\":\"{TRACE_A}\",\"span_id\":\"{SPAN_2}\",\"request_id\":\"req-1\""),
    );
    line(
        Some(4),
        &format!(",\"trace_id\":\"{TRACE_A}\",\"span_id\":\"{SPAN_2}\""),
    );
    // Same trace, no timestamp: a member that cannot be sequenced.
    line(
        None,
        &format!(",\"trace_id\":\"{TRACE_A}\",\"span_id\":\"{SPAN_1}\""),
    );
    // Trace B: two records share an event time.
    line(Some(5), &format!(",\"trace_id\":\"{TRACE_B}\""));
    line(Some(5), &format!(",\"trace_id\":\"{TRACE_B}\""));
    line(Some(9), &format!(",\"trace_id\":\"{TRACE_B}\""));
    // Malformed identifier: kept for diagnostics, never an exact match.
    line(Some(6), ",\"trace_id\":\"not-a-valid-trace-id\"");
    // A span with no trace is not a key.
    line(Some(7), &format!(",\"span_id\":\"{SPAN_1}\""));
    // One record alone under its own trace.
    line(Some(8), &format!(",\"trace_id\":\"{TRACE_LONELY}\""));
    // A second request ID appearing once.
    line(Some(10), ",\"request_id\":\"req-2\"");
}

/// jsonl_generic plus the canonical request-ID mapping.
fn profile() -> ImportProfile {
    let mut p = builtin::jsonl_generic();
    p.profile_id = "test.jsonl.correlation".into();
    p.generic_fields =
        BTreeMap::from([("request_id".to_string(), vec![FieldRef::name("request_id")])]);
    p
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
    let mut ws = Workspace::create(&dir.path().join("ws"), "corr", "0.4.0-test").unwrap();
    let (ctx, _c) = fg_ctx("job-import");
    let outcome = run_import(
        &mut ws,
        &engine,
        &ImportRequest::new(vec![input], profile(), "logs"),
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

fn corr_def(e: &Env, config_json: &str, limits_json: &str) -> String {
    analysis::create_definition(
        &e.ws,
        &analysis::NewDefinitionRequest {
            kind: "correlation".into(),
            name: "correlate".into(),
            description: None,
            dataset_ids: vec![e.dataset_id.clone()],
            query_text: String::new(),
            time_strategy: TimeStrategy::All,
            field_selection_json: "{}".into(),
            algorithm_id: "corr-rules".into(),
            algorithm_version: 1,
            config_json: config_json.into(),
            masking_profile_json: "{}".into(),
            thresholds_json: "{}".into(),
            limits_json: limits_json.into(),
        },
    )
    .unwrap()
    .definition_id
}

fn run_corr(
    e: &Env,
    config_json: &str,
    limits_json: &str,
    job: &str,
) -> logscope_workspace::AnalysisRunRow {
    let def = corr_def(e, config_json, limits_json);
    let (ctx, _c) = fg_ctx(job);
    correlation::run_correlation_analysis(&e.ws, &e.engine, &def, &ctx).unwrap()
}

fn by_key<'a>(
    groups: &'a [correlation::CorrelationGroup],
    key: &str,
) -> &'a correlation::CorrelationGroup {
    groups
        .iter()
        .find(|g| g.key_value == key)
        .unwrap_or_else(|| panic!("missing group {key:?}"))
}

#[test]
fn exact_groups_come_only_from_canonical_telemetry_identity() {
    let e = env();
    let run = run_corr(&e, "{\"key\":\"trace_id\"}", "{}", "job-trace");
    assert_eq!(run.state, "completed");
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert_eq!(counts["scanned"], 12);
    // Keyed: 5 in trace A + 3 in trace B + 1 lonely = 9. The malformed
    // trace, the span-only record, and the request-only record have no
    // canonical trace ID at all.
    assert_eq!(counts["keyed"], 9);
    assert_eq!(counts["rejected"]["missing_field"], 3);
    assert_eq!(
        counts["singleton_keys"], 1,
        "a trace appearing once is not a relationship"
    );

    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    assert_eq!(groups.len(), 2);
    assert!(groups.iter().all(|g| g.confidence == "exact"));
    assert!(groups.iter().all(|g| g.group_id.starts_with("acor-")));
    assert!(
        groups.iter().all(|g| g.key_value != TRACE_LONELY),
        "the lonely trace produced no group"
    );

    let a = by_key(&groups, TRACE_A);
    assert_eq!(a.event_count, 4, "four dated members");
    assert_eq!(a.undated_count, 1, "the undated member is counted");
    assert_eq!(a.truncated_count, 0);
    assert_eq!(a.edge_count, 3, "consecutive pairs, never a pairwise join");
    let b = by_key(&groups, TRACE_B);
    assert_eq!((b.event_count, b.edge_count), (3, 2));
    // Stored order: larger group first.
    assert_eq!(groups[0].key_value, TRACE_A);

    // The malformed identifier is still visible on its record for
    // diagnostics — it simply never reaches the canonical column, which
    // is why it cannot produce an exact relationship.
    let (kept, canonical): (i64, i64) = segment_counts(
        &e,
        "count(*) FILTER (WHERE attributes_json LIKE '%trace_id.raw%' \
           AND attributes_json LIKE '%not-a-valid-trace-id%' AND trace_id IS NULL), \
         count(*) FILTER (WHERE trace_id = 'not-a-valid-trace-id')",
    );
    assert_eq!(
        kept, 1,
        "the invalid original stays visible as an attribute"
    );
    assert_eq!(canonical, 0, "and never reaches the canonical column");
}

/// Runs one aggregate over the dataset's published segments.
fn segment_counts(e: &Env, projection: &str) -> (i64, i64) {
    let files =
        logscope_app::explorer::segment_files_for(&e.ws, std::slice::from_ref(&e.dataset_id))
            .unwrap();
    let quoted: Vec<String> = files
        .iter()
        .map(|p| format!("'{}'", p.display().to_string().replace('\\', "/")))
        .collect();
    let conn = e.engine.raw();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {projection} FROM read_parquet([{}])",
            quoted.join(",")
        ))
        .unwrap();
    let mut rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows.pop().unwrap()
}

#[test]
fn span_alone_is_refused_and_trace_span_needs_both_halves() {
    let e = env();
    // Span alone cannot select a group, and the refusal explains why.
    let def = corr_def(&e, "{\"key\":\"span_id\"}", "{}");
    let (ctx, _c) = fg_ctx("job-span");
    let err = correlation::run_correlation_analysis(&e.ws, &e.engine, &def, &ctx).unwrap_err();
    assert_eq!(err.code, "analysis/invalid-definition");
    assert!(
        err.message.contains("unique only within its trace"),
        "{}",
        err.message
    );
    assert!(
        err.message.contains("trace_span"),
        "the answer names the fix"
    );
    assert!(
        e.ws.meta.list_analysis_runs(Some(&def)).unwrap().is_empty(),
        "refused before any run record exists"
    );

    // trace_span groups the records of one span, and a trace without a
    // span is an explicitly counted incomplete pair.
    let run = run_corr(&e, "{\"key\":\"trace_span\"}", "{}", "job-trace-span");
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert_eq!(
        counts["rejected"]["incomplete_pair"], 4,
        "trace B's three records and the lonely trace carry no span"
    );
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let span1 = by_key(&groups, &format!("{TRACE_A}/{SPAN_1}"));
    assert_eq!((span1.event_count, span1.undated_count), (2, 1));
    let span2 = by_key(&groups, &format!("{TRACE_A}/{SPAN_2}"));
    assert_eq!(span2.event_count, 2);
    assert!(groups.iter().all(|g| g.confidence == "exact"));
}

#[test]
fn correlated_keys_are_typed_visible_and_never_claim_more_than_they_prove() {
    let e = env();
    let run = run_corr(&e, "{\"key\":\"request_id\"}", "{}", "job-req");
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    assert_eq!(groups.len(), 1, "req-2 appears once and is not a group");
    let g = by_key(&groups, "req-1");
    assert_eq!(g.confidence, "correlated", "a typed app ID is never exact");
    assert_eq!(g.event_count, 3);
    assert!(g.reason.contains("typed request ID req-1"));
    assert!(g.reason.contains("no normalization"));
    assert!(g.reason.contains("does not establish ordering"));
    assert!(
        !g.reason.contains("reconstructed trace"),
        "the trace limitation belongs to trace groups only"
    );

    // Normalization is allowed here, but it is versioned and reported.
    let run = run_corr(
        &e,
        "{\"key\":\"request_id\",\"normalization\":{\"case_fold\":true,\"strip_prefix\":\"req-\"}}",
        "{}",
        "job-req-norm",
    );
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let g = by_key(&groups, "1");
    assert!(g
        .reason
        .contains("Normalization applied before matching: strip_prefix"));
    let manifest: serde_json::Value =
        serde_json::from_str(run.manifest_json.as_deref().unwrap()).unwrap();
    assert_eq!(manifest["normalization"]["strip_prefix"], "req-");

    // Normalizing a canonical identifier is refused outright.
    let def = corr_def(
        &e,
        "{\"key\":\"trace_id\",\"normalization\":{\"case_fold\":true}}",
        "{}",
    );
    let (ctx, _c) = fg_ctx("job-bad-norm");
    let err = correlation::run_correlation_analysis(&e.ws, &e.engine, &def, &ctx).unwrap_err();
    assert!(err.message.contains("validated and normalized at ingest"));

    // Unknown config keys are refused, never silently defaulted.
    let def = corr_def(&e, "{\"key\":\"request_id\",\"fuzzy\":true}", "{}");
    let (ctx, _c) = fg_ctx("job-bad-cfg");
    let err = correlation::run_correlation_analysis(&e.ws, &e.engine, &def, &ctx).unwrap_err();
    assert!(err.message.contains("fuzzy"));
}

#[test]
fn ordering_is_deterministic_and_undated_records_are_never_sequenced() {
    let e = env();
    let run1 = run_corr(&e, "{\"key\":\"trace_id\"}", "{}", "job-order-1");
    let edges =
        correlation::list_correlation_edges(&e.ws, &e.engine, &run1.run_id, "", 1000).unwrap();
    assert!(edges.is_empty(), "an unknown group has no edges");

    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run1.run_id, 0, 100).unwrap();
    let b = by_key(&groups, TRACE_B);
    let edges =
        correlation::list_correlation_edges(&e.ws, &e.engine, &run1.run_id, &b.group_id, 1000)
            .unwrap();
    assert_eq!(edges.len(), 2);
    // Two records share an event time: the delta is zero and the order
    // comes from the deterministic record ID, not from arrival.
    assert_eq!(edges[0].delta_nanos, 0);
    assert!(edges[0].from_record_id < edges[0].to_record_id);
    assert_eq!(edges[1].delta_nanos, 4_000_000_000, "5s to 9s");
    assert!(edges.iter().all(|edge| edge.confidence == "exact"));

    // Undated members never appear in any edge.
    let a = by_key(&groups, TRACE_A);
    let a_edges =
        correlation::list_correlation_edges(&e.ws, &e.engine, &run1.run_id, &a.group_id, 1000)
            .unwrap();
    assert_eq!(a_edges.len(), 3);
    let sequenced: Vec<&String> = a_edges.iter().map(|edge| &edge.from_record_id).collect();
    assert_eq!(
        sequenced.len(),
        3,
        "four dated members produce three consecutive pairs"
    );

    // Identical inputs produce identical identities and identical order.
    let run2 = run_corr(&e, "{\"key\":\"trace_id\"}", "{}", "job-order-2");
    assert_eq!(run1.semantic_fingerprint, run2.semantic_fingerprint);
    let groups2 =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run2.run_id, 0, 100).unwrap();
    let ids = |gs: &[correlation::CorrelationGroup]| {
        gs.iter()
            .map(|g| (g.group_id.clone(), g.key_value.clone(), g.event_count))
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&groups), ids(&groups2));
}

#[test]
fn every_explanation_reports_evidence_without_implying_causation() {
    let e = env();
    let run = run_corr(&e, "{\"key\":\"trace_id\"}", "{}", "job-reasons");
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let mut texts: Vec<String> = groups.iter().map(|g| g.reason.clone()).collect();
    for g in &groups {
        let edges =
            correlation::list_correlation_edges(&e.ws, &e.engine, &run.run_id, &g.group_id, 1000)
                .unwrap();
        texts.extend(edges.into_iter().map(|edge| edge.reason));
    }
    assert!(texts.len() >= 7);
    for text in &texts {
        let lowered = text.to_lowercase();
        for forbidden in [
            "caused by",
            "because of",
            "root cause",
            "therefore",
            "proves",
            "confirmed",
        ] {
            assert!(!lowered.contains(forbidden), "{forbidden:?} in {text:?}");
        }
    }
    for g in &groups {
        assert!(g.reason.contains("validated at ingest"));
        assert!(g.reason.contains("not a reconstructed trace"));
    }
}

#[test]
fn limits_bound_groups_events_and_edges_with_honest_counts() {
    let e = env();
    // Two members per group at most: trace A loses two of its four
    // dated records, and says so.
    let run = run_corr(
        &e,
        "{\"key\":\"trace_id\"}",
        "{\"max_events_per_group\":2}",
        "job-cap-events",
    );
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert_eq!(
        counts["events_truncated_in_groups"], 3,
        "2 from A, 1 from B"
    );
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let a = by_key(&groups, TRACE_A);
    assert_eq!((a.event_count, a.truncated_count, a.edge_count), (2, 2, 1));

    // One group per run: the high-cardinality guard counts what it
    // could not hold rather than quietly narrowing the domain.
    let run = run_corr(
        &e,
        "{\"key\":\"trace_id\"}",
        "{\"max_groups\":1}",
        "job-cap-groups",
    );
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert!(
        counts["records_over_group_limit"].as_u64().unwrap() > 0,
        "the guard fired and was counted"
    );
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    assert_eq!(groups.len(), 1);

    // Meaningless limits are refused instead of being clamped silently.
    let def = corr_def(&e, "{\"key\":\"trace_id\"}", "{\"min_group_size\":1}");
    let (ctx, _c) = fg_ctx("job-bad-limits");
    let err = correlation::run_correlation_analysis(&e.ws, &e.engine, &def, &ctx).unwrap_err();
    assert!(err.message.contains("min_group_size must be at least 2"));
}

#[test]
fn drill_down_returns_group_members_and_refuses_stale_runs() {
    let e = env();
    let def = corr_def(&e, "{\"key\":\"trace_id\"}", "{}");
    let (ctx, _c) = fg_ctx("job-drill");
    let run = correlation::run_correlation_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let a = by_key(&groups, TRACE_A);

    let records =
        correlation::correlation_records(&e.ws, &e.engine, &run.run_id, &a.group_id, 100).unwrap();
    assert_eq!(records.len(), 5, "four dated members plus the undated one");
    assert!(records
        .iter()
        .all(|r| r.trace_id.as_deref() == Some(TRACE_A)));
    // Dated members keep canonical order; the undated one sorts last
    // rather than being interleaved by import time.
    assert!(records.last().unwrap().event_time.is_none());
    let dated: Vec<i64> = records.iter().filter_map(|r| r.event_time).collect();
    assert!(dated.windows(2).all(|w| w[0] <= w[1]));

    assert!(
        correlation::correlation_records(&e.ws, &e.engine, &run.run_id, "acor-nope", 10).is_err(),
        "an unknown group is refused"
    );

    // Revised definition => stale => drill-down refused.
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
            config_fingerprint: "acfg-new".into(),
            masking_profile_json: d.masking_profile_json.clone(),
            thresholds_json: d.thresholds_json.clone(),
            limits_json: d.limits_json.clone(),
        })
        .unwrap();
    let err = correlation::correlation_records(&e.ws, &e.engine, &run.run_id, &a.group_id, 10)
        .unwrap_err();
    assert_eq!(err.code, "analysis/stale-run");
}

#[test]
fn cancellation_is_terminal_and_never_an_empty_success() {
    let e = env();
    let def = corr_def(&e, "{\"key\":\"trace_id\"}", "{}");
    let (ctx, control) = fg_ctx("job-cancelled");
    control.cancel();
    let run = correlation::run_correlation_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.state, "cancelled");
    assert!(run.manifest_json.is_none());
    assert!(e
        .ws
        .meta
        .list_derived_artifacts(&run.run_id)
        .unwrap()
        .is_empty());
}
