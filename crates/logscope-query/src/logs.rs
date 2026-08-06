//! Bounded, parameterized log queries over published Parquet segments.

use std::path::PathBuf;
use std::time::Duration;

use duckdb::types::Value;
use logscope_store::FtsIndex;
use serde::{Deserialize, Serialize};

use crate::cancel::{run_bounded, QueryCancelHandle};
use crate::engine::EngineConnection;
use crate::error::QueryError;

/// Hard cap for one result page.
pub const MAX_PAGE_SIZE: u32 = 1_000;
/// FTS pre-selection bound (hits fed into the structured query).
pub const MAX_FTS_CANDIDATES: usize = 5_000;
/// Default execution budget for one page query.
pub const DEFAULT_BUDGET_MS: u64 = 15_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogQueryRequest {
    pub dataset_ids: Vec<String>,
    /// Event-time range [start, end) in UTC nanos.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_end: Option<i64>,
    /// Minimum OTLP severity number (inclusive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_severity: Option<i32>,
    /// Full-text search over display messages (FTS5 semantics: AND of terms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains_text: Option<String>,
    /// Attribute string-equality filters (key, value), ANDed. Values are
    /// compared against the tagged canonical attribute JSON via
    /// json_extract_string(attributes_json, '$.<key>.v').
    #[serde(default)]
    pub attr_equals: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Page size; clamped to [1, MAX_PAGE_SIZE].
    pub limit: u32,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRow {
    pub record_id: String,
    pub event_time: Option<i64>,
    pub severity_text: Option<String>,
    pub severity_number: Option<i32>,
    pub display_message: String,
    pub resource_id: String,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub dataset_id: String,
    pub source_id: String,
    pub record_number: Option<u64>,
    pub line_start: Option<u64>,
    pub attributes_json: String,
    pub provenance_json: String,
    /// Canonical generic fields: present only when the import profile
    /// mapped them. `None` is "the source never carried this field",
    /// which analysis must report as an exclusion, never as a value.
    pub operation: Option<String>,
    pub outcome: Option<String>,
    pub event_name: Option<String>,
    pub event_type: Option<String>,
    /// Stable application/transport identifiers — correlation keys.
    /// They carry no ordering or causation meaning by themselves.
    pub request_id: Option<String>,
    pub transaction_id: Option<String>,
    pub message_id: Option<String>,
    pub entity_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPage {
    pub rows: Vec<LogRow>,
    pub has_more: bool,
    /// Effective (clamped) limit used.
    pub limit: u32,
}

fn sql_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub(crate) fn files_expr(files: &[PathBuf]) -> String {
    let list = files
        .iter()
        .map(|p| sql_quote(&p.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("read_parquet([{list}], union_by_name = true)")
}

/// Executes one bounded page query. `segment_files` must be the published
/// segment files of the requested datasets (the caller resolves them from
/// workspace metadata; only published segments are ever visible here).
pub fn query_log_page(
    engine: &EngineConnection,
    segment_files: &[PathBuf],
    request: &LogQueryRequest,
    fts: Option<&FtsIndex>,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<LogPage, QueryError> {
    let limit = request.limit.clamp(1, MAX_PAGE_SIZE);
    if segment_files.is_empty() {
        return Ok(LogPage {
            rows: vec![],
            has_more: false,
            limit,
        });
    }

    // FTS pre-selection: resolve matching record IDs first (bounded).
    let mut fts_ids: Option<Vec<String>> = None;
    if let Some(text) = request
        .contains_text
        .as_deref()
        .filter(|t| !t.trim().is_empty())
    {
        let Some(fts) = fts else {
            return Err(QueryError::InvalidParameter(
                "contains_text requires the full-text index".into(),
            ));
        };
        let hits = fts.search_logs(&request.dataset_ids, text, MAX_FTS_CANDIDATES)?;
        let ids: Vec<String> = hits.into_iter().map(|h| h.record_id).collect();
        if ids.is_empty() {
            return Ok(LogPage {
                rows: vec![],
                has_more: false,
                limit,
            });
        }
        fts_ids = Some(ids);
    }

    let mut where_clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if !request.dataset_ids.is_empty() {
        let placeholders = request
            .dataset_ids
            .iter()
            .map(|_| "?".to_string())
            .collect::<Vec<_>>()
            .join(", ");
        where_clauses.push(format!("dataset_id IN ({placeholders})"));
        for d in &request.dataset_ids {
            params.push(Value::Text(d.clone()));
        }
    }
    if let Some(start) = request.time_start {
        where_clauses.push("event_time >= ?".into());
        params.push(Value::BigInt(start));
    }
    if let Some(end) = request.time_end {
        where_clauses.push("event_time < ?".into());
        params.push(Value::BigInt(end));
    }
    if let Some(min) = request.min_severity {
        where_clauses.push("severity_number >= ?".into());
        params.push(Value::Int(min));
    }
    if let Some(res) = &request.resource_id {
        where_clauses.push("resource_id = ?".into());
        params.push(Value::Text(res.clone()));
    }
    if let Some(trace) = &request.trace_id {
        where_clauses.push("trace_id = ?".into());
        params.push(Value::Text(trace.clone()));
    }
    for (key, value) in &request.attr_equals {
        if key.contains('\'') || key.contains('"') || key.contains('\\') {
            return Err(QueryError::InvalidParameter(format!(
                "unsupported attribute key: {key:?}"
            )));
        }
        // Tagged canonical JSON: {"<key>":{"t":"str","v":"..."}}.
        where_clauses.push(format!(
            "json_extract_string(attributes_json, '$.\"{key}\".v') = ?"
        ));
        params.push(Value::Text(value.clone()));
    }
    if let Some(ids) = &fts_ids {
        let placeholders = ids
            .iter()
            .map(|_| "?".to_string())
            .collect::<Vec<_>>()
            .join(", ");
        where_clauses.push(format!("record_id IN ({placeholders})"));
        for id in ids {
            params.push(Value::Text(id.clone()));
        }
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    let sql = format!(
        "SELECT record_id, event_time, severity_text, severity_number, display_message,
                resource_id, trace_id, span_id, dataset_id, source_id,
                record_number, line_start, attributes_json, provenance_json,
                operation, outcome, event_name, event_type, request_id,
                transaction_id, message_id, entity_id
         FROM {files}
         {where_sql}
         ORDER BY event_time NULLS LAST, record_id
         LIMIT {fetch} OFFSET {offset}",
        files = files_expr(segment_files),
        fetch = limit as i64 + 1,
        offset = request.offset as i64,
    );

    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = Vec::new();
        let mapped = stmt.query_map(duckdb::params_from_iter(params.iter()), |r| {
            Ok(LogRow {
                record_id: r.get(0)?,
                event_time: r.get(1)?,
                severity_text: r.get(2)?,
                severity_number: r.get(3)?,
                display_message: r.get(4)?,
                resource_id: r.get(5)?,
                trace_id: r.get(6)?,
                span_id: r.get(7)?,
                dataset_id: r.get(8)?,
                source_id: r.get(9)?,
                record_number: r.get(10)?,
                line_start: r.get(11)?,
                attributes_json: r.get(12)?,
                provenance_json: r.get(13)?,
                operation: r.get(14)?,
                outcome: r.get(15)?,
                event_name: r.get(16)?,
                event_type: r.get(17)?,
                request_id: r.get(18)?,
                transaction_id: r.get(19)?,
                message_id: r.get(20)?,
                entity_id: r.get(21)?,
            })
        })?;
        for row in mapped {
            rows.push(row?);
        }
        let has_more = rows.len() as u32 > limit;
        rows.truncate(limit as usize);
        Ok(LogPage {
            rows,
            has_more,
            limit,
        })
    })
}
