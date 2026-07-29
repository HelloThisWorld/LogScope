//! Golden OTLP equivalence proof (v0.0 gate):
//! semantically identical telemetry through OTLP/gRPC, OTLP/HTTP protobuf,
//! OTLP/HTTP JSON, and OTLP JSONL file import produces equivalent canonical
//! values. Transport-specific provenance differs and is asserted separately.

use std::io::Write as _;

use logscope_model::{IngestProvenance, PhysicalOrigin, RecordLocator, SourceProtocol, UnixNanos};
use logscope_otlp::*;
use opentelemetry_proto::tonic::collector::logs::v1::logs_service_client::LogsServiceClient;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_client::MetricsServiceClient;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::{
    any_value, AnyValue, ArrayValue, InstrumentationScope, KeyValue, KeyValueList,
};
use opentelemetry_proto::tonic::metrics::v1 as pbm;
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1 as pbt;
use prost::Message;

const SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.30.0";

fn kv(key: &str, value: any_value::Value) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue { value: Some(value) }),
        ..Default::default()
    }
}
fn s(v: &str) -> any_value::Value {
    any_value::Value::StringValue(v.to_string())
}
fn i(v: i64) -> any_value::Value {
    any_value::Value::IntValue(v)
}
fn d(v: f64) -> any_value::Value {
    any_value::Value::DoubleValue(v)
}

fn sample_resource() -> Resource {
    Resource {
        attributes: vec![
            kv("service.name", s("checkout-svc")),
            kv("service.namespace", s("shop")),
            kv("deployment.environment.name", s("staging")),
            kv("host.name", s("node-a")),
            kv("custom.unknown.attribute", s("survives")),
        ],
        dropped_attributes_count: 1,
        ..Default::default()
    }
}

fn sample_scope() -> InstrumentationScope {
    InstrumentationScope {
        name: "io.example.emitter".into(),
        version: "2.1.0".into(),
        attributes: vec![kv("scope.custom", i(7))],
        dropped_attributes_count: 0,
    }
}

const TRACE_ID: [u8; 16] = [
    0x0a, 0xf7, 0x65, 0x19, 0x16, 0xcd, 0x43, 0xdd, 0x84, 0x48, 0xeb, 0x21, 0x1c, 0x80, 0x31, 0x9c,
];
const SPAN_ID: [u8; 8] = [0xb7, 0xad, 0x6b, 0x71, 0x69, 0x20, 0x33, 0x31];
const PARENT_ID: [u8; 8] = [0x00, 0xf0, 0x67, 0xaa, 0x0b, 0xa9, 0x02, 0xb7];

pub fn sample_logs() -> ExportLogsServiceRequest {
    use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(sample_resource()),
            scope_logs: vec![ScopeLogs {
                scope: Some(sample_scope()),
                log_records: vec![
                    LogRecord {
                        time_unix_nano: 1_717_000_000_123_456_789,
                        observed_time_unix_nano: 1_717_000_001_000_000_000,
                        severity_number: 17,
                        severity_text: "ERROR".into(),
                        body: Some(AnyValue {
                            value: Some(any_value::Value::KvlistValue(KeyValueList {
                                values: vec![
                                    kv("message", s("payment declined")),
                                    kv("attempt", i(3)),
                                    kv(
                                        "amounts",
                                        any_value::Value::ArrayValue(ArrayValue {
                                            values: vec![
                                                AnyValue {
                                                    value: Some(d(19.99)),
                                                },
                                                AnyValue { value: Some(i(42)) },
                                            ],
                                        }),
                                    ),
                                ],
                            })),
                        }),
                        attributes: vec![
                            kv("http.status_code", i(402)),
                            kv(
                                "payload.bytes",
                                any_value::Value::BytesValue(vec![1, 2, 255]),
                            ),
                            kv("unicode", s("größe-\u{1F50D}")),
                        ],
                        dropped_attributes_count: 2,
                        flags: 1,
                        trace_id: TRACE_ID.to_vec(),
                        span_id: SPAN_ID.to_vec(),
                        event_name: "payment.declined".into(),
                    },
                    // Minimal record: no event time, no severity.
                    LogRecord {
                        observed_time_unix_nano: 1_717_000_002_000_000_000,
                        body: Some(AnyValue {
                            value: Some(s("plain line")),
                        }),
                        ..Default::default()
                    },
                ],
                schema_url: SCHEMA_URL.into(),
            }],
            schema_url: SCHEMA_URL.into(),
        }],
    }
}

pub fn sample_metrics() -> ExportMetricsServiceRequest {
    use pbm::{metric, Metric, ResourceMetrics, ScopeMetrics};
    let exemplar = pbm::Exemplar {
        filtered_attributes: vec![kv("user.tier", s("gold"))],
        time_unix_nano: 1_717_000_000_500_000_000,
        value: Some(pbm::exemplar::Value::AsDouble(0.25)),
        span_id: SPAN_ID.to_vec(),
        trace_id: TRACE_ID.to_vec(),
    };
    let number_point = |t: u64, v: pbm::number_data_point::Value| pbm::NumberDataPoint {
        attributes: vec![kv("shard", s("s1"))],
        start_time_unix_nano: 1_717_000_000_000_000_000,
        time_unix_nano: t,
        value: Some(v),
        exemplars: vec![exemplar.clone()],
        flags: 0,
    };
    ExportMetricsServiceRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: Some(sample_resource()),
            scope_metrics: vec![ScopeMetrics {
                scope: Some(sample_scope()),
                metrics: vec![
                    Metric {
                        name: "queue.depth".into(),
                        description: "queued items".into(),
                        unit: "{item}".into(),
                        metadata: vec![],
                        data: Some(metric::Data::Gauge(pbm::Gauge {
                            data_points: vec![number_point(
                                1_717_000_001_000_000_000,
                                pbm::number_data_point::Value::AsInt(41),
                            )],
                        })),
                    },
                    Metric {
                        name: "http.requests".into(),
                        description: String::new(),
                        unit: "{request}".into(),
                        metadata: vec![kv("origin", s("synthetic"))],
                        data: Some(metric::Data::Sum(pbm::Sum {
                            data_points: vec![number_point(
                                1_717_000_002_000_000_000,
                                pbm::number_data_point::Value::AsDouble(12.5),
                            )],
                            aggregation_temporality: pbm::AggregationTemporality::Delta as i32,
                            is_monotonic: true,
                        })),
                    },
                    Metric {
                        name: "latency".into(),
                        description: String::new(),
                        unit: "ms".into(),
                        metadata: vec![],
                        data: Some(metric::Data::Histogram(pbm::Histogram {
                            data_points: vec![pbm::HistogramDataPoint {
                                attributes: vec![kv("route", s("/pay"))],
                                start_time_unix_nano: 1_717_000_000_000_000_000,
                                time_unix_nano: 1_717_000_003_000_000_000,
                                count: 7,
                                sum: Some(123.5),
                                bucket_counts: vec![1, 2, 3, 1],
                                explicit_bounds: vec![5.0, 50.0, 500.0],
                                exemplars: vec![exemplar.clone()],
                                flags: 0,
                                min: Some(0.7),
                                max: Some(432.1),
                            }],
                            aggregation_temporality: pbm::AggregationTemporality::Cumulative as i32,
                        })),
                    },
                    Metric {
                        name: "latency.exp".into(),
                        description: String::new(),
                        unit: "ms".into(),
                        metadata: vec![],
                        data: Some(metric::Data::ExponentialHistogram(
                            pbm::ExponentialHistogram {
                                data_points: vec![pbm::ExponentialHistogramDataPoint {
                                    attributes: vec![],
                                    start_time_unix_nano: 1_717_000_000_000_000_000,
                                    time_unix_nano: 1_717_000_004_000_000_000,
                                    count: 10,
                                    sum: Some(55.5),
                                    scale: 2,
                                    zero_count: 1,
                                    positive: Some(
                                        pbm::exponential_histogram_data_point::Buckets {
                                            offset: -3,
                                            bucket_counts: vec![4, 3, 2],
                                        },
                                    ),
                                    negative: Some(
                                        pbm::exponential_histogram_data_point::Buckets {
                                            offset: 0,
                                            bucket_counts: vec![],
                                        },
                                    ),
                                    flags: 0,
                                    exemplars: vec![],
                                    min: Some(0.1),
                                    max: Some(20.0),
                                    zero_threshold: 1e-6,
                                }],
                                aggregation_temporality: pbm::AggregationTemporality::Delta as i32,
                            },
                        )),
                    },
                    Metric {
                        name: "gc.pause".into(),
                        description: String::new(),
                        unit: "s".into(),
                        metadata: vec![],
                        data: Some(metric::Data::Summary(pbm::Summary {
                            data_points: vec![pbm::SummaryDataPoint {
                                attributes: vec![],
                                start_time_unix_nano: 1_717_000_000_000_000_000,
                                time_unix_nano: 1_717_000_005_000_000_000,
                                count: 100,
                                sum: 3.5,
                                quantile_values: vec![
                                    pbm::summary_data_point::ValueAtQuantile {
                                        quantile: 0.5,
                                        value: 0.02,
                                    },
                                    pbm::summary_data_point::ValueAtQuantile {
                                        quantile: 0.99,
                                        value: 0.2,
                                    },
                                ],
                                flags: 0,
                            }],
                        })),
                    },
                    // Sum with unspecified temporality: preserved + flagged.
                    Metric {
                        name: "odd.sum".into(),
                        description: String::new(),
                        unit: String::new(),
                        metadata: vec![],
                        data: Some(metric::Data::Sum(pbm::Sum {
                            data_points: vec![number_point(
                                1_717_000_006_000_000_000,
                                pbm::number_data_point::Value::AsInt(1),
                            )],
                            aggregation_temporality: 0,
                            is_monotonic: false,
                        })),
                    },
                ],
                schema_url: SCHEMA_URL.into(),
            }],
            schema_url: SCHEMA_URL.into(),
        }],
    }
}

pub fn sample_traces() -> ExportTraceServiceRequest {
    use pbt::{span, ResourceSpans, ScopeSpans, Span, Status};
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(sample_resource()),
            scope_spans: vec![ScopeSpans {
                scope: Some(sample_scope()),
                spans: vec![
                    Span {
                        trace_id: TRACE_ID.to_vec(),
                        span_id: SPAN_ID.to_vec(),
                        trace_state: "congo=t61rcWkgMzE".into(),
                        parent_span_id: vec![],
                        flags: 0x0101,
                        name: "POST /pay".into(),
                        kind: span::SpanKind::Server as i32,
                        start_time_unix_nano: 1_717_000_000_000_000_000,
                        end_time_unix_nano: 1_717_000_000_900_000_000,
                        attributes: vec![kv("http.method", s("POST"))],
                        dropped_attributes_count: 0,
                        events: vec![span::Event {
                            time_unix_nano: 1_717_000_000_400_000_000,
                            name: "exception".into(),
                            attributes: vec![kv("exception.type", s("TimeoutError"))],
                            dropped_attributes_count: 1,
                        }],
                        dropped_events_count: 0,
                        links: vec![span::Link {
                            trace_id: TRACE_ID.to_vec(),
                            span_id: PARENT_ID.to_vec(),
                            trace_state: String::new(),
                            attributes: vec![kv("link.kind", s("follows-from"))],
                            dropped_attributes_count: 0,
                            flags: 1,
                        }],
                        dropped_links_count: 2,
                        status: Some(Status {
                            message: "upstream timeout".into(),
                            code: pbt::status::StatusCode::Error as i32,
                        }),
                    },
                    // Incomplete span: no end time.
                    Span {
                        trace_id: TRACE_ID.to_vec(),
                        span_id: PARENT_ID.to_vec(),
                        parent_span_id: SPAN_ID.to_vec(),
                        name: "charge".into(),
                        kind: span::SpanKind::Client as i32,
                        start_time_unix_nano: 1_717_000_000_100_000_000,
                        end_time_unix_nano: 0,
                        ..Default::default()
                    },
                ],
                schema_url: SCHEMA_URL.into(),
            }],
            schema_url: SCHEMA_URL.into(),
        }],
    }
}

// ---------------------------------------------------------------------------
// Canonical comparison helpers
// ---------------------------------------------------------------------------

fn neutral_provenance() -> IngestProvenance {
    IngestProvenance {
        dataset_id: "ds".into(),
        logical_source_id: "src".into(),
        origin: PhysicalOrigin::OtlpSession {
            session_id: "x".into(),
        },
        locator: RecordLocator::default(),
        parser_id: "otlp".into(),
        parser_version: "0".into(),
        profile_id: None,
        profile_version: None,
        normalizer_version: "0".into(),
        protocol: SourceProtocol::OtlpGrpc,
        content_type: None,
        ingest_time: UnixNanos(0),
        raw_hash: String::new(),
        original_timestamp_precision: None,
        flags: vec![],
    }
}

/// Strips transport-specific fields so canonical values can be compared.
fn canonical_view(batch: &ConvertedBatch) -> ConvertedBatch {
    let mut out = batch.clone();
    for log in &mut out.logs {
        log.provenance = neutral_provenance();
        log.record_id = String::new();
    }
    for metric in &mut out.metrics {
        metric.provenance = neutral_provenance();
        metric.record_id = String::new();
    }
    for span in &mut out.spans {
        span.provenance = neutral_provenance();
        span.record_id = String::new();
    }
    out.rejects.clear();
    out
}

fn ctx(protocol: SourceProtocol, hash: &str, batch_index: u64) -> ConvertContext {
    ConvertContext {
        dataset_id: "ds-eq".into(),
        logical_source_id: "src-eq".into(),
        origin: PhysicalOrigin::OtlpSession {
            session_id: "session-eq".into(),
        },
        protocol,
        content_type: None,
        // Fixed ingest time so observed-time defaulting is identical.
        ingest_time: UnixNanos(1_717_000_010_000_000_000),
        batch_index,
        envelope_hash: hash.to_string(),
        extra_flags: vec![],
    }
}

fn convert_payload(
    payload: &EnvelopePayload,
    meta_protocol: SourceProtocol,
    hash: &str,
) -> ConvertedBatch {
    match payload {
        EnvelopePayload::Logs(req) => convert_logs(req, &ctx(meta_protocol, hash, 0)),
        EnvelopePayload::Metrics(req) => convert_metrics(req, &ctx(meta_protocol, hash, 0)),
        EnvelopePayload::Traces(req) => convert_traces(req, &ctx(meta_protocol, hash, 0)),
    }
}

// ---------------------------------------------------------------------------
// The equivalence test
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn all_four_transports_yield_equivalent_canonical_values() {
    let config = OtlpReceiverConfig {
        http_port: Some(0),
        grpc_port: Some(0),
        ..Default::default()
    };
    let mut handle = start(config).await.unwrap();
    let http = handle.http_addr.unwrap();
    let grpc = handle.grpc_addr.unwrap();
    assert!(http.ip().is_loopback() && grpc.ip().is_loopback());

    let logs = sample_logs();
    let metrics = sample_metrics();
    let traces = sample_traces();

    // Path 1: gRPC.
    let endpoint = format!("http://{grpc}");
    LogsServiceClient::connect(endpoint.clone())
        .await
        .unwrap()
        .export(logs.clone())
        .await
        .unwrap();
    MetricsServiceClient::connect(endpoint.clone())
        .await
        .unwrap()
        .export(metrics.clone())
        .await
        .unwrap();
    TraceServiceClient::connect(endpoint)
        .await
        .unwrap()
        .export(traces.clone())
        .await
        .unwrap();

    // Path 2 + 3: HTTP protobuf and HTTP JSON.
    let client = reqwest::Client::new();
    for (path, pb_body, json_body) in [
        (
            "v1/logs",
            logs.encode_to_vec(),
            serde_json::to_vec(&logs).unwrap(),
        ),
        (
            "v1/metrics",
            metrics.encode_to_vec(),
            serde_json::to_vec(&metrics).unwrap(),
        ),
        (
            "v1/traces",
            traces.encode_to_vec(),
            serde_json::to_vec(&traces).unwrap(),
        ),
    ] {
        let r = client
            .post(format!("http://{http}/{path}"))
            .header("content-type", "application/x-protobuf")
            .body(pb_body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = client
            .post(format!("http://{http}/{path}"))
            .header("content-type", "application/json")
            .body(json_body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }

    // Collect the nine receiver envelopes (3 gRPC + 3 pb + 3 json).
    let mut received: Vec<ReceivedEnvelope> = Vec::new();
    for _ in 0..9 {
        received.push(handle.envelopes.recv().await.expect("envelope"));
    }

    // Path 4: OTLP JSONL file (one envelope per line, plus an unknown
    // future field that must be tolerated).
    let dir = tempfile::tempdir().unwrap();
    let jsonl_path = dir.path().join("export.jsonl");
    {
        let mut f = std::fs::File::create(&jsonl_path).unwrap();
        let mut logs_line = serde_json::to_value(&logs).unwrap();
        logs_line
            .as_object_mut()
            .unwrap()
            .insert("futureUnknownField".into(), serde_json::json!({"x": 1}));
        writeln!(f, "{}", serde_json::to_string(&logs_line).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&metrics).unwrap()).unwrap();
        writeln!(f, "{}", serde_json::to_string(&traces).unwrap()).unwrap();
    }
    let jsonl = read_otlp_jsonl(std::fs::File::open(&jsonl_path).unwrap()).unwrap();
    assert_eq!(jsonl.rejects.len(), 0, "{:?}", jsonl.rejects);
    assert_eq!(jsonl.envelopes.len(), 3);

    // Convert every path to canonical batches.
    let mut by_path: std::collections::BTreeMap<String, Vec<ConvertedBatch>> = Default::default();
    for env in &received {
        let key = format!("{:?}", env.meta.protocol);
        by_path.entry(key).or_default().push(convert_payload(
            &env.payload,
            env.meta.protocol,
            &env.meta.raw_hash,
        ));
    }
    for env in &jsonl.envelopes {
        by_path
            .entry("OtlpJsonlFile".to_string())
            .or_default()
            .push(convert_payload(
                &env.payload,
                env.meta.protocol,
                &env.meta.raw_hash,
            ));
    }
    assert_eq!(by_path.len(), 4, "four transports: {:?}", by_path.keys());

    // Merge each path's batches into one canonical view triple and compare
    // against the gRPC reference.
    let reference: Vec<ConvertedBatch> = by_path
        .remove("OtlpGrpc")
        .unwrap()
        .iter()
        .map(canonical_view)
        .collect();
    let ref_logs: Vec<_> = reference.iter().flat_map(|b| b.logs.clone()).collect();
    let ref_metrics: Vec<_> = reference.iter().flat_map(|b| b.metrics.clone()).collect();
    let ref_spans: Vec<_> = reference.iter().flat_map(|b| b.spans.clone()).collect();
    assert_eq!(ref_logs.len(), 2);
    assert_eq!(ref_metrics.len(), 6);
    assert_eq!(ref_spans.len(), 2);

    for (path, batches) in by_path {
        let views: Vec<ConvertedBatch> = batches.iter().map(canonical_view).collect();
        let logs: Vec<_> = views.iter().flat_map(|b| b.logs.clone()).collect();
        let metrics: Vec<_> = views.iter().flat_map(|b| b.metrics.clone()).collect();
        let spans: Vec<_> = views.iter().flat_map(|b| b.spans.clone()).collect();
        assert_eq!(logs, ref_logs, "log equivalence failed for {path}");
        assert_eq!(metrics, ref_metrics, "metric equivalence failed for {path}");
        assert_eq!(spans, ref_spans, "span equivalence failed for {path}");
    }

    // Transport-specific provenance differs and is asserted separately.
    let grpc_batch = convert_payload(
        &received
            .iter()
            .find(|e| e.meta.protocol == SourceProtocol::OtlpGrpc)
            .unwrap()
            .payload,
        SourceProtocol::OtlpGrpc,
        "hash-grpc",
    );
    let jsonl_batch = convert_payload(
        &jsonl.envelopes[0].payload,
        SourceProtocol::OtlpJsonlFile,
        "hash-file",
    );
    if let (Some(a), Some(b)) = (grpc_batch.logs.first(), jsonl_batch.logs.first()) {
        assert_eq!(a.provenance.protocol, SourceProtocol::OtlpGrpc);
        assert_eq!(b.provenance.protocol, SourceProtocol::OtlpJsonlFile);
        assert_ne!(a.provenance.raw_hash, b.provenance.raw_hash);
        assert_ne!(a.record_id, b.record_id, "record ids embed provenance");
        assert!(a.provenance.locator.otlp.is_some());
        assert!(b.provenance.locator.otlp.is_some());
    } else {
        panic!("expected log records on both paths");
    }

    // Canonical facts survived (spot checks on the reference path).
    let rich = &ref_logs[0];
    assert_eq!(rich.severity_number, Some(17));
    assert_eq!(rich.event_name.as_deref(), Some("payment.declined"));
    assert!(rich.attributes.contains_key("payload.bytes"));
    assert!(rich.attributes.contains_key("unicode"));
    let unknown_metric_free: Vec<_> = ref_metrics.iter().filter(|m| m.name == "odd.sum").collect();
    assert_eq!(unknown_metric_free.len(), 1);
    assert!(matches!(
        unknown_metric_free[0].data,
        logscope_model::MetricData::Sum {
            temporality: logscope_model::Temporality::Unspecified,
            ..
        }
    ));
    let incomplete_span = ref_spans.iter().find(|s| s.name == "charge").unwrap();
    assert!(incomplete_span.end_time.is_none());

    handle.shutdown().await;
}

#[test]
fn unknown_metric_type_is_rejected_not_coerced() {
    let req = ExportMetricsServiceRequest {
        resource_metrics: vec![pbm::ResourceMetrics {
            resource: Some(sample_resource()),
            scope_metrics: vec![pbm::ScopeMetrics {
                scope: Some(sample_scope()),
                metrics: vec![pbm::Metric {
                    name: "mystery".into(),
                    description: String::new(),
                    unit: String::new(),
                    metadata: vec![],
                    data: None,
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    };
    let batch = convert_metrics(&req, &ctx(SourceProtocol::OtlpGrpc, "h", 0));
    assert!(batch.metrics.is_empty());
    assert_eq!(batch.rejects.len(), 1);
    assert_eq!(batch.rejects[0].reason_code, "otlp/unknown-metric-type");
    assert!(batch.rejects[0].locator.otlp.is_some());
}

#[test]
fn unknown_attributes_and_schema_urls_survive() {
    let batch = convert_logs(&sample_logs(), &ctx(SourceProtocol::OtlpHttpJson, "h", 0));
    let resource = &batch.resources[0];
    assert_eq!(resource.schema_url.as_deref(), Some(SCHEMA_URL));
    assert!(resource.attributes.contains_key("custom.unknown.attribute"));
    assert_eq!(
        resource.derived.service_name.as_deref(),
        Some("checkout-svc")
    );
    assert_eq!(
        resource.derived.deployment_environment.as_deref(),
        Some("staging")
    );
    let scope = &batch.scopes[0];
    assert_eq!(scope.schema_url.as_deref(), Some(SCHEMA_URL));
    assert!(scope.attributes.contains_key("scope.custom"));
    assert_eq!(resource.dropped_attributes_count, 1);
}
