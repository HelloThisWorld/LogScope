//! W5 report proofs: byte-determinism, hostile-content inertness for
//! both renderers, the explicit-`unknown` narrative rule, exact-revision
//! capture with drift labels, overwrite refusal, and artifact records
//! whose checksum matches the published bytes.

use std::path::PathBuf;

use logscope_app::report::{self, ReportFormat};
use logscope_case::envelope::{
    self, EventRef, EventSnapshot, EvidenceReference, EvidenceSnapshot, SnapshotRow,
};
use logscope_case::{new_id, EVIDENCE_ENVELOPE_VERSION};
use logscope_workspace::{NewEvidence, NewInvestigation, NewMarker, NewReportDef, Workspace};
use sha2::{Digest, Sha256};

struct Env {
    dir: tempfile::TempDir,
    ws: Workspace,
    inv: String,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::create(&dir.path().join("ws"), "reports", "0.3.0-test").unwrap();
    let inv = ws
        .meta
        .create_investigation(&NewInvestigation {
            investigation_id: new_id("inv"),
            title: "Checkout latency".into(),
            description: None,
            severity: Some("sev2".into()),
            owner_text: None,
            tags_json: "[]".into(),
            incident_started_at: None,
            window_start: None,
            window_end: None,
        })
        .unwrap()
        .investigation_id;
    Env { dir, ws, inv }
}

fn insert_event_evidence(e: &Env, id: &str, title: &str, message: &str) -> i64 {
    let reference = EvidenceReference::Event(EventRef {
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
        event_time: Some(1_700_000_000_000_000_000),
        timestamp_quality: vec![],
    });
    let snapshot = EvidenceSnapshot::Event(EventSnapshot {
        row: SnapshotRow {
            record_id: "log-00000000000000000000000000000000".into(),
            event_time: Some(1_700_000_000_000_000_000),
            severity_text: Some("ERROR".into()),
            severity_number: Some(17),
            display_message: message.into(),
            display_message_truncated: false,
            fields: vec![],
        },
        raw_excerpt: None,
        raw_excerpt_truncated: false,
    });
    e.ws.meta
        .insert_evidence(&NewEvidence {
            evidence_id: id.into(),
            investigation_id: e.inv.clone(),
            envelope_version: EVIDENCE_ENVELOPE_VERSION,
            kind: "event".into(),
            signal: "log".into(),
            title: title.into(),
            annotation: Some("seen during triage".into()),
            relevance: None,
            captured_investigation_revision: 1,
            group_id: None,
            supersedes_evidence_id: None,
            reference_json: envelope::encode_reference(&reference).unwrap(),
            snapshot_json: envelope::encode_snapshot(&snapshot).unwrap(),
        })
        .unwrap()
        .revision
}

fn all_sections_json() -> String {
    serde_json::json!([
        {"kind": "summary", "content": "Typed summary."},
        {"kind": "impact", "content": ""},
        {"kind": "symptoms", "content": "p99 spikes"},
        {"kind": "timeline"},
        {"kind": "hypotheses"},
        {"kind": "evidence"},
        {"kind": "root_cause", "content": "unknown"},
        {"kind": "resolution"},
        {"kind": "validation", "content": "  "},
        {"kind": "follow_up", "content": "rotate the pager"},
    ])
    .to_string()
}

fn make_def(e: &Env, evidence: &[(&str, i64)], markers: &[(&str, i64)]) -> String {
    let sel_e: Vec<serde_json::Value> = evidence
        .iter()
        .map(|(id, rev)| serde_json::json!({"id": id, "revision": rev}))
        .collect();
    let sel_m: Vec<serde_json::Value> = markers
        .iter()
        .map(|(id, rev)| serde_json::json!({"id": id, "revision": rev}))
        .collect();
    e.ws.meta
        .create_report_def(&NewReportDef {
            report_def_id: new_id("rep"),
            investigation_id: e.inv.clone(),
            title: "Checkout incident report".into(),
            subtitle: Some("postmortem".into()),
            sections_json: all_sections_json(),
            selected_evidence_json: serde_json::to_string(&sel_e).unwrap(),
            selected_markers_json: serde_json::to_string(&sel_m).unwrap(),
            options_json: "{}".into(),
        })
        .unwrap()
        .report_def_id
}

fn out(e: &Env, name: &str) -> PathBuf {
    e.dir.path().join(name)
}

#[test]
fn generation_is_byte_deterministic_and_checksummed() {
    let e = env();
    let rev = insert_event_evidence(&e, "evd-a", "first error", "boom");
    e.ws.meta
        .create_marker(&NewMarker {
            marker_id: "mark-a".into(),
            investigation_id: e.inv.clone(),
            kind: "deployment".into(),
            label: "rollout".into(),
            description: None,
            at_nanos: Some(1_699_999_000_000_000_000),
            end_nanos: None,
            original_tz_offset_min: Some(0),
            original_time_text: Some("2023-11-14T22:36:40Z".into()),
        })
        .unwrap();
    let def = make_def(&e, &[("evd-a", rev)], &[("mark-a", 1)]);

    let a1 =
        report::generate_report(&e.ws, &def, ReportFormat::Markdown, &out(&e, "r1.md")).unwrap();
    let a2 =
        report::generate_report(&e.ws, &def, ReportFormat::Markdown, &out(&e, "r2.md")).unwrap();
    let b1 = std::fs::read(out(&e, "r1.md")).unwrap();
    let b2 = std::fs::read(out(&e, "r2.md")).unwrap();
    assert_eq!(b1, b2, "same definition + data must yield identical bytes");
    assert!(!b1.contains(&b'\r'), "LF endings only");

    // The recorded checksum is the checksum of the published bytes.
    let digest = {
        let mut s = String::new();
        for b in Sha256::digest(&b1) {
            use std::fmt::Write;
            let _ = write!(s, "{b:02x}");
        }
        s
    };
    assert_eq!(a1.checksum_sha256.as_deref(), Some(digest.as_str()));
    assert_eq!(a1.byte_size, Some(b1.len() as i64));
    assert_eq!(a1.status, "completed");
    assert_ne!(a1.artifact_id, a2.artifact_id);

    let h1 = report::generate_report(&e.ws, &def, ReportFormat::Html, &out(&e, "r1.html")).unwrap();
    let h2 = report::generate_report(&e.ws, &def, ReportFormat::Html, &out(&e, "r2.html")).unwrap();
    assert_eq!(
        std::fs::read(out(&e, "r1.html")).unwrap(),
        std::fs::read(out(&e, "r2.html")).unwrap()
    );
    assert_eq!(h1.status, "completed");
    assert!(h2.checksum_sha256.is_some());
}

#[test]
fn hostile_content_stays_inert_in_both_formats() {
    let e = env();
    let rev = insert_event_evidence(
        &e,
        "evd-h",
        "titles ``` can ` fence [x](y) <script>alert(1)</script>",
        "``` \n```` \n<script>alert('pwn')</script> & <iframe src=x>",
    );
    let def = make_def(&e, &[("evd-h", rev)], &[]);

    report::generate_report(&e.ws, &def, ReportFormat::Markdown, &out(&e, "h.md")).unwrap();
    let md = std::fs::read_to_string(out(&e, "h.md")).unwrap();
    // The snapshot fence must be longer than any backtick run inside it.
    assert!(
        md.contains("`````"),
        "fence must outsize the 4-backtick run in the content"
    );
    // Inline hostile title is escaped, not active.
    assert!(md.contains("\\`\\`\\`"));
    assert!(!md.contains("[x](y)"), "link syntax must be escaped");

    report::generate_report(&e.ws, &def, ReportFormat::Html, &out(&e, "h.html")).unwrap();
    let html = std::fs::read_to_string(out(&e, "h.html")).unwrap();
    assert!(!html.contains("<script"), "no script may survive escaping");
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<iframe"));
    assert!(!html.contains("<a "), "nothing may render as a link");
    assert!(html.contains("Content-Security-Policy"));
    assert!(html.contains("default-src 'none'"));
    assert!(!html.to_lowercase().contains("http://"));
    assert!(!html.to_lowercase().contains("https://"));
}

#[test]
fn blank_narratives_render_the_explicit_unknown_rule() {
    let e = env();
    let def = make_def(&e, &[], &[]);
    report::generate_report(&e.ws, &def, ReportFormat::Markdown, &out(&e, "u.md")).unwrap();
    let md = std::fs::read_to_string(out(&e, "u.md")).unwrap();
    // impact "", resolution omitted, validation whitespace → all unknown.
    assert_eq!(md.matches("*unknown*").count(), 3);
    // Typed narratives render as typed.
    assert!(md.contains("Typed summary."));
    assert!(md.contains("rotate the pager"));

    report::generate_report(&e.ws, &def, ReportFormat::Html, &out(&e, "u.html")).unwrap();
    let html = std::fs::read_to_string(out(&e, "u.html")).unwrap();
    assert_eq!(html.matches("<em>unknown</em>").count(), 3);
}

#[test]
fn drifted_selection_renders_the_captured_revision_with_a_label() {
    let e = env();
    let rev1 = insert_event_evidence(&e, "evd-d", "original title", "msg");
    // Move the live row past the selection.
    let live = e.ws.meta.get_evidence("evd-d").unwrap().unwrap();
    e.ws.meta
        .update_evidence_annotation(
            "evd-d",
            live.revision,
            "renamed after selection",
            Some("new annotation"),
            None,
        )
        .unwrap();
    let def = make_def(&e, &[("evd-d", rev1)], &[]);

    report::generate_report(&e.ws, &def, ReportFormat::Markdown, &out(&e, "d.md")).unwrap();
    let md = std::fs::read_to_string(out(&e, "d.md")).unwrap();
    assert!(md.contains("As captured at revision 1"));
    assert!(md.contains("original title"), "captured state must render");
    assert!(
        !md.contains("renamed after selection"),
        "live state must not silently replace the captured one"
    );

    // An unrecoverable selection states that instead of guessing.
    let def2 = make_def(&e, &[("evd-d", 999)], &[]);
    report::generate_report(&e.ws, &def2, ReportFormat::Html, &out(&e, "d2.html")).unwrap();
    let html = std::fs::read_to_string(out(&e, "d2.html")).unwrap();
    assert!(html.contains("Selected revision 999 unavailable"));
}

#[test]
fn destinations_are_never_overwritten_and_bad_definitions_are_refused() {
    let e = env();
    let def = make_def(&e, &[], &[]);
    let dest = out(&e, "exists.md");
    std::fs::write(&dest, "already here").unwrap();
    let err = report::generate_report(&e.ws, &def, ReportFormat::Markdown, &dest).unwrap_err();
    assert_eq!(err.code, "report/destination-exists");
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "already here");
    // Refusal happened before any artifact row was created.
    assert!(e.ws.meta.list_report_artifacts(&e.inv).unwrap().is_empty());

    // Unknown and duplicate section kinds are structured refusals.
    assert!(report::parse_sections(r#"[{"kind":"conclusions"}]"#).is_err());
    assert!(report::parse_sections(r#"[{"kind":"summary"},{"kind":"summary"}]"#).is_err());
}
