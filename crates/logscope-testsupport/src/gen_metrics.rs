//! Deterministic OTLP JSONL metric corpus generator with controlled
//! cardinality (fixed seed -> identical corpus).

use std::io::Write;

use logscope_otlp::opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use logscope_otlp::opentelemetry_proto::tonic::common::v1::{any_value, AnyValue, KeyValue};
use logscope_otlp::opentelemetry_proto::tonic::metrics::v1 as pbm;
use logscope_otlp::opentelemetry_proto::tonic::resource::v1::Resource;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct MetricCorpusShape {
    pub envelopes: u64,
    pub points: u64,
    pub bytes: u64,
    pub seed: u64,
    pub series_cardinality: u32,
}

fn kv(key: &str, value: any_value::Value) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue { value: Some(value) }),
        ..Default::default()
    }
}

/// Writes OTLP/JSON envelopes (one per line) totalling `total_points`
/// gauge+delta-sum points across `cardinality` distinct series.
pub fn write_metrics_otlp_jsonl(
    mut w: impl Write,
    total_points: u64,
    cardinality: u32,
    seed: u64,
) -> std::io::Result<MetricCorpusShape> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let points_per_envelope: u64 = 2_000;
    let base: u64 = 1_717_236_000_000_000_000;
    let mut shape = MetricCorpusShape {
        envelopes: 0,
        points: 0,
        bytes: 0,
        seed,
        series_cardinality: cardinality.max(1),
    };

    let mut written = 0u64;
    while written < total_points {
        let in_this = points_per_envelope.min(total_points - written);
        let half = in_this / 2;
        let mut gauge_points = Vec::with_capacity(half as usize);
        let mut sum_points = Vec::with_capacity((in_this - half) as usize);
        for k in 0..in_this {
            let idx = written + k;
            let series = (idx % shape.series_cardinality as u64) as u32;
            let point = pbm::NumberDataPoint {
                attributes: vec![
                    kv(
                        "pod",
                        any_value::Value::StringValue(format!("pod-{series:05}")),
                    ),
                    kv("shard", any_value::Value::IntValue((series % 16) as i64)),
                ],
                start_time_unix_nano: base,
                time_unix_nano: base + idx * 1_000_000,
                value: Some(if k % 2 == 0 {
                    pbm::number_data_point::Value::AsInt(rng.random_range(0..10_000))
                } else {
                    pbm::number_data_point::Value::AsDouble(rng.random_range(0.0..100.0))
                }),
                exemplars: vec![],
                flags: 0,
            };
            if k < half {
                gauge_points.push(point);
            } else {
                sum_points.push(point);
            }
        }
        let req = ExportMetricsServiceRequest {
            resource_metrics: vec![pbm::ResourceMetrics {
                resource: Some(Resource {
                    attributes: vec![kv(
                        "service.name",
                        any_value::Value::StringValue("metrics-bench-svc".into()),
                    )],
                    ..Default::default()
                }),
                scope_metrics: vec![pbm::ScopeMetrics {
                    scope: None,
                    metrics: vec![
                        pbm::Metric {
                            name: "bench.queue.depth".into(),
                            description: String::new(),
                            unit: "{item}".into(),
                            metadata: vec![],
                            data: Some(pbm::metric::Data::Gauge(pbm::Gauge {
                                data_points: gauge_points,
                            })),
                        },
                        pbm::Metric {
                            name: "bench.requests".into(),
                            description: String::new(),
                            unit: "{request}".into(),
                            metadata: vec![],
                            data: Some(pbm::metric::Data::Sum(pbm::Sum {
                                data_points: sum_points,
                                aggregation_temporality: pbm::AggregationTemporality::Delta as i32,
                                is_monotonic: true,
                            })),
                        },
                    ],
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
        shape.points += in_this;
        written += in_this;
    }
    Ok(shape)
}
