//! W2 evidence proofs: dataset revision fingerprints, the six pin
//! services capturing typed references + bounded snapshots through the
//! authoritative pipeline, and the batched cancellable resolver reaching
//! every documented integrity state without rewriting snapshots.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use logscope_app::case::{self, PinCommon, QueryScope};
use logscope_app::{explorer, run_import, ImportRequest};
use logscope_case::envelope::{
    self, CountState, DatasetRevRef, EventRef, EvidenceReference, EvidenceSnapshot, ItemSnapshot,
    QueryContext, QueryRef,
};
use logscope_case::{new_id, EVIDENCE_ENVELOPE_VERSION};
use logscope_ingest::builtin;
use logscope_jobs::{JobContext, JobControl};
use logscope_query::{query_page, EngineConnection, PageRequest, QueryCancelHandle, TimeStrategy};
use logscope_query_lang::LANGUAGE_VERSION;
use logscope_workspace::{NewEvidence, NewInvestigation, NewItem, Workspace};

fn write_es_jsonl(path: &Path, records: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    for i in 0..records {
        let level = match i % 5 {
            0 => "ERROR",
            1 => "WARN",
            _ => "INFO",
        };
        let outcome = if i % 5 == 0 { "failure" } else { "success" };
        writeln!(
            f,
            "{{\"@timestamp\":\"2024-06-01T10:{:02}:{:02}Z\",\"log.level\":\"{level}\",\
             \"message\":\"handler {} finished{}\",\"service.name\":\"orders\",\
             \"event.outcome\":\"{outcome}\",\"retry\":{{\"count\":{}}},\"idx\":{i}}}",
            (i / 60) % 60,
            i % 60,
            i,
            if i % 7 == 0 { " with timeout" } else { "" },
            i % 3,
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
    engine: EngineConnection,
    dataset_id: String,
    input: PathBuf,
    investigation_id: String,
}

fn env(records: usize) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    let input = dir.path().join("input.jsonl");
    write_es_jsonl(&input, records);
    let engine = EngineConnection::open_in_memory().unwrap();
    let mut ws = Workspace::create(&root, "case", "0.3.0-test").unwrap();
    let request = ImportRequest::new(
        vec![input.clone()],
        builtin::elasticsearch_export(),
        "es export",
    );
    let (ctx, _control) = fg_ctx(&format!("job-{}", uuid::Uuid::new_v4()));
    let outcome = run_import(&mut ws, &engine, &request, &ctx).expect("import succeeds");
    let investigation = ws
        .meta
        .create_investigation(&NewInvestigation {
            investigation_id: new_id("inv"),
            title: "checkout errors".into(),
            description: None,
            severity: Some("sev2".into()),
            owner_text: None,
            tags_json: "[]".into(),
            incident_started_at: None,
            window_start: None,
            window_end: None,
        })
        .unwrap();
    Env {
        _dir: dir,
        ws,
        engine,
        dataset_id: outcome.dataset_id,
        input,
        investigation_id: investigation.investigation_id,
    }
}

fn common(e: &Env, title: &str) -> PinCommon {
    PinCommon {
        investigation_id: e.investigation_id.clone(),
        title: title.into(),
        annotation: Some("seen during triage".into()),
        relevance: Some("matches the reported symptom".into()),
        group_id: None,
    }
}

fn scope_all(e: &Env, query: &str) -> QueryScope {
    QueryScope {
        query_text: query.into(),
        dataset_ids: vec![e.dataset_id.clone()],
        time_strategy: TimeStrategy::All,
    }
}

fn first_record_ids(e: &Env, n: u32) -> Vec<(String, String)> {
    let selection = explorer::resolve_dataset_selection(&e.ws, &[]).unwrap();
    let files = explorer::segment_files_for(&e.ws, &selection).unwrap();
    let analysis = explorer::analyze_query(&e.ws, &selection, "");
    let filter = explorer::compile_for_execution(&e.ws, &selection, &analysis).unwrap();
    let cancel = QueryCancelHandle::new(e.engine.interrupt_handle());
    let window = logscope_query::resolve_window(&TimeStrategy::All, None);
    let page = query_page(
        &e.engine,
        &files,
        &filter,
        &window,
        &PageRequest {
            cursor: None,
            backward: false,
            limit: n,
        },
        &cancel,
        None,
    )
    .unwrap();
    page.rows
        .into_iter()
        .map(|r| (r.dataset_id, r.record_id))
        .collect()
}

fn verify_all(e: &Env) -> case::VerificationReport {
    let (ctx, _control) = fg_ctx("job-verify");
    case::verify_evidence(&e.ws, &e.engine, &e.investigation_id, None, &ctx).unwrap()
}

fn state_of(e: &Env, evidence_id: &str) -> (String, serde_json::Value) {
    let row = e.ws.meta.get_evidence(evidence_id).unwrap().unwrap();
    (
        row.resolver_state,
        serde_json::from_str(&row.resolver_detail_json).unwrap(),
    )
}

/// Inserts a crafted evidence row directly (simulating rows written by
/// other builds or corrupted storage); the service path is exercised by
/// the pin tests.
fn insert_crafted(
    e: &Env,
    envelope_version: i64,
    kind: &str,
    reference_json: &str,
    snapshot_json: &str,
) -> String {
    let row =
        e.ws.meta
            .insert_evidence(&NewEvidence {
                evidence_id: new_id("ev"),
                investigation_id: e.investigation_id.clone(),
                envelope_version,
                kind: kind.into(),
                signal: "log".into(),
                title: "crafted".into(),
                annotation: None,
                relevance: None,
                captured_investigation_revision: 1,
                group_id: None,
                supersedes_evidence_id: None,
                reference_json: reference_json.into(),
                snapshot_json: snapshot_json.into(),
            })
            .unwrap();
    row.evidence_id
}

fn item_snapshot_json() -> String {
    envelope::encode_snapshot(&EvidenceSnapshot::ItemRef(ItemSnapshot {
        item_kind: "note".into(),
        content: "crafted snapshot".into(),
        content_truncated: false,
    }))
    .unwrap()
}

#[test]
fn dataset_revision_is_deterministic() {
    let e = env(50);
    let a = case::dataset_revision(&e.ws, &e.dataset_id).unwrap();
    let b = case::dataset_revision(&e.ws, &e.dataset_id).unwrap();
    assert!(a.starts_with("dsrev-"), "{a}");
    assert_eq!(a, b, "same segment set must fingerprint identically");
}

#[test]
fn pin_event_captures_and_verifies_without_snapshot_rewrite() {
    let e = env(120);
    let (dataset_id, record_id) = first_record_ids(&e, 1).remove(0);
    let row = case::pin_event(
        &e.ws,
        &e.engine,
        &case::PinEventRequest {
            common: common(&e, "first symptom"),
            dataset_id: dataset_id.clone(),
            record_id: record_id.clone(),
            display_fields: vec!["service.name".into(), "retry.count".into()],
            include_raw_excerpt: true,
        },
    )
    .unwrap();
    assert_eq!(row.kind, "event");
    assert_eq!(row.signal, "log");
    assert_eq!(row.resolver_state, "unverified");

    // The live reference carries provenance identity, not display text.
    let reference = match envelope::decode_reference(row.envelope_version, &row.reference_json) {
        envelope::DecodeOutcome::Decoded(EvidenceReference::Event(ev)) => ev,
        other => panic!("expected event reference, got {other:?}"),
    };
    assert_eq!(reference.record_id, record_id);
    assert!(reference.dataset_revision.starts_with("dsrev-"));
    assert!(reference.source_file_id.is_some());
    assert!(reference.source_content_hash.is_some());
    assert!(!reference.parser_id.is_empty());
    // The bounded snapshot keeps the visible row readable on its own.
    let snapshot = match envelope::decode_snapshot(row.envelope_version, &row.snapshot_json) {
        envelope::DecodeOutcome::Decoded(EvidenceSnapshot::Event(s)) => s,
        other => panic!("expected event snapshot, got {other:?}"),
    };
    assert!(snapshot.row.display_message.contains("handler"));
    assert!(snapshot
        .row
        .fields
        .iter()
        .any(|f| f.name == "service.name" && f.value == "orders"));
    assert!(snapshot.raw_excerpt.is_some(), "raw excerpt captured");

    let report = verify_all(&e);
    assert_eq!(report.total, 1);
    assert_eq!(report.states.get("verified"), Some(&1));
    assert_eq!(report.dataset_lookups, 1);

    // Verification touched only the resolver columns.
    let after = e.ws.meta.get_evidence(&row.evidence_id).unwrap().unwrap();
    assert_eq!(after.resolver_state, "verified");
    assert!(after.last_verified_at.is_some());
    assert_eq!(after.snapshot_json, row.snapshot_json, "snapshot untouched");
    assert_eq!(after.reference_json, row.reference_json);
    assert_eq!(after.revision, row.revision, "no revision bump");

    // One investigation-level history event records the run.
    let history =
        e.ws.meta
            .list_entity_history("investigation", &e.investigation_id)
            .unwrap();
    assert!(history.iter().any(|h| h.action == "verified"));
}

#[test]
fn pin_selection_verifies_and_partial_resolution_is_reported() {
    let e = env(120);
    let ids: Vec<String> = first_record_ids(&e, 3)
        .into_iter()
        .map(|(_, r)| r)
        .collect();
    let pinned = case::pin_selection(
        &e.ws,
        &e.engine,
        &case::PinSelectionRequest {
            common: common(&e, "three related rows"),
            record_ids: ids.clone(),
            scope: scope_all(&e, "severity:ERROR"),
        },
    )
    .unwrap();

    // A crafted selection with one id that is not canonical anywhere.
    let dsrev = case::dataset_revision(&e.ws, &e.dataset_id).unwrap();
    let mut with_ghost = ids.clone();
    with_ghost.push("log-00000000000000000000000000000000".into());
    let reference = EvidenceReference::Selection(envelope::SelectionRef {
        record_ids: with_ghost,
        datasets: vec![DatasetRevRef {
            dataset_id: e.dataset_id.clone(),
            dataset_revision: dsrev,
        }],
        context: QueryContext {
            query_text: "".into(),
            language_version: LANGUAGE_VERSION as i64,
            fingerprint: None,
            dataset_ids: vec![e.dataset_id.clone()],
            time_strategy_json: "{\"kind\":\"all\"}".into(),
            resolved_start: None,
            resolved_end: None,
            omitted_untimestamped: None,
        },
        selected_count: 4,
        max_allowed: 500,
        truncated: false,
    });
    let crafted = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION,
        "selection",
        &envelope::encode_reference(&reference).unwrap(),
        &item_snapshot_json(),
    );

    let report = verify_all(&e);
    assert_eq!(report.total, 2);
    let (state, _) = state_of(&e, &pinned.evidence_id);
    assert_eq!(state, "verified");
    let (state, detail) = state_of(&e, &crafted);
    assert_eq!(state, "partially_resolved");
    assert_eq!(detail["resolved_count"], 3);
    assert_eq!(detail["missing_count"], 1);
}

#[test]
fn pin_query_pins_concrete_bounds_and_saved_search_identity() {
    let e = env(180);
    let resolved = e.ws.meta.list_saved_searches().unwrap().len();
    assert_eq!(resolved, 0);
    e.ws.meta
        .upsert_saved_search(
            "ss-1",
            "timeouts",
            "\"timeout\"",
            LANGUAGE_VERSION as i64,
            "qry-original",
            "{\"kind\":\"all\"}",
            "{\"kind\":\"all\"}",
            None,
        )
        .unwrap();

    let row = case::pin_query(
        &e.ws,
        &e.engine,
        &case::PinQueryRequest {
            common: common(&e, "timeout query"),
            scope: QueryScope {
                query_text: "\"timeout\"".into(),
                dataset_ids: vec![e.dataset_id.clone()],
                time_strategy: TimeStrategy::RelativeToLatest {
                    duration_nanos: 3_600_000_000_000,
                },
            },
            saved_search_id: Some("ss-1".into()),
        },
    )
    .unwrap();
    let reference = match envelope::decode_reference(row.envelope_version, &row.reference_json) {
        envelope::DecodeOutcome::Decoded(EvidenceReference::Query(q)) => q,
        other => panic!("expected query reference, got {other:?}"),
    };
    // A relative strategy is pinned to concrete instants.
    assert!(reference.context.resolved_start.is_some());
    assert!(reference.context.resolved_end.is_some());
    assert!(matches!(reference.count, CountState::Exact { count } if count > 0));
    assert!(!reference.representative_ids.is_empty());
    assert_eq!(reference.saved_search_id.as_deref(), Some("ss-1"));

    let report = verify_all(&e);
    assert_eq!(report.states.get("verified"), Some(&1));

    // The saved definition changes: the pinned query still verifies on its
    // own, and the drifted saved-search identity is reported, never
    // silently substituted.
    e.ws.meta
        .upsert_saved_search(
            "ss-1",
            "timeouts",
            "\"timeout\" AND severity:ERROR",
            LANGUAGE_VERSION as i64,
            "qry-edited",
            "{\"kind\":\"all\"}",
            "{\"kind\":\"all\"}",
            None,
        )
        .unwrap();
    verify_all(&e);
    let (state, detail) = state_of(&e, &row.evidence_id);
    assert_eq!(state, "verified");
    assert_eq!(detail["secondary"]["saved_search"], "changed");

    e.ws.meta.delete_saved_search("ss-1").unwrap();
    verify_all(&e);
    let (_, detail) = state_of(&e, &row.evidence_id);
    assert_eq!(detail["secondary"]["saved_search"], "missing");
}

#[test]
fn query_drift_and_unsupported_versions_are_distinct_states() {
    let e = env(60);
    let dsrev = case::dataset_revision(&e.ws, &e.dataset_id).unwrap();
    let ctx = |text: &str, lang: i64| QueryContext {
        query_text: text.into(),
        language_version: lang,
        fingerprint: None,
        dataset_ids: vec![e.dataset_id.clone()],
        time_strategy_json: "{\"kind\":\"all\"}".into(),
        resolved_start: None,
        resolved_end: None,
        omitted_untimestamped: None,
    };
    let query_ref = |context: QueryContext, count: i64| {
        envelope::encode_reference(&EvidenceReference::Query(QueryRef {
            context,
            datasets: vec![DatasetRevRef {
                dataset_id: e.dataset_id.clone(),
                dataset_revision: dsrev.clone(),
            }],
            saved_search_id: None,
            saved_search_fingerprint: None,
            sort: "event_time DESC NULLS LAST, record_id DESC, dataset_id DESC".into(),
            count: CountState::Exact { count },
            representative_ids: vec![],
        }))
        .unwrap()
    };

    // Captured count no longer matches → drift with expected/actual.
    let drifted = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION,
        "query",
        &query_ref(ctx("severity:ERROR", LANGUAGE_VERSION as i64), 999_999),
        &item_snapshot_json(),
    );
    // Query no longer validates against the catalog → drift, reported.
    let invalid = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION,
        "query",
        &query_ref(ctx("no_such_field_xyz:1", LANGUAGE_VERSION as i64), 1),
        &item_snapshot_json(),
    );
    // Newer language version → unsupported, never reinterpreted.
    let newer_lang = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION,
        "query",
        &query_ref(ctx("severity:ERROR", LANGUAGE_VERSION as i64 + 1), 1),
        &item_snapshot_json(),
    );
    // Newer envelope version → unsupported without decoding.
    let newer_envelope = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION + 1,
        "query",
        "{\"kind\":\"telepathy\"}",
        &item_snapshot_json(),
    );
    // Corrupt reference, readable snapshot → unsupported (cause recorded).
    let corrupt_ref = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION,
        "query",
        "not json at all",
        &item_snapshot_json(),
    );
    // Corrupt reference AND snapshot → broken.
    let broken = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION,
        "query",
        "not json at all",
        "also not json",
    );
    // Dataset gone → dataset_revision_unavailable.
    let gone = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION,
        "query",
        &{
            let mut c = ctx("severity:ERROR", LANGUAGE_VERSION as i64);
            c.dataset_ids = vec!["ds-gone".into()];
            envelope::encode_reference(&EvidenceReference::Query(QueryRef {
                context: c,
                datasets: vec![DatasetRevRef {
                    dataset_id: "ds-gone".into(),
                    dataset_revision: "dsrev-void".into(),
                }],
                saved_search_id: None,
                saved_search_fingerprint: None,
                sort: "event_time DESC NULLS LAST, record_id DESC, dataset_id DESC".into(),
                count: CountState::Unknown,
                representative_ids: vec![],
            }))
            .unwrap()
        },
        &item_snapshot_json(),
    );

    verify_all(&e);
    let (state, detail) = state_of(&e, &drifted);
    assert_eq!(state, "query_drift");
    assert_eq!(detail["cause"], "count");
    assert_eq!(detail["expected"], 999_999);
    let (state, detail) = state_of(&e, &invalid);
    assert_eq!(state, "query_drift");
    assert_eq!(detail["validates"], false);
    let (state, _) = state_of(&e, &newer_lang);
    assert_eq!(state, "unsupported_reference_version");
    let (state, detail) = state_of(&e, &newer_envelope);
    assert_eq!(state, "unsupported_reference_version");
    assert_eq!(detail["cause"], "envelope_version");
    let (state, detail) = state_of(&e, &corrupt_ref);
    assert_eq!(state, "unsupported_reference_version");
    assert_eq!(detail["cause"], "undecodable");
    assert_eq!(detail["snapshot_readable"], true);
    let (state, _) = state_of(&e, &broken);
    assert_eq!(state, "broken");
    let (state, _) = state_of(&e, &gone);
    assert_eq!(state, "dataset_revision_unavailable");
}

#[test]
fn pin_group_uses_the_authoritative_language_for_predicates() {
    let e = env(300);
    let row = case::pin_group(
        &e.ws,
        &e.engine,
        &case::PinGroupRequest {
            common: common(&e, "no-retry rows"),
            scope: scope_all(&e, "service.name:orders"),
            field: "retry.count".into(),
            value_json: "0".into(),
        },
    )
    .unwrap();
    let reference = match envelope::decode_reference(row.envelope_version, &row.reference_json) {
        envelope::DecodeOutcome::Decoded(EvidenceReference::ExplorerGroup(g)) => g,
        other => panic!("expected group reference, got {other:?}"),
    };
    assert_eq!(reference.predicate_text, "retry.count:0");
    // i % 3 == 0 over 300 records.
    assert!(matches!(reference.count, CountState::Exact { count: 100 }));
    let snapshot = match envelope::decode_snapshot(row.envelope_version, &row.snapshot_json) {
        envelope::DecodeOutcome::Decoded(EvidenceSnapshot::ExplorerGroup(s)) => s,
        other => panic!("expected group snapshot, got {other:?}"),
    };
    assert_eq!(snapshot.share_bp, Some(3_333), "100 of 300");

    // The missing-value group uses the documented missing test.
    let missing = case::pin_group(
        &e.ws,
        &e.engine,
        &case::PinGroupRequest {
            common: common(&e, "rows without retry count"),
            scope: scope_all(&e, ""),
            field: "retry.count".into(),
            value_json: "null".into(),
        },
    )
    .unwrap();
    let reference =
        match envelope::decode_reference(missing.envelope_version, &missing.reference_json) {
            envelope::DecodeOutcome::Decoded(EvidenceReference::ExplorerGroup(g)) => g,
            other => panic!("expected group reference, got {other:?}"),
        };
    assert_eq!(reference.predicate_text, "NOT retry.count:*");
    assert!(matches!(reference.count, CountState::Exact { count: 0 }));

    let report = verify_all(&e);
    assert_eq!(report.states.get("verified"), Some(&2));
}

#[test]
fn pin_interval_captures_half_open_bounds_and_verifies() {
    let e = env(300);
    let start = chrono::DateTime::parse_from_rfc3339("2024-06-01T10:00:00Z")
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap();
    let end = chrono::DateTime::parse_from_rfc3339("2024-06-01T10:01:00Z")
        .unwrap()
        .timestamp_nanos_opt()
        .unwrap();
    let row = case::pin_interval(
        &e.ws,
        &e.engine,
        &case::PinIntervalRequest {
            common: common(&e, "first minute"),
            scope: scope_all(&e, ""),
            start,
            end,
            bucket_width_nanos: 60_000_000_000,
            display_timezone: "UTC".into(),
            neighbor_buckets: vec![(start - 60_000_000_000, 0), (end, 60)],
        },
    )
    .unwrap();
    let reference = match envelope::decode_reference(row.envelope_version, &row.reference_json) {
        envelope::DecodeOutcome::Decoded(EvidenceReference::HistogramInterval(i)) => i,
        other => panic!("expected interval reference, got {other:?}"),
    };
    // Seconds 0..=59 of minute zero: exactly 60 records, end exclusive.
    assert!(matches!(reference.count, CountState::Exact { count: 60 }));
    assert_eq!(reference.start, start);
    assert_eq!(reference.end, end);

    let report = verify_all(&e);
    assert_eq!(report.states.get("verified"), Some(&1));
}

#[test]
fn pin_item_tracks_revision_drift_honestly() {
    let e = env(30);
    let item =
        e.ws.meta
            .create_item(&NewItem {
                item_id: new_id("item"),
                investigation_id: e.investigation_id.clone(),
                kind: "note".into(),
                content: "retry storm started after the config push".into(),
                task_status: None,
                question_status: None,
            })
            .unwrap();
    let row = case::pin_item(
        &e.ws,
        &case::PinItemRequest {
            common: common(&e, "triage note"),
            item_id: item.item_id.clone(),
        },
    )
    .unwrap();
    assert_eq!(row.signal, "manual");
    assert_eq!(row.kind, "item_ref");

    let report = verify_all(&e);
    assert_eq!(report.states.get("verified"), Some(&1));

    // Editing the item advances its revision: the evidence now points at
    // an older revision and says so instead of silently following.
    e.ws.meta
        .update_item_content(&item.item_id, item.revision, "edited content")
        .unwrap();
    verify_all(&e);
    let (state, detail) = state_of(&e, &row.evidence_id);
    assert_eq!(state, "source_changed");
    assert_eq!(detail["cause"], "item_revision");
    assert_eq!(detail["captured_revision"], 1);
    assert_eq!(detail["current_revision"], 2);
}

#[test]
fn source_mutations_walk_through_missing_and_changed_states() {
    let e = env(90);
    let (dataset_id, record_id) = first_record_ids(&e, 1).remove(0);
    let row = case::pin_event(
        &e.ws,
        &e.engine,
        &case::PinEventRequest {
            common: common(&e, "the record"),
            dataset_id,
            record_id,
            display_fields: vec![],
            include_raw_excerpt: false,
        },
    )
    .unwrap();
    let original = std::fs::read(&e.input).unwrap();

    // Size change → source_changed via the fast path (no hashing).
    let mut appended = original.clone();
    appended.extend_from_slice(b"tail\n");
    std::fs::write(&e.input, &appended).unwrap();
    verify_all(&e);
    let (state, detail) = state_of(&e, &row.evidence_id);
    assert_eq!(state, "source_changed");
    assert_eq!(detail["cause"], "size");

    // Restore → verified again; canonical data never went anywhere.
    std::fs::write(&e.input, &original).unwrap();
    verify_all(&e);
    let (state, _) = state_of(&e, &row.evidence_id);
    assert_eq!(state, "verified");

    // Same-size content change → BLAKE3 catches it behind the fast path.
    let mut flipped = original.clone();
    let pos = flipped
        .iter()
        .position(|b| *b == b'h')
        .expect("fixture contains 'h'");
    flipped[pos] = b'H';
    assert_eq!(flipped.len(), original.len());
    std::fs::write(&e.input, &flipped).unwrap();
    verify_all(&e);
    let (state, detail) = state_of(&e, &row.evidence_id);
    assert_eq!(state, "source_changed");
    assert_eq!(detail["cause"], "content_hash");

    // Deletion → source_missing while the canonical record still resolves,
    // and the captured snapshot stays byte-identical throughout.
    std::fs::remove_file(&e.input).unwrap();
    verify_all(&e);
    let (state, _) = state_of(&e, &row.evidence_id);
    assert_eq!(state, "source_missing");
    let after = e.ws.meta.get_evidence(&row.evidence_id).unwrap().unwrap();
    assert_eq!(after.snapshot_json, row.snapshot_json);
}

#[test]
fn canonical_without_checkable_source_is_its_own_state() {
    let e = env(40);
    let (dataset_id, record_id) = first_record_ids(&e, 1).remove(0);
    let dsrev = case::dataset_revision(&e.ws, &e.dataset_id).unwrap();
    let reference = envelope::encode_reference(&EvidenceReference::Event(EventRef {
        record_id,
        dataset_id,
        dataset_revision: dsrev,
        segment_id: None,
        source_file_id: None,
        source_content_hash: None,
        source_locator_json: None,
        profile_id: None,
        profile_version: None,
        parser_id: "p".into(),
        parser_version: "1".into(),
        event_time: None,
        timestamp_quality: vec![],
    }))
    .unwrap();
    let crafted = insert_crafted(
        &e,
        EVIDENCE_ENVELOPE_VERSION,
        "event",
        &reference,
        &item_snapshot_json(),
    );
    verify_all(&e);
    let (state, detail) = state_of(&e, &crafted);
    assert_eq!(state, "canonical_available_source_unavailable");
    assert_eq!(detail["cause"], "no_source_reference");
}

#[test]
fn batch_verification_is_batched_and_cancellable() {
    let e = env(200);
    for (dataset_id, record_id) in first_record_ids(&e, 10) {
        case::pin_event(
            &e.ws,
            &e.engine,
            &case::PinEventRequest {
                common: common(&e, &format!("pin {record_id}")),
                dataset_id,
                record_id,
                display_fields: vec![],
                include_raw_excerpt: false,
            },
        )
        .unwrap();
    }

    // Ten event pins in one dataset resolve through ONE id-set lookup.
    let report = verify_all(&e);
    assert_eq!(report.total, 10);
    assert_eq!(report.updated, 10);
    assert_eq!(report.dataset_lookups, 1, "no per-evidence queries");
    assert_eq!(report.states.get("verified"), Some(&10));

    // Pin one more; a pre-cancelled run writes nothing and says so.
    let (dataset_id, record_id) = first_record_ids(&e, 11).remove(10);
    let fresh = case::pin_event(
        &e.ws,
        &e.engine,
        &case::PinEventRequest {
            common: common(&e, "late pin"),
            dataset_id,
            record_id,
            display_fields: vec![],
            include_raw_excerpt: false,
        },
    )
    .unwrap();
    let (ctx, control) = fg_ctx("job-cancelled");
    control.cancel();
    let report = case::verify_evidence(&e.ws, &e.engine, &e.investigation_id, None, &ctx).unwrap();
    assert!(report.cancelled);
    assert_eq!(report.updated, 0);
    let (state, _) = state_of(&e, &fresh.evidence_id);
    assert_eq!(state, "unverified", "unreached items keep their state");
}

#[test]
fn pins_into_archived_investigations_are_refused() {
    let e = env(30);
    let inv =
        e.ws.meta
            .get_investigation(&e.investigation_id)
            .unwrap()
            .unwrap();
    e.ws.meta
        .set_investigation_status(&e.investigation_id, inv.revision, "archived", "archived")
        .unwrap();
    let (dataset_id, record_id) = first_record_ids(&e, 1).remove(0);
    let err = case::pin_event(
        &e.ws,
        &e.engine,
        &case::PinEventRequest {
            common: common(&e, "should fail"),
            dataset_id,
            record_id,
            display_fields: vec![],
            include_raw_excerpt: false,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, "case/investigation-archived");
}
