//! Golden fixture tests for the v0.0 reader/normalizer paths (JSONL + CSV).
//! Text/multiline/UTF-16 fixtures exist in the repository for the v0.1
//! parsers and are inventoried in fixtures/README.md.

use std::path::PathBuf;

use logscope_ingest::*;
use logscope_model::{QualityFlag, SourceProtocol, UnixNanos};

fn fixture(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
}

fn ctx() -> NormalizeContext {
    NormalizeContext {
        dataset_id: "ds-fix".into(),
        logical_source_id: "src-fix".into(),
        file_id: "file-fix".into(),
        archive_entry: None,
        resource_id: "res-fix".into(),
        scope_id: "scp-fix".into(),
        parser_id: JSONL_PARSER_ID.into(),
        parser_version: PARSER_VERSION.into(),
        protocol: SourceProtocol::FileImport,
        content_type: Some("application/x-ndjson".into()),
        ingest_time: UnixNanos(1_700_000_000_000_000_000),
    }
}

fn read_all(path: &PathBuf) -> Vec<ReadItem> {
    let file = std::fs::File::open(path).unwrap();
    let mut reader = JsonlReader::new(file);
    let mut items = Vec::new();
    loop {
        let batch = reader.next_batch(128).unwrap();
        if batch.is_empty() {
            break;
        }
        items.extend(batch);
    }
    items
}

fn normalize_all(path: &PathBuf) -> (Vec<logscope_model::LogRecord>, Vec<NormalizeReject>) {
    let profile = builtin::jsonl_generic();
    let context = ctx();
    let mut records = Vec::new();
    let mut rejects = Vec::new();
    for item in read_all(path) {
        match item {
            ReadItem::Parsed(p) => match normalize_log(p, &profile, &context) {
                Ok(r) => records.push(r),
                Err(e) => rejects.push(e),
            },
            ReadItem::Malformed(m) => rejects.push(reject_from_malformed(&m)),
        }
    }
    (records, rejects)
}

#[test]
fn ecs_fixture_normalizes_with_severity_time_and_nested_attrs() {
    let (records, rejects) = normalize_all(&fixture("logs/ecs.jsonl"));
    assert!(rejects.is_empty());
    assert_eq!(records.len(), 3);

    let first = &records[0];
    assert_eq!(first.severity_text.as_deref(), Some("info"));
    assert_eq!(first.severity_number, Some(9));
    assert_eq!(first.display_message, "request accepted");
    assert_eq!(
        first.event_time.unwrap().to_rfc3339(),
        "2024-06-01T10:00:00.123Z"
    );
    // Nested unmapped fields stay typed and nested.
    assert!(first.attributes.contains_key("http"));
    assert!(first.attributes.contains_key("service"));
    let err = &records[2];
    assert_eq!(err.severity_number, Some(17));
    assert!(err.attributes.contains_key("error"));
}

#[test]
fn nodejs_numeric_levels_are_preserved_not_guessed() {
    let (records, rejects) = normalize_all(&fixture("logs/nodejs-structured.jsonl"));
    assert!(rejects.is_empty());
    assert_eq!(records.len(), 3);
    // pino numeric levels (30/40/50) are outside the OTLP 1..=24 mapping:
    // text preserved, number unmapped, flagged - never invented.
    let first = &records[0];
    assert_eq!(first.severity_text.as_deref(), Some("30"));
    assert_eq!(first.severity_number, None);
    assert!(first
        .provenance
        .flags
        .contains(&QualityFlag::SeverityUnmapped));
    // pino "time" is epoch millis text; RFC3339 parse fails -> flagged,
    // original preserved. (The v0.1 Node profile declares EpochMillis.)
    assert!(first
        .provenance
        .flags
        .contains(&QualityFlag::TimestampUnparsed));
    assert_eq!(first.original_timestamp_text.as_deref(), Some("1717236000123"));
    assert_eq!(first.display_message, "server listening");
}

#[test]
fn go_and_python_fixtures_normalize_cleanly() {
    for (path, expected) in [
        ("logs/go-structured.jsonl", 3usize),
        ("logs/python-json.jsonl", 3usize),
    ] {
        let (records, rejects) = normalize_all(&fixture(path));
        assert!(rejects.is_empty(), "{path}: {rejects:?}");
        assert_eq!(records.len(), expected, "{path}");
        assert!(records.iter().all(|r| r.event_time.is_some()), "{path}");
    }
    // Python WARNING maps to WARN.
    let (records, _) = normalize_all(&fixture("logs/python-json.jsonl"));
    assert_eq!(records[1].severity_number, Some(13));
    // Multiline traceback inside the JSON string survives in attributes.
    assert!(records[2]
        .attributes
        .get("exc_info")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.contains("ZeroDivisionError")));
}

#[test]
fn utf8_bom_crlf_fixture_parses_with_exact_locators() {
    let (records, rejects) = normalize_all(&fixture("logs/utf8-bom-crlf.jsonl"));
    assert!(rejects.is_empty(), "{rejects:?}");
    assert_eq!(records.len(), 2);
    // BOM skipped: first record's bytes start at offset 3.
    assert_eq!(records[0].provenance.locator.byte_start, Some(3));
    assert_eq!(records[0].display_message, "bom line one");
}

#[test]
fn malformed_jsonl_fixture_rejects_with_locators_and_truncation() {
    let (records, rejects) = normalize_all(&fixture("logs/malformed.jsonl"));
    assert_eq!(records.len(), 1);
    assert_eq!(rejects.len(), 2);
    assert_eq!(rejects[0].reason_code, "parse/invalid-json");
    assert_eq!(rejects[0].locator.line_start, Some(2));
    assert!(!rejects[0].raw_excerpt.is_empty());
    assert_eq!(rejects[1].reason_code, "parse/truncated-record");
    assert_eq!(rejects[1].locator.line_start, Some(3));
}

#[test]
fn generic_csv_fixture_with_quoted_newline_normalizes() {
    let file = std::fs::File::open(fixture("logs/generic.csv")).unwrap();
    let mut reader = CsvReader::new(file, b',', true).unwrap();
    let profile = builtin::csv_basic();
    let context = NormalizeContext {
        parser_id: CSV_PARSER_ID.into(),
        content_type: Some("text/csv".into()),
        ..ctx()
    };
    let mut records = Vec::new();
    loop {
        let batch = reader.next_batch(64).unwrap();
        if batch.is_empty() {
            break;
        }
        for item in batch {
            match item {
                ReadItem::Parsed(p) => {
                    records.push(normalize_log(p, &profile, &context).unwrap())
                }
                ReadItem::Malformed(m) => panic!("unexpected malformed: {m:?}"),
            }
        }
    }
    assert_eq!(records.len(), 4);
    let multi = &records[2];
    assert_eq!(multi.severity_number, Some(17));
    assert!(multi
        .body
        .as_ref()
        .unwrap()
        .as_str()
        .unwrap()
        .contains("disk full\nretrying"));
    // Display message stays single-line.
    assert_eq!(multi.display_message, "write failed: disk full");
    // Unmapped columns preserved as attributes.
    assert!(multi.attributes.contains_key("component"));
    assert!(multi.attributes.contains_key("elapsed_ms"));
}

#[test]
fn malformed_csv_unbalanced_quote_yields_bounded_outcome() {
    let file = std::fs::File::open(fixture("logs/malformed.csv")).unwrap();
    let mut reader = CsvReader::new(file, b',', true).unwrap();
    let mut parsed = 0;
    let mut malformed = 0;
    loop {
        let batch = reader.next_batch(64).unwrap();
        if batch.is_empty() {
            break;
        }
        for item in batch {
            match item {
                ReadItem::Parsed(_) => parsed += 1,
                ReadItem::Malformed(_) => malformed += 1,
            }
        }
    }
    // The unterminated quote swallows the remainder into one record or one
    // malformed item depending on parser mode; either way nothing is lost
    // silently and the reader terminates.
    assert!(parsed >= 2, "parsed={parsed} malformed={malformed}");
    assert_eq!(parsed + malformed, 3);
}
