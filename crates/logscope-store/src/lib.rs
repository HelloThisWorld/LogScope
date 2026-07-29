//! Analytical storage for LogScope.
//!
//! Writes canonical batches into immutable, partitioned Parquet segments and
//! maintains the derived SQLite FTS5 full-text index over published segments.

pub mod error;
pub mod fts;
pub mod log_writer;
pub mod metric_writer;
pub mod provenance_cols;
pub mod schema;
pub mod segment;
pub mod span_writer;

pub use error::StoreError;
pub use fts::{escape_match_query, FtsHit, FtsIndex};
pub use log_writer::LogSegmentWriter;
pub use metric_writer::MetricSegmentWriter;
pub use schema::{logs_schema, metrics_schema, spans_schema, STORAGE_SCHEMA_VERSION};
pub use segment::SegmentStats;
pub use span_writer::SpanSegmentWriter;
