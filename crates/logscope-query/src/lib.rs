//! Query service for LogScope.
//!
//! DuckDB over immutable Parquet segments: structured filtering, bounded
//! first-page results, cancellation, full-text search (via the SQLite FTS5
//! index), representative metric rollups, and span graph reconstruction.
//! The same service backs the UI, CLI, Agent API, dashboards, and reports.

pub mod cancel;
pub mod engine;
pub mod error;
pub mod fts_maintenance;
pub mod logs;
pub mod metrics;
pub mod spans;

pub use cancel::{run_bounded, QueryCancelHandle};
pub use engine::EngineConnection;
pub use error::{ErrorEnvelope, QueryError};
pub use fts_maintenance::index_segment_into_fts;
pub use logs::{query_log_page, LogPage, LogQueryRequest, LogRow, MAX_PAGE_SIZE};
pub use metrics::{rollup_gauge_or_delta_sum, RollupRow};
pub use spans::{reconstruct_trace, SpanNode, TraceGraph, TraceIntegrity};
