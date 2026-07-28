//! Query service for LogScope.
//!
//! DuckDB over immutable Parquet segments: structured filtering, bounded
//! first-page results, cancellation, full-text search (via the SQLite FTS5
//! index), representative metric rollups, and span graph reconstruction.
//! The same service backs the UI, CLI, Agent API, dashboards, and reports.

pub mod engine;
pub mod error;

pub use engine::EngineConnection;
pub use error::{ErrorEnvelope, QueryError};
