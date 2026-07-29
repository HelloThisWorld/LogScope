//! Parquet segment writer for canonical metric records (one row per point).

use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanBuilder, Float64Builder, Int64Builder, StringBuilder, UInt32Builder,
    UInt64Builder,
};
use arrow::record_batch::RecordBatch;
use logscope_model::{
    attrs_canonical_json, MetricData, MetricRecord, NumberValue, PointCommon, Temporality,
};

use crate::error::StoreError;
use crate::provenance_cols::ProvenanceBuilders;
use crate::schema::metrics_schema;
use crate::segment::{SegmentFile, SegmentStats};

fn temporality_str(t: Temporality) -> &'static str {
    match t {
        Temporality::Unspecified => "unspecified",
        Temporality::Delta => "delta",
        Temporality::Cumulative => "cumulative",
    }
}

pub struct MetricSegmentWriter {
    file: SegmentFile,
    schema: Arc<arrow::datatypes::Schema>,
}

#[derive(Default)]
struct PointRow {
    temporality: Option<&'static str>,
    is_monotonic: Option<bool>,
    value_int: Option<i64>,
    value_double: Option<f64>,
    count: Option<u64>,
    sum: Option<f64>,
    histogram_json: Option<String>,
    min: Option<f64>,
    max: Option<f64>,
}

impl MetricSegmentWriter {
    pub fn create(path: &Path) -> Result<Self, StoreError> {
        let schema = metrics_schema();
        Ok(MetricSegmentWriter {
            file: SegmentFile::create(path, schema.clone())?,
            schema,
        })
    }

    pub fn rows_written(&self) -> u64 {
        self.file.rows()
    }

    pub fn write_batch(&mut self, records: &[MetricRecord]) -> Result<(), StoreError> {
        let mut record_id = StringBuilder::new();
        let mut point_index = UInt32Builder::new();
        let mut metric_name = StringBuilder::new();
        let mut description = StringBuilder::new();
        let mut unit = StringBuilder::new();
        let mut metric_type = StringBuilder::new();
        let mut temporality = StringBuilder::new();
        let mut is_monotonic = BooleanBuilder::new();
        let mut point_attrs = StringBuilder::new();
        let mut start_time = Int64Builder::new();
        let mut time = Int64Builder::new();
        let mut point_flags = UInt32Builder::new();
        let mut value_int = Int64Builder::new();
        let mut value_double = Float64Builder::new();
        let mut count = UInt64Builder::new();
        let mut sum = Float64Builder::new();
        let mut histogram_json = StringBuilder::new();
        let mut min = Float64Builder::new();
        let mut max = Float64Builder::new();
        let mut exemplars_json = StringBuilder::new();
        let mut point_quality = StringBuilder::new();
        let mut metric_metadata = StringBuilder::new();
        let mut resource_id = StringBuilder::new();
        let mut scope_id = StringBuilder::new();
        let mut prov = ProvenanceBuilders::new();
        let mut rows = 0usize;

        let mut append_point = |r: &MetricRecord,
                                idx: u32,
                                common: &PointCommon,
                                row: PointRow,
                                prov: &mut ProvenanceBuilders|
         -> Result<(), StoreError> {
            record_id.append_value(&r.record_id);
            point_index.append_value(idx);
            metric_name.append_value(&r.name);
            description.append_option(r.description.as_deref());
            unit.append_option(r.unit.as_deref());
            metric_type.append_value(r.data.type_name());
            temporality.append_option(row.temporality);
            is_monotonic.append_option(row.is_monotonic);
            point_attrs.append_value(attrs_canonical_json(&common.attributes));
            start_time.append_option(common.start_time.map(|t| t.0));
            time.append_value(common.time.0);
            point_flags.append_value(common.flags);
            value_int.append_option(row.value_int);
            value_double.append_option(row.value_double);
            count.append_option(row.count);
            sum.append_option(row.sum);
            histogram_json.append_option(row.histogram_json.as_deref());
            min.append_option(row.min);
            max.append_option(row.max);
            if common.exemplars.is_empty() {
                exemplars_json.append_null();
            } else {
                exemplars_json.append_value(serde_json::to_string(&common.exemplars)?);
            }
            point_quality.append_value(serde_json::to_string(&common.quality)?);
            metric_metadata.append_value(attrs_canonical_json(&r.metadata));
            resource_id.append_value(&r.resource_id);
            scope_id.append_value(&r.scope_id);
            prov.append(&r.provenance)?;
            Ok(())
        };

        for r in records {
            match &r.data {
                MetricData::Gauge { points } => {
                    for (i, p) in points.iter().enumerate() {
                        self.file.observe_event_time(Some(p.common.time.0));
                        let (vi, vd) = match p.value {
                            NumberValue::Int(v) => (Some(v), None),
                            NumberValue::Double(d) => (None, Some(d.0)),
                        };
                        append_point(
                            r,
                            i as u32,
                            &p.common,
                            PointRow {
                                value_int: vi,
                                value_double: vd,
                                ..Default::default()
                            },
                            &mut prov,
                        )?;
                        rows += 1;
                    }
                }
                MetricData::Sum {
                    temporality: t,
                    is_monotonic: mono,
                    points,
                } => {
                    for (i, p) in points.iter().enumerate() {
                        self.file.observe_event_time(Some(p.common.time.0));
                        let (vi, vd) = match p.value {
                            NumberValue::Int(v) => (Some(v), None),
                            NumberValue::Double(d) => (None, Some(d.0)),
                        };
                        append_point(
                            r,
                            i as u32,
                            &p.common,
                            PointRow {
                                temporality: Some(temporality_str(*t)),
                                is_monotonic: Some(*mono),
                                value_int: vi,
                                value_double: vd,
                                ..Default::default()
                            },
                            &mut prov,
                        )?;
                        rows += 1;
                    }
                }
                MetricData::Histogram {
                    temporality: t,
                    points,
                } => {
                    for (i, p) in points.iter().enumerate() {
                        self.file.observe_event_time(Some(p.common.time.0));
                        let hist = serde_json::json!({
                            "bucket_counts": p.bucket_counts,
                            "explicit_bounds": p.explicit_bounds,
                        });
                        append_point(
                            r,
                            i as u32,
                            &p.common,
                            PointRow {
                                temporality: Some(temporality_str(*t)),
                                count: Some(p.count),
                                sum: p.sum.map(|f| f.0),
                                histogram_json: Some(hist.to_string()),
                                min: p.min.map(|f| f.0),
                                max: p.max.map(|f| f.0),
                                ..Default::default()
                            },
                            &mut prov,
                        )?;
                        rows += 1;
                    }
                }
                MetricData::ExponentialHistogram {
                    temporality: t,
                    points,
                } => {
                    for (i, p) in points.iter().enumerate() {
                        self.file.observe_event_time(Some(p.common.time.0));
                        let hist = serde_json::json!({
                            "scale": p.scale,
                            "zero_count": p.zero_count,
                            "zero_threshold": p.zero_threshold,
                            "positive": p.positive,
                            "negative": p.negative,
                        });
                        append_point(
                            r,
                            i as u32,
                            &p.common,
                            PointRow {
                                temporality: Some(temporality_str(*t)),
                                count: Some(p.count),
                                sum: p.sum.map(|f| f.0),
                                histogram_json: Some(hist.to_string()),
                                min: p.min.map(|f| f.0),
                                max: p.max.map(|f| f.0),
                                ..Default::default()
                            },
                            &mut prov,
                        )?;
                        rows += 1;
                    }
                }
                MetricData::Summary { points } => {
                    for (i, p) in points.iter().enumerate() {
                        self.file.observe_event_time(Some(p.common.time.0));
                        let hist = serde_json::json!({
                            "quantiles": p.quantile_values,
                        });
                        append_point(
                            r,
                            i as u32,
                            &p.common,
                            PointRow {
                                count: Some(p.count),
                                sum: Some(p.sum.0),
                                histogram_json: Some(hist.to_string()),
                                ..Default::default()
                            },
                            &mut prov,
                        )?;
                        rows += 1;
                    }
                }
            }
        }

        if rows == 0 {
            return Ok(());
        }

        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(record_id.finish()),
            Arc::new(point_index.finish()),
            Arc::new(metric_name.finish()),
            Arc::new(description.finish()),
            Arc::new(unit.finish()),
            Arc::new(metric_type.finish()),
            Arc::new(temporality.finish()),
            Arc::new(is_monotonic.finish()),
            Arc::new(point_attrs.finish()),
            Arc::new(start_time.finish()),
            Arc::new(time.finish()),
            Arc::new(point_flags.finish()),
            Arc::new(value_int.finish()),
            Arc::new(value_double.finish()),
            Arc::new(count.finish()),
            Arc::new(sum.finish()),
            Arc::new(histogram_json.finish()),
            Arc::new(min.finish()),
            Arc::new(max.finish()),
            Arc::new(exemplars_json.finish()),
            Arc::new(point_quality.finish()),
            Arc::new(metric_metadata.finish()),
            Arc::new(resource_id.finish()),
            Arc::new(scope_id.finish()),
        ];
        arrays.extend(prov.finish());

        let batch = RecordBatch::try_new(self.schema.clone(), arrays)?;
        self.file.write(&batch)
    }

    pub fn finish(self) -> Result<SegmentStats, StoreError> {
        self.file.finish()
    }
}
