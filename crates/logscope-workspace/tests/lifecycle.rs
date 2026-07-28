//! Workspace lifecycle, publish atomicity, and crash-recovery tests.

use std::fs;

use logscope_workspace::*;

fn seg(id: &str, file_name: &str, rows: i64) -> SegmentToPublish {
    SegmentToPublish {
        segment_id: id.to_string(),
        signal: Signal::Logs,
        file_name: file_name.to_string(),
        row_count: rows,
        byte_size: 1000,
        min_event_time: Some(1),
        max_event_time: Some(100),
    }
}

#[test]
fn create_open_close_reopen_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws1");

    let ws = Workspace::create(&root, "Test Workspace", "0.0.0").unwrap();
    let workspace_id = ws.manifest.workspace_id.clone();
    assert!(root.join("manifest.json").exists());
    assert!(root.join("workspace.db").exists());
    assert!(root.join("staging").is_dir());
    drop(ws);

    let ws2 = Workspace::open(&root, "0.0.0").unwrap();
    assert_eq!(ws2.manifest.workspace_id, workspace_id);
    assert!(ws2.recovery.is_clean());

    // Creating over an existing workspace is refused.
    let err = Workspace::create(&root, "Again", "0.0.0").err().unwrap();
    assert!(matches!(err, WorkspaceError::AlreadyExists(_)));

    // Opening a non-workspace directory is refused.
    let empty = dir.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let err = Workspace::open(&empty, "0.0.0").err().unwrap();
    assert!(matches!(err, WorkspaceError::NotAWorkspace(_)));
}

#[test]
fn publish_makes_segments_visible_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    let ws = Workspace::create(&root, "W", "0.0.0").unwrap();

    ws.meta.insert_job("job-1", "import", Some("ds-1")).unwrap();
    ws.meta
        .create_dataset("ds-1", "app logs", Signal::Logs)
        .unwrap();
    ws.meta
        .insert_source("src-1", "static_file_set", "app.jsonl", "referenced", "{}")
        .unwrap();
    ws.meta.link_dataset_source("ds-1", "src-1").unwrap();
    ws.meta
        .insert_source_file(
            "file-1",
            "src-1",
            "C:/logs/app.jsonl",
            None,
            None,
            123,
            None,
            "hash1",
        )
        .unwrap();

    let staging = ws.begin_staging("job-1").unwrap();
    let staged_file = staging.join("logs-seg1.parquet");
    fs::write(&staged_file, b"fake parquet payload").unwrap();

    ws.publish_staged_import(
        "job-1",
        "ds-1",
        &[(staged_file.clone(), seg("seg-1", "logs-seg1.parquet", 42))],
        &[LedgerEntry {
            source_id: "src-1".into(),
            file_id: "file-1".into(),
            checkpoint_json: "{}".into(),
            counts: LedgerCounts {
                accepted: 42,
                ..Default::default()
            },
        }],
        &PublishVersions {
            parser_id: Some("jsonl".into()),
            parser_version: Some("0.0.1".into()),
            normalizer_version: Some("0.0.1".into()),
            model_version: Some("0.0.1".into()),
            ..Default::default()
        },
    )
    .unwrap();

    // File moved out of staging into the dataset dir.
    assert!(!staged_file.exists());
    assert!(root.join("data/ds-1/logs-seg1.parquet").exists());

    let dataset = ws.meta.get_dataset("ds-1").unwrap().unwrap();
    assert_eq!(dataset.status, "published");
    assert_eq!(dataset.parser_id.as_deref(), Some("jsonl"));
    let segments = ws.meta.segments_for_dataset("ds-1").unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].row_count, 42);

    // Reopen: everything still there, recovery clean.
    drop(ws);
    let ws2 = Workspace::open(&root, "0.0.0").unwrap();
    assert!(ws2.recovery.is_clean(), "recovery: {:?}", ws2.recovery);
    assert_eq!(ws2.meta.segments_for_dataset("ds-1").unwrap().len(), 1);
}

#[test]
fn interrupted_import_is_discarded_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    let ws = Workspace::create(&root, "W", "0.0.0").unwrap();

    // Simulate a crash mid-import: running job, staging dataset, staged
    // file, and an orphan file already moved into data/ without a commit.
    ws.meta.insert_job("job-x", "import", Some("ds-x")).unwrap();
    ws.meta
        .create_dataset("ds-x", "half done", Signal::Logs)
        .unwrap();
    let staging = ws.begin_staging("job-x").unwrap();
    fs::write(staging.join("logs-a.parquet"), b"staged").unwrap();
    let ds_dir = root.join("data/ds-x");
    fs::create_dir_all(&ds_dir).unwrap();
    fs::write(ds_dir.join("logs-orphan.parquet"), b"moved-but-uncommitted").unwrap();
    drop(ws); // "crash"

    let ws2 = Workspace::open(&root, "0.0.0").unwrap();
    let report = &ws2.recovery;
    assert_eq!(report.interrupted_jobs, vec!["job-x".to_string()]);
    assert_eq!(report.discarded_staging_dirs, vec!["job-x".to_string()]);
    assert!(report
        .removed_orphan_files
        .contains(&"ds-x/logs-orphan.parquet".to_string()));
    assert_eq!(report.discarded_staging_datasets, vec!["ds-x".to_string()]);

    // Workspace is clean and usable.
    assert!(!root.join("staging/job-x").exists());
    assert!(!ds_dir.join("logs-orphan.parquet").exists());
    assert!(ws2.meta.get_dataset("ds-x").unwrap().is_none());
    let jobs = ws2.meta.list_jobs().unwrap();
    assert_eq!(jobs[0].status, "failed");
    assert!(jobs[0]
        .error_json
        .as_deref()
        .unwrap()
        .contains("job/interrupted"));

    // Published data from other datasets would be untouched; a fresh import
    // into a new dataset works.
    ws2.meta
        .insert_job("job-y", "import", Some("ds-y"))
        .unwrap();
    ws2.meta
        .create_dataset("ds-y", "retry", Signal::Logs)
        .unwrap();
    let staging = ws2.begin_staging("job-y").unwrap();
    let f = staging.join("logs-b.parquet");
    fs::write(&f, b"data").unwrap();
    ws2.publish_staged_import(
        "job-y",
        "ds-y",
        &[(f, seg("seg-b", "logs-b.parquet", 1))],
        &[],
        &PublishVersions::default(),
    )
    .unwrap();
    assert_eq!(
        ws2.meta.get_dataset("ds-y").unwrap().unwrap().status,
        "published"
    );
}

#[test]
fn cancelled_import_leaves_existing_data_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ws");
    let ws = Workspace::create(&root, "W", "0.0.0").unwrap();

    // Publish dataset A.
    ws.meta.insert_job("job-a", "import", Some("ds-a")).unwrap();
    ws.meta
        .create_dataset("ds-a", "keep me", Signal::Logs)
        .unwrap();
    let staging = ws.begin_staging("job-a").unwrap();
    let f = staging.join("logs-a.parquet");
    fs::write(&f, b"a").unwrap();
    ws.publish_staged_import(
        "job-a",
        "ds-a",
        &[(f, seg("seg-a", "logs-a.parquet", 5))],
        &[],
        &PublishVersions::default(),
    )
    .unwrap();

    // Start and cancel import B.
    ws.meta.insert_job("job-b", "import", Some("ds-b")).unwrap();
    ws.meta
        .create_dataset("ds-b", "cancel me", Signal::Logs)
        .unwrap();
    let staging_b = ws.begin_staging("job-b").unwrap();
    fs::write(staging_b.join("logs-b.parquet"), b"b").unwrap();
    ws.discard_staging("job-b", Some("ds-b")).unwrap();
    ws.meta
        .update_job_status("job-b", "cancelled", None)
        .unwrap();

    assert!(!root.join("staging/job-b").exists());
    assert!(ws.meta.get_dataset("ds-b").unwrap().is_none());
    // Dataset A untouched.
    assert_eq!(
        ws.meta.get_dataset("ds-a").unwrap().unwrap().status,
        "published"
    );
    assert!(root.join("data/ds-a/logs-a.parquet").exists());
}

#[test]
fn rejected_records_are_bounded_and_retrievable() {
    let dir = tempfile::tempdir().unwrap();
    let ws = Workspace::create(&dir.path().join("ws"), "W", "0.0.0").unwrap();
    let big = vec![b'x'; 10_000];
    ws.meta
        .insert_rejected(
            "ds-1",
            "src-1",
            "file-1",
            &logscope_model::RecordLocator {
                record_number: Some(3),
                line_start: Some(3),
                ..Default::default()
            },
            "parse/invalid-json",
            "unexpected end of input",
            Some(&big),
            ("jsonl", "0.0.1"),
            Some(("generic-jsonl", "1")),
            false,
        )
        .unwrap();
    let rows = ws.meta.rejected_for_dataset("ds-1", 10, 0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason_code, "parse/invalid-json");
    assert_eq!(
        rows[0].raw_excerpt.as_ref().unwrap().len(),
        MAX_REJECT_EXCERPT_BYTES
    );
    let locator: logscope_model::RecordLocator =
        serde_json::from_str(&rows[0].locator_json).unwrap();
    assert_eq!(locator.record_number, Some(3));
}
