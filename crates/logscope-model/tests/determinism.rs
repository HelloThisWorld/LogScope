//! Cross-cutting determinism tests for the canonical model.
//!
//! These tests pin the determinism contract: identical canonical content
//! produces identical IDs; wall-clock and workspace-instance fields do not
//! affect hashes; serde round trips preserve every canonical value.

use logscope_model::*;

fn sample_provenance(dataset: &str, ingest_time: i64) -> IngestProvenance {
    IngestProvenance {
        dataset_id: dataset.to_string(),
        logical_source_id: "src-test".to_string(),
        origin: PhysicalOrigin::File {
            file_id: "file-abc".to_string(),
            archive_entry: None,
        },
        locator: RecordLocator {
            record_number: Some(7),
            line_start: Some(7),
            line_end: Some(7),
            byte_start: Some(512),
            byte_end: Some(640),
            ..Default::default()
        },
        parser_id: "jsonl".to_string(),
        parser_version: "0.0.1".to_string(),
        profile_id: Some("generic-jsonl".to_string()),
        profile_version: Some("1".to_string()),
        normalizer_version: "0.0.1".to_string(),
        protocol: SourceProtocol::FileImport,
        content_type: Some("application/x-ndjson".to_string()),
        ingest_time: UnixNanos(ingest_time),
        raw_hash: "aa11".to_string(),
        original_timestamp_precision: Some(TimestampPrecision::Milliseconds),
        flags: vec![],
    }
}

fn sample_log(dataset: &str, ingest_time: i64, observed: i64) -> LogRecord {
    let mut attributes = AttrMap::new();
    attributes.insert("http.status_code".into(), AnyValue::int(500));
    attributes.insert("http.route".into(), AnyValue::str("/api/orders"));
    attributes.insert("retryable".into(), AnyValue::bool(false));
    attributes.insert("elapsed_ms".into(), AnyValue::double(12.75));

    LogRecord {
        record_id: String::new(),
        event_time: Some(UnixNanos(1_700_000_000_123_000_000)),
        observed_time: UnixNanos(observed),
        original_timestamp_text: Some("2023-11-14T22:13:20.123Z".into()),
        timezone_assumption: Some(TimezoneAssumption::OffsetInText),
        severity_text: Some("ERROR".into()),
        severity_number: Some(17),
        body: Some(AnyValue::str("order failed")),
        display_message: "order failed".into(),
        event_name: None,
        trace_id: Some(TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap()),
        span_id: Some(SpanId::from_hex("b7ad6b7169203331").unwrap()),
        trace_flags: Some(1),
        resource_id: "res-0123".into(),
        scope_id: "scp-4567".into(),
        operation: Some("POST /api/orders".into()),
        outcome: Some("failure".into()),
        event_type: None,
        request_id: Some("req-9".into()),
        transaction_id: None,
        message_id: None,
        entity_id: Some("order-77".into()),
        attributes,
        dropped_attributes_count: 0,
        provenance: sample_provenance(dataset, ingest_time),
    }
    .seal()
}

#[test]
fn log_record_id_is_deterministic_and_ignores_instance_fields() {
    let a = sample_log("ds-1", 111, 999);
    // Different dataset, different ingest time, different observed time:
    // identical content must hash identically (re-import determinism).
    let b = sample_log("ds-2", 222, 555);
    assert_eq!(a.record_id, b.record_id);
    assert!(a.record_id.starts_with("log-"));

    // Any canonical content change must change the hash.
    let mut c = sample_log("ds-1", 111, 999);
    c.severity_number = Some(13);
    let c = c.seal();
    assert_ne!(a.record_id, c.record_id);

    // A change in locator (same content, different position) also changes it:
    // two identical lines at different offsets are distinct records.
    let mut d = sample_log("ds-1", 111, 999);
    d.provenance.locator.record_number = Some(8);
    let d = d.seal();
    assert_ne!(a.record_id, d.record_id);
}

#[test]
fn log_record_serde_round_trip_is_lossless() {
    let a = sample_log("ds-1", 111, 999);
    let json = serde_json::to_string(&a).unwrap();
    let back: LogRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
    assert_eq!(back.compute_record_id(), a.record_id);
}

#[test]
fn metric_record_hash_covers_all_point_content() {
    let point = |v: i64| NumberPoint {
        common: PointCommon {
            attributes: AttrMap::new(),
            start_time: Some(UnixNanos(1_000)),
            time: UnixNanos(2_000),
            flags: 0,
            exemplars: vec![Exemplar {
                filtered_attributes: AttrMap::new(),
                time: UnixNanos(1_500),
                value: NumberValue::Double(F64(0.5)),
                trace_id: TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").ok(),
                span_id: None,
            }],
            quality: vec![],
        },
        value: NumberValue::Int(v),
    };
    let make = |v: i64, temporality| {
        MetricRecord {
            record_id: String::new(),
            name: "http.server.requests".into(),
            description: Some("requests".into()),
            unit: Some("{request}".into()),
            data: MetricData::Sum {
                temporality,
                is_monotonic: true,
                points: vec![point(v)],
            },
            metadata: AttrMap::new(),
            resource_id: "res-1".into(),
            scope_id: "scp-1".into(),
            provenance: sample_provenance("ds-1", 1),
        }
        .seal()
    };
    let a = make(5, Temporality::Delta);
    let b = make(5, Temporality::Delta);
    let c = make(6, Temporality::Delta);
    let d = make(5, Temporality::Cumulative);
    assert_eq!(a.record_id, b.record_id);
    assert_ne!(a.record_id, c.record_id);
    assert_ne!(a.record_id, d.record_id, "temporality must be hashed");

    let json = serde_json::to_string(&a).unwrap();
    let back: MetricRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
}

#[test]
fn span_record_round_trip_and_duration() {
    let span = SpanRecord {
        record_id: String::new(),
        trace_id: TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap(),
        span_id: SpanId::from_hex("b7ad6b7169203331").unwrap(),
        parent_span_id: Some(SpanId::from_hex("00f067aa0ba902b7").unwrap()),
        trace_state: Some("congo=t61rcWkgMzE".into()),
        flags: Some(0x0000_0101),
        name: "GET /orders/{id}".into(),
        kind: SpanKind::Server,
        start_time: UnixNanos(1_000_000),
        end_time: Some(UnixNanos(3_500_000)),
        status: SpanStatus {
            code: StatusCode::Error,
            message: Some("upstream timeout".into()),
        },
        resource_id: "res-1".into(),
        scope_id: "scp-1".into(),
        attributes: AttrMap::new(),
        events: vec![SpanEvent {
            time: UnixNanos(2_000_000),
            name: "exception".into(),
            attributes: AttrMap::new(),
            dropped_attributes_count: 1,
        }],
        links: vec![SpanLink {
            trace_id: TraceId::from_hex("1af7651916cd43dd8448eb211c80319d").unwrap(),
            span_id: SpanId::from_hex("a7ad6b7169203330").ok(),
            trace_state: None,
            attributes: AttrMap::new(),
            dropped_attributes_count: 0,
            flags: None,
        }],
        dropped_attributes_count: 2,
        dropped_events_count: 0,
        dropped_links_count: 0,
        provenance: sample_provenance("ds-1", 1),
    }
    .seal();

    assert_eq!(span.duration_nanos(), Some(2_500_000));
    let json = serde_json::to_string(&span).unwrap();
    let back: SpanRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(span, back);
    assert_eq!(back.compute_record_id(), span.record_id);

    // A span without an end time has no duration and hashes differently.
    let mut open = span.clone();
    open.end_time = None;
    let open = open.seal();
    assert_eq!(open.duration_nanos(), None);
    assert_ne!(open.record_id, span.record_id);
}

/// Golden stability pin: if this hash changes, the canonical encoding
/// changed and MODEL_VERSION must be bumped (see docs/adr/0004).
#[test]
fn golden_record_id_pin() {
    let a = sample_log("ds-golden", 0, 0);
    let expected_prefix = "log-";
    assert!(a.record_id.starts_with(expected_prefix));
    // The concrete value is pinned after first execution; see
    // tests/golden/record_id.txt for the recorded value.
    let recorded = include_str!("golden/record_id.txt").trim();
    assert_eq!(
        a.record_id, recorded,
        "canonical encoding changed; bump MODEL_VERSION and regenerate golden"
    );
}
