//! Converts OTLP protobuf types into canonical records.
//!
//! Lossless by construction: attributes, Resource, Instrumentation Scope,
//! schema URLs, trace flags, temporality, exemplars, span events, links, and
//! dropped counts all survive. Records this version cannot represent are
//! rejected individually with a reason — never silently coerced (an unknown
//! metric type is a reject, not a gauge).

use logscope_model::{
    AnyValue, AttrMap, Exemplar, ExpBuckets, ExponentialHistogramPoint, HistogramPoint,
    IngestProvenance, LogRecord, MetricData, MetricRecord, NumberPoint, NumberValue,
    OtlpBatchLocator, PhysicalOrigin, PointCommon, QualityFlag, QuantileValue, RecordLocator,
    ResourceDescriptor, ScopeDescriptor, SourceProtocol, SpanEvent, SpanId, SpanKind, SpanLink,
    SpanRecord, SpanStatus, StatusCode, SummaryPoint, Temporality, TraceId, UnixNanos, F64,
};
use logscope_normalize::attrs_from_pairs;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1 as pb_common;
use opentelemetry_proto::tonic::metrics::v1 as pb_metrics;
use opentelemetry_proto::tonic::trace::v1 as pb_trace;

/// Static identity for one converted envelope.
#[derive(Debug, Clone)]
pub struct ConvertContext {
    pub dataset_id: String,
    pub logical_source_id: String,
    pub origin: PhysicalOrigin,
    pub protocol: SourceProtocol,
    pub content_type: Option<String>,
    pub ingest_time: UnixNanos,
    pub batch_index: u64,
    /// BLAKE3 hex of the raw envelope bytes.
    pub envelope_hash: String,
    /// Extra flags attached to every record of this envelope
    /// (e.g. `RawEnvelopeRetained`).
    pub extra_flags: Vec<QualityFlag>,
}

#[derive(Debug, Clone)]
pub struct OtlpReject {
    pub locator: RecordLocator,
    pub reason_code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ConvertedBatch {
    pub logs: Vec<LogRecord>,
    pub metrics: Vec<MetricRecord>,
    pub spans: Vec<SpanRecord>,
    pub resources: Vec<ResourceDescriptor>,
    pub scopes: Vec<ScopeDescriptor>,
    pub rejects: Vec<OtlpReject>,
}

fn pb_any_to_canonical(v: &pb_common::AnyValue) -> AnyValue {
    use pb_common::any_value::Value;
    match &v.value {
        None => AnyValue::Empty,
        Some(Value::StringValue(s)) => AnyValue::Str(s.clone()),
        Some(Value::BoolValue(b)) => AnyValue::Bool(*b),
        Some(Value::IntValue(i)) => AnyValue::Int(*i),
        Some(Value::DoubleValue(d)) => AnyValue::double(*d),
        Some(Value::BytesValue(b)) => AnyValue::bytes(b.clone()),
        Some(Value::ArrayValue(arr)) => {
            AnyValue::Array(arr.values.iter().map(pb_any_to_canonical).collect())
        }
        Some(Value::KvlistValue(kvs)) => {
            let (map, _dups) = attrs_from_pairs(kvs.values.iter().map(|kv| {
                (
                    kv.key.clone(),
                    kv.value
                        .as_ref()
                        .map(pb_any_to_canonical)
                        .unwrap_or(AnyValue::Empty),
                )
            }));
            AnyValue::Map(map)
        }
        // Development-status strindex encoding: the index is only resolvable
        // against a string table this converter does not receive. Preserve
        // the raw index visibly instead of dropping or guessing.
        Some(Value::StringValueStrindex(idx)) => {
            let mut map = AttrMap::new();
            map.insert("otlp.strindex".to_string(), AnyValue::Int(*idx as i64));
            AnyValue::Map(map)
        }
    }
}

fn pb_attrs(kvs: &[pb_common::KeyValue]) -> (AttrMap, Vec<QualityFlag>) {
    attrs_from_pairs(kvs.iter().map(|kv| {
        (
            kv.key.clone(),
            kv.value
                .as_ref()
                .map(pb_any_to_canonical)
                .unwrap_or(AnyValue::Empty),
        )
    }))
}

fn opt_string(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn nanos(v: u64) -> Result<Option<UnixNanos>, &'static str> {
    if v == 0 {
        return Ok(None);
    }
    i64::try_from(v)
        .map(|n| Some(UnixNanos(n)))
        .map_err(|_| "timestamp exceeds representable range")
}

fn resource_descriptor(
    resource: Option<&opentelemetry_proto::tonic::resource::v1::Resource>,
    schema_url: &str,
) -> ResourceDescriptor {
    let (attrs, _dups) = resource
        .map(|r| pb_attrs(&r.attributes))
        .unwrap_or_default();
    let dropped = resource.map(|r| r.dropped_attributes_count).unwrap_or(0);
    ResourceDescriptor::from_attributes(attrs, opt_string(schema_url), dropped)
}

fn scope_descriptor(
    scope: Option<&pb_common::InstrumentationScope>,
    schema_url: &str,
) -> ScopeDescriptor {
    match scope {
        None => ScopeDescriptor::new(None, None, opt_string(schema_url), AttrMap::new(), 0),
        Some(s) => {
            let (attrs, _dups) = pb_attrs(&s.attributes);
            ScopeDescriptor::new(
                opt_string(&s.name),
                opt_string(&s.version),
                opt_string(schema_url),
                attrs,
                s.dropped_attributes_count,
            )
        }
    }
}

impl ConvertContext {
    fn provenance(&self, locator: RecordLocator, flags: Vec<QualityFlag>) -> IngestProvenance {
        let mut all_flags = self.extra_flags.clone();
        all_flags.extend(flags);
        IngestProvenance {
            dataset_id: self.dataset_id.clone(),
            logical_source_id: self.logical_source_id.clone(),
            origin: self.origin.clone(),
            locator,
            parser_id: "otlp".to_string(),
            parser_version: crate::OTLP_PARSER_VERSION.to_string(),
            profile_id: None,
            profile_version: None,
            normalizer_version: logscope_normalize::NORMALIZER_VERSION.to_string(),
            protocol: self.protocol,
            content_type: self.content_type.clone(),
            ingest_time: self.ingest_time,
            raw_hash: self.envelope_hash.clone(),
            original_timestamp_precision: Some(logscope_model::TimestampPrecision::Nanoseconds),
            flags: all_flags,
        }
    }

    fn locator(&self, resource_index: u32, scope_index: u32, record_index: u32) -> RecordLocator {
        RecordLocator {
            otlp: Some(OtlpBatchLocator {
                batch_index: self.batch_index,
                resource_index,
                scope_index,
                record_index,
            }),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

pub fn convert_logs(req: &ExportLogsServiceRequest, ctx: &ConvertContext) -> ConvertedBatch {
    let mut out = ConvertedBatch::default();
    for (ri, rl) in req.resource_logs.iter().enumerate() {
        let resource = resource_descriptor(rl.resource.as_ref(), &rl.schema_url);
        for (si, sl) in rl.scope_logs.iter().enumerate() {
            let scope = scope_descriptor(sl.scope.as_ref(), &sl.schema_url);
            for (li, lr) in sl.log_records.iter().enumerate() {
                let locator = ctx.locator(ri as u32, si as u32, li as u32);
                match convert_log_record(lr, &resource, &scope, ctx, locator.clone()) {
                    Ok(record) => out.logs.push(record),
                    Err((code, msg)) => out.rejects.push(OtlpReject {
                        locator,
                        reason_code: code,
                        message: msg,
                    }),
                }
            }
            out.scopes.push(scope);
        }
        out.resources.push(resource);
    }
    out
}

fn convert_log_record(
    lr: &opentelemetry_proto::tonic::logs::v1::LogRecord,
    resource: &ResourceDescriptor,
    scope: &ScopeDescriptor,
    ctx: &ConvertContext,
    locator: RecordLocator,
) -> Result<LogRecord, (&'static str, String)> {
    let mut flags = Vec::new();
    let event_time =
        nanos(lr.time_unix_nano).map_err(|e| ("otlp/timestamp-out-of-range", e.to_string()))?;
    if event_time.is_none() {
        flags.push(QualityFlag::TimestampMissing);
    }
    let observed = nanos(lr.observed_time_unix_nano)
        .map_err(|e| ("otlp/timestamp-out-of-range", e.to_string()))?
        .unwrap_or(ctx.ingest_time);

    let (attributes, dup_flags) = pb_attrs(&lr.attributes);
    flags.extend(dup_flags);

    let trace_id =
        TraceId::from_bytes(&lr.trace_id).map_err(|e| ("otlp/invalid-trace-id", e.to_string()))?;
    let span_id =
        SpanId::from_bytes(&lr.span_id).map_err(|e| ("otlp/invalid-span-id", e.to_string()))?;

    let body = lr.body.as_ref().map(pb_any_to_canonical);
    let display_message = logscope_normalize::derive_display_message(body.as_ref());

    Ok(LogRecord {
        record_id: String::new(),
        event_time,
        observed_time: observed,
        original_timestamp_text: None,
        timezone_assumption: None,
        severity_text: opt_string(&lr.severity_text),
        severity_number: (lr.severity_number != 0).then_some(lr.severity_number),
        body,
        display_message,
        event_name: opt_string(&lr.event_name),
        trace_id,
        span_id,
        trace_flags: (lr.flags != 0).then_some(lr.flags),
        resource_id: resource.resource_id.clone(),
        scope_id: scope.scope_id.clone(),
        operation: None,
        outcome: None,
        event_type: None,
        request_id: None,
        transaction_id: None,
        message_id: None,
        entity_id: None,
        attributes,
        dropped_attributes_count: lr.dropped_attributes_count,
        provenance: ctx.provenance(locator, flags),
    }
    .seal())
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

fn temporality(v: i32) -> (Temporality, Option<QualityFlag>) {
    match pb_metrics::AggregationTemporality::try_from(v) {
        Ok(pb_metrics::AggregationTemporality::Delta) => (Temporality::Delta, None),
        Ok(pb_metrics::AggregationTemporality::Cumulative) => (Temporality::Cumulative, None),
        _ => (
            Temporality::Unspecified,
            Some(QualityFlag::MetricTemporalityUnspecified),
        ),
    }
}

fn convert_exemplar(e: &pb_metrics::Exemplar) -> Option<Exemplar> {
    use pb_metrics::exemplar::Value;
    let value = match &e.value {
        Some(Value::AsDouble(d)) => NumberValue::Double(F64(*d)),
        Some(Value::AsInt(i)) => NumberValue::Int(*i),
        // An exemplar without a value carries no information; skipped.
        None => return None,
    };
    let (filtered_attributes, _dups) = pb_attrs(&e.filtered_attributes);
    Some(Exemplar {
        filtered_attributes,
        time: UnixNanos(i64::try_from(e.time_unix_nano).ok()?),
        value,
        trace_id: TraceId::from_bytes(&e.trace_id).ok().flatten(),
        span_id: SpanId::from_bytes(&e.span_id).ok().flatten(),
    })
}

fn point_common(
    attributes: &[pb_common::KeyValue],
    start: u64,
    time: u64,
    flags: u32,
    exemplars: &[pb_metrics::Exemplar],
    extra_quality: Vec<QualityFlag>,
) -> Result<PointCommon, (&'static str, String)> {
    let (attrs, mut quality) = pb_attrs(attributes);
    quality.extend(extra_quality);
    Ok(PointCommon {
        attributes: attrs,
        start_time: nanos(start).map_err(|e| ("otlp/timestamp-out-of-range", e.to_string()))?,
        time: nanos(time)
            .map_err(|e| ("otlp/timestamp-out-of-range", e.to_string()))?
            .unwrap_or(UnixNanos(0)),
        flags,
        exemplars: exemplars.iter().filter_map(convert_exemplar).collect(),
        quality,
    })
}

fn number_point(p: &pb_metrics::NumberDataPoint) -> Result<NumberPoint, (&'static str, String)> {
    use pb_metrics::number_data_point::Value;
    const NO_RECORDED_VALUE: u32 = 1;
    let value = match &p.value {
        Some(Value::AsDouble(d)) => NumberValue::Double(F64(*d)),
        Some(Value::AsInt(i)) => NumberValue::Int(*i),
        None if p.flags & NO_RECORDED_VALUE != 0 => {
            // Explicit "no recorded value" point: the flags bit carries the
            // semantics; NaN is the placeholder payload.
            NumberValue::Double(F64(f64::NAN))
        }
        None => {
            return Err((
                "otlp/missing-point-value",
                "number data point has neither value nor NO_RECORDED_VALUE flag".into(),
            ))
        }
    };
    Ok(NumberPoint {
        common: point_common(
            &p.attributes,
            p.start_time_unix_nano,
            p.time_unix_nano,
            p.flags,
            &p.exemplars,
            vec![],
        )?,
        value,
    })
}

pub fn convert_metrics(req: &ExportMetricsServiceRequest, ctx: &ConvertContext) -> ConvertedBatch {
    let mut out = ConvertedBatch::default();
    for (ri, rm) in req.resource_metrics.iter().enumerate() {
        let resource = resource_descriptor(rm.resource.as_ref(), &rm.schema_url);
        for (si, sm) in rm.scope_metrics.iter().enumerate() {
            let scope = scope_descriptor(sm.scope.as_ref(), &sm.schema_url);
            for (mi, m) in sm.metrics.iter().enumerate() {
                let locator = ctx.locator(ri as u32, si as u32, mi as u32);
                match convert_metric(m, &resource, &scope, ctx, locator.clone()) {
                    Ok(record) => out.metrics.push(record),
                    Err((code, msg)) => out.rejects.push(OtlpReject {
                        locator,
                        reason_code: code,
                        message: msg,
                    }),
                }
            }
            out.scopes.push(scope);
        }
        out.resources.push(resource);
    }
    out
}

fn convert_metric(
    m: &pb_metrics::Metric,
    resource: &ResourceDescriptor,
    scope: &ScopeDescriptor,
    ctx: &ConvertContext,
    locator: RecordLocator,
) -> Result<MetricRecord, (&'static str, String)> {
    use pb_metrics::metric::Data;
    let mut envelope_flags: Vec<QualityFlag> = Vec::new();

    let data = match &m.data {
        // Unknown/absent metric type: reject, never coerce to gauge.
        None => {
            return Err((
                "otlp/unknown-metric-type",
                format!("metric {:?} carries no known data type", m.name),
            ))
        }
        Some(Data::Gauge(g)) => MetricData::Gauge {
            points: g
                .data_points
                .iter()
                .map(number_point)
                .collect::<Result<Vec<_>, _>>()?,
        },
        Some(Data::Sum(s)) => {
            let (t, flag) = temporality(s.aggregation_temporality);
            envelope_flags.extend(flag);
            MetricData::Sum {
                temporality: t,
                is_monotonic: s.is_monotonic,
                points: s
                    .data_points
                    .iter()
                    .map(number_point)
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        Some(Data::Histogram(h)) => {
            let (t, flag) = temporality(h.aggregation_temporality);
            envelope_flags.extend(flag);
            MetricData::Histogram {
                temporality: t,
                points: h
                    .data_points
                    .iter()
                    .map(|p| {
                        Ok(HistogramPoint {
                            common: point_common(
                                &p.attributes,
                                p.start_time_unix_nano,
                                p.time_unix_nano,
                                p.flags,
                                &p.exemplars,
                                vec![],
                            )?,
                            count: p.count,
                            sum: p.sum.map(F64),
                            bucket_counts: p.bucket_counts.clone(),
                            explicit_bounds: p.explicit_bounds.iter().map(|b| F64(*b)).collect(),
                            min: p.min.map(F64),
                            max: p.max.map(F64),
                        })
                    })
                    .collect::<Result<Vec<_>, (&'static str, String)>>()?,
            }
        }
        Some(Data::ExponentialHistogram(h)) => {
            let (t, flag) = temporality(h.aggregation_temporality);
            envelope_flags.extend(flag);
            MetricData::ExponentialHistogram {
                temporality: t,
                points: h
                    .data_points
                    .iter()
                    .map(|p| {
                        let buckets =
                            |b: &Option<pb_metrics::exponential_histogram_data_point::Buckets>| {
                                b.as_ref()
                                    .map(|b| ExpBuckets {
                                        offset: b.offset,
                                        bucket_counts: b.bucket_counts.clone(),
                                    })
                                    .unwrap_or_default()
                            };
                        Ok(ExponentialHistogramPoint {
                            common: point_common(
                                &p.attributes,
                                p.start_time_unix_nano,
                                p.time_unix_nano,
                                p.flags,
                                &p.exemplars,
                                vec![],
                            )?,
                            count: p.count,
                            sum: p.sum.map(F64),
                            scale: p.scale,
                            zero_count: p.zero_count,
                            zero_threshold: F64(p.zero_threshold),
                            positive: buckets(&p.positive),
                            negative: buckets(&p.negative),
                            min: p.min.map(F64),
                            max: p.max.map(F64),
                        })
                    })
                    .collect::<Result<Vec<_>, (&'static str, String)>>()?,
            }
        }
        Some(Data::Summary(s)) => MetricData::Summary {
            points: s
                .data_points
                .iter()
                .map(|p| {
                    Ok(SummaryPoint {
                        common: point_common(
                            &p.attributes,
                            p.start_time_unix_nano,
                            p.time_unix_nano,
                            p.flags,
                            &[],
                            vec![],
                        )?,
                        count: p.count,
                        sum: F64(p.sum),
                        quantile_values: p
                            .quantile_values
                            .iter()
                            .map(|q| QuantileValue {
                                quantile: F64(q.quantile),
                                value: F64(q.value),
                            })
                            .collect(),
                    })
                })
                .collect::<Result<Vec<_>, (&'static str, String)>>()?,
        },
    };

    let (metadata, meta_dups) = pb_attrs(&m.metadata);
    envelope_flags.extend(meta_dups);

    Ok(MetricRecord {
        record_id: String::new(),
        name: m.name.clone(),
        description: opt_string(&m.description),
        unit: opt_string(&m.unit),
        data,
        metadata,
        resource_id: resource.resource_id.clone(),
        scope_id: scope.scope_id.clone(),
        provenance: ctx.provenance(locator, envelope_flags),
    }
    .seal())
}

// ---------------------------------------------------------------------------
// Traces
// ---------------------------------------------------------------------------

pub fn convert_traces(req: &ExportTraceServiceRequest, ctx: &ConvertContext) -> ConvertedBatch {
    let mut out = ConvertedBatch::default();
    for (ri, rs) in req.resource_spans.iter().enumerate() {
        let resource = resource_descriptor(rs.resource.as_ref(), &rs.schema_url);
        for (si, ss) in rs.scope_spans.iter().enumerate() {
            let scope = scope_descriptor(ss.scope.as_ref(), &ss.schema_url);
            for (pi, span) in ss.spans.iter().enumerate() {
                let locator = ctx.locator(ri as u32, si as u32, pi as u32);
                match convert_span(span, &resource, &scope, ctx, locator.clone()) {
                    Ok(record) => out.spans.push(record),
                    Err((code, msg)) => out.rejects.push(OtlpReject {
                        locator,
                        reason_code: code,
                        message: msg,
                    }),
                }
            }
            out.scopes.push(scope);
        }
        out.resources.push(resource);
    }
    out
}

fn convert_span(
    s: &pb_trace::Span,
    resource: &ResourceDescriptor,
    scope: &ScopeDescriptor,
    ctx: &ConvertContext,
    locator: RecordLocator,
) -> Result<SpanRecord, (&'static str, String)> {
    let mut flags = Vec::new();

    let trace_id = TraceId::from_bytes(&s.trace_id)
        .map_err(|e| ("otlp/invalid-trace-id", e.to_string()))?
        .ok_or(("otlp/invalid-trace-id", "span without trace id".to_string()))?;
    let span_id = SpanId::from_bytes(&s.span_id)
        .map_err(|e| ("otlp/invalid-span-id", e.to_string()))?
        .ok_or(("otlp/invalid-span-id", "span without span id".to_string()))?;
    let parent_span_id = SpanId::from_bytes(&s.parent_span_id)
        .map_err(|e| ("otlp/invalid-span-id", e.to_string()))?;

    let start_time = nanos(s.start_time_unix_nano)
        .map_err(|e| ("otlp/timestamp-out-of-range", e.to_string()))?
        .unwrap_or(UnixNanos(0));
    let end_time =
        nanos(s.end_time_unix_nano).map_err(|e| ("otlp/timestamp-out-of-range", e.to_string()))?;
    if end_time.is_none() {
        flags.push(QualityFlag::SpanMissingEndTime);
    }

    let (attributes, dup_flags) = pb_attrs(&s.attributes);
    flags.extend(dup_flags);

    let kind = match pb_trace::span::SpanKind::try_from(s.kind) {
        Ok(pb_trace::span::SpanKind::Internal) => SpanKind::Internal,
        Ok(pb_trace::span::SpanKind::Server) => SpanKind::Server,
        Ok(pb_trace::span::SpanKind::Client) => SpanKind::Client,
        Ok(pb_trace::span::SpanKind::Producer) => SpanKind::Producer,
        Ok(pb_trace::span::SpanKind::Consumer) => SpanKind::Consumer,
        _ => SpanKind::Unspecified,
    };
    let status = match &s.status {
        None => SpanStatus {
            code: StatusCode::Unset,
            message: None,
        },
        Some(st) => SpanStatus {
            code: match pb_trace::status::StatusCode::try_from(st.code) {
                Ok(pb_trace::status::StatusCode::Ok) => StatusCode::Ok,
                Ok(pb_trace::status::StatusCode::Error) => StatusCode::Error,
                _ => StatusCode::Unset,
            },
            message: opt_string(&st.message),
        },
    };

    let events = s
        .events
        .iter()
        .map(|e| {
            let (attrs, _d) = pb_attrs(&e.attributes);
            Ok(SpanEvent {
                time: nanos(e.time_unix_nano)
                    .map_err(|err| ("otlp/timestamp-out-of-range", err.to_string()))?
                    .unwrap_or(UnixNanos(0)),
                name: e.name.clone(),
                attributes: attrs,
                dropped_attributes_count: e.dropped_attributes_count,
            })
        })
        .collect::<Result<Vec<_>, (&'static str, String)>>()?;

    let links = s
        .links
        .iter()
        .filter_map(|l| {
            let link_trace = TraceId::from_bytes(&l.trace_id).ok().flatten()?;
            let (attrs, _d) = pb_attrs(&l.attributes);
            Some(SpanLink {
                trace_id: link_trace,
                span_id: SpanId::from_bytes(&l.span_id).ok().flatten(),
                trace_state: opt_string(&l.trace_state),
                attributes: attrs,
                dropped_attributes_count: l.dropped_attributes_count,
                flags: (l.flags != 0).then_some(l.flags),
            })
        })
        .collect();

    Ok(SpanRecord {
        record_id: String::new(),
        trace_id,
        span_id,
        parent_span_id,
        trace_state: opt_string(&s.trace_state),
        flags: (s.flags != 0).then_some(s.flags),
        name: s.name.clone(),
        kind,
        start_time,
        end_time,
        status,
        resource_id: resource.resource_id.clone(),
        scope_id: scope.scope_id.clone(),
        attributes,
        events,
        links,
        dropped_attributes_count: s.dropped_attributes_count,
        dropped_events_count: s.dropped_events_count,
        dropped_links_count: s.dropped_links_count,
        provenance: ctx.provenance(locator, flags),
    }
    .seal())
}
