//! Common segment-writer plumbing.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};

use crate::error::StoreError;

/// Result of closing a segment writer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentStats {
    pub rows: u64,
    pub byte_size: u64,
    pub min_event_time: Option<i64>,
    pub max_event_time: Option<i64>,
    pub path: PathBuf,
}

pub(crate) struct SegmentFile {
    writer: ArrowWriter<File>,
    path: PathBuf,
    rows: u64,
    min_event_time: Option<i64>,
    max_event_time: Option<i64>,
}

impl SegmentFile {
    pub fn create(path: &Path, schema: Arc<Schema>) -> Result<Self, StoreError> {
        let file = File::create(path).map_err(|e| StoreError::io(path.display().to_string(), e))?;
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(
                ZstdLevel::try_new(3).expect("static zstd level"),
            ))
            .set_max_row_group_row_count(Some(128 * 1024))
            .build();
        let writer = ArrowWriter::try_new(file, schema, Some(props))?;
        Ok(SegmentFile {
            writer,
            path: path.to_path_buf(),
            rows: 0,
            min_event_time: None,
            max_event_time: None,
        })
    }

    pub fn write(&mut self, batch: &RecordBatch) -> Result<(), StoreError> {
        self.writer.write(batch)?;
        self.rows += batch.num_rows() as u64;
        Ok(())
    }

    pub fn observe_event_time(&mut self, t: Option<i64>) {
        if let Some(t) = t {
            self.min_event_time = Some(self.min_event_time.map_or(t, |m| m.min(t)));
            self.max_event_time = Some(self.max_event_time.map_or(t, |m| m.max(t)));
        }
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn finish(self) -> Result<SegmentStats, StoreError> {
        let path = self.path;
        self.writer.close()?;
        let byte_size = std::fs::metadata(&path)
            .map_err(|e| StoreError::io(path.display().to_string(), e))?
            .len();
        Ok(SegmentStats {
            rows: self.rows,
            byte_size,
            min_event_time: self.min_event_time,
            max_event_time: self.max_event_time,
            path,
        })
    }
}
