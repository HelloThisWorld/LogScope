//! W6 disclosure-projection proofs: per-rule behaviour, default-closed
//! posture, preview==final equality, excluded values absent from final
//! artifacts, canonical data untouched, deterministic pseudonymization,
//! and bounded/refused hostile profiles.

use std::path::PathBuf;

use logscope_app::redact::{pseudonym, Projection, RedactionSummary, MASK_TOKEN};
use logscope_app::report::{self, ReportFormat};
use logscope_case::envelope::{
    self, EventRef, EventSnapshot, EvidenceReference, EvidenceSnapshot, SnapshotField, SnapshotRow,
};
use logscope_case::{new_id, EVIDENCE_ENVELOPE_VERSION};
use logscope_workspace::{NewEvidence, NewInvestigation, NewReportDef, Workspace};

const SECRET: &str = "hunter2-super-secret";
const USER: &str = "bob@example.com";

struct Env {
    dir: tempfile::TempDir,
    ws: Workspace,
    inv: String,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::create(&dir.path().join("ws"), "redact", "0.3.0-test").unwrap();
    let inv = ws
        .meta
        .create_investigation(&NewInvestigation {
            investigation_id: new_id("inv"),
            title: format!("incident involving {SECRET}"),
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
    Env { dir, ws, inv }
}

fn insert_secret_evidence(e: &Env) {
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
            display_message: format!("login failed for {USER} token={SECRET}"),
            display_message_truncated: false,
            fields: vec![
                SnapshotField {
                    name: "user".into(),
                    value: USER.into(),
                    truncated: false,
                },
                SnapshotField {
                    name: "api_key".into(),
                    value: SECRET.into(),
                    truncated: false,
                },
                SnapshotField {
                    name: "region".into(),
                    value: "eu-west-1".into(),
                    truncated: false,
                },
            ],
        },
        raw_excerpt: None,
        raw_excerpt_truncated: false,
    });
    e.ws.meta
        .insert_evidence(&NewEvidence {
            evidence_id: "evd-s".into(),
            investigation_id: e.inv.clone(),
            envelope_version: EVIDENCE_ENVELOPE_VERSION,
            kind: "event".into(),
            signal: "log".into(),
            title: format!("suspicious login by {USER}"),
            annotation: Some(format!("the token {SECRET} appeared in plain text")),
            relevance: None,
            captured_investigation_revision: 1,
            group_id: None,
            supersedes_evidence_id: None,
            reference_json: envelope::encode_reference(&reference).unwrap(),
            snapshot_json: envelope::encode_snapshot(&snapshot).unwrap(),
        })
        .unwrap();
}

fn make_def(e: &Env, profile_id: Option<&str>) -> String {
    let def =
        e.ws.meta
            .create_report_def(&NewReportDef {
                report_def_id: new_id("rep"),
                investigation_id: e.inv.clone(),
                title: format!("report mentioning {SECRET}"),
                subtitle: None,
                sections_json: serde_json::json!([
                    {"kind": "summary", "content": format!("user {USER} leaked {SECRET}")},
                    {"kind": "evidence"},
                ])
                .to_string(),
                selected_evidence_json: r#"[{"id":"evd-s","revision":1}]"#.into(),
                selected_markers_json: "[]".into(),
                options_json: "{}".into(),
            })
            .unwrap();
    if let Some(pid) = profile_id {
        e.ws.meta
            .set_report_def_redaction(&def.report_def_id, def.revision, Some(pid))
            .unwrap();
    }
    def.report_def_id
}

fn out(e: &Env, name: &str) -> PathBuf {
    e.dir.path().join(name)
}

#[test]
fn every_rule_kind_applies_and_is_counted() {
    let rules = serde_json::json!([
        {"kind": "omit_field", "field": "api_key"},
        {"kind": "mask_field", "field": "password"},
        {"kind": "pseudonymize", "field": "user", "prefix": "subject"},
        {"kind": "replace_exact", "find": "eu-west-1", "replace": "[region]"},
        {"kind": "replace_regex", "pattern": "tok_[a-z0-9]+", "replace": "[token]"},
    ])
    .to_string();
    let p = Projection::compile(&rules, "{}").unwrap();
    let mut sum = RedactionSummary::default();

    let snap = serde_json::json!({
        "fields": [
            {"name": "api_key", "value": "sk-123", "truncated": false},
            {"name": "password", "value": "hunter2", "truncated": false},
            {"name": "user", "value": "bob", "truncated": false},
        ],
        "display_message": "req tok_abc9 from eu-west-1",
    })
    .to_string();
    let projected = p.snapshot_json(&snap, &mut sum);

    assert!(!projected.contains("sk-123"), "omitted value must vanish");
    assert!(projected.contains("omitted_by_disclosure_profile"));
    assert!(!projected.contains("hunter2"));
    assert!(projected.contains(MASK_TOKEN));
    assert!(!projected.contains("\"bob\""));
    assert!(projected.contains(&pseudonym("subject", "bob")));
    assert!(!projected.contains("tok_abc9"));
    assert!(projected.contains("[token]"));
    assert!(!projected.contains("eu-west-1"));
    assert!(projected.contains("[region]"));

    assert_eq!(sum.fields_omitted, 1);
    assert_eq!(sum.fields_masked, 1);
    assert_eq!(sum.pseudonymized, 1);
    assert!(sum.text_replacements >= 2);

    // Deterministic pseudonyms: same value → same token, different → different.
    assert_eq!(pseudonym("s", "alice"), pseudonym("s", "alice"));
    assert_ne!(pseudonym("s", "alice"), pseudonym("s", "bob"));
}

#[test]
fn posture_deny_allow_paths_and_bounds() {
    let p = Projection::compile(
        "[]",
        &serde_json::json!({
            "field_deny": ["secret"],
            "path_policy": "basename",
            "max_text_chars": 10,
        })
        .to_string(),
    )
    .unwrap();
    let mut sum = RedactionSummary::default();
    let snap = serde_json::json!({
        "fields": [{"name": "secret", "value": "x", "truncated": false}],
        "path": "C:\\Users\\alice\\logs\\app.log",
        "note": "this text is far longer than ten characters",
    })
    .to_string();
    let projected = p.snapshot_json(&snap, &mut sum);
    assert!(projected.contains("omitted_by_disclosure_profile"));
    assert!(
        !projected.contains("Users"),
        "path directories must not leak"
    );
    assert!(
        projected.contains("app.log"),
        "basename policy keeps the file name"
    );
    assert!(projected.contains("truncated by disclosure profile"));
    assert!(sum.paths_redacted >= 1);
    assert!(sum.truncated_blocks >= 1);

    // Allowlist mode: everything not allowed is omitted.
    let p2 = Projection::compile(
        "[]",
        &serde_json::json!({"field_allow": ["region"]}).to_string(),
    )
    .unwrap();
    let mut sum2 = RedactionSummary::default();
    let projected2 = p2.snapshot_json(
        &serde_json::json!({
            "fields": [
                {"name": "region", "value": "eu", "truncated": false},
                {"name": "user", "value": "bob", "truncated": false},
            ]
        })
        .to_string(),
        &mut sum2,
    );
    assert!(projected2.contains("\"eu\""));
    assert!(!projected2.contains("\"bob\""));
}

#[test]
fn hostile_profiles_are_refused_with_structured_errors() {
    // Unknown rule kind.
    assert!(Projection::compile(r#"[{"kind":"delete_everything"}]"#, "{}").is_err());
    // Unknown posture key.
    assert!(Projection::compile("[]", r#"{"surprise":true}"#).is_err());
    // Oversized pattern.
    let long = "a".repeat(600);
    let rule = serde_json::json!([{"kind":"replace_regex","pattern":long,"replace":"x"}]);
    let err = match Projection::compile(&rule.to_string(), "{}") {
        Err(e) => e,
        Ok(_) => panic!("oversized pattern must be refused"),
    };
    assert_eq!(err.code, "redaction/invalid-profile");
    // Invalid regex.
    assert!(Projection::compile(
        r#"[{"kind":"replace_regex","pattern":"(","replace":"x"}]"#,
        "{}"
    )
    .is_err());
    // Rule-count bound.
    let many: Vec<_> = (0..200)
        .map(|i| serde_json::json!({"kind":"omit_field","field":format!("f{i}")}))
        .collect();
    assert!(Projection::compile(&serde_json::to_string(&many).unwrap(), "{}").is_err());
}

#[test]
fn preview_equals_final_bytes_and_secrets_are_absent_everywhere() {
    let e = env();
    insert_secret_evidence(&e);
    let profile =
        e.ws.meta
            .create_redaction_profile(
                "red-t",
                "outbound",
                &serde_json::json!([
                    {"kind": "replace_exact", "find": SECRET, "replace": "[secret]"},
                    {"kind": "pseudonymize", "field": "user", "prefix": "subject"},
                    {"kind": "replace_exact", "find": USER, "replace": "[user]"},
                ])
                .to_string(),
                "{}",
            )
            .unwrap();
    let def = make_def(&e, Some(&profile.profile_id));

    for format in [ReportFormat::Markdown, ReportFormat::Html] {
        let preview = report::render_preview(&e.ws, &def, format).unwrap();
        let dest = out(&e, &format!("r.{}", format.as_str()));
        report::generate_report(&e.ws, &def, format, &dest).unwrap();
        let published = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(
            preview,
            published,
            "the preview must be the final bytes ({})",
            format.as_str()
        );
        // The excluded values are absent from the entire artifact —
        // title, narrative, evidence body, and snapshot alike.
        assert!(
            !published.contains(SECRET),
            "secret leaked in {}",
            format.as_str()
        );
        assert!(
            !published.contains(USER),
            "user leaked in {}",
            format.as_str()
        );
        assert!(published.contains("Disclosure profile applied"));
        assert!(published.contains("pseudonymized (deterministic, labeled)"));
    }

    // Canonical data is untouched: the stored evidence still carries the
    // original values.
    let ev = e.ws.meta.get_evidence("evd-s").unwrap().unwrap();
    assert!(ev.title.contains(USER));
    assert!(ev.annotation.unwrap().contains(SECRET));
    assert!(ev.snapshot_json.contains(SECRET));

    // Without a profile the same report still renders the raw values —
    // redaction is an explicit choice, not a hidden default.
    let plain_def = make_def(&e, None);
    let plain = report::render_preview(&e.ws, &plain_def, ReportFormat::Markdown).unwrap();
    assert!(!plain.contains("[REDACTED]"));
    assert!(plain.contains(SECRET));
}

#[test]
fn missing_profile_refuses_generation() {
    let e = env();
    insert_secret_evidence(&e);
    let def = make_def(&e, None);
    // Attach then delete-by-never-creating: point at an unknown profile
    // via direct SQL is impossible (FK), so simulate by deleting… the FK
    // prevents that too. Instead: attach a real profile, then assert a
    // definition naming a *known* profile with invalid rules refuses.
    let bad =
        e.ws.meta
            .create_redaction_profile("red-bad", "broken", r#"[{"kind":"nope"}]"#, "{}")
            .unwrap();
    let d = e.ws.meta.get_report_def(&def).unwrap().unwrap();
    e.ws.meta
        .set_report_def_redaction(&def, d.revision, Some(&bad.profile_id))
        .unwrap();
    let err = report::render_preview(&e.ws, &def, ReportFormat::Markdown).unwrap_err();
    assert_eq!(err.code, "redaction/invalid-profile");
    // Generation refuses identically - unprojected content is never
    // published under a broken profile.
    let err2 =
        report::generate_report(&e.ws, &def, ReportFormat::Markdown, &out(&e, "x.md")).unwrap_err();
    assert_eq!(err2.code, "redaction/invalid-profile");
    assert!(!out(&e, "x.md").exists());
}
