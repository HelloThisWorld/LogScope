//! WP1 analysis-domain proofs: the v3 → v4 migration (existing data —
//! including manual findings — untouched), definition CRUD with history
//! and optimistic revisions, the two-phase immutable run lifecycle,
//! deterministic-finding storage rules, derived-artifact cataloging,
//! retention protection, and crash recovery finishing interrupted runs
//! as honest failed tombstones (idempotently).

use logscope_workspace::{
    AnalysisDefinitionEdit, DerivedArtifactRow, MetaDb, NewAnalysisDefinition, NewAnalysisFinding,
    NewAnalysisRun, NewInvestigation, NewItem, Workspace,
};
use rusqlite::Connection;

fn new_def(id: &str) -> NewAnalysisDefinition {
    NewAnalysisDefinition {
        definition_id: id.to_string(),
        definition_schema_version: 1,
        kind: "message_pattern".into(),
        name: "checkout error templates".into(),
        description: None,
        dataset_selection_json: "[\"ds-1\"]".into(),
        query_text: "severity:ERROR".into(),
        query_language_version: 1,
        query_fingerprint: Some("qry-abc".into()),
        time_strategy_json: "{\"kind\":\"all\"}".into(),
        field_selection_json: "{}".into(),
        algorithm_id: "template.mask".into(),
        algorithm_version: 1,
        config_json: "{}".into(),
        config_fingerprint: "acfg-1".into(),
        masking_profile_json: "{}".into(),
        thresholds_json: "{}".into(),
        limits_json: "{}".into(),
    }
}

fn new_run(id: &str, def: &str) -> NewAnalysisRun {
    NewAnalysisRun {
        run_id: id.to_string(),
        definition_id: def.to_string(),
        definition_revision: 1,
        semantic_fingerprint: "asem-x".into(),
        dataset_revs_json: "[{\"dataset_id\":\"ds-1\",\"dataset_revision\":\"dsrev-a\"}]".into(),
        query_fingerprint: Some("qry-abc".into()),
        query_language_version: 1,
        bounds_json: "{\"start\":null,\"end\":null}".into(),
        algorithm_id: "template.mask".into(),
        algorithm_version: 1,
        config_fingerprint: "acfg-1".into(),
    }
}

fn new_finding(id: &str, run: &str) -> NewAnalysisFinding {
    NewAnalysisFinding {
        finding_id: id.to_string(),
        finding_schema_version: 1,
        rule_id: "rule.new-high-severity-pattern".into(),
        rule_version: 1,
        run_id: run.to_string(),
        subject_json: "{\"pattern_id\":\"pat-1\"}".into(),
        title: "new high-severity pattern above minimum count".into(),
        explanation: "pattern pat-1 first appears in this analysis scope".into(),
        calculation_json: "{\"count\":\"12\",\"min\":\"5\"}".into(),
        severity: "sev2".into(),
        severity_rule_json: "{\"rule\":\"severity-ceiling+count\"}".into(),
        confidence: None,
        contributing_json: "{\"record_ids\":[\"log-1\"]}".into(),
        examples_json: "[]".into(),
        state_json: "{}".into(),
    }
}

fn open_db(dir: &tempfile::TempDir) -> MetaDb {
    MetaDb::open(&dir.path().join("workspace.db")).unwrap()
}

#[test]
fn v3_workspace_migrates_to_v4_without_touching_existing_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.db");

    // Build a genuine schema-v3 database exactly as a 0.3.0 build left
    // it: migrations 0001–0003 applied and stamped, with a live manual
    // finding (authored_by_user = 1, no provenance).
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(include_str!("../src/migrations/0001_init.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../src/migrations/0002_explorer.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../src/migrations/0003_investigations.sql"))
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL
             ) STRICT;
             INSERT INTO schema_migrations VALUES (1, '0001_init', '2026-08-05T00:00:00Z');
             INSERT INTO schema_migrations VALUES (2, '0002_explorer', '2026-08-05T00:00:00Z');
             INSERT INTO schema_migrations VALUES (3, '0003_investigations', '2026-08-05T00:00:00Z');
             INSERT INTO investigations (investigation_id, entity_version, title, status,
               tags_json, created_at, updated_at)
               VALUES ('inv-v3', 1, 'legacy case', 'open', '[]',
                       '2026-08-05T00:00:00Z', '2026-08-05T00:00:00Z');
             INSERT INTO investigation_items (item_id, investigation_id, kind, content,
               authored_by_user, created_at, updated_at)
               VALUES ('item-v3', 'inv-v3', 'finding', 'user-authored conclusion', 1,
                       '2026-08-05T00:00:00Z', '2026-08-05T00:00:00Z');",
        )
        .unwrap();
    }

    let db = MetaDb::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), 4);

    // Existing case data is untouched; the manual finding stays manual.
    let inv = db.get_investigation("inv-v3").unwrap().unwrap();
    assert_eq!(inv.title, "legacy case");
    let items = db.list_items("inv-v3", true).unwrap();
    assert_eq!(items.len(), 1);
    assert!(items[0].authored_by_user, "manual findings remain manual");
    assert!(items[0].finding_provenance_json.is_none());

    // The analysis domain is immediately usable — no re-import needed.
    db.create_analysis_definition(&new_def("adef-m")).unwrap();
    assert_eq!(db.list_analysis_definitions().unwrap().len(), 1);
}

#[test]
fn definition_edits_are_revisioned_guarded_and_historied() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_db(&dir);
    let row = db.create_analysis_definition(&new_def("adef-1")).unwrap();
    assert_eq!(row.revision, 1);

    let edit = AnalysisDefinitionEdit {
        definition_id: "adef-1".into(),
        expected_revision: 1,
        name: "renamed".into(),
        description: Some("tightened scope".into()),
        dataset_selection_json: row.dataset_selection_json.clone(),
        query_text: row.query_text.clone(),
        query_language_version: row.query_language_version,
        query_fingerprint: row.query_fingerprint.clone(),
        time_strategy_json: row.time_strategy_json.clone(),
        field_selection_json: row.field_selection_json.clone(),
        config_json: "{\"min_count\":\"5\"}".into(),
        config_fingerprint: "acfg-2".into(),
        masking_profile_json: row.masking_profile_json.clone(),
        thresholds_json: row.thresholds_json.clone(),
        limits_json: row.limits_json.clone(),
    };
    let updated = db.update_analysis_definition(&edit).unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.config_fingerprint, "acfg-2");

    // Stale revision is a structured conflict, not last-write-wins.
    let mut stale = edit.clone();
    stale.expected_revision = 1;
    let err = db.update_analysis_definition(&stale).err().unwrap();
    assert_eq!(err.code(), "workspace/stale-revision");
    let mut missing = edit.clone();
    missing.definition_id = "adef-none".into();
    let err = db.update_analysis_definition(&missing).err().unwrap();
    assert_eq!(err.code(), "workspace/missing-entity");

    // Both mutations are in the shared history ledger.
    let history = db
        .list_entity_history("analysis_definition", "adef-1")
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].action, "created");
    assert_eq!(history[1].action, "updated");
}

#[test]
fn runs_are_two_phase_and_immutable_once_finished() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_db(&dir);
    db.create_analysis_definition(&new_def("adef-1")).unwrap();

    let run = db.start_analysis_run(&new_run("arun-1", "adef-1")).unwrap();
    assert_eq!(run.state, "pending");
    assert!(run.finished_at.is_none());

    // Progress before running is refused; the order is enforced.
    assert!(db
        .update_analysis_run_progress("arun-1", "scan", "{}")
        .is_err());
    db.mark_analysis_run_running("arun-1").unwrap();
    assert!(
        db.mark_analysis_run_running("arun-1").is_err(),
        "running is entered once"
    );
    db.update_analysis_run_progress("arun-1", "scan", "{\"accepted\":10}")
        .unwrap();

    // Completion requires a manifest; an empty success cannot exist.
    let err = db
        .finish_analysis_run("arun-1", "completed", "{}", None, None)
        .err()
        .unwrap();
    assert_eq!(err.code(), "workspace/invalid-argument");
    let done = db
        .finish_analysis_run(
            "arun-1",
            "completed",
            "{\"accepted\":10}",
            Some("{\"patterns\":0}"),
            None,
        )
        .unwrap();
    assert_eq!(done.state, "completed");
    assert!(done.finished_at.is_some());
    assert!(done.progress_stage.is_none());

    // Finished runs are immutable — a second finish is refused.
    assert!(db
        .finish_analysis_run("arun-1", "failed", "{}", None, Some("{}"))
        .is_err());

    // Invalid terminal states are refused outright.
    db.start_analysis_run(&new_run("arun-2", "adef-1")).unwrap();
    assert!(db
        .finish_analysis_run("arun-2", "stale", "{}", None, None)
        .is_err());

    // Cancellation records the structured error and is terminal.
    let cancelled = db
        .finish_analysis_run(
            "arun-2",
            "cancelled",
            "{}",
            None,
            Some("{\"code\":\"job/cancelled\"}"),
        )
        .unwrap();
    assert_eq!(cancelled.state, "cancelled");
    assert!(cancelled
        .error_json
        .as_deref()
        .unwrap()
        .contains("job/cancelled"));

    // Stale applies only to completed runs, and records why.
    assert!(db.mark_analysis_run_stale("arun-2", "x").is_err());
    let stale = db
        .mark_analysis_run_stale("arun-1", "dataset ds-1 moved")
        .unwrap();
    assert_eq!(stale.state, "stale");
    assert_eq!(
        stale.invalidation_reason.as_deref(),
        Some("dataset ds-1 moved")
    );
}

#[test]
fn findings_require_completed_runs_and_protect_them_from_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let db = open_db(&dir);
    db.create_analysis_definition(&new_def("adef-1")).unwrap();
    db.start_analysis_run(&new_run("arun-1", "adef-1")).unwrap();

    // Findings evaluate completed runs only.
    let err = db
        .record_analysis_finding(&new_finding("afind-1", "arun-1"))
        .err()
        .unwrap();
    assert_eq!(err.code(), "workspace/invalid-argument");
    let err = db
        .record_analysis_finding(&new_finding("afind-1", "arun-none"))
        .err()
        .unwrap();
    assert_eq!(err.code(), "workspace/missing-entity");

    db.mark_analysis_run_running("arun-1").unwrap();
    db.finish_analysis_run("arun-1", "completed", "{}", Some("{}"), None)
        .unwrap();
    let finding = db
        .record_analysis_finding(&new_finding("afind-1", "arun-1"))
        .unwrap();
    assert_eq!(finding.origin, "deterministic");

    // A referenced run is never silently deleted.
    let err = db.delete_unreferenced_analysis_run("arun-1").err().unwrap();
    assert_eq!(err.code(), "workspace/invalid-argument");
    assert!(db.get_analysis_run("arun-1").unwrap().is_some());

    // Derived artifacts catalog and cascade with an unreferenced run.
    db.start_analysis_run(&new_run("arun-2", "adef-1")).unwrap();
    db.record_derived_artifact(&DerivedArtifactRow {
        artifact_id: "dart-1".into(),
        run_id: "arun-2".into(),
        kind: "pattern_membership".into(),
        rel_path: "derived/analysis/arun-2/membership.parquet".into(),
        row_count: 10,
        byte_size: 1024,
        sha256: "00".repeat(32),
        schema_version: 1,
        created_at: "2026-08-05T00:00:00Z".into(),
    })
    .unwrap();
    assert_eq!(db.list_derived_artifacts("arun-2").unwrap().len(), 1);
    db.finish_analysis_run("arun-2", "failed", "{}", None, Some("{}"))
        .unwrap();
    db.delete_unreferenced_analysis_run("arun-2").unwrap();
    assert!(db.get_analysis_run("arun-2").unwrap().is_none());
    assert_eq!(db.list_derived_artifacts("arun-2").unwrap().len(), 0);
}

#[test]
fn recovery_finishes_interrupted_runs_as_failed_tombstones_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    {
        let ws = Workspace::create(&root, "t", "0.4.0-test").unwrap();
        ws.meta
            .create_investigation(&NewInvestigation {
                investigation_id: "inv-1".into(),
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
        ws.meta
            .create_item(&NewItem {
                item_id: "item-1".into(),
                investigation_id: "inv-1".into(),
                kind: "finding".into(),
                content: "manual".into(),
                task_status: None,
                question_status: None,
            })
            .unwrap();
        ws.meta
            .create_analysis_definition(&new_def("adef-1"))
            .unwrap();
        ws.meta
            .start_analysis_run(&new_run("arun-p", "adef-1"))
            .unwrap();
        ws.meta
            .start_analysis_run(&new_run("arun-r", "adef-1"))
            .unwrap();
        ws.meta.mark_analysis_run_running("arun-r").unwrap();
        // Simulated crash: the workspace is dropped with unfinished runs.
    }

    let ws = Workspace::open(&root, "0.4.0-test").unwrap();
    let recovered: Vec<&String> = ws
        .recovery
        .interrupted_case_records
        .iter()
        .filter(|r| r.starts_with("analysis_run:"))
        .collect();
    assert_eq!(recovered.len(), 2, "both unfinished runs are recovered");
    for run_id in ["arun-p", "arun-r"] {
        let run = ws.meta.get_analysis_run(run_id).unwrap().unwrap();
        assert_eq!(run.state, "failed", "tombstone completed, not deleted");
        assert!(run
            .error_json
            .as_deref()
            .unwrap()
            .contains("job/interrupted"));
        assert!(run.finished_at.is_some());
    }
    // Manual findings survive recovery untouched.
    assert!(ws.meta.list_items("inv-1", true).unwrap()[0].authored_by_user);
    drop(ws);

    // Idempotent: a second open recovers nothing further.
    let ws = Workspace::open(&root, "0.4.0-test").unwrap();
    assert!(
        ws.recovery.is_clean(),
        "second open finds nothing to recover"
    );
}
