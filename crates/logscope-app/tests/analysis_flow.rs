//! WP1 service-layer proofs over a real imported workspace: definition
//! validation refusals, scope freezing (dataset revisions + concrete
//! bounds + semantic fingerprint), rerun identity (same inputs ⇒ same
//! semantic fingerprint, new execution id), the run lifecycle including
//! cancellation, and staleness detection against moved datasets and
//! revised definitions.

use std::io::Write as _;
use std::path::Path;

use logscope_app::{analysis, run_import, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::{JobContext, JobControl, JobError};
use logscope_query::{EngineConnection, TimeStrategy};
use logscope_workspace::Workspace;

fn write_jsonl(path: &Path, records: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    for i in 0..records {
        let level = if i % 4 == 0 { "ERROR" } else { "INFO" };
        writeln!(
            f,
            "{{\"@timestamp\":\"2024-06-01T10:{:02}:{:02}Z\",\"level\":\"{level}\",\
             \"message\":\"handler {} finished\",\"service\":\"orders\",\"idx\":{i}}}",
            (i / 60) % 60,
            i % 60,
            i,
        )
        .unwrap();
    }
}

fn fg_ctx(job_id: &str) -> (JobContext, JobControl) {
    let (ctx, control, rx) = JobContext::detached(job_id);
    std::mem::forget(rx);
    (ctx, control)
}

struct Env {
    _dir: tempfile::TempDir,
    ws: Workspace,
    dataset_id: String,
}

fn env(records: usize) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jsonl");
    write_jsonl(&input, records);
    let engine = EngineConnection::open_in_memory().unwrap();
    let mut ws = Workspace::create(&dir.path().join("ws"), "analysis", "0.4.0-test").unwrap();
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
        dataset_id: outcome.dataset_id,
    }
}

fn valid_request(e: &Env) -> analysis::NewDefinitionRequest {
    analysis::NewDefinitionRequest {
        kind: "message_pattern".into(),
        name: "error templates".into(),
        description: None,
        dataset_ids: vec![e.dataset_id.clone()],
        query_text: "severity:ERROR".into(),
        time_strategy: TimeStrategy::All,
        field_selection_json: "{}".into(),
        algorithm_id: "template.mask".into(),
        algorithm_version: 1,
        config_json: "{\"min_count\":\"2\"}".into(),
        masking_profile_json: "{}".into(),
        thresholds_json: "{}".into(),
        limits_json: "{}".into(),
    }
}

#[test]
fn definition_validation_refuses_bad_kind_config_and_query() {
    let e = env(40);

    let mut bad_kind = valid_request(&e);
    bad_kind.kind = "vibes".into();
    let err = analysis::create_definition(&e.ws, &bad_kind).unwrap_err();
    assert_eq!(err.code, "analysis/invalid-definition");

    // Floats cannot enter an identity: thresholds travel as strings.
    let mut float_config = valid_request(&e);
    float_config.config_json = "{\"threshold\":0.5}".into();
    let err = analysis::create_definition(&e.ws, &float_config).unwrap_err();
    assert_eq!(err.code, "analysis/invalid-definition");
    assert!(err.message.contains("threshold"));

    let mut array_config = valid_request(&e);
    array_config.config_json = "[]".into();
    let err = analysis::create_definition(&e.ws, &array_config).unwrap_err();
    assert_eq!(err.code, "analysis/invalid-definition");

    let mut bad_query = valid_request(&e);
    bad_query.query_text = "(((".into();
    let err = analysis::create_definition(&e.ws, &bad_query).unwrap_err();
    assert_eq!(err.code, "query/invalid");
}

#[test]
fn scope_freezing_is_deterministic_and_reruns_share_the_semantic_fingerprint() {
    let e = env(60);
    let def = analysis::create_definition(&e.ws, &valid_request(&e)).unwrap();
    assert!(
        def.query_fingerprint.is_some(),
        "validated query is fingerprinted"
    );
    assert!(def.config_fingerprint.starts_with("acfg-"));

    let run1 = analysis::begin_run(&e.ws, &def.definition_id).unwrap();
    let run2 = analysis::begin_run(&e.ws, &def.definition_id).unwrap();
    assert_eq!(run1.state, "pending");
    assert_ne!(run1.run_id, run2.run_id, "each execution is its own record");
    assert_eq!(
        run1.semantic_fingerprint, run2.semantic_fingerprint,
        "identical inputs share one semantic identity"
    );
    assert!(run1.semantic_fingerprint.starts_with("asem-"));
    let revs: serde_json::Value = serde_json::from_str(&run1.dataset_revs_json).unwrap();
    assert_eq!(revs[0]["dataset_id"], e.dataset_id.as_str());
    assert!(revs[0]["dataset_revision"]
        .as_str()
        .unwrap()
        .starts_with("dsrev-"));

    // Lifecycle: running → completed with a manifest; check stays None
    // while nothing moved.
    analysis::mark_running(&e.ws, &run1.run_id).unwrap();
    let done = analysis::complete_run(&e.ws, &run1.run_id, "{\"accepted\":60}", "{\"patterns\":0}")
        .unwrap();
    assert_eq!(done.state, "completed");
    assert_eq!(analysis::check_run_current(&e.ws, &done).unwrap(), None);

    // Cancellation is terminal and never an empty success.
    let aborted = analysis::abort_run(
        &e.ws,
        &run2.run_id,
        true,
        &JobError::new("job/cancelled", "the job was cancelled"),
    )
    .unwrap();
    assert_eq!(aborted.state, "cancelled");
    assert!(aborted.manifest_json.is_none());
}

#[test]
fn staleness_reports_revised_definitions_and_unresolvable_datasets() {
    let e = env(30);
    let def = analysis::create_definition(&e.ws, &valid_request(&e)).unwrap();
    let run = analysis::begin_run(&e.ws, &def.definition_id).unwrap();
    analysis::mark_running(&e.ws, &run.run_id).unwrap();
    let done = analysis::complete_run(&e.ws, &run.run_id, "{}", "{}").unwrap();

    // Definition revision moves on → the completed run reports why.
    e.ws.meta
        .update_analysis_definition(&logscope_workspace::AnalysisDefinitionEdit {
            definition_id: def.definition_id.clone(),
            expected_revision: def.revision,
            name: "revised".into(),
            description: None,
            dataset_selection_json: def.dataset_selection_json.clone(),
            query_text: def.query_text.clone(),
            query_language_version: def.query_language_version,
            query_fingerprint: def.query_fingerprint.clone(),
            time_strategy_json: def.time_strategy_json.clone(),
            field_selection_json: def.field_selection_json.clone(),
            config_json: "{\"min_count\":\"3\"}".into(),
            config_fingerprint: "acfg-new".into(),
            masking_profile_json: def.masking_profile_json.clone(),
            thresholds_json: def.thresholds_json.clone(),
            limits_json: def.limits_json.clone(),
        })
        .unwrap();
    let reason = analysis::check_run_current(&e.ws, &done).unwrap();
    assert!(reason.as_deref().unwrap().contains("definition revised"));
    let stale = analysis::mark_stale(&e.ws, &done.run_id, reason.as_deref().unwrap()).unwrap();
    assert_eq!(stale.state, "stale");

    // A run whose captured dataset can no longer be resolved says so.
    let crafted =
        e.ws.meta
            .start_analysis_run(&logscope_workspace::NewAnalysisRun {
                run_id: "arun-ghost".into(),
                definition_id: def.definition_id.clone(),
                definition_revision: 2,
                semantic_fingerprint: "asem-ghost".into(),
                dataset_revs_json:
                    "[{\"dataset_id\":\"ds-gone\",\"dataset_revision\":\"dsrev-x\"}]".into(),
                query_fingerprint: None,
                query_language_version: 1,
                bounds_json: "{\"start\":null,\"end\":null}".into(),
                algorithm_id: "template.mask".into(),
                algorithm_version: 1,
                config_fingerprint: "acfg-new".into(),
            })
            .unwrap();
    let reason = analysis::check_run_current(&e.ws, &crafted).unwrap();
    assert!(reason.as_deref().unwrap().contains("no longer resolvable"));
}

#[test]
fn future_definition_schemas_are_refused_actionably() {
    let e = env(20);
    let mut new = logscope_workspace::NewAnalysisDefinition {
        definition_id: "adef-future".into(),
        definition_schema_version: 99,
        kind: "message_pattern".into(),
        name: "from the future".into(),
        description: None,
        dataset_selection_json: "[]".into(),
        query_text: String::new(),
        query_language_version: 1,
        query_fingerprint: None,
        time_strategy_json: "{\"kind\":\"all\"}".into(),
        field_selection_json: "{}".into(),
        algorithm_id: "template.mask".into(),
        algorithm_version: 1,
        config_json: "{}".into(),
        config_fingerprint: "acfg-f".into(),
        masking_profile_json: "{}".into(),
        thresholds_json: "{}".into(),
        limits_json: "{}".into(),
    };
    e.ws.meta.create_analysis_definition(&new).unwrap();
    let err = analysis::begin_run(&e.ws, "adef-future").unwrap_err();
    assert_eq!(err.code, "analysis/unsupported-version");
    assert!(err.message.contains("update LogScope"));

    // And a definition that never existed is a missing entity.
    new.definition_id.clear();
    let err = analysis::begin_run(&e.ws, "adef-none").unwrap_err();
    assert_eq!(err.code, "workspace/missing-entity");
}
