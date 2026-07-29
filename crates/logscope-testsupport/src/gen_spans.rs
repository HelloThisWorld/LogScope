//! Deterministic OTLP JSONL span corpus generator: realistic-shaped traces
//! including links, occasional orphans, duplicates, and missing ends.

use std::io::Write;

use logscope_otlp::opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use logscope_otlp::opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, KeyValue};
use logscope_otlp::opentelemetry_proto::tonic::resource::v1::Resource;
use logscope_otlp::opentelemetry_proto::tonic::trace::v1 as pbt;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct SpanCorpusShape {
    pub envelopes: u64,
    pub spans: u64,
    pub traces: u64,
    pub bytes: u64,
    pub seed: u64,
    pub orphan_spans: u64,
    pub duplicate_spans: u64,
    pub incomplete_spans: u64,
}

fn kv(key: &str, value: any_value::Value) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue { value: Some(value) }),
        ..Default::default()
    }
}

fn id16(rng: &mut ChaCha8Rng) -> Vec<u8> {
    let mut v = vec![0u8; 16];
    rng.fill(&mut v[..]);
    if v.iter().all(|b| *b == 0) {
        v[0] = 1;
    }
    v
}
fn id8(rng: &mut ChaCha8Rng) -> Vec<u8> {
    let mut v = vec![0u8; 8];
    rng.fill(&mut v[..]);
    if v.iter().all(|b| *b == 0) {
        v[0] = 1;
    }
    v
}

/// Writes traces of ~8 spans each until `total_spans` is reached; each
/// envelope (line) carries the spans of ~12 traces.
pub fn write_spans_otlp_jsonl(
    mut w: impl Write,
    total_spans: u64,
    seed: u64,
) -> std::io::Result<SpanCorpusShape> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let base: u64 = 1_717_236_000_000_000_000;
    let mut shape = SpanCorpusShape {
        envelopes: 0,
        spans: 0,
        traces: 0,
        bytes: 0,
        seed,
        orphan_spans: 0,
        duplicate_spans: 0,
        incomplete_spans: 0,
    };

    let mut prev_trace: Option<(Vec<u8>, Vec<u8>)> = None;
    while shape.spans < total_spans {
        let mut spans: Vec<pbt::Span> = Vec::with_capacity(128);
        for _ in 0..12 {
            if shape.spans + (spans.len() as u64) >= total_spans {
                break;
            }
            let trace_id = id16(&mut rng);
            let root_id = id8(&mut rng);
            let t0 = base + shape.traces * 5_000_000;
            let child_count = rng.random_range(4..10usize);
            let root = pbt::Span {
                trace_id: trace_id.clone(),
                span_id: root_id.clone(),
                name: "handle-request".into(),
                kind: pbt::span::SpanKind::Server as i32,
                start_time_unix_nano: t0,
                end_time_unix_nano: t0 + 4_000_000,
                attributes: vec![kv(
                    "http.route",
                    any_value::Value::StringValue("/api/work".into()),
                )],
                // Occasionally link the previous trace (messaging pattern).
                links: match (&prev_trace, rng.random_bool(0.3)) {
                    (Some((pt, ps)), true) => vec![pbt::span::Link {
                        trace_id: pt.clone(),
                        span_id: ps.clone(),
                        ..Default::default()
                    }],
                    _ => vec![],
                },
                ..Default::default()
            };
            spans.push(root);
            let mut trace_spans = 1u64;
            for c in 0..child_count {
                let mut child = pbt::Span {
                    trace_id: trace_id.clone(),
                    span_id: id8(&mut rng),
                    parent_span_id: root_id.clone(),
                    name: format!("step-{c}"),
                    kind: pbt::span::SpanKind::Internal as i32,
                    start_time_unix_nano: t0 + 100_000 + c as u64 * 300_000,
                    end_time_unix_nano: t0 + 200_000 + c as u64 * 300_000,
                    ..Default::default()
                };
                // Deterministic defect sprinkles.
                if rng.random_bool(0.02) {
                    child.parent_span_id = id8(&mut rng); // orphan
                    shape.orphan_spans += 1;
                }
                if rng.random_bool(0.01) {
                    child.end_time_unix_nano = 0; // incomplete
                    shape.incomplete_spans += 1;
                }
                if rng.random_bool(0.01) {
                    spans.push(child.clone()); // duplicate
                    shape.duplicate_spans += 1;
                    trace_spans += 1;
                }
                spans.push(child);
                trace_spans += 1;
            }
            prev_trace = Some((trace_id, root_id));
            shape.traces += 1;
            shape.spans += trace_spans;
        }

        let req = ExportTraceServiceRequest {
            resource_spans: vec![pbt::ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![kv(
                        "service.name",
                        any_value::Value::StringValue("spans-bench-svc".into()),
                    )],
                    ..Default::default()
                }),
                scope_spans: vec![pbt::ScopeSpans {
                    scope: None,
                    spans,
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        };
        let line = serde_json::to_string(&req).expect("serialize envelope");
        w.write_all(line.as_bytes())?;
        w.write_all(b"\n")?;
        shape.bytes += line.len() as u64 + 1;
        shape.envelopes += 1;
    }
    Ok(shape)
}
