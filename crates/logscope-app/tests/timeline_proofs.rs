//! W4 timeline proofs: deterministic merge ordering over markers and
//! evidence, documented time derivation per evidence kind, the explicit
//! undated section with stated reasons, and archived exclusion.

use logscope_app::timeline::{timeline, TimelineModel};
use logscope_case::envelope::{
    self, CountState, DatasetRevRef, EventRef, EventSnapshot, EvidenceReference, EvidenceSnapshot,
    IntervalRef, ItemReference, ItemSnapshot, QueryContext, QueryRef, SnapshotRow,
};
use logscope_case::{new_id, EVIDENCE_ENVELOPE_VERSION};
use logscope_workspace::{NewEvidence, NewInvestigation, NewMarker, Workspace};

struct Env {
    _dir: tempfile::TempDir,
    ws: Workspace,
    inv: String,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::create(&dir.path().join("ws"), "timeline", "0.3.0-test").unwrap();
    let inv = ws
        .meta
        .create_investigation(&NewInvestigation {
            investigation_id: new_id("inv"),
            title: "timeline".into(),
            description: None,
            severity: None,
            owner_text: None,
            tags_json: "[]".into(),
            incident_started_at: None,
            window_start: None,
            window_end: None,
        })
        .unwrap()
        .investigation_id;
    Env { _dir: dir, ws, inv }
}

fn marker(e: &Env, id_hint: &str, at: Option<i64>, end: Option<i64>) -> String {
    let id = format!("mark-{id_hint}");
    e.ws.meta
        .create_marker(&NewMarker {
            marker_id: id.clone(),
            investigation_id: e.inv.clone(),
            kind: "deployment".into(),
            label: format!("marker {id_hint}"),
            description: None,
            at_nanos: at,
            end_nanos: end,
            original_tz_offset_min: None,
            original_time_text: None,
        })
        .unwrap();
    id
}

fn ctx(resolved: Option<(i64, i64)>) -> QueryContext {
    QueryContext {
        query_text: "severity:ERROR".into(),
        language_version: 1,
        fingerprint: None,
        dataset_ids: vec!["ds-1".into()],
        time_strategy_json: "{\"kind\":\"all\"}".into(),
        resolved_start: resolved.map(|r| r.0),
        resolved_end: resolved.map(|r| r.1),
        omitted_untimestamped: None,
    }
}

fn snapshot_row() -> SnapshotRow {
    SnapshotRow {
        record_id: "log-00000000000000000000000000000000".into(),
        event_time: None,
        severity_text: None,
        severity_number: None,
        display_message: "captured".into(),
        display_message_truncated: false,
        fields: vec![],
    }
}

fn insert(
    e: &Env,
    id_hint: &str,
    kind: &str,
    reference: &EvidenceReference,
    snapshot: &EvidenceSnapshot,
) -> String {
    let id = format!("evd-{id_hint}");
    e.ws.meta
        .insert_evidence(&NewEvidence {
            evidence_id: id.clone(),
            investigation_id: e.inv.clone(),
            envelope_version: EVIDENCE_ENVELOPE_VERSION,
            kind: kind.into(),
            signal: "log".into(),
            title: format!("evidence {id_hint}"),
            annotation: None,
            relevance: None,
            captured_investigation_revision: 1,
            group_id: None,
            supersedes_evidence_id: None,
            reference_json: envelope::encode_reference(reference).unwrap(),
            snapshot_json: envelope::encode_snapshot(snapshot).unwrap(),
        })
        .unwrap();
    id
}

fn event_ref(t: Option<i64>) -> EvidenceReference {
    EvidenceReference::Event(EventRef {
        record_id: "log-00000000000000000000000000000000".into(),
        dataset_id: "ds-1".into(),
        dataset_revision: "dsrev-0".into(),
        segment_id: None,
        source_file_id: None,
        source_content_hash: None,
        source_locator_json: None,
        profile_id: None,
        profile_version: None,
        parser_id: "p".into(),
        parser_version: "1".into(),
        event_time: t,
        timestamp_quality: vec![],
    })
}

fn event_snap() -> EvidenceSnapshot {
    EvidenceSnapshot::Event(EventSnapshot {
        row: snapshot_row(),
        raw_excerpt: None,
        raw_excerpt_truncated: false,
    })
}

fn query_ref(resolved: Option<(i64, i64)>) -> EvidenceReference {
    EvidenceReference::Query(QueryRef {
        context: ctx(resolved),
        datasets: vec![DatasetRevRef {
            dataset_id: "ds-1".into(),
            dataset_revision: "dsrev-0".into(),
        }],
        saved_search_id: None,
        saved_search_fingerprint: None,
        sort: "keyset".into(),
        count: CountState::Unknown,
        representative_ids: vec![],
    })
}

fn query_snap() -> EvidenceSnapshot {
    EvidenceSnapshot::Query(logscope_case::envelope::QuerySummarySnapshot {
        count: CountState::Unknown,
        duration_ms: None,
        rows: vec![],
        rows_truncated: false,
    })
}

fn names(list: &[logscope_app::timeline::TimelineEntry]) -> Vec<&str> {
    list.iter().map(|e| e.id.as_str()).collect()
}

#[test]
fn merge_order_is_deterministic_and_documented() {
    let e = env();
    // Same instant: an instant sorts before an interval starting there;
    // id breaks the final tie.
    marker(&e, "b-instant", Some(1000), None);
    marker(&e, "a-interval", Some(1000), Some(2000));
    marker(&e, "early", Some(500), None);
    let ev_time = insert(&e, "event", "event", &event_ref(Some(1500)), &event_snap());
    let ev_window = insert(
        &e,
        "window",
        "query",
        &query_ref(Some((800, 1800))),
        &query_snap(),
    );

    let t: TimelineModel = timeline(&e.ws, &e.inv).unwrap();
    assert_eq!(
        names(&t.dated),
        [
            "mark-early",
            "evd-window",      // 800 interval
            "mark-b-instant",  // 1000 instant before interval
            "mark-a-interval", // 1000 interval
            "evd-event",       // 1500
        ]
    );
    assert!(t.undated.is_empty());
    // Sources are documented per entry.
    let by_id = |id: &str| t.dated.iter().find(|x| x.id == id).unwrap();
    assert_eq!(by_id(&ev_time).time_source, "event_time");
    assert_eq!(by_id(&ev_window).time_source, "resolved_window");
    assert_eq!(by_id("mark-early").time_source, "marker");

    // Determinism: a second read yields the identical order.
    let again = timeline(&e.ws, &e.inv).unwrap();
    assert_eq!(names(&again.dated), names(&t.dated));
}

#[test]
fn every_undated_rule_states_its_reason() {
    let e = env();
    marker(&e, "undated", None, None);
    insert(&e, "no-time", "event", &event_ref(None), &event_snap());
    insert(&e, "allscope", "query", &query_ref(None), &query_snap());
    insert(
        &e,
        "manual",
        "item_ref",
        &EvidenceReference::ItemRef(ItemReference {
            item_id: "itm-x".into(),
            item_revision: 1,
        }),
        &EvidenceSnapshot::ItemRef(ItemSnapshot {
            item_kind: "note".into(),
            content: "n".into(),
            content_truncated: false,
        }),
    );
    // A future-version envelope must land undated, not error the timeline.
    e.ws.meta
        .insert_evidence(&NewEvidence {
            evidence_id: "evd-future".into(),
            investigation_id: e.inv.clone(),
            envelope_version: EVIDENCE_ENVELOPE_VERSION + 999,
            kind: "event".into(),
            signal: "log".into(),
            title: "from the future".into(),
            annotation: None,
            relevance: None,
            captured_investigation_revision: 1,
            group_id: None,
            supersedes_evidence_id: None,
            reference_json: "{\"kind\":\"who_knows\"}".into(),
            snapshot_json: "{}".into(),
        })
        .unwrap();

    let t = timeline(&e.ws, &e.inv).unwrap();
    assert!(t.dated.is_empty());
    assert_eq!(t.undated.len(), 5);
    for entry in &t.undated {
        assert_eq!(entry.time_source, "none");
        let reason = entry.undated_reason.as_deref().unwrap();
        assert!(!reason.is_empty());
    }
    let reason_of = |id: &str| {
        t.undated
            .iter()
            .find(|x| x.id == id)
            .unwrap()
            .undated_reason
            .as_deref()
            .unwrap()
            .to_string()
    };
    assert!(reason_of("evd-no-time").contains("no timestamp"));
    assert!(reason_of("evd-allscope").contains("unbounded"));
    assert!(reason_of("evd-manual").contains("manual item"));
    assert!(reason_of("evd-future").contains("envelope"));
    assert!(reason_of("mark-undated").contains("without a timestamp"));
    // Undated order is stable: sorted by id.
    let mut ids = names(&t.undated);
    let sorted = {
        let mut s = ids.clone();
        s.sort();
        s
    };
    assert_eq!(ids, sorted, "undated section must be id-ordered");
    ids.clear();
}

#[test]
fn archived_entries_are_excluded_and_counted() {
    let e = env();
    marker(&e, "keep", Some(100), None);
    let gone = marker(&e, "gone", Some(200), None);
    e.ws.meta.set_marker_archived(&gone, 1, true).unwrap();
    let ev = insert(&e, "keep", "event", &event_ref(Some(300)), &event_snap());
    let ev_gone = insert(&e, "gone", "event", &event_ref(Some(400)), &event_snap());
    let row = e.ws.meta.get_evidence(&ev_gone).unwrap().unwrap();
    e.ws.meta
        .set_evidence_archived(&ev_gone, row.revision, true)
        .unwrap();

    let t = timeline(&e.ws, &e.inv).unwrap();
    assert_eq!(names(&t.dated), ["mark-keep", ev.as_str()]);
    assert_eq!(t.archived_excluded, 2);

    // Interval bounds derivation is exact for histogram-interval pins.
    let iv = insert(
        &e,
        "iv",
        "histogram_interval",
        &EvidenceReference::HistogramInterval(IntervalRef {
            context: ctx(None),
            datasets: vec![DatasetRevRef {
                dataset_id: "ds-1".into(),
                dataset_revision: "dsrev-0".into(),
            }],
            start: 250,
            end: 350,
            bucket_width_nanos: 10,
            display_timezone: "UTC".into(),
            count: CountState::Unknown,
            representative_ids: vec![],
        }),
        &EvidenceSnapshot::HistogramInterval(logscope_case::envelope::IntervalSnapshot {
            count: CountState::Unknown,
            neighbor_buckets: vec![],
            rows: vec![],
            rows_truncated: false,
        }),
    );
    let t2 = timeline(&e.ws, &e.inv).unwrap();
    let entry = t2.dated.iter().find(|x| x.id == iv).unwrap();
    assert_eq!((entry.at_nanos, entry.end_nanos), (Some(250), Some(350)));
    assert_eq!(entry.time_source, "interval_bounds");
}

#[test]
fn unknown_investigation_is_refused() {
    let e = env();
    let err = timeline(&e.ws, "inv-does-not-exist").unwrap_err();
    assert_eq!(err.code, "workspace/missing-entity");
}

#[test]
fn marker_time_parsing_preserves_the_offset_and_normalizes_to_utc() {
    use logscope_app::timeline::parse_marker_time;
    // Zulu.
    let (n, off) = parse_marker_time("2026-08-04T10:00:00Z").unwrap();
    assert_eq!(off, 0);
    assert_eq!(n, 1_785_837_600_000_000_000);
    // Positive offset: same instant, offset preserved.
    let (n2, off2) = parse_marker_time("2026-08-04T12:00:00+02:00").unwrap();
    assert_eq!((n2, off2), (n, 120));
    // Negative offset with fractional seconds.
    let (n3, off3) = parse_marker_time("2026-08-04T05:30:00.5-04:30").unwrap();
    assert_eq!(off3, -270);
    assert_eq!(n3, n + 500_000_000);
    // Garbage is refused with the stable code, not coerced.
    let err = parse_marker_time("yesterday at noon").unwrap_err();
    assert_eq!(err.code, "case/invalid-timestamp");
    let err2 = parse_marker_time("2026-08-04 10:00:00").unwrap_err();
    assert_eq!(err2.code, "case/invalid-timestamp");
}
