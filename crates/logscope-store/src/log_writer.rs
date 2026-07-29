//! Parquet segment writer for canonical log records.

use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int32Builder, Int64Builder, StringBuilder, UInt32Builder};
use arrow::record_batch::RecordBatch;
use logscope_model::{attrs_canonical_json, LogRecord};

use crate::error::StoreError;
use crate::provenance_cols::ProvenanceBuilders;
use crate::schema::logs_schema;
use crate::segment::{SegmentFile, SegmentStats};

/// Streaming writer for one `logs-*.parquet` segment. Callers feed bounded
/// batches; memory use is proportional to the batch, never the source.
pub struct LogSegmentWriter {
    file: SegmentFile,
    schema: Arc<arrow::datatypes::Schema>,
}

impl LogSegmentWriter {
    pub fn create(path: &Path) -> Result<Self, StoreError> {
        let schema = logs_schema();
        Ok(LogSegmentWriter {
            file: SegmentFile::create(path, schema.clone())?,
            schema,
        })
    }

    pub fn rows_written(&self) -> u64 {
        self.file.rows()
    }

    pub fn write_batch(&mut self, records: &[LogRecord]) -> Result<(), StoreError> {
        if records.is_empty() {
            return Ok(());
        }
        let mut record_id = StringBuilder::new();
        let mut event_time = Int64Builder::new();
        let mut observed_time = Int64Builder::new();
        let mut original_ts = StringBuilder::new();
        let mut tz_json = StringBuilder::new();
        let mut severity_text = StringBuilder::new();
        let mut severity_number = Int32Builder::new();
        let mut body_json = StringBuilder::new();
        let mut display_message = StringBuilder::new();
        let mut event_name = StringBuilder::new();
        let mut trace_id = StringBuilder::new();
        let mut span_id = StringBuilder::new();
        let mut trace_flags = UInt32Builder::new();
        let mut resource_id = StringBuilder::new();
        let mut scope_id = StringBuilder::new();
        let mut operation = StringBuilder::new();
        let mut outcome = StringBuilder::new();
        let mut event_type = StringBuilder::new();
        let mut request_id = StringBuilder::new();
        let mut transaction_id = StringBuilder::new();
        let mut message_id = StringBuilder::new();
        let mut entity_id = StringBuilder::new();
        let mut attributes_json = StringBuilder::new();
        let mut dropped_attrs = UInt32Builder::new();
        let mut prov = ProvenanceBuilders::new();

        for r in records {
            record_id.append_value(&r.record_id);
            event_time.append_option(r.event_time.map(|t| t.0));
            self.file.observe_event_time(r.event_time.map(|t| t.0));
            observed_time.append_value(r.observed_time.0);
            original_ts.append_option(r.original_timestamp_text.as_deref());
            match &r.timezone_assumption {
                None => tz_json.append_null(),
                Some(a) => tz_json.append_value(serde_json::to_string(a)?),
            }
            severity_text.append_option(r.severity_text.as_deref());
            severity_number.append_option(r.severity_number);
            match &r.body {
                None => body_json.append_null(),
                Some(b) => body_json.append_value(serde_json::to_string(b)?),
            }
            display_message.append_value(&r.display_message);
            event_name.append_option(r.event_name.as_deref());
            trace_id.append_option(r.trace_id.as_ref().map(|t| t.as_str()));
            span_id.append_option(r.span_id.as_ref().map(|s| s.as_str()));
            trace_flags.append_option(r.trace_flags);
            resource_id.append_value(&r.resource_id);
            scope_id.append_value(&r.scope_id);
            operation.append_option(r.operation.as_deref());
            outcome.append_option(r.outcome.as_deref());
            event_type.append_option(r.event_type.as_deref());
            request_id.append_option(r.request_id.as_deref());
            transaction_id.append_option(r.transaction_id.as_deref());
            message_id.append_option(r.message_id.as_deref());
            entity_id.append_option(r.entity_id.as_deref());
            attributes_json.append_value(attrs_canonical_json(&r.attributes));
            dropped_attrs.append_value(r.dropped_attributes_count);
            prov.append(&r.provenance)?;
        }

        let mut arrays: Vec<ArrayRef> = vec![
            Arc::new(record_id.finish()),
            Arc::new(event_time.finish()),
            Arc::new(observed_time.finish()),
            Arc::new(original_ts.finish()),
            Arc::new(tz_json.finish()),
            Arc::new(severity_text.finish()),
            Arc::new(severity_number.finish()),
            Arc::new(body_json.finish()),
            Arc::new(display_message.finish()),
            Arc::new(event_name.finish()),
            Arc::new(trace_id.finish()),
            Arc::new(span_id.finish()),
            Arc::new(trace_flags.finish()),
            Arc::new(resource_id.finish()),
            Arc::new(scope_id.finish()),
            Arc::new(operation.finish()),
            Arc::new(outcome.finish()),
            Arc::new(event_type.finish()),
            Arc::new(request_id.finish()),
            Arc::new(transaction_id.finish()),
            Arc::new(message_id.finish()),
            Arc::new(entity_id.finish()),
            Arc::new(attributes_json.finish()),
            Arc::new(dropped_attrs.finish()),
        ];
        arrays.extend(prov.finish());

        let batch = RecordBatch::try_new(self.schema.clone(), arrays)?;
        self.file.write(&batch)
    }

    pub fn finish(self) -> Result<SegmentStats, StoreError> {
        self.file.finish()
    }
}
