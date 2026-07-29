//! Representative metric rollup (v0.0 storage/query proof).
//!
//! Scope: time-bucket aggregation for gauges and delta sums — the cases
//! where per-bucket count/sum/min/max/avg are semantically sound without
//! rate conversion. Cumulative series and histograms need temporality-aware
//! analysis and are explicitly out of the v0.0 proof (the data is stored
//! losslessly for later milestones).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::engine::EngineConnection;
use crate::error::QueryError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollupRow {
    pub bucket_start: i64,
    pub metric_type: String,
    pub points: u64,
    pub sum: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub avg: Option<f64>,
}

/// Aggregates one metric into fixed time buckets.
pub fn rollup_gauge_or_delta_sum(
    engine: &EngineConnection,
    segment_files: &[PathBuf],
    metric_name: &str,
    bucket_nanos: i64,
    time_start: Option<i64>,
    time_end: Option<i64>,
) -> Result<Vec<RollupRow>, QueryError> {
    if bucket_nanos <= 0 {
        return Err(QueryError::InvalidParameter(
            "bucket_nanos must be positive".into(),
        ));
    }
    if segment_files.is_empty() {
        return Ok(vec![]);
    }
    let list = segment_files
        .iter()
        .map(|p| format!("'{}'", p.to_string_lossy().replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");

    let mut filters = vec![
        "metric_name = ?".to_string(),
        "(metric_type = 'gauge' OR (metric_type = 'sum' AND temporality = 'delta'))".to_string(),
    ];
    let mut params: Vec<duckdb::types::Value> =
        vec![duckdb::types::Value::Text(metric_name.to_string())];
    if let Some(s) = time_start {
        filters.push("time >= ?".into());
        params.push(duckdb::types::Value::BigInt(s));
    }
    if let Some(e) = time_end {
        filters.push("time < ?".into());
        params.push(duckdb::types::Value::BigInt(e));
    }

    let sql = format!(
        "SELECT (time // {bucket}) * {bucket} AS bucket_start,
                metric_type,
                count(*) AS points,
                sum(COALESCE(value_double, CAST(value_int AS DOUBLE))) AS sum_v,
                min(COALESCE(value_double, CAST(value_int AS DOUBLE))) AS min_v,
                max(COALESCE(value_double, CAST(value_int AS DOUBLE))) AS max_v,
                avg(COALESCE(value_double, CAST(value_int AS DOUBLE))) AS avg_v
         FROM read_parquet([{list}], union_by_name = true)
         WHERE {where_sql}
         GROUP BY 1, 2
         ORDER BY 1, 2",
        bucket = bucket_nanos,
        where_sql = filters.join(" AND "),
    );

    let conn = engine.raw();
    let mut stmt = conn.prepare(&sql)?;
    let mapped = stmt.query_map(duckdb::params_from_iter(params.iter()), |r| {
        Ok(RollupRow {
            bucket_start: r.get(0)?,
            metric_type: r.get(1)?,
            points: r.get::<_, i64>(2)? as u64,
            sum: r.get(3)?,
            min: r.get(4)?,
            max: r.get(5)?,
            avg: r.get(6)?,
        })
    })?;
    let mut rows = Vec::new();
    for row in mapped {
        rows.push(row?);
    }
    Ok(rows)
}
