//! Versioned Arrow/Parquet storage schemas for canonical records.
//!
//! Mapping rules (ADR-0004/0006):
//! - hot, filterable fields are flat columns;
//! - typed structures (body, attributes, timezone assumption, events, links,
//!   bucket layouts, exemplars) are stored as canonical tagged JSON text so
//!   no type information is lost;
//! - complete `IngestProvenance` is stored as canonical JSON alongside the
//!   hot provenance columns, so every record remains exactly traceable.

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};

/// Bump when any storage column changes meaning, name, or type.
pub const STORAGE_SCHEMA_VERSION: u32 = 1;

fn utf8(name: &str) -> Field {
    Field::new(name, DataType::Utf8, false)
}
fn utf8_null(name: &str) -> Field {
    Field::new(name, DataType::Utf8, true)
}
fn i64_field(name: &str, nullable: bool) -> Field {
    Field::new(name, DataType::Int64, nullable)
}
fn u64_null(name: &str) -> Field {
    Field::new(name, DataType::UInt64, true)
}
fn u32_field(name: &str, nullable: bool) -> Field {
    Field::new(name, DataType::UInt32, nullable)
}

/// Hot provenance columns shared by all three signal schemas, followed by
/// the complete provenance JSON.
pub fn provenance_fields() -> Vec<Field> {
    vec![
        utf8("dataset_id"),
        utf8("source_id"),
        utf8("origin_id"),
        utf8_null("archive_entry"),
        u64_null("record_number"),
        u64_null("line_start"),
        u64_null("byte_start"),
        u64_null("byte_end"),
        utf8("raw_hash"),
        i64_field("ingest_time", false),
        utf8("provenance_json"),
    ]
}

/// Parquet schema for canonical log records.
pub fn logs_schema() -> Arc<Schema> {
    let mut fields = vec![
        utf8("record_id"),
        i64_field("event_time", true),
        i64_field("observed_time", false),
        utf8_null("original_timestamp_text"),
        utf8_null("timezone_assumption_json"),
        utf8_null("severity_text"),
        Field::new("severity_number", DataType::Int32, true),
        utf8_null("body_json"),
        utf8("display_message"),
        utf8_null("event_name"),
        utf8_null("trace_id"),
        utf8_null("span_id"),
        u32_field("trace_flags", true),
        utf8("resource_id"),
        utf8("scope_id"),
        utf8_null("operation"),
        utf8_null("outcome"),
        utf8_null("event_type"),
        utf8_null("request_id"),
        utf8_null("transaction_id"),
        utf8_null("message_id"),
        utf8_null("entity_id"),
        utf8("attributes_json"),
        u32_field("dropped_attributes_count", false),
    ];
    fields.extend(provenance_fields());
    Arc::new(Schema::new(fields))
}

/// Parquet schema for canonical metric points (one row per data point;
/// metric-level fields repeat per point).
pub fn metrics_schema() -> Arc<Schema> {
    let mut fields = vec![
        utf8("record_id"),
        u32_field("point_index", false),
        utf8("metric_name"),
        utf8_null("description"),
        utf8_null("unit"),
        utf8("metric_type"), // gauge|sum|histogram|exponential_histogram|summary
        utf8_null("temporality"),
        Field::new("is_monotonic", DataType::Boolean, true),
        utf8("point_attributes_json"),
        i64_field("start_time", true),
        i64_field("time", false),
        u32_field("point_flags", false),
        i64_field("value_int", true),
        Field::new("value_double", DataType::Float64, true),
        u64_null("count"),
        Field::new("sum", DataType::Float64, true),
        utf8_null("histogram_json"), // buckets/bounds or exp-histogram or quantiles
        Field::new("min", DataType::Float64, true),
        Field::new("max", DataType::Float64, true),
        utf8_null("exemplars_json"),
        utf8("point_quality_json"),
        utf8("metric_metadata_json"),
        utf8("resource_id"),
        utf8("scope_id"),
    ];
    fields.extend(provenance_fields());
    Arc::new(Schema::new(fields))
}

/// Parquet schema for canonical span records.
pub fn spans_schema() -> Arc<Schema> {
    let mut fields = vec![
        utf8("record_id"),
        utf8("trace_id"),
        utf8("span_id"),
        utf8_null("parent_span_id"),
        utf8_null("trace_state"),
        u32_field("flags", true),
        utf8("name"),
        utf8("kind"),
        i64_field("start_time", false),
        i64_field("end_time", true),
        i64_field("duration_nanos", true),
        utf8("status_code"),
        utf8_null("status_message"),
        utf8("resource_id"),
        utf8("scope_id"),
        utf8("attributes_json"),
        utf8("events_json"),
        utf8("links_json"),
        u32_field("dropped_attributes_count", false),
        u32_field("dropped_events_count", false),
        u32_field("dropped_links_count", false),
    ];
    fields.extend(provenance_fields());
    Arc::new(Schema::new(fields))
}
