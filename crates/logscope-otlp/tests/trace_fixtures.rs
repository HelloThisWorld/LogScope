//! OTLP JSONL trace fixture tests: messaging links and defective spans
//! survive conversion without fabrication.

use logscope_model::{PhysicalOrigin, QualityFlag, SourceProtocol, UnixNanos};
use logscope_otlp::*;

fn fixture(rel: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(rel)
}

fn ctx(hash: &str, batch: u64) -> ConvertContext {
    ConvertContext {
        dataset_id: "ds-fix".into(),
        logical_source_id: "src-fix".into(),
        origin: PhysicalOrigin::File {
            file_id: "file-fix".into(),
            archive_entry: None,
        },
        protocol: SourceProtocol::OtlpJsonlFile,
        content_type: Some("application/x-ndjson".into()),
        ingest_time: UnixNanos(1_700_000_000_000_000_000),
        batch_index: batch,
        envelope_hash: hash.into(),
        extra_flags: vec![],
    }
}

fn convert_fixture(rel: &str) -> Vec<ConvertedBatch> {
    let file = std::fs::File::open(fixture(rel)).unwrap();
    let result = read_otlp_jsonl(file).unwrap();
    assert!(result.rejects.is_empty(), "{:?}", result.rejects);
    result
        .envelopes
        .iter()
        .enumerate()
        .map(|(i, e)| match &e.payload {
            EnvelopePayload::Traces(req) => convert_traces(req, &ctx(&e.meta.raw_hash, i as u64)),
            other => panic!("expected traces, got {other:?}"),
        })
        .collect()
}

#[test]
fn kafka_producer_consumer_links_survive() {
    let batches = convert_fixture("traces/kafka-spans-links.jsonl");
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert!(batch.rejects.is_empty());
    assert_eq!(batch.spans.len(), 2);

    let consumer = batch
        .spans
        .iter()
        .find(|s| s.name == "orders process")
        .unwrap();
    assert_eq!(consumer.kind, logscope_model::SpanKind::Consumer);
    assert_eq!(consumer.links.len(), 1);
    let link = &consumer.links[0];
    assert_eq!(link.trace_id.as_str(), "aaaa1111bbbb2222cccc3333dddd4444");
    assert_eq!(
        link.span_id.as_ref().map(|s| s.as_str()),
        Some("1111222233334444")
    );
    assert!(link.attributes.contains_key("messaging.operation.type"));
    // Two resources (producer + consumer services) with derived names.
    let names: Vec<_> = batch
        .resources
        .iter()
        .filter_map(|r| r.derived.service_name.clone())
        .collect();
    assert!(names.contains(&"order-producer-svc".to_string()));
    assert!(names.contains(&"order-consumer-svc".to_string()));
}

#[test]
fn problem_spans_convert_without_fabrication() {
    let batches = convert_fixture("traces/problem-spans.jsonl");
    let batch = &batches[0];
    assert!(batch.rejects.is_empty());
    assert_eq!(batch.spans.len(), 6);

    // Incomplete span keeps a missing end + flag, no invented duration.
    let incomplete = batch.spans.iter().find(|s| s.name == "incomplete").unwrap();
    assert!(incomplete.end_time.is_none());
    assert!(incomplete
        .provenance
        .flags
        .contains(&QualityFlag::SpanMissingEndTime));
    assert_eq!(incomplete.duration_nanos(), None);

    // Orphan parent reference is preserved exactly as sent.
    let orphan = batch.spans.iter().find(|s| s.name == "orphan-child").unwrap();
    assert_eq!(
        orphan.parent_span_id.as_ref().map(|s| s.as_str()),
        Some("9999999900000000")
    );

    // Duplicate span IDs both survive.
    let dupes = batch
        .spans
        .iter()
        .filter(|s| s.span_id.as_str() == "bbbbbbbb22222222")
        .count();
    assert_eq!(dupes, 2);

    // Sampled flag survives on the root (flags 257 = 0x101).
    let root = batch.spans.iter().find(|s| s.name == "root").unwrap();
    assert_eq!(root.flags, Some(257));
}
