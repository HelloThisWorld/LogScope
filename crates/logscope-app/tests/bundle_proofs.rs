//! W7 bundle proofs: round-trip fidelity, byte-determinism, disclosure
//! projection over every entry, the data-subset closure, and a hostile
//! import corpus that must be refused before anything is extracted.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use logscope_app::bundle::{self, BundleManifest, BundleOptions};
use logscope_app::case::{self, PinCommon, QueryScope};
use logscope_app::{run_import, ImportRequest};
use logscope_ingest::builtin;
use logscope_jobs::{JobContext, JobControl};
use logscope_query::{EngineConnection, TimeStrategy};
use logscope_workspace::{NewInvestigation, NewMarker, Workspace};
use sha2::{Digest, Sha256};

const SECRET: &str = "hunter2-super-secret";

fn write_es_jsonl(path: &Path, records: usize) {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    for i in 0..records {
        writeln!(
            f,
            "{{\"@timestamp\":\"2024-06-01T10:{:02}:{:02}Z\",\"log.level\":\"ERROR\",\
             \"message\":\"handler {i} failed carrying {SECRET}\",\"service.name\":\"orders\",\
             \"idx\":{i}}}",
            (i / 60) % 60,
            i % 60,
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
    dir: tempfile::TempDir,
    ws: Workspace,
    engine: EngineConnection,
    inv: String,
}

/// Full pipeline env: real import, one investigation, one event pin
/// (carrying a real canonical record id), one marker.
fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.jsonl");
    write_es_jsonl(&input, 40);
    let engine = EngineConnection::open_in_memory().unwrap();
    let mut ws = Workspace::create(&dir.path().join("ws"), "bundle", "0.3.0-test").unwrap();
    let request = ImportRequest::new(vec![input], builtin::elasticsearch_export(), "es");
    let (ctx, _c) = fg_ctx("job-import");
    run_import(&mut ws, &engine, &request, &ctx).expect("import succeeds");

    let inv = ws
        .meta
        .create_investigation(&NewInvestigation {
            investigation_id: "inv-bundle".into(),
            title: format!("incident with {SECRET}"),
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
    ws.meta
        .create_marker(&NewMarker {
            marker_id: "mark-b".into(),
            investigation_id: inv.clone(),
            kind: "deployment".into(),
            label: format!("rollout of {SECRET}"),
            description: None,
            at_nanos: Some(1_700_000_000_000_000_000),
            end_nanos: None,
            original_tz_offset_min: None,
            original_time_text: None,
        })
        .unwrap();

    // Pin one real event so the closure planner has a canonical id.
    let selection = logscope_app::explorer::resolve_dataset_selection(&ws, &[]).unwrap();
    let files = logscope_app::explorer::segment_files_for(&ws, &selection).unwrap();
    let analysis = logscope_app::explorer::analyze_query(&ws, &selection, "");
    let filter = logscope_app::explorer::compile_for_execution(&ws, &selection, &analysis).unwrap();
    let cancel = logscope_query::QueryCancelHandle::new(engine.interrupt_handle());
    let window = logscope_query::resolve_window(&TimeStrategy::All, None);
    let page = logscope_query::query_page(
        &engine,
        &files,
        &filter,
        &window,
        &logscope_query::PageRequest {
            cursor: None,
            backward: false,
            limit: 1,
        },
        &cancel,
        None,
    )
    .unwrap();
    let row = page.rows.first().expect("one row");
    case::pin_event(
        &ws,
        &engine,
        &case::PinEventRequest {
            common: PinCommon {
                investigation_id: inv.clone(),
                title: format!("event mentioning {SECRET}"),
                annotation: Some(format!("annotation with {SECRET}")),
                relevance: None,
                group_id: None,
            },
            dataset_id: row.dataset_id.clone(),
            record_id: row.record_id.clone(),
            display_fields: vec!["service.name".into()],
            include_raw_excerpt: true,
        },
    )
    .unwrap();
    // A query pin referencing the current scope (no saved search).
    case::pin_query(
        &ws,
        &engine,
        &case::PinQueryRequest {
            common: PinCommon {
                investigation_id: inv.clone(),
                title: "all errors".into(),
                annotation: None,
                relevance: None,
                group_id: None,
            },
            scope: QueryScope {
                query_text: "severity:ERROR".into(),
                dataset_ids: vec![],
                time_strategy: TimeStrategy::All,
            },
            saved_search_id: None,
        },
    )
    .unwrap();

    Env {
        dir,
        ws,
        engine,
        inv,
    }
}

fn out(e: &Env, name: &str) -> PathBuf {
    e.dir.path().join(name)
}

#[test]
fn round_trip_preserves_the_case_and_records_provenance() {
    let e = env();
    let export = bundle::export_bundle(
        &e.ws,
        &e.engine,
        &e.inv,
        &out(&e, "case.logscope-case"),
        &BundleOptions::default(),
    )
    .unwrap();
    assert_eq!(export.status, "completed");
    let manifest: BundleManifest =
        serde_json::from_str(export.manifest_json.as_deref().unwrap()).unwrap();
    // A real pinned record means the data subset is included.
    assert_eq!(manifest.reproduction_scope, "included_subset_only");
    assert!(manifest
        .entries
        .iter()
        .any(|x| x.path == "data/logs.parquet"));

    let dest = out(&e, "imported-ws");
    let summary = bundle::import_bundle(
        &out(&e, "case.logscope-case"),
        &dest,
        "imported",
        "0.3.0-test",
    )
    .unwrap();
    assert_eq!(summary.investigation_id, e.inv);
    assert_eq!(summary.evidence, 2);
    assert_eq!(summary.markers, 1);
    assert!(summary.data_included);

    let imported = Workspace::open(&dest, "0.3.0-test").unwrap();
    let inv = imported.meta.get_investigation(&e.inv).unwrap().unwrap();
    assert!(inv.title.contains(SECRET), "no profile = verbatim export");
    let evidence = imported.meta.list_evidence(&e.inv, true).unwrap();
    assert_eq!(evidence.len(), 2);
    // Snapshots survive byte-identical (readable without the dataset).
    let original = e.ws.meta.list_evidence(&e.inv, true).unwrap();
    for (a, b) in original.iter().zip(evidence.iter()) {
        assert_eq!(a.evidence_id, b.evidence_id);
        assert_eq!(a.snapshot_json, b.snapshot_json);
        assert_eq!(a.reference_json, b.reference_json);
    }
    // Provenance recorded in the destination.
    let imports = imported.meta.list_bundle_imports().unwrap();
    assert_eq!(imports.len(), 1);
    assert!(!imports[0].bundle_checksum.is_empty());
    // Data parquet extracted as an inert file.
    assert!(dest.join("imported-data").join("logs.parquet").exists());
}

#[test]
fn export_is_byte_deterministic() {
    let e = env();
    bundle::export_bundle(
        &e.ws,
        &e.engine,
        &e.inv,
        &out(&e, "a.logscope-case"),
        &BundleOptions::default(),
    )
    .unwrap();
    bundle::export_bundle(
        &e.ws,
        &e.engine,
        &e.inv,
        &out(&e, "b.logscope-case"),
        &BundleOptions::default(),
    )
    .unwrap();
    let a = std::fs::read(out(&e, "a.logscope-case")).unwrap();
    let b = std::fs::read(out(&e, "b.logscope-case")).unwrap();
    assert_eq!(a, b, "same workspace state must export identical bytes");
}

#[test]
fn disclosure_profile_projects_every_entry_and_excludes_raw_data() {
    let e = env();
    let profile =
        e.ws.meta
            .create_redaction_profile(
                "red-b",
                "outbound",
                &serde_json::json!([
                    {"kind": "replace_exact", "find": SECRET, "replace": "[secret]"}
                ])
                .to_string(),
                "{}",
            )
            .unwrap();
    let dest = out(&e, "redacted.logscope-case");
    let export = bundle::export_bundle(
        &e.ws,
        &e.engine,
        &e.inv,
        &dest,
        &BundleOptions {
            redaction_profile_id: Some(profile.profile_id.clone()),
            include_reports: false,
        },
    )
    .unwrap();
    let manifest: BundleManifest =
        serde_json::from_str(export.manifest_json.as_deref().unwrap()).unwrap();
    // Under a profile no raw rows leave at all.
    assert_eq!(manifest.reproduction_scope, "snapshot_only");
    assert!(!manifest
        .entries
        .iter()
        .any(|x| x.path == "data/logs.parquet"));
    assert!(manifest.disclosure.is_some());
    assert!(manifest
        .inclusions
        .get("data")
        .unwrap()
        .contains("disclosure profile"));

    // The planted secret is absent from EVERY entry in the archive.
    let bytes = std::fs::read(&dest).unwrap();
    let mut zipr = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    for i in 0..zipr.len() {
        let mut entry = zipr.by_index(i).unwrap();
        let mut content = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut content).unwrap();
        let name = entry.name().to_string();
        assert!(
            !String::from_utf8_lossy(&content).contains(SECRET),
            "secret leaked in bundle entry {name:?}"
        );
    }
    // Canonical data untouched.
    let inv = e.ws.meta.get_investigation(&e.inv).unwrap().unwrap();
    assert!(inv.title.contains(SECRET));
}

// ---- hostile corpus ---------------------------------------------------------

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            w.start_file(*name, opts).unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

fn sha_hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    for b in Sha256::digest(bytes) {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn manifest_for(entries: &[(&str, &[u8])], version: u32) -> Vec<u8> {
    let entry_values: Vec<serde_json::Value> = entries
        .iter()
        .map(|(name, bytes)| {
            serde_json::json!({
                "path": name,
                "entry_type": "metadata",
                "byte_size": bytes.len(),
                "sha256": sha_hex(bytes),
            })
        })
        .collect();
    let m = serde_json::json!({
        "bundle_schema_version": version,
        "min_compatible_version": version,
        "investigation_id": "inv-x",
        "investigation_title": "x",
        "investigation_revision": 1,
        "reproduction_scope": "snapshot_only",
        "entries": entry_values,
        "envelope_version": 1,
        "app_version": "test",
        "disclosure": null,
        "inclusions": {},
    });
    serde_json::to_vec(&m).unwrap()
}

fn try_import(
    dir: &Path,
    zip_bytes: &[u8],
) -> Result<bundle::ImportSummary, logscope_jobs::JobError> {
    let bundle_path = dir.join("hostile.logscope-case");
    std::fs::write(&bundle_path, zip_bytes).unwrap();
    let dest = dir.join("hostile-ws");
    let r = bundle::import_bundle(&bundle_path, &dest, "x", "0.3.0-test");
    if r.is_err() {
        assert!(
            !dest.exists(),
            "a refused import must leave no destination behind"
        );
    }
    let _ = std::fs::remove_file(&bundle_path);
    let _ = std::fs::remove_dir_all(&dest);
    r
}

#[test]
fn hostile_entry_paths_are_refused_before_extraction() {
    let dir = tempfile::tempdir().unwrap();
    let hostile_names = [
        "../evil.json",
        "/etc/passwd",
        "a\\b.json",
        "c:evil",
        "data:stream.json",
        "con.json",
        "investigation/aux.txt",
        "bad\u{0007}name",
        "dir./x.json",
        "deep/a/b/c/d/e/f/g/h/i.json",
    ];
    for name in hostile_names {
        let z = zip_of(&[(name, b"x")]);
        let err = try_import(dir.path(), &z).unwrap_err();
        assert_eq!(err.code, "bundle/invalid", "path {name:?} must refuse");
    }
    // Case-collision.
    let z = zip_of(&[("A.json", b"x"), ("a.json", b"y")]);
    assert_eq!(
        try_import(dir.path(), &z).unwrap_err().code,
        "bundle/invalid"
    );
    // Not a zip at all.
    assert_eq!(
        try_import(dir.path(), b"MZ this is not a zip")
            .unwrap_err()
            .code,
        "bundle/invalid"
    );
}

#[test]
fn manifest_gates_hidden_payloads_checksums_and_versions() {
    let dir = tempfile::tempdir().unwrap();

    // Hidden payload: entry present in the ZIP but absent from the manifest.
    let body: &[u8] = b"{}";
    let manifest = manifest_for(&[("investigation/investigation.json", body)], 1);
    let z = zip_of(&[
        ("manifest.json", manifest.as_slice()),
        ("investigation/investigation.json", body),
        ("hidden/payload.bin", b"sneaky"),
        ("checksums.sha256", b""),
    ]);
    let err = try_import(dir.path(), &z).unwrap_err();
    assert_eq!(err.code, "bundle/invalid");
    assert!(err.message.contains("not in manifest"));

    // Checksum mismatch.
    let manifest = manifest_for(&[("investigation/investigation.json", b"{\"a\":1}")], 1);
    let z = zip_of(&[
        ("manifest.json", manifest.as_slice()),
        ("investigation/investigation.json", b"{\"a\":2}"),
        ("checksums.sha256", b""),
    ]);
    let err = try_import(dir.path(), &z).unwrap_err();
    assert!(err.message.contains("checksum") || err.message.contains("declares"));

    // Future bundle version fails actionably.
    let manifest = manifest_for(&[("investigation/investigation.json", body)], 99);
    let z = zip_of(&[
        ("manifest.json", manifest.as_slice()),
        ("investigation/investigation.json", body),
        ("checksums.sha256", b""),
    ]);
    let err = try_import(dir.path(), &z).unwrap_err();
    assert_eq!(err.code, "bundle/unsupported-version");
    assert!(err.message.contains("update LogScope"));
}

#[test]
fn destination_and_duplicate_rules() {
    let e = env();
    let bundle_path = out(&e, "dup.logscope-case");
    bundle::export_bundle(
        &e.ws,
        &e.engine,
        &e.inv,
        &bundle_path,
        &BundleOptions::default(),
    )
    .unwrap();
    // Existing destination refuses.
    let dest = out(&e, "already-there");
    std::fs::create_dir_all(&dest).unwrap();
    let err = bundle::import_bundle(&bundle_path, &dest, "x", "0.3.0-test").unwrap_err();
    assert_eq!(err.code, "bundle/invalid");
    // Export destination overwrite refuses too.
    let err2 = bundle::export_bundle(
        &e.ws,
        &e.engine,
        &e.inv,
        &bundle_path,
        &BundleOptions::default(),
    )
    .unwrap_err();
    assert_eq!(err2.code, "bundle/destination-exists");
}
