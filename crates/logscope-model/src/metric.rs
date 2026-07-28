//! Canonical normalized metric records: lossless Gauge, Sum, Histogram,
//! Exponential Histogram, and Summary. Unknown metric types are never
//! silently converted; they are rejected at the adapter boundary with an
//! explicit reason.

use serde::{Deserialize, Serialize};

use crate::hashing::{stable_id, Digest};
use crate::provenance::{IngestProvenance, QualityFlag};
use crate::time::UnixNanos;
use crate::trace_ids::{SpanId, TraceId};
use crate::value::{digest_attrs, AttrMap, F64};
use crate::MODEL_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Temporality {
    /// Preserved as-is; flagged, never guessed.
    Unspecified,
    Delta,
    Cumulative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum NumberValue {
    Int(i64),
    Double(F64),
}

impl NumberValue {
    pub fn digest_into(&self, d: &mut Digest) {
        match self {
            NumberValue::Int(i) => {
                d.tag(1).i64(*i);
            }
            NumberValue::Double(F64(f)) => {
                d.tag(2).f64_bits(*f);
            }
        }
    }

    pub fn as_f64(&self) -> f64 {
        match self {
            NumberValue::Int(i) => *i as f64,
            NumberValue::Double(F64(f)) => *f,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exemplar {
    pub filtered_attributes: AttrMap,
    pub time: UnixNanos,
    pub value: NumberValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<TraceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<SpanId>,
}

impl Exemplar {
    fn digest_into(&self, d: &mut Digest) {
        digest_attrs(&self.filtered_attributes, d);
        d.i64(self.time.0);
        self.value.digest_into(d);
        d.opt_str(self.trace_id.as_ref().map(|t| t.as_str()));
        d.opt_str(self.span_id.as_ref().map(|s| s.as_str()));
    }
}

/// Common per-point fields shared by all point kinds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PointCommon {
    pub attributes: AttrMap,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<UnixNanos>,
    pub time: UnixNanos,
    /// OTLP DataPointFlags bit field (bit 0 = NO_RECORDED_VALUE).
    #[serde(default)]
    pub flags: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exemplars: Vec<Exemplar>,
    /// Reset / gap / duplicate / out-of-order metadata attached during
    /// ingest ordering analysis. Preserved, never used to rewrite values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quality: Vec<QualityFlag>,
}

impl PointCommon {
    fn digest_into(&self, d: &mut Digest) {
        digest_attrs(&self.attributes, d);
        d.opt_i64(self.start_time.map(|t| t.0));
        d.i64(self.time.0);
        d.u32(self.flags);
        d.u64(self.exemplars.len() as u64);
        for e in &self.exemplars {
            e.digest_into(d);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberPoint {
    #[serde(flatten)]
    pub common: PointCommon,
    pub value: NumberValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistogramPoint {
    #[serde(flatten)]
    pub common: PointCommon,
    pub count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum: Option<F64>,
    /// len == explicit_bounds.len() + 1
    pub bucket_counts: Vec<u64>,
    pub explicit_bounds: Vec<F64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<F64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<F64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ExpBuckets {
    pub offset: i32,
    pub bucket_counts: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExponentialHistogramPoint {
    #[serde(flatten)]
    pub common: PointCommon,
    pub count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum: Option<F64>,
    pub scale: i32,
    pub zero_count: u64,
    pub zero_threshold: F64,
    pub positive: ExpBuckets,
    pub negative: ExpBuckets,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<F64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<F64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuantileValue {
    pub quantile: F64,
    pub value: F64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryPoint {
    #[serde(flatten)]
    pub common: PointCommon,
    pub count: u64,
    pub sum: F64,
    pub quantile_values: Vec<QuantileValue>,
}

/// Metric payload by type. There is intentionally no `Unknown` variant:
/// unknown types must be rejected with provenance, never coerced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MetricData {
    Gauge {
        points: Vec<NumberPoint>,
    },
    Sum {
        temporality: Temporality,
        is_monotonic: bool,
        points: Vec<NumberPoint>,
    },
    Histogram {
        temporality: Temporality,
        points: Vec<HistogramPoint>,
    },
    ExponentialHistogram {
        temporality: Temporality,
        points: Vec<ExponentialHistogramPoint>,
    },
    Summary {
        points: Vec<SummaryPoint>,
    },
}

impl MetricData {
    pub fn type_name(&self) -> &'static str {
        match self {
            MetricData::Gauge { .. } => "gauge",
            MetricData::Sum { .. } => "sum",
            MetricData::Histogram { .. } => "histogram",
            MetricData::ExponentialHistogram { .. } => "exponential_histogram",
            MetricData::Summary { .. } => "summary",
        }
    }

    pub fn point_count(&self) -> usize {
        match self {
            MetricData::Gauge { points } => points.len(),
            MetricData::Sum { points, .. } => points.len(),
            MetricData::Histogram { points, .. } => points.len(),
            MetricData::ExponentialHistogram { points, .. } => points.len(),
            MetricData::Summary { points } => points.len(),
        }
    }

    fn digest_into(&self, d: &mut Digest) {
        fn opt_f64(d: &mut Digest, v: &Option<F64>) {
            match v {
                None => {
                    d.tag(0);
                }
                Some(F64(f)) => {
                    d.tag(1).f64_bits(*f);
                }
            }
        }
        fn temporality_tag(t: Temporality) -> u8 {
            match t {
                Temporality::Unspecified => 0,
                Temporality::Delta => 1,
                Temporality::Cumulative => 2,
            }
        }
        match self {
            MetricData::Gauge { points } => {
                d.tag(0x10).u64(points.len() as u64);
                for p in points {
                    p.common.digest_into(d);
                    p.value.digest_into(d);
                }
            }
            MetricData::Sum {
                temporality,
                is_monotonic,
                points,
            } => {
                d.tag(0x11)
                    .u8(temporality_tag(*temporality))
                    .bool(*is_monotonic)
                    .u64(points.len() as u64);
                for p in points {
                    p.common.digest_into(d);
                    p.value.digest_into(d);
                }
            }
            MetricData::Histogram {
                temporality,
                points,
            } => {
                d.tag(0x12)
                    .u8(temporality_tag(*temporality))
                    .u64(points.len() as u64);
                for p in points {
                    p.common.digest_into(d);
                    d.u64(p.count);
                    opt_f64(d, &p.sum);
                    d.u64(p.bucket_counts.len() as u64);
                    for c in &p.bucket_counts {
                        d.u64(*c);
                    }
                    d.u64(p.explicit_bounds.len() as u64);
                    for F64(b) in &p.explicit_bounds {
                        d.f64_bits(*b);
                    }
                    opt_f64(d, &p.min);
                    opt_f64(d, &p.max);
                }
            }
            MetricData::ExponentialHistogram {
                temporality,
                points,
            } => {
                d.tag(0x13)
                    .u8(temporality_tag(*temporality))
                    .u64(points.len() as u64);
                for p in points {
                    p.common.digest_into(d);
                    d.u64(p.count);
                    opt_f64(d, &p.sum);
                    d.i32(p.scale)
                        .u64(p.zero_count)
                        .f64_bits(p.zero_threshold.0);
                    for buckets in [&p.positive, &p.negative] {
                        d.i32(buckets.offset)
                            .u64(buckets.bucket_counts.len() as u64);
                        for c in &buckets.bucket_counts {
                            d.u64(*c);
                        }
                    }
                    opt_f64(d, &p.min);
                    opt_f64(d, &p.max);
                }
            }
            MetricData::Summary { points } => {
                d.tag(0x14).u64(points.len() as u64);
                for p in points {
                    p.common.digest_into(d);
                    d.u64(p.count).f64_bits(p.sum.0);
                    d.u64(p.quantile_values.len() as u64);
                    for q in &p.quantile_values {
                        d.f64_bits(q.quantile.0).f64_bits(q.value.0);
                    }
                }
            }
        }
    }
}

/// A normalized metric (one named metric with its points from one envelope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricRecord {
    /// `met-<32 hex>` deterministic content hash.
    pub record_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub data: MetricData,
    /// Metric-level metadata attributes (OTLP `Metric.metadata`).
    #[serde(default, skip_serializing_if = "AttrMap::is_empty")]
    pub metadata: AttrMap,
    pub resource_id: String,
    pub scope_id: String,
    pub provenance: IngestProvenance,
}

impl MetricRecord {
    pub fn compute_record_id(&self) -> String {
        stable_id("met", |d| {
            d.str("metric.v1");
            d.str(MODEL_VERSION);
            d.str(&self.name);
            d.opt_str(self.description.as_deref());
            d.opt_str(self.unit.as_deref());
            self.data.digest_into(d);
            digest_attrs(&self.metadata, d);
            d.str(&self.resource_id);
            d.str(&self.scope_id);
            self.provenance.digest_stable_into(d);
        })
    }

    pub fn seal(mut self) -> Self {
        self.record_id = self.compute_record_id();
        self
    }
}
