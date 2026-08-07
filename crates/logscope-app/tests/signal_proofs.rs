//! WP4b proofs over a real imported workspace: the four `sig-rules` v1
//! signals on top of correlated groups, and probable neighborhoods.
//!
//! What these prove, beyond "the code runs":
//! - a retry reaches `documented` only when the source counted the
//!   attempts, and is otherwise labelled an investigative lead;
//! - an operational duplicate needs distinct source positions;
//! - skew and gap report both original timestamps, the delta and the
//!   tolerance, and never an adjusted time;
//! - a neighborhood is `probable` and nothing else, exposes all six
//!   things gate 21 asks for, and drops its least relevant members
//!   first;
//! - no generated string anywhere claims causation.

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

/// Corpus notes (all synthetic), one record per source line so record
/// numbers carry the order the source wrote them in:
/// - `rq-retry`: the source counts attempts, so the retry is documented;
/// - `rq-corr`: same operation after a failure with no counter, so the
///   retry is corroborated and stays a lead;
/// - `rq-dup`: one message emitted twice at distinct source positions;
/// - `rq-skew`: written later in the file, timestamped earlier;
/// - `rq-gap`: ten minutes of silence;
/// - `rq-near`: three records around one anchor for the neighborhood,
///   two sharing its operation and one not.
fn write_corpus(path: &Path) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut line = |ts: &str, msg: &str, extra: &str| {
        writeln!(
            f,
            "{{\"@timestamp\":\"{ts}\",\"level\":\"INFO\",\"message\":\"{msg}\"{extra}}}"
        )
        .unwrap();
    };

    line(
        "2024-06-01T10:00:00Z",
        "first try",
        ",\"request_id\":\"rq-retry\",\"operation\":\"checkout\",\"outcome\":\"failure\",\"attempt\":1",
    );
    line(
        "2024-06-01T10:00:01Z",
        "second try",
        ",\"request_id\":\"rq-retry\",\"operation\":\"checkout\",\"outcome\":\"success\",\"attempt\":2",
    );

    line(
        "2024-06-01T10:00:00Z",
        "refund failed",
        ",\"request_id\":\"rq-corr\",\"operation\":\"refund\",\"outcome\":\"failure\"",
    );
    line(
        "2024-06-01T10:00:01Z",
        "refund ok",
        ",\"request_id\":\"rq-corr\",\"operation\":\"refund\",\"outcome\":\"success\"",
    );

    // No operation on these two, so the retry rule stays silent and the
    // duplicate rule is what the assertions are actually reading.
    line(
        "2024-06-01T10:00:00Z",
        "queue full",
        ",\"request_id\":\"rq-dup\"",
    );
    line(
        "2024-06-01T10:00:01Z",
        "queue full",
        ",\"request_id\":\"rq-dup\"",
    );

    line(
        "2024-06-01T10:00:10Z",
        "before",
        ",\"request_id\":\"rq-skew\"",
    );
    line(
        "2024-06-01T10:00:05Z",
        "after",
        ",\"request_id\":\"rq-skew\"",
    );

    line(
        "2024-06-01T10:00:00Z",
        "start",
        ",\"request_id\":\"rq-gap\"",
    );
    line(
        "2024-06-01T10:10:00Z",
        "resume",
        ",\"request_id\":\"rq-gap\"",
    );

    line(
        "2024-06-01T11:00:00Z",
        "anchor",
        ",\"request_id\":\"rq-near\",\"operation\":\"index\"",
    );
    line(
        "2024-06-01T11:00:01Z",
        "one second later",
        ",\"request_id\":\"rq-near\",\"operation\":\"index\"",
    );
    line(
        "2024-06-01T11:00:03Z",
        "three seconds later",
        ",\"request_id\":\"rq-near\",\"operation\":\"index\"",
    );
    line(
        "2024-06-01T11:00:02Z",
        "different operation",
        ",\"request_id\":\"rq-near\",\"operation\":\"compact\"",
    );
    // Far outside any tolerance used below.
    line(
        "2024-06-01T12:00:00Z",
        "an hour later",
        ",\"request_id\":\"rq-near\",\"operation\":\"index\"",
    );
}

fn profile() -> ImportProfile {
    let mut p = builtin::jsonl_generic();
    p.profile_id = "test.jsonl.signals".into();
    p.generic_fields = BTreeMap::from([
        ("request_id".to_string(), vec![FieldRef::name("request_id")]),
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
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jsonl");
    write_corpus(&input);
    let engine = EngineConnection::open_in_memory().unwrap();
    let mut ws = Workspace::create(&dir.path().join("ws"), "signals", "0.4.0-test").unwrap();
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

fn run_corr(e: &Env, config_json: &str, job: &str) -> logscope_workspace::AnalysisRunRow {
    let def = analysis::create_definition(
        &e.ws,
        &analysis::NewDefinitionRequest {
            kind: "correlation".into(),
            name: "signals".into(),
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
            limits_json: "{}".into(),
        },
    )
    .unwrap()
    .definition_id;
    let (ctx, _c) = fg_ctx(job);
    correlation::run_correlation_analysis(&e.ws, &e.engine, &def, &ctx).unwrap()
}

/// Signals of one group, by the key value that produced it.
fn signals_for(
    e: &Env,
    run: &logscope_workspace::AnalysisRunRow,
    key: &str,
) -> Vec<correlation::CorrelationSignal> {
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let group = groups
        .iter()
        .find(|g| g.key_value == key)
        .unwrap_or_else(|| panic!("missing group {key:?}"));
    correlation::list_correlation_signals(&e.ws, &e.engine, &run.run_id, &group.group_id, 100)
        .unwrap()
}

/// The single signal of `kind`, owned so callers can read it after the
/// listing that produced it has gone out of scope.
fn only(signals: &[correlation::CorrelationSignal], kind: &str) -> correlation::CorrelationSignal {
    let matching: Vec<_> = signals.iter().filter(|s| s.kind == kind).collect();
    assert_eq!(
        matching.len(),
        1,
        "expected exactly one {kind} signal, got {:?}",
        signals.iter().map(|s| &s.kind).collect::<Vec<_>>()
    );
    matching[0].clone()
}

const RETRY_CONFIG: &str = r#"{"key":"request_id","attempt_attribute":"attempt"}"#;

#[test]
fn a_retry_is_documented_only_when_the_source_counted_the_attempts() {
    let e = env();
    let run = run_corr(&e, RETRY_CONFIG, "job-retry");
    assert_eq!(run.state, "completed");

    // The source carried an attempt counter that advanced.
    let documented = only(&signals_for(&e, &run, "rq-retry"), "retry");
    assert_eq!(documented.strength, "documented");
    assert!(
        !documented.investigative_lead,
        "the source stated this itself"
    );
    assert_eq!(documented.rule_id, "retry-signal");
    let matched: Vec<String> = serde_json::from_str(&documented.matched_json).unwrap();
    assert!(matched.contains(&"attempt".to_string()));
    assert!(matched.contains(&"operation".to_string()));
    assert!(!documented.reason.contains("Investigative lead only."));

    // Same operation after a failure, but nothing counted the attempts.
    let corroborated = only(&signals_for(&e, &run, "rq-corr"), "retry");
    assert_eq!(corroborated.strength, "corroborated");
    assert!(corroborated.investigative_lead);
    assert!(corroborated.reason.contains("Investigative lead only."));
    let missing: Vec<String> = serde_json::from_str(&corroborated.missing_json).unwrap();
    assert!(
        missing.contains(&"attempt".to_string()),
        "the reason names what was absent, not just what matched"
    );

    // Two identical messages with no shared operation are not a retry,
    // however alike they read.
    let dup_group = signals_for(&e, &run, "rq-dup");
    assert!(
        !dup_group.iter().any(|s| s.kind == "retry"),
        "identical text is not retry evidence"
    );
}

#[test]
fn an_operational_duplicate_needs_distinct_source_positions() {
    let e = env();
    let run = run_corr(&e, RETRY_CONFIG, "job-dup");
    let dup = only(&signals_for(&e, &run, "rq-dup"), "operational_duplicate");
    assert_eq!(dup.rule_id, "operational-duplicate");
    assert_eq!(dup.strength, "indicative");
    assert!(dup.investigative_lead);
    assert_ne!(
        dup.from_record_id, dup.to_record_id,
        "two distinct records, not one record seen twice"
    );
    assert!(dup
        .reason
        .contains("not establish that the underlying work happened more than once"));

    // The run counts ingestion duplicates separately from operational
    // ones, so the distinction is visible rather than merely respected.
    let counts: serde_json::Value = serde_json::from_str(&run.counts_json).unwrap();
    assert!(counts.get("ingest_duplicates_excluded").is_some());
    assert_eq!(counts["signals_by_kind"]["operational_duplicate"], 1);

    // Groups whose members carry different text produce none.
    for key in ["rq-retry", "rq-gap"] {
        assert!(
            !signals_for(&e, &run, key)
                .iter()
                .any(|s| s.kind == "operational_duplicate"),
            "{key} has no repeated message"
        );
    }
}

#[test]
fn skew_and_gap_report_originals_deltas_and_tolerances_without_rewriting_time() {
    let e = env();
    let run = run_corr(&e, RETRY_CONFIG, "job-time");

    // Written later in the source, timestamped five seconds earlier.
    let skew = only(&signals_for(&e, &run, "rq-skew"), "clock_skew");
    assert_eq!(skew.rule_id, "clock-skew");
    assert_eq!(skew.delta_nanos, -5_000_000_000);
    assert_eq!(skew.tolerance_nanos, Some(1_000_000));
    assert!(skew.from_event_time > skew.to_event_time);
    assert_eq!(
        skew.to_event_time - skew.from_event_time,
        skew.delta_nanos,
        "the delta is exactly the difference of the two stored times"
    );
    assert!(skew.reason.contains("no timestamp was adjusted"));
    assert!(
        skew.investigative_lead,
        "attributing this to a clock is a reading"
    );

    // Ten minutes of silence, against the five-minute default.
    let gap = only(&signals_for(&e, &run, "rq-gap"), "gap");
    assert_eq!(gap.rule_id, "gap");
    assert_eq!(gap.delta_nanos, 600_000_000_000);
    assert_eq!(gap.tolerance_nanos, Some(300_000_000_000));
    assert!(gap.reason.contains("not evidence that nothing happened"));
    assert!(gap.reason.contains("retention"));
    assert!(gap.investigative_lead);

    // One second apart is not a gap.
    assert!(!signals_for(&e, &run, "rq-dup")
        .iter()
        .any(|s| s.kind == "gap"));
    // Time moving forward is not skew.
    assert!(!signals_for(&e, &run, "rq-gap")
        .iter()
        .any(|s| s.kind == "clock_skew"));
}

#[test]
fn signals_can_be_selected_and_an_empty_selection_is_honoured_as_written() {
    let e = env();
    let only_gap = run_corr(
        &e,
        r#"{"key":"request_id","signals":["gap"]}"#,
        "job-only-gap",
    );
    let counts: serde_json::Value = serde_json::from_str(&only_gap.counts_json).unwrap();
    // Two quiet intervals exceed the five-minute default: rq-gap's ten
    // minutes, and the hour between rq-near's last two records.
    assert_eq!(counts["signals"], 2);
    assert_eq!(counts["signals_by_kind"]["gap"], 2);
    assert_eq!(
        counts["signals_by_kind"].as_object().unwrap().len(),
        1,
        "only the selected signal ran"
    );

    // An explicit empty list is a different request from omitting the
    // key, and is obeyed rather than treated as "unset".
    let none = run_corr(&e, r#"{"key":"request_id","signals":[]}"#, "job-no-signals");
    let counts: serde_json::Value = serde_json::from_str(&none.counts_json).unwrap();
    assert_eq!(counts["signals"], 0);

    // A run still writes the artifact, so "none were computed" and
    // "none were found" stay distinguishable through the manifest.
    let manifest: serde_json::Value =
        serde_json::from_str(none.manifest_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        manifest["signal_rules"]["enabled"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(manifest["signals"]["rows"], 0);
    assert_eq!(manifest["signal_rules"]["rule_set"], "sig-rules");

    // Unknown signal names and duplicates are refused, not ignored.
    for bad in [
        r#"{"key":"request_id","signals":["hunch"]}"#,
        r#"{"key":"request_id","signals":["gap","gap"]}"#,
        r#"{"key":"request_id","thresholds":{"gap_threshold_nanos":0}}"#,
        r#"{"key":"request_id","attempt_attribute":"  "}"#,
    ] {
        assert!(
            correlation::CorrelationConfig::parse(bad, "{}").is_err(),
            "{bad} must be refused"
        );
    }
}

#[test]
fn a_neighborhood_is_probable_bounded_and_states_all_of_its_own_terms() {
    let e = env();
    let run = run_corr(&e, RETRY_CONFIG, "job-near");
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let near = groups.iter().find(|g| g.key_value == "rq-near").unwrap();
    let rows = correlation::correlation_records(&e.ws, &e.engine, &run.run_id, &near.group_id, 50)
        .unwrap();
    let anchor = rows
        .iter()
        .find(|r| r.display_message == "anchor")
        .expect("anchor record");

    // Two seconds either side, and the neighbour must share `operation`.
    let hood = correlation::probable_neighborhood(
        &e.ws,
        &e.engine,
        &run.run_id,
        &anchor.record_id,
        &["operation".to_string()],
        2_000_000_000,
        10,
    )
    .unwrap();

    // Everything gate 21 asks a probable relationship to expose.
    assert_eq!(hood.confidence, "probable");
    assert_eq!(hood.rule_id, "corr-rules");
    assert_eq!(hood.compatible_fields, ["operation"]);
    assert_eq!(hood.tolerance_nanos, 2_000_000_000);
    assert!(hood.constraints.contains("equal operation"));
    assert!(hood.constraints.contains("2000000000 ns"));
    assert_eq!(hood.anchor_time_quality, "observed");
    assert!(hood.reason.contains("Investigative lead only."));
    assert!(hood.reason.contains("not thereby related"));

    // +1s shares the operation; +2s does not; +3s and +1h are outside
    // the window; the anchor is not its own neighbour.
    let ids: Vec<&str> = hood
        .neighbors
        .iter()
        .map(|n| n.record_id.as_str())
        .collect();
    assert_eq!(hood.neighbors.len(), 1, "got {ids:?}");
    assert_eq!(hood.neighbors[0].delta_nanos, 1_000_000_000);
    assert_eq!(hood.neighbors[0].matched_fields, ["operation"]);
    assert!(!ids.contains(&anchor.record_id.as_str()));

    // Dropping the field constraint admits the differing operation too,
    // and the nearest record still comes first.
    let wide = correlation::probable_neighborhood(
        &e.ws,
        &e.engine,
        &run.run_id,
        &anchor.record_id,
        &[],
        4_000_000_000,
        10,
    )
    .unwrap();
    assert_eq!(wide.neighbors.len(), 3);
    assert!(wide.constraints.contains("time proximity alone"));
    let deltas: Vec<i64> = wide.neighbors.iter().map(|n| n.delta_nanos).collect();
    assert_eq!(deltas, [1_000_000_000, 2_000_000_000, 3_000_000_000]);
    assert!(wide.neighbors.iter().all(|n| n.time_quality == "observed"));

    // The limit keeps the nearest and says how many it dropped.
    let capped = correlation::probable_neighborhood(
        &e.ws,
        &e.engine,
        &run.run_id,
        &anchor.record_id,
        &[],
        4_000_000_000,
        2,
    )
    .unwrap();
    assert_eq!(capped.neighbors.len(), 2);
    assert_eq!(capped.admitted, 3);
    assert_eq!(capped.truncated, 1);
    assert_eq!(
        capped
            .neighbors
            .iter()
            .map(|n| n.delta_nanos)
            .collect::<Vec<_>>(),
        [1_000_000_000, 2_000_000_000],
        "the record dropped is the least relevant one"
    );
    assert!(capped.reason.contains("1 further record(s)"));
}

#[test]
fn a_neighborhood_refuses_what_it_cannot_ground() {
    let e = env();
    let run = run_corr(&e, RETRY_CONFIG, "job-refuse");

    // A record outside the run's scope cannot anchor one.
    let err = correlation::probable_neighborhood(
        &e.ws,
        &e.engine,
        &run.run_id,
        "rec-does-not-exist",
        &[],
        1_000_000_000,
        10,
    )
    .unwrap_err();
    assert!(err.to_string().contains("not in this run's scope"), "{err}");

    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();
    let any =
        correlation::correlation_records(&e.ws, &e.engine, &run.run_id, &groups[0].group_id, 1)
            .unwrap();
    let anchor = &any[0].record_id;

    // A zero-width window and an unknown field are refused with reasons.
    let err = correlation::probable_neighborhood(&e.ws, &e.engine, &run.run_id, anchor, &[], 0, 10)
        .unwrap_err();
    assert!(err.to_string().contains("no width"), "{err}");
    let err = correlation::probable_neighborhood(
        &e.ws,
        &e.engine,
        &run.run_id,
        anchor,
        &["message".to_string()],
        1_000,
        10,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("unknown compatible field"),
        "similarity is not a neighborhood constraint: {err}"
    );
}

#[test]
fn no_signal_or_neighborhood_string_claims_causation() {
    let e = env();
    let run = run_corr(&e, RETRY_CONFIG, "job-wording");
    let groups =
        correlation::list_correlation_groups(&e.ws, &e.engine, &run.run_id, 0, 100).unwrap();

    let mut texts: Vec<String> = Vec::new();
    for g in &groups {
        texts.push(g.reason.clone());
        for s in
            correlation::list_correlation_signals(&e.ws, &e.engine, &run.run_id, &g.group_id, 100)
                .unwrap()
        {
            // Every lead says so, and only leads say so.
            assert_eq!(
                s.investigative_lead,
                s.reason.contains("Investigative lead only."),
                "the column and the prose must agree for {}",
                s.signal_id
            );
            texts.push(s.reason);
        }
    }
    let rows =
        correlation::correlation_records(&e.ws, &e.engine, &run.run_id, &groups[0].group_id, 1)
            .unwrap();
    texts.push(
        correlation::probable_neighborhood(
            &e.ws,
            &e.engine,
            &run.run_id,
            &rows[0].record_id,
            &[],
            1_000_000_000,
            5,
        )
        .unwrap()
        .reason,
    );

    assert!(texts.len() > 5, "the corpus must actually produce text");
    for text in texts {
        let lowered = text.to_lowercase();
        for forbidden in [
            "caused by",
            "because of",
            "root cause",
            "therefore",
            "proves",
            "confirms that",
            "resulted in",
        ] {
            assert!(!lowered.contains(forbidden), "{forbidden:?} in {text}");
        }
    }
}
