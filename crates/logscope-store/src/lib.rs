//! Analytical storage for LogScope.
//!
//! Writes canonical batches into immutable, partitioned Parquet segments and
//! maintains the derived SQLite FTS5 full-text index over published segments.
