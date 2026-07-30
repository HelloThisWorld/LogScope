//! Investigation-domain proofs: durability, revision history, optimistic
//! concurrency, transactional rollback, and the v2 -> v3 workspace schema
//! migration.

use logscope_case::envelope::{
    self, EventRef, EventSnapshot, EvidenceReference, EvidenceSnapshot, SnapshotRow,
};
use logscope_workspace::case_meta::{
    InvestigationEdit, NewEvidence, NewHypothesis, NewInvestigation, NewItem, NewScopeRef,
};
use logscope_workspace::{MetaDb, WorkspaceError};
use rusqlite::Connection;

fn new_inv(id: &str, title: &str) -> NewInvestigation {
    NewInvestigation {
        investigation_id: id.to_string(),
        title: title.to_string(),
        description: None,
        severity: Some("sev2".into()),
        owner_text: Some("typed by the analyst".into()),
        tags_json: "[\"latency\",\"checkout\"]".into(),
        incident_started_at: Some(1_700_000_000_000_000_000),
        window_start: Some(1_700_000_000_000_000_000),
        window_end: Some(1_700_003_600_000_000_000),
    }
}

#[test]
fn investigation_lifecycle_history_and_reopen_durability() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.db");
    let db = MetaDb::open(&path).unwrap();

    let created = db
        .create_investigation(&new_inv("inv-a", "Checkout latency"))
        .unwrap();
    assert_eq!(created.status, "open");
    assert_eq!(created.revision, 1);
    assert_eq!(created.entity_version, 1);

    // Explicit status transition, then archive, then restore.
    let r2 = db
        .set_investigation_status("inv-a", 1, "investigating", "status_changed")
        .unwrap();
    assert_eq!(r2.status, "investigating");
    assert_eq!(r2.revision, 2);
    assert!(r2.status_changed_at.is_some());

    let r3 = db
        .set_investigation_status("inv-a", 2, "archived", "archived")
        .unwrap();
    assert_eq!(r3.status, "archived");
    // Archived investigations stay readable and are restorable.
    assert!(db.list_investigations(false).unwrap().is_empty());
    assert_eq!(db.list_investigations(true).unwrap().len(), 1);
    let r4 = db
        .set_investigation_status("inv-a", 3, "open", "restored")
        .unwrap();
    assert_eq!(r4.status, "open");

    // Edit all editable fields.
    let r5 = db
        .update_investigation(&InvestigationEdit {
            investigation_id: "inv-a".into(),
            expected_revision: 4,
            title: "Checkout latency spike".into(),
            description: Some("p99 regression during the evening peak".into()),
            severity: Some("sev1".into()),
            owner_text: None,
            tags_json: "[\"latency\"]".into(),
            incident_started_at: Some(1_700_000_100_000_000_000),
            mitigated_at: Some(1_700_002_000_000_000_000),
            resolved_at: None,
            window_start: Some(1_700_000_000_000_000_000),
            window_end: Some(1_700_007_200_000_000_000),
        })
        .unwrap();
    assert_eq!(r5.revision, 5);
    assert_eq!(r5.title, "Checkout latency spike");
    assert_eq!(r5.mitigated_at, Some(1_700_002_000_000_000_000));

    // Full non-destructive history: every prior revision is retrievable.
    let history = db.list_entity_history("investigation", "inv-a").unwrap();
    let actions: Vec<&str> = history.iter().map(|h| h.action.as_str()).collect();
    assert_eq!(
        actions,
        vec![
            "created",
            "status_changed",
            "archived",
            "restored",
            "edited"
        ]
    );
    let first: serde_json::Value = serde_json::from_str(&history[0].payload_json).unwrap();
    assert_eq!(first["title"], "Checkout latency");
    assert_eq!(first["revision"], 1);

    // Close and reopen: everything survives.
    drop(db);
    let db = MetaDb::open(&path).unwrap();
    let reloaded = db.get_investigation("inv-a").unwrap().unwrap();
    assert_eq!(reloaded.revision, 5);
    assert_eq!(reloaded.title, "Checkout latency spike");
    assert_eq!(
        db.list_entity_history("investigation", "inv-a")
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn stale_revision_conflicts_roll_back_without_history() {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(&dir.path().join("workspace.db")).unwrap();
    db.create_investigation(&new_inv("inv-b", "Original title"))
        .unwrap();

    let err = db
        .update_investigation(&InvestigationEdit {
            investigation_id: "inv-b".into(),
            expected_revision: 99, // stale
            title: "Should not land".into(),
            description: None,
            severity: None,
            owner_text: None,
            tags_json: "[]".into(),
            incident_started_at: None,
            mitigated_at: None,
            resolved_at: None,
            window_start: None,
            window_end: None,
        })
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::StaleRevision { .. }));
    assert_eq!(err.code(), "workspace/stale-revision");

    // Nothing changed, and no history row was recorded for the failure.
    let row = db.get_investigation("inv-b").unwrap().unwrap();
    assert_eq!(row.title, "Original title");
    assert_eq!(row.revision, 1);
    assert_eq!(
        db.list_entity_history("investigation", "inv-b")
            .unwrap()
            .len(),
        1
    );

    // Missing entities are a distinct structured error.
    let missing = db
        .set_investigation_status("inv-nope", 1, "resolved", "status_changed")
        .unwrap_err();
    assert!(matches!(missing, WorkspaceError::MissingEntity { .. }));
    assert_eq!(missing.code(), "workspace/missing-entity");
}

#[test]
fn foreign_key_failure_rolls_back_the_whole_transaction() {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(&dir.path().join("workspace.db")).unwrap();

    // Hypothesis pointing at a nonexistent investigation must fail and
    // leave no partial rows (hypothesis or history) behind.
    let err = db.create_hypothesis(&NewHypothesis {
        hypothesis_id: "hyp-x".into(),
        investigation_id: "inv-ghost".into(),
        statement: "orphan".into(),
        rationale: None,
    });
    assert!(err.is_err());
    assert!(db
        .list_entity_history("hypothesis", "hyp-x")
        .unwrap()
        .is_empty());
}

#[test]
fn hypothesis_states_links_and_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.db");
    let db = MetaDb::open(&path).unwrap();
    db.create_investigation(&new_inv("inv-c", "API errors"))
        .unwrap();

    let h = db
        .create_hypothesis(&NewHypothesis {
            hypothesis_id: "hyp-1".into(),
            investigation_id: "inv-c".into(),
            statement: "Connection pool exhaustion caused the timeouts".into(),
            rationale: Some("pool metrics unavailable; inferred from log volume".into()),
        })
        .unwrap();
    assert_eq!(h.state, "unverified");

    // Every state is reachable only through an explicit manual call.
    let h = db.set_hypothesis_state("hyp-1", 1, "supported").unwrap();
    assert_eq!(h.state, "supported");
    let h = db.set_hypothesis_state("hyp-1", 2, "rejected").unwrap();
    assert_eq!(h.state, "rejected");
    let h = db.set_hypothesis_state("hyp-1", 3, "confirmed").unwrap();
    assert_eq!(h.state, "confirmed");

    // Transition history is auditable with from/to detail.
    let hist = db.list_entity_history("hypothesis", "hyp-1").unwrap();
    let transitions: Vec<(String, String)> = hist
        .iter()
        .filter(|r| r.action == "state_changed")
        .map(|r| {
            let d: serde_json::Value = serde_json::from_str(&r.detail_json).unwrap();
            (
                d["from"].as_str().unwrap().into(),
                d["to"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        transitions,
        vec![
            ("unverified".into(), "supported".into()),
            ("supported".into(), "rejected".into()),
            ("rejected".into(), "confirmed".into()),
        ]
    );

    // Evidence links (an evidence fixture row is inserted through a second
    // connection; the typed pin flow is exercised in the evidence suites).
    let fixture = Connection::open(&path).unwrap();
    fixture
        .execute_batch(
            "INSERT INTO evidence (evidence_id, investigation_id, envelope_version, kind, signal,
                title, captured_investigation_revision, position, reference_json, snapshot_json,
                created_at, updated_at)
             VALUES ('ev-1', 'inv-c', 1, 'event', 'log', 't', 1, 0, '{}', '{}',
                     '2026-07-30T00:00:00Z', '2026-07-30T00:00:00Z')",
        )
        .unwrap();
    drop(fixture);
    let h = db.link_hypothesis_evidence("hyp-1", 4, "ev-1").unwrap();
    assert_eq!(db.linked_evidence_ids("hyp-1").unwrap(), vec!["ev-1"]);
    let h2 = db
        .unlink_hypothesis_evidence("hyp-1", h.revision, "ev-1")
        .unwrap();
    assert!(db.linked_evidence_ids("hyp-1").unwrap().is_empty());
    assert!(h2.revision > h.revision);
}

#[test]
fn items_cover_all_kinds_statuses_and_archive() {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(&dir.path().join("workspace.db")).unwrap();
    db.create_investigation(&new_inv("inv-d", "Disk pressure"))
        .unwrap();

    let note = db
        .create_item(&NewItem {
            item_id: "item-n".into(),
            investigation_id: "inv-d".into(),
            kind: "note".into(),
            content: "Compaction backlog observed at 18:00".into(),
            task_status: None,
            question_status: None,
        })
        .unwrap();
    let task = db
        .create_item(&NewItem {
            item_id: "item-t".into(),
            investigation_id: "inv-d".into(),
            kind: "task".into(),
            content: "Pull retention settings for the affected volume".into(),
            task_status: Some("todo".into()),
            question_status: None,
        })
        .unwrap();
    let finding = db
        .create_item(&NewItem {
            item_id: "item-f".into(),
            investigation_id: "inv-d".into(),
            kind: "finding".into(),
            content: "Writes stalled while the cleanup job held the lock".into(),
            task_status: None,
            question_status: None,
        })
        .unwrap();
    let question = db
        .create_item(&NewItem {
            item_id: "item-q".into(),
            investigation_id: "inv-d".into(),
            kind: "question".into(),
            content: "Why did the cleanup job start twice?".into(),
            task_status: None,
            question_status: Some("open".into()),
        })
        .unwrap();

    // v0.3 items are always user-authored, with no machine provenance.
    for it in [&note, &task, &finding, &question] {
        assert!(it.authored_by_user);
        assert!(it.finding_provenance_json.is_none());
    }

    let task = db
        .set_item_status("item-t", task.revision, Some("doing"), None)
        .unwrap();
    let task = db
        .set_item_status("item-t", task.revision, Some("done"), None)
        .unwrap();
    assert_eq!(task.task_status.as_deref(), Some("done"));
    let question = db
        .set_item_status("item-q", question.revision, None, Some("answered"))
        .unwrap();
    assert_eq!(question.question_status.as_deref(), Some("answered"));

    let edited = db
        .update_item_content("item-n", note.revision, "Compaction backlog began 17:40")
        .unwrap();
    assert_eq!(edited.revision, note.revision + 1);

    // Archive is the removal path; content stays retrievable.
    let archived = db
        .set_item_archived("item-f", finding.revision, true)
        .unwrap();
    assert!(archived.archived);
    assert_eq!(db.list_items("inv-d", false).unwrap().len(), 3);
    assert_eq!(db.list_items("inv-d", true).unwrap().len(), 4);
    let restored = db
        .set_item_archived("item-f", archived.revision, false)
        .unwrap();
    assert!(!restored.archived);

    // Prior content is retrievable from history after the edit.
    let hist = db.list_entity_history("item", "item-n").unwrap();
    let v1: serde_json::Value = serde_json::from_str(&hist[0].payload_json).unwrap();
    assert_eq!(v1["content"], "Compaction backlog observed at 18:00");
}

#[test]
fn reorder_preserves_ids_and_is_guarded() {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(&dir.path().join("workspace.db")).unwrap();
    let inv = db
        .create_investigation(&new_inv("inv-e", "Reorder"))
        .unwrap();
    for (id, s) in [("hyp-a", "A"), ("hyp-b", "B"), ("hyp-c", "C")] {
        db.create_hypothesis(&NewHypothesis {
            hypothesis_id: id.into(),
            investigation_id: "inv-e".into(),
            statement: s.into(),
            rationale: None,
        })
        .unwrap();
    }

    let inv = db
        .reorder_children(
            "inv-e",
            inv.revision,
            "hypothesis",
            &["hyp-c".into(), "hyp-a".into(), "hyp-b".into()],
        )
        .unwrap();
    let order: Vec<String> = db
        .list_hypotheses("inv-e")
        .unwrap()
        .into_iter()
        .map(|h| h.hypothesis_id)
        .collect();
    assert_eq!(order, vec!["hyp-c", "hyp-a", "hyp-b"]);

    // Reordering never drops or invents rows.
    let err = db
        .reorder_children("inv-e", inv.revision, "hypothesis", &["hyp-c".into()])
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::Invalid(_)));

    // A stale guard is a structured conflict.
    let err = db
        .reorder_children(
            "inv-e",
            inv.revision + 7,
            "hypothesis",
            &["hyp-a".into(), "hyp-b".into(), "hyp-c".into()],
        )
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::StaleRevision { .. }));
}

#[test]
fn scope_refs_attach_list_remove_with_history() {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(&dir.path().join("workspace.db")).unwrap();
    db.create_investigation(&new_inv("inv-f", "Scope")).unwrap();

    db.add_scope_ref(&NewScopeRef {
        scope_ref_id: "iscope-1".into(),
        investigation_id: "inv-f".into(),
        kind: "dataset".into(),
        dataset_id: Some("ds-1".into()),
        dataset_revision: Some("dsrev-abc".into()),
        selector_json: None,
        saved_search_id: None,
        query_json: None,
        window_start: None,
        window_end: None,
        label: None,
    })
    .unwrap();
    db.add_scope_ref(&NewScopeRef {
        scope_ref_id: "iscope-2".into(),
        investigation_id: "inv-f".into(),
        kind: "time_window".into(),
        dataset_id: None,
        dataset_revision: None,
        selector_json: None,
        saved_search_id: None,
        query_json: None,
        window_start: Some(1),
        window_end: Some(2),
        label: None,
    })
    .unwrap();

    let refs = db.list_scope_refs("inv-f").unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].dataset_revision.as_deref(), Some("dsrev-abc"));

    db.remove_scope_ref("iscope-2").unwrap();
    assert_eq!(db.list_scope_refs("inv-f").unwrap().len(), 1);
    // Removal keeps the final payload in history.
    let hist = db.list_entity_history("scope_ref", "iscope-2").unwrap();
    assert_eq!(hist.last().unwrap().action, "removed");
    let last: serde_json::Value = serde_json::from_str(&hist.last().unwrap().payload_json).unwrap();
    assert_eq!(last["window_end"], 2);
}

#[test]
fn no_os_identity_is_captured_anywhere() {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(&dir.path().join("workspace.db")).unwrap();
    let mut new = new_inv("inv-g", "Identity hygiene");
    new.owner_text = Some("whoever the user typed".into());
    db.create_investigation(&new).unwrap();
    db.set_investigation_status("inv-g", 1, "resolved", "status_changed")
        .unwrap();

    let os_user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    assert!(
        !os_user.is_empty(),
        "test needs an OS username to prove absence"
    );

    let row = db.get_investigation("inv-g").unwrap().unwrap();
    assert_eq!(row.owner_text.as_deref(), Some("whoever the user typed"));
    for h in db.list_investigation_activity("inv-g", 100).unwrap() {
        assert!(
            !h.payload_json.contains(&os_user) && !h.detail_json.contains(&os_user),
            "history must not auto-capture the OS identity"
        );
    }
}

fn typed_event_evidence(evidence_id: &str, investigation_id: &str) -> NewEvidence {
    let reference = EvidenceReference::Event(EventRef {
        record_id: "log-0123456789abcdef0123456789abcdef".into(),
        dataset_id: "ds-1".into(),
        dataset_revision: "dsrev-abc".into(),
        segment_id: Some("seg-1".into()),
        source_file_id: Some("file-1".into()),
        source_content_hash: Some("blake3-hash".into()),
        source_locator_json: Some("{\"record_number\":42}".into()),
        profile_id: Some("builtin.jsonl".into()),
        profile_version: Some("1".into()),
        parser_id: "jsonl".into(),
        parser_version: "1".into(),
        event_time: Some(1_700_000_000_000_000_000),
        timestamp_quality: vec![],
    });
    envelope::validate_reference(&reference).unwrap();
    let snapshot = EvidenceSnapshot::Event(EventSnapshot {
        row: SnapshotRow {
            record_id: "log-0123456789abcdef0123456789abcdef".into(),
            event_time: Some(1_700_000_000_000_000_000),
            severity_text: Some("ERROR".into()),
            severity_number: Some(17),
            display_message: "connection reset by peer".into(),
            display_message_truncated: false,
            fields: vec![],
        },
        raw_excerpt: None,
        raw_excerpt_truncated: false,
    });
    NewEvidence {
        evidence_id: evidence_id.into(),
        investigation_id: investigation_id.into(),
        envelope_version: logscope_case::EVIDENCE_ENVELOPE_VERSION,
        kind: "event".into(),
        signal: "log".into(),
        title: "First reset error".into(),
        annotation: Some("first occurrence after the deploy".into()),
        relevance: Some("marks the start of the incident window".into()),
        captured_investigation_revision: 1,
        group_id: None,
        supersedes_evidence_id: None,
        reference_json: envelope::encode_reference(&reference).unwrap(),
        snapshot_json: envelope::encode_snapshot(&snapshot).unwrap(),
    }
}

#[test]
fn evidence_pin_edit_group_archive_and_supersede() {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(&dir.path().join("workspace.db")).unwrap();
    db.create_investigation(&new_inv("inv-ev", "Evidence storage"))
        .unwrap();

    // Pin: stable id, envelope v1, typed reference + bounded snapshot.
    let ev = db
        .insert_evidence(&typed_event_evidence("ev-a", "inv-ev"))
        .unwrap();
    assert_eq!(ev.revision, 1);
    assert_eq!(ev.resolver_state, "unverified");
    assert_eq!(ev.envelope_version, 1);

    // The stored live reference decodes back to the typed form.
    match envelope::decode_reference(ev.envelope_version, &ev.reference_json) {
        envelope::DecodeOutcome::Decoded(EvidenceReference::Event(e)) => {
            assert_eq!(e.dataset_revision, "dsrev-abc");
            assert_eq!(e.parser_id, "jsonl");
        }
        other => panic!("expected decoded event reference, got {other:?}"),
    }

    // Edit annotation: revision bumps, snapshot bytes stay identical.
    let edited = db
        .update_evidence_annotation("ev-a", 1, "First reset error", Some("updated note"), None)
        .unwrap();
    assert_eq!(edited.revision, 2);
    assert_eq!(edited.snapshot_json, ev.snapshot_json);
    assert_eq!(edited.reference_json, ev.reference_json);

    // Grouping.
    let g = db
        .create_evidence_group("evg-1", "inv-ev", "Deploy window")
        .unwrap();
    let grouped = db.set_evidence_group("ev-a", 2, Some(&g.group_id)).unwrap();
    assert_eq!(grouped.group_id.as_deref(), Some("evg-1"));
    // Deleting the group clears the pointer without touching evidence
    // content or history.
    db.delete_evidence_group("evg-1").unwrap();
    let after = db.get_evidence("ev-a").unwrap().unwrap();
    assert_eq!(after.group_id, None);
    assert_eq!(after.snapshot_json, ev.snapshot_json);

    // Supersession: the old item and its history stay intact and visible.
    let mut newer = typed_event_evidence("ev-b", "inv-ev");
    newer.supersedes_evidence_id = Some("ev-a".into());
    newer.title = "Corrected pin (later occurrence)".into();
    let newer = db.supersede_evidence(&newer, "ev-a").unwrap();
    assert_eq!(newer.supersedes_evidence_id.as_deref(), Some("ev-a"));
    let old = db.get_evidence("ev-a").unwrap().unwrap();
    assert!(
        !old.archived,
        "supersession never deletes or hides the old item"
    );
    let old_hist = db.list_entity_history("evidence", "ev-a").unwrap();
    assert_eq!(old_hist.last().unwrap().action, "superseded");

    // Archive = normal removal, restorable, history-tombstoned.
    let archived = db
        .set_evidence_archived("ev-a", old.revision, true)
        .unwrap();
    assert!(archived.archived);
    assert_eq!(db.list_evidence("inv-ev", false).unwrap().len(), 1);
    assert_eq!(db.list_evidence("inv-ev", true).unwrap().len(), 2);

    // Stale guard works on evidence too.
    let err = db
        .update_evidence_annotation("ev-b", 99, "x", None, None)
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::StaleRevision { .. }));
}

#[test]
fn verification_updates_only_resolver_columns_and_never_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(&dir.path().join("workspace.db")).unwrap();
    db.create_investigation(&new_inv("inv-vr", "Resolver hygiene"))
        .unwrap();
    let ev = db
        .insert_evidence(&typed_event_evidence("ev-v", "inv-vr"))
        .unwrap();
    let history_before = db.list_entity_history("evidence", "ev-v").unwrap().len();

    db.update_evidence_resolution(
        "ev-v",
        "source_changed",
        "{\"expected\":\"blake3-hash\",\"found\":\"other\"}",
        "2026-07-30T12:00:00Z",
    )
    .unwrap();

    let after = db.get_evidence("ev-v").unwrap().unwrap();
    assert_eq!(after.resolver_state, "source_changed");
    assert_eq!(
        after.last_verified_at.as_deref(),
        Some("2026-07-30T12:00:00Z")
    );
    // Content untouched: same revision, byte-identical snapshot/reference,
    // and no per-item history entry was added by verification.
    assert_eq!(after.revision, ev.revision);
    assert_eq!(after.snapshot_json, ev.snapshot_json);
    assert_eq!(after.reference_json, ev.reference_json);
    assert_eq!(
        db.list_entity_history("evidence", "ev-v").unwrap().len(),
        history_before
    );

    // The batch run records one investigation-level activity event.
    db.record_verification_run(
        "inv-vr",
        "{\"checked\":1,\"verified\":0,\"source_changed\":1}",
    )
    .unwrap();
    let activity = db.list_investigation_activity("inv-vr", 10).unwrap();
    assert_eq!(activity[0].action, "verified");
}

#[test]
fn v2_workspace_migrates_to_v3_without_touching_existing_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.db");

    // Build a genuine schema-v2 database exactly as a 0.2.0 build left it:
    // migrations 0001 + 0002 applied and stamped, with live rows.
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(include_str!("../src/migrations/0001_init.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../src/migrations/0002_explorer.sql"))
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL
             ) STRICT;
             INSERT INTO schema_migrations VALUES (1, '0001_init', '2026-07-29T00:00:00Z');
             INSERT INTO schema_migrations VALUES (2, '0002_explorer', '2026-07-29T00:00:00Z');
             INSERT INTO datasets (dataset_id, name, signal, status, created_at)
               VALUES ('ds-v2', 'legacy dataset', 'logs', 'published', '2026-07-29T00:00:00Z');
             INSERT INTO saved_searches (saved_search_id, name, query_text, language_version,
               fingerprint, dataset_selection_json, time_strategy_json, created_at, updated_at)
               VALUES ('ss-v2', 'errors', 'severity:ERROR', 1, 'qry-x', '{\"kind\":\"all\"}',
                       '{\"kind\":\"all\"}', '2026-07-29T00:00:00Z', '2026-07-29T00:00:00Z');",
        )
        .unwrap();
    }

    // Opening with the v0.3 build migrates transactionally to v3.
    let db = MetaDb::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), 3);

    // Existing v2 data is untouched and still readable.
    let ds = db.get_dataset("ds-v2").unwrap().unwrap();
    assert_eq!(ds.name, "legacy dataset");
    let saved = db.list_saved_searches().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].query_text, "severity:ERROR");

    // The investigation domain is immediately usable — no re-import needed.
    db.create_investigation(&new_inv("inv-m", "Post-migration case"))
        .unwrap();
    assert_eq!(db.list_investigations(false).unwrap().len(), 1);
}
