//! Parquet segment writer for canonical span records.

use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Builder, StringBuilder, UInt32Builder};
use arrow::record_batch::RecordBatch;
use logscope_model::{attrs_canonical_json, SpanKind, SpanRecord, StatusCode};

use crate::error::StoreError;
use crate::provenance_cols::ProvenanceBuilders;
use crate::schema::spans_schema;
use crate::segment::{SegmentFile, SegmentStats};

fn kind_str(k: SpanKind) -> &'static str {
    match k {
        SpanKind::Unspecified => "unspecified",
        SpanKind::Internal => "internal",
        SpanKind::Server => "server",
        SpanKind::Client => "client",
        SpanKind::Producer => "producer",
        SpanKind::Consumer => "consumer",
    }
}

fn status_str(c: StatusCode) -> &'static str {
    match c {
        StatusCode::Unset => "unset",
        StatusCode::Ok => "ok",
        StatusCode::Error => "error",
    }
}

pub struct SpanSegmentWriter {
    file: SegmentFile,
    schema: Arc<arrow::datatypes::Schema>,
}

impl SpanSegmentWriter {
    pub fn create(path: &Path) -> Result<Self, StoreError> {
        let schema = spans_schema();
        Ok(SpanSegmentWriter {
            file: SegmentFile::create(path, schema.clone())?,
            schema,
        })
    }

    pub fn rows_written(&self) -> u64 {
        self.file.rows()
    }

    pub fn write_batch(&mut self, records: &[SpanRecord]) -> Result<(), StoreError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut record_id = StringBuilder::new();
        let mut trace_id = StringBuilder::new();
        let mut span_id = StringBuilder::new();
        let mut parent_span_id = StringBuilder::new();
        let mut trace_state = StringBuilder::new();
        let mut flags = UInt32Builder::new();
        let mut name = StringBuilder::new();
        let mut kind = StringBuilder::new();
        let mut start_time = Int64Builder::new();
        let mut end_time = Int64Builder::new();
        let mut duration = Int64Builder::new();
        let mut status_code = StringBuilder::new();
        let mut status_message = StringBuilder::new();
        let mut resource_id = StringBuilder::new();
        let mut scope_id = StringBuilder::new();
        let mut attributes_json = StringBuilder::new();
        let mut events_json = StringBuilder::new();
        let mut links_json = StringBuilder::new();
        let mut dropped_attrs = UInt32Builder::new();
        let mut dropped_events = UInt32Builder::new();
        let mut dropped_links = UInt32Builder::new();
        let mut prov = ProvenanceBuilders::new();

        for r in records {
            record_id.append_value(&r.record_id);
            trace_id.append_value(r.trace_id.as_str());
            span_id.append_value(r.span_id.as_str());
            parent_span_id.append_option(r.parent_span_id.as_ref().map(|s| s.as_str()));
            trace_state.append_option(r.trace_state.as_deref());
            flags.append_option(r.flags);
            name.append_value(&r.name);
            kind.append_value(kind_str(r.kind));
            start_time.append_value(r.start_time.0);
            self.file.observe_event_time(Some(r.start_time.0));
            end_time.append_option(r.end_time.map(|t| t.0));
            self.file.observe_event_time(r.end_time.map(|t| t.0));
            duration.append_option(r.duration_nanos());
            status_code.append_value(status_str(r.status.code));
            status_message.append_option(r.status.message.as_deref());
            resource_id.append_value(&r.resource_id);
            scope_id.append_value(&r.scope_id);
            attributes_json.append_value(attrs_canonical_json(&r.attributes));
            events_json.append_value(serde_json::to_string(&r.events)?);
            links_json.append_value(serde_json::to_string(&r.links)?);
            dropped_attrs.append_value(r.dropped_attributes_count);
            dropped_events.append_value(r.dropped_events_count);
            dropped_links.append_value(r.dropped_links_count);
            prov.append(&r.provenance)?;
        }

        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(record_id.finish()),
            Arc::new(trace_id.finish()),
            Arc::new(span_id.finish()),
            Arc::new(parent_span_id.finish()),
            Arc::new(trace_state.finish()),
            Arc::new(flags.finish()),
            Arc::new(name.finish()),
            Arc::new(kind.finish()),
            Arc::new(start_time.finish()),
            Arc::new(end_time.finish()),
            Arc::new(duration.finish()),
            Arc::new(status_code.finish()),
            Arc::new(status_message.finish()),
            Arc::new(resource_id.finish()),
            Arc::new(scope_id.finish()),
            Arc::new(attributes_json.finish()),
            Arc::new(events_json.finish()),
            Arc::new(links_json.finish()),
            Arc::new(dropped_attrs.finish()),
            Arc::new(dropped_events.finish()),
            Arc::new(dropped_links.finish()),
        ];
        arrays.extend(prov.finish());

        let batch = RecordBatch::try_new(self.schema.clone(), arrays)?;
        self.file.write(&batch)
    }

    pub fn finish(self) -> Result<SegmentStats, StoreError> {
        self.file.finish()
    }
}
