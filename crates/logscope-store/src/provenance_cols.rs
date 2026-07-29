//! Shared column builders for the provenance tail of every segment schema.

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Builder, StringBuilder, UInt64Builder};
use logscope_model::{IngestProvenance, PhysicalOrigin};

use crate::error::StoreError;

pub struct ProvenanceBuilders {
    dataset_id: StringBuilder,
    source_id: StringBuilder,
    origin_id: StringBuilder,
    archive_entry: StringBuilder,
    record_number: UInt64Builder,
    line_start: UInt64Builder,
    byte_start: UInt64Builder,
    byte_end: UInt64Builder,
    raw_hash: StringBuilder,
    ingest_time: Int64Builder,
    provenance_json: StringBuilder,
}

impl Default for ProvenanceBuilders {
    fn default() -> Self {
        Self::new()
    }
}

impl ProvenanceBuilders {
    pub fn new() -> Self {
        ProvenanceBuilders {
            dataset_id: StringBuilder::new(),
            source_id: StringBuilder::new(),
            origin_id: StringBuilder::new(),
            archive_entry: StringBuilder::new(),
            record_number: UInt64Builder::new(),
            line_start: UInt64Builder::new(),
            byte_start: UInt64Builder::new(),
            byte_end: UInt64Builder::new(),
            raw_hash: StringBuilder::new(),
            ingest_time: Int64Builder::new(),
            provenance_json: StringBuilder::new(),
        }
    }

    pub fn append(&mut self, p: &IngestProvenance) -> Result<(), StoreError> {
        self.dataset_id.append_value(&p.dataset_id);
        self.source_id.append_value(&p.logical_source_id);
        match &p.origin {
            PhysicalOrigin::File {
                file_id,
                archive_entry,
            } => {
                self.origin_id.append_value(file_id);
                self.archive_entry.append_option(archive_entry.as_deref());
            }
            PhysicalOrigin::OtlpSession { session_id } => {
                self.origin_id.append_value(session_id);
                self.archive_entry.append_null();
            }
        }
        self.record_number.append_option(p.locator.record_number);
        self.line_start.append_option(p.locator.line_start);
        self.byte_start.append_option(p.locator.byte_start);
        self.byte_end.append_option(p.locator.byte_end);
        self.raw_hash.append_value(&p.raw_hash);
        self.ingest_time.append_value(p.ingest_time.0);
        self.provenance_json.append_value(serde_json::to_string(p)?);
        Ok(())
    }

    pub fn finish(mut self) -> Vec<ArrayRef> {
        vec![
            Arc::new(self.dataset_id.finish()),
            Arc::new(self.source_id.finish()),
            Arc::new(self.origin_id.finish()),
            Arc::new(self.archive_entry.finish()),
            Arc::new(self.record_number.finish()),
            Arc::new(self.line_start.finish()),
            Arc::new(self.byte_start.finish()),
            Arc::new(self.byte_end.finish()),
            Arc::new(self.raw_hash.finish()),
            Arc::new(self.ingest_time.finish()),
            Arc::new(self.provenance_json.finish()),
        ]
    }
}
