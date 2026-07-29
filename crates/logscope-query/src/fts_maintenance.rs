//! Populates the FTS index from published segments.
//!
//! Runs after atomic publication: segments are immutable, so each is indexed
//! exactly once by streaming `record_id` + `display_message` back out of the
//! Parquet file. Publication and indexing are decoupled on purpose — a crash
//! between them leaves a valid workspace with `fts_indexed = 0`, and the
//! caller re-indexes on next use.

use std::path::Path;

use logscope_store::FtsIndex;

use crate::engine::EngineConnection;
use crate::error::QueryError;

/// Streams one published segment into the FTS index. Returns indexed rows.
pub fn index_segment_into_fts(
    engine: &EngineConnection,
    fts: &mut FtsIndex,
    dataset_id: &str,
    segment_id: &str,
    segment_path: &Path,
) -> Result<u64, QueryError> {
    let path_sql = segment_path.to_string_lossy().replace('\'', "''");
    let conn = engine.raw();
    let mut stmt = conn.prepare(&format!(
        "SELECT record_id, display_message FROM read_parquet('{path_sql}')"
    ))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut entries: Vec<(String, String)> = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    let n = fts.index_log_segment(
        dataset_id,
        segment_id,
        entries.iter().map(|(a, b)| (a.as_str(), b.as_str())),
    )?;
    Ok(n)
}
