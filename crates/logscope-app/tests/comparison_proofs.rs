//! WP3 comparison proofs over a real imported workspace: every
//! classification produced from exact constructed counts, integer
//! rate math across the two windows, honest untimestamped exclusion,
//! deterministic reruns, overlap refusal before any run record exists,
//! side-correct drill-down with stale refusal, explicit top-K
//! truncation with a counted remainder, and cancellation that is never
//! an empty success.

use std::io::Write as _;
use std::path::Path;

use logscope_app::{analysis, comparison, run_import, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::{JobContext, JobControl};
use logscope_query::{EngineConnection, TimeStrategy};
use logscope_workspace::Workspace;

fn fg_ctx(job_id: &str) -> (JobContext, JobControl) {
    let (ctx, control, rx) = JobContext::detached(job_id);
    std::mem::forget(rx);
    (ctx, control)
}

// 2024-06-01T10:00:00Z and 11:00:00Z in UTC nanos; the suspect window
// is the following hour.
const T10: i64 = 1_717_236_000_000_000_000;
const T11: i64 = 1_717_239_600_000_000_000;

fn write_corpus(path: &Path) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    // (family text, baseline count, suspect count, severity)
    let families: &[(&str, usize, usize, &str)] = &[
        ("steady flow", 20, 20, "INFO"),
        ("error spike", 10, 40, "ERROR"),
        ("calmed down", 40, 10, "INFO"),
        ("fresh problem", 0, 12, "ERROR"),
        ("old noise", 12, 0, "INFO"),
        ("rare blip", 2, 3, "INFO"),
    ];
    let mut emit = |hour: u32, idx: usize, family: &str, level: &str, n: usize| {
        writeln!(
            f,
            "{{\"@timestamp\":\"2024-06-01T{hour:02}:{:02}:{:02}Z\",\"level\":\"{level}\",\
             \"message\":\"{family} {n}\",\"service\":\"orders\"}}",
            (idx / 60) % 60,
            idx % 60,
        )
        .unwrap();
    };
    for (family, base_n, susp_n, level) in families {
        for i in 0..*base_n {
            emit(10, i, family, level, i);
        }
        for i in 0..*susp_n {
            emit(11, i, family, level, i);
        }
    }
    // Untimestamped records can never enter a bounded window.
    for i in 0..4 {
        writeln!(
            f,
            "{{\"level\":\"INFO\",\"message\":\"steady flow {i}\",\"service\":\"orders\"}}"
        )
        .unwrap();
    }
}

/// `(operation, outcome)` when the source carries them at all.
type GenericFields<'a> = Option<(&'a str, &'a str)>;

/// Corpus for the canonical generic-field dimensions. Group C carries
/// neither field, so the honest exclusion count is observable.
fn write_generic_corpus(path: &Path) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    // (operation, outcome, baseline count, suspect count)
    let groups: &[(GenericFields, usize, usize)] = &[
        (Some(("checkout", "success")), 30, 12),
        (Some(("refund", "failure")), 4, 20),
        (None, 6, 6),
    ];
    let mut emit = |hour: u32, idx: usize, fields: Option<(&str, &str)>| {
        let tail = match fields {
            Some((op, outcome)) => {
                format!(",\"operation\":\"{op}\",\"outcome\":\"{outcome}\"")
            }
            None => String::new(),
        };
        writeln!(
            f,
            "{{\"@timestamp\":\"2024-06-01T{hour:02}:{:02}:{:02}Z\",\"level\":\"INFO\",\
             \"message\":\"request handled\"{tail}}}",
            (idx / 60) % 60,
            idx % 60,
        )
        .unwrap();
    };
    for (fields, base_n, susp_n) in groups {
        for i in 0..*base_n {
            emit(10, i, *fields);
        }
        for i in 0..*susp_n {
            emit(11, i, *fields);
        }
    }
}

/// Maps the canonical generic fields so the operation/outcome
/// dimensions have real values. `event_name` is deliberately left
/// unmapped — JSONL sources never carry it, and the honest result is a
/// counted exclusion rather than one giant "(none)" key.
fn generic_profile() -> logscope_ingest::ImportProfile {
    use logscope_ingest::FieldRef;
    let mut p = builtin::jsonl_generic();
    p.profile_id = "test.jsonl.generic-fields".into();
    p.generic_fields = std::collections::BTreeMap::from([
        ("operation".to_string(), vec![FieldRef::name("operation")]),
        ("outcome".to_string(), vec![FieldRef::name("outcome")]),
    ]);
    p
}

struct Env {
    _dir: tempfile::TempDir,
    ws: Workspace,
    engine: EngineConnection,
    dataset_id: String,
}

fn env() -> Env {
    env_with(write_corpus, builtin::jsonl_generic())
}

fn env_with(write: fn(&Path), profile: logscope_ingest::ImportProfile) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jsonl");
    write(&input);
    let engine = EngineConnection::open_in_memory().unwrap();
    let mut ws = Workspace::create(&dir.path().join("ws"), "cmp", "0.4.0-test").unwrap();
    let (ctx, _c) = fg_ctx("job-import");
    let outcome = run_import(
        &mut ws,
        &engine,
        &ImportRequest::new(vec![input], profile, "logs"),
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

fn cmp_def(e: &Env, dimension: &str, extra: &str, thresholds: &str) -> String {
    let t12 = T11 + (T11 - T10);
    let config = format!(
        "{{\"dimension\":\"{dimension}\",{extra}\"baseline_start\":{T10},\
         \"baseline_end\":{T11},\"suspect_start\":{T11},\"suspect_end\":{t12},\
         \"top_k\":100}}"
    );
    analysis::create_definition(
        &e.ws,
        &analysis::NewDefinitionRequest {
            kind: "comparison".into(),
            name: format!("compare {dimension}"),
            description: None,
            dataset_ids: vec![e.dataset_id.clone()],
            query_text: String::new(),
            time_strategy: TimeStrategy::All,
            field_selection_json: "{}".into(),
            algorithm_id: "cmp-rules".into(),
            algorithm_version: 1,
            config_json: config,
            masking_profile_json: "{}".into(),
            thresholds_json: thresholds.into(),
            limits_json: "{}".into(),
        },
    )
    .unwrap()
    .definition_id
}

fn by_key<'a>(
    rows: &'a [comparison::ComparisonResult],
    key: &str,
) -> &'a comparison::ComparisonResult {
    rows.iter()
        .find(|r| r.key == key)
        .unwrap_or_else(|| panic!("missing key {key:?}"))
}

#[test]
fn every_classification_comes_from_exact_counts_and_reruns_are_identical() {
    let e = env();
    let def = cmp_def(&e, "message_pattern", "", "{\"min_count\":5,\"min_new_count\":5,\"min_gone_count\":5,\"rel_threshold_bp\":5000,\"abs_threshold\":10}");
    let (ctx, _c) = fg_ctx("job-cmp");
    let run = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.state, "completed");

    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert_eq!(counts["untimestamped_excluded"], 4, "no silent vanishing");
    assert_eq!(counts["baseline_accepted"], 84); // 20+10+40+0+12+2
    assert_eq!(counts["suspect_accepted"], 85); // 20+40+10+12+0+3

    let rows = comparison::list_comparison_results(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let steady = by_key(&rows, "steady flow <num>");
    assert_eq!(steady.classification, "unchanged");
    assert_eq!(steady.rate_change_bp, "0");
    let spike = by_key(&rows, "error spike <num>");
    assert_eq!(spike.classification, "increased");
    assert_eq!(spike.rate_change_bp, "30000", "+300 % as basis points");
    assert_eq!((spike.baseline_count, spike.suspect_count), (10, 40));
    let calmed = by_key(&rows, "calmed down <num>");
    assert_eq!(calmed.classification, "decreased");
    assert_eq!(calmed.rate_change_bp, "-7500");
    let fresh = by_key(&rows, "fresh problem <num>");
    assert_eq!(fresh.classification, "new");
    assert_eq!(
        fresh.rate_change_bp, "undefined",
        "zero baseline never divides"
    );
    let old = by_key(&rows, "old noise <num>");
    assert_eq!(old.classification, "disappeared");
    let rare = by_key(&rows, "rare blip <num>");
    assert_eq!(rare.classification, "insufficient_data");
    // Stored order: combined count desc, then key.
    assert_eq!(rows[0].key, "calmed down <num>");
    assert!(rows[0].result_id.starts_with("acmp-"));

    // Rerun: same semantic identity, identical result rows and ids.
    let (ctx, _c) = fg_ctx("job-cmp-2");
    let run2 = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.semantic_fingerprint, run2.semantic_fingerprint);
    let rows2 =
        comparison::list_comparison_results(&e.ws, &e.engine, &run2.run_id, 0, 100).unwrap();
    let key = |r: &comparison::ComparisonResult| {
        (
            r.result_id.clone(),
            r.key.clone(),
            r.classification.clone(),
            r.rate_change_bp.clone(),
        )
    };
    assert_eq!(
        rows.iter().map(key).collect::<Vec<_>>(),
        rows2.iter().map(key).collect::<Vec<_>>()
    );
}

#[test]
fn severity_dimension_and_top_k_truncation_are_honest() {
    let e = env();
    let def = cmp_def(&e, "severity", "", "{\"min_count\":2,\"min_new_count\":2,\"min_gone_count\":2,\"rel_threshold_bp\":2000,\"abs_threshold\":1000}");
    let (ctx, _c) = fg_ctx("job-sev");
    let run = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    let rows = comparison::list_comparison_results(&e.ws, &e.engine, &run.run_id, 0, 10).unwrap();
    // ERROR: 10 baseline (spike) vs 52 suspect (spike 40 + fresh 12).
    let error = by_key(&rows, "ERROR");
    assert_eq!((error.baseline_count, error.suspect_count), (10, 52));
    assert_eq!(error.classification, "increased");

    // Unknown config keys are refused, never silently defaulted — the
    // same strict-parse rule as mask sets and thresholds.
    let def = cmp_def(&e, "message_pattern", "\"top_k_override\":0,", "{}");
    let (ctx, _c) = fg_ctx("job-badcfg");
    let err = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap_err();
    assert_eq!(err.code, "analysis/invalid-definition");
    assert!(err.message.contains("top_k_override"));

    // top_k = 2 on the pattern dimension: 6 keys → 2 stored + counted
    // remainder, never a silent drop.
    let t12 = T11 + (T11 - T10);
    let config = format!(
        "{{\"dimension\":\"message_pattern\",\"baseline_start\":{T10},\"baseline_end\":{T11},\
         \"suspect_start\":{T11},\"suspect_end\":{t12},\"top_k\":2}}"
    );
    let def = analysis::create_definition(
        &e.ws,
        &analysis::NewDefinitionRequest {
            kind: "comparison".into(),
            name: "top-k".into(),
            description: None,
            dataset_ids: vec![e.dataset_id.clone()],
            query_text: String::new(),
            time_strategy: TimeStrategy::All,
            field_selection_json: "{}".into(),
            algorithm_id: "cmp-rules".into(),
            algorithm_version: 1,
            config_json: config,
            masking_profile_json: "{}".into(),
            thresholds_json: "{}".into(),
            limits_json: "{}".into(),
        },
    )
    .unwrap()
    .definition_id;
    let (ctx, _c) = fg_ctx("job-topk");
    let run = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(run.manifest_json.as_deref().unwrap()).unwrap();
    assert_eq!(manifest["distinct_keys"], 6);
    assert_eq!(manifest["remainder"]["keys"], 4);
    let rows = comparison::list_comparison_results(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    assert_eq!(rows.len(), 2);
}

#[test]
fn overlapping_windows_are_refused_before_any_run_exists() {
    let e = env();
    let config = format!(
        "{{\"dimension\":\"severity\",\"baseline_start\":{T10},\"baseline_end\":{T11},\
         \"suspect_start\":{},\"suspect_end\":{}}}",
        T11 - 60_000_000_000i64,
        T11 + 3_600_000_000_000i64
    );
    let def = analysis::create_definition(
        &e.ws,
        &analysis::NewDefinitionRequest {
            kind: "comparison".into(),
            name: "overlap".into(),
            description: None,
            dataset_ids: vec![e.dataset_id.clone()],
            query_text: String::new(),
            time_strategy: TimeStrategy::All,
            field_selection_json: "{}".into(),
            algorithm_id: "cmp-rules".into(),
            algorithm_version: 1,
            config_json: config,
            masking_profile_json: "{}".into(),
            thresholds_json: "{}".into(),
            limits_json: "{}".into(),
        },
    )
    .unwrap()
    .definition_id;
    let (ctx, _c) = fg_ctx("job-overlap");
    let err = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap_err();
    assert_eq!(err.code, "analysis/invalid-definition");
    assert!(err.message.contains("overlap"));
    assert!(
        e.ws.meta.list_analysis_runs(Some(&def)).unwrap().is_empty(),
        "refusal happens before any run record exists"
    );
}

#[test]
fn drill_down_is_side_correct_and_refuses_stale_runs() {
    let e = env();
    let def = cmp_def(&e, "message_pattern", "", "{}");
    let (ctx, _c) = fg_ctx("job-drill");
    let run = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();

    let baseline = comparison::comparison_records(
        &e.ws,
        &e.engine,
        &run.run_id,
        "error spike <num>",
        "baseline",
        1000,
    )
    .unwrap();
    assert_eq!(baseline.len(), 10);
    let suspect = comparison::comparison_records(
        &e.ws,
        &e.engine,
        &run.run_id,
        "error spike <num>",
        "suspect",
        1000,
    )
    .unwrap();
    assert_eq!(suspect.len(), 40);
    assert!(suspect
        .iter()
        .all(|r| r.display_message.starts_with("error spike")));
    assert!(comparison::comparison_records(
        &e.ws,
        &e.engine,
        &run.run_id,
        "error spike <num>",
        "sideways",
        10
    )
    .is_err());

    // Revised definition → stale → drill-down refused.
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
    let err = comparison::comparison_records(
        &e.ws,
        &e.engine,
        &run.run_id,
        "error spike <num>",
        "baseline",
        10,
    )
    .unwrap_err();
    assert_eq!(err.code, "analysis/stale-run");
}

#[test]
fn canonical_generic_dimensions_compare_and_count_their_exclusions() {
    let e = env_with(write_generic_corpus, generic_profile());

    // operation: 30 → 12 over equal hours is −60 %; 4 → 20 is +400 %.
    let def = cmp_def(&e, "operation", "", "{}");
    let (ctx, _c) = fg_ctx("job-op");
    let run = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.state, "completed");
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert_eq!(counts["baseline_accepted"], 34);
    assert_eq!(counts["suspect_accepted"], 32);
    assert_eq!(
        counts["excluded_missing_field"], 12,
        "records without the field are counted, never bucketed as (none)"
    );
    let rows = comparison::list_comparison_results(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let checkout = by_key(&rows, "checkout");
    assert_eq!((checkout.baseline_count, checkout.suspect_count), (30, 12));
    assert_eq!(checkout.classification, "decreased");
    assert_eq!(checkout.rate_change_bp, "-6000");
    let refund = by_key(&rows, "refund");
    assert_eq!(refund.classification, "increased");
    assert_eq!(refund.rate_change_bp, "40000");

    // outcome: the same records keyed by the other canonical field.
    let def = cmp_def(&e, "outcome", "", "{}");
    let (ctx, _c) = fg_ctx("job-outcome");
    let run = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    let rows = comparison::list_comparison_results(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(by_key(&rows, "success").classification, "decreased");
    assert_eq!(by_key(&rows, "failure").classification, "increased");
    // Drill-down proves the key really selected those records.
    let recs =
        comparison::comparison_records(&e.ws, &e.engine, &run.run_id, "failure", "suspect", 1000)
            .unwrap();
    assert_eq!(recs.len(), 20);

    // event_name is unmapped for JSONL: an empty result with every
    // record counted as excluded, not an invented distribution.
    let def = cmp_def(&e, "event_name", "", "{}");
    let (ctx, _c) = fg_ctx("job-evname");
    let run = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.state, "completed");
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert_eq!(counts["baseline_accepted"], 0);
    assert_eq!(counts["suspect_accepted"], 0);
    assert_eq!(counts["excluded_missing_field"], 78, "40 + 38 records");
    let manifest: serde_json::Value =
        serde_json::from_str(run.manifest_json.as_deref().unwrap()).unwrap();
    assert_eq!(manifest["distinct_keys"], 0);
    assert!(
        comparison::list_comparison_results(&e.ws, &e.engine, &run.run_id, 0, 100)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cancellation_is_terminal_and_never_an_empty_success() {
    let e = env();
    let def = cmp_def(&e, "message_pattern", "", "{}");
    let (ctx, control) = fg_ctx("job-cancelled");
    control.cancel();
    let run = comparison::run_comparison_analysis(&e.ws, &e.engine, &def, &ctx).unwrap();
    assert_eq!(run.state, "cancelled");
    assert!(run.manifest_json.is_none());
    assert!(e
        .ws
        .meta
        .list_derived_artifacts(&run.run_id)
        .unwrap()
        .is_empty());
}
