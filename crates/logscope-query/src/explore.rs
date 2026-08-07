//! Explorer execution: time windows, keyset pages, counts, histogram,
//! facets, field summaries, and source-order context — all over the SAME
//! compiled filter (ADR-0013). No component may re-implement filtering.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use duckdb::types::Value;
use logscope_query_lang::{AttrType, CanonicalField};
use serde::{Deserialize, Serialize};

use crate::cancel::{run_bounded, QueryCancelHandle};
use crate::compile::{attr_value_path, install_temp_tables, CompiledFilter};
use crate::engine::EngineConnection;
use crate::error::QueryError;
use crate::logs::{files_expr, LogRow, MAX_PAGE_SIZE};

/// Default execution budget for one Explorer query.
pub const DEFAULT_BUDGET_MS: u64 = 15_000;
/// Histogram bin-count bound.
pub const MAX_HISTOGRAM_BINS: u32 = 500;
/// Facet request bounds.
pub const MAX_FACET_FIELDS: usize = 8;
pub const MAX_FACET_TOP_K: u32 = 50;
/// Source-context neighbor bound (each direction).
pub const MAX_CONTEXT_NEIGHBORS: u32 = 100;

/// Persistable time-window strategy (ADR-0014).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimeStrategy {
    /// Every record, including records without a valid event timestamp.
    All,
    /// Explicit half-open interval `[start, end)` in UTC nanos.
    Absolute { start: i64, end: i64 },
    /// `[latest - duration, latest]` anchored to the newest event timestamp
    /// in the selected datasets (not wall clock — offline cases are
    /// historical).
    RelativeToLatest { duration_nanos: i64 },
}

/// A strategy resolved against the selected datasets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedWindow {
    pub strategy: TimeStrategy,
    /// `None` bounds = all data (untimestamped records included).
    pub start: Option<i64>,
    pub end: Option<i64>,
    /// True when a relative strategy found no timestamped records at all.
    pub empty_anchor: bool,
}

/// Resolves a strategy. `latest` is the maximum event timestamp across the
/// selected datasets' published segments (from workspace metadata).
pub fn resolve_window(strategy: &TimeStrategy, latest: Option<i64>) -> ResolvedWindow {
    match strategy {
        TimeStrategy::All => ResolvedWindow {
            strategy: strategy.clone(),
            start: None,
            end: None,
            empty_anchor: false,
        },
        TimeStrategy::Absolute { start, end } => ResolvedWindow {
            strategy: strategy.clone(),
            start: Some(*start),
            end: Some(*end),
            empty_anchor: false,
        },
        TimeStrategy::RelativeToLatest { duration_nanos } => match latest {
            Some(anchor) => ResolvedWindow {
                strategy: strategy.clone(),
                start: Some(anchor.saturating_sub(*duration_nanos)),
                end: Some(anchor.saturating_add(1)),
                empty_anchor: false,
            },
            None => ResolvedWindow {
                strategy: strategy.clone(),
                start: Some(0),
                end: Some(0),
                empty_anchor: true,
            },
        },
    }
}

fn window_sql(window: &ResolvedWindow, params: &mut Vec<Value>) -> String {
    match (window.start, window.end) {
        (Some(s), Some(e)) => {
            params.push(Value::BigInt(s));
            params.push(Value::BigInt(e));
            "(event_time >= ? AND event_time < ?)".to_string()
        }
        _ => "true".to_string(),
    }
}

/// Opaque keyset cursor position (base64 of canonical JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CursorPos {
    v: u32,
    /// Event time of the boundary row (`None` = inside the untimestamped
    /// tail block).
    t: Option<i64>,
    r: String,
    d: String,
}

pub fn encode_cursor(event_time: Option<i64>, record_id: &str, dataset_id: &str) -> String {
    let pos = CursorPos {
        v: 1,
        t: event_time,
        r: record_id.to_string(),
        d: dataset_id.to_string(),
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&pos).expect("cursor serialization cannot fail"))
}

fn decode_cursor(cursor: &str) -> Result<CursorPos, QueryError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_bytes())
        .map_err(|_| QueryError::InvalidParameter("malformed cursor".into()))?;
    let pos: CursorPos = serde_json::from_slice(&bytes)
        .map_err(|_| QueryError::InvalidParameter("malformed cursor".into()))?;
    if pos.r.len() > 128 || pos.d.len() > 128 {
        return Err(QueryError::InvalidParameter("malformed cursor".into()));
    }
    if pos.v != 1 {
        return Err(QueryError::InvalidParameter(format!(
            "unsupported cursor version {}",
            pos.v
        )));
    }
    Ok(pos)
}

/// Deterministic total order: event_time DESC NULLS LAST, record_id DESC,
/// dataset_id DESC. Untimestamped records form the tail block.
const ORDER_FORWARD: &str = "ORDER BY event_time DESC NULLS LAST, record_id DESC, dataset_id DESC";
const ORDER_BACKWARD: &str = "ORDER BY event_time ASC NULLS FIRST, record_id ASC, dataset_id ASC";

fn keyset_sql(pos: &CursorPos, backward: bool, params: &mut Vec<Value>) -> String {
    match (pos.t, backward) {
        (Some(t), false) => {
            params.push(Value::BigInt(t));
            params.push(Value::BigInt(t));
            params.push(Value::Text(pos.r.clone()));
            params.push(Value::Text(pos.r.clone()));
            params.push(Value::Text(pos.d.clone()));
            "(event_time IS NULL OR event_time < ? \
             OR (event_time = ? AND (record_id < ? OR (record_id = ? AND dataset_id < ?))))"
                .to_string()
        }
        (None, false) => {
            params.push(Value::Text(pos.r.clone()));
            params.push(Value::Text(pos.r.clone()));
            params.push(Value::Text(pos.d.clone()));
            "(event_time IS NULL AND (record_id < ? OR (record_id = ? AND dataset_id < ?)))"
                .to_string()
        }
        (Some(t), true) => {
            params.push(Value::BigInt(t));
            params.push(Value::BigInt(t));
            params.push(Value::Text(pos.r.clone()));
            params.push(Value::Text(pos.r.clone()));
            params.push(Value::Text(pos.d.clone()));
            "(event_time IS NOT NULL AND (event_time > ? \
             OR (event_time = ? AND (record_id > ? OR (record_id = ? AND dataset_id > ?)))))"
                .to_string()
        }
        (None, true) => {
            params.push(Value::Text(pos.r.clone()));
            params.push(Value::Text(pos.r.clone()));
            params.push(Value::Text(pos.d.clone()));
            "(event_time IS NOT NULL \
             OR (record_id > ? OR (record_id = ? AND dataset_id > ?)))"
                .to_string()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageRequest {
    pub cursor: Option<String>,
    pub backward: bool,
    pub limit: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryPage {
    pub rows: Vec<LogRow>,
    /// Cursor after the last row (forward continuation), if any rows.
    pub next_cursor: Option<String>,
    /// Cursor before the first row (backward continuation), if any rows.
    pub prev_cursor: Option<String>,
    /// More rows exist in the requested direction.
    pub has_more: bool,
    pub limit: u32,
}

const LOG_COLUMNS: &str = "record_id, event_time, severity_text, severity_number, \
     display_message, resource_id, trace_id, span_id, dataset_id, source_id, \
     record_number, line_start, attributes_json, provenance_json, \
     operation, outcome, event_name, event_type, request_id, \
     transaction_id, message_id, entity_id";

/// Number of columns `LOG_COLUMNS` selects. Queries that append their
/// own columns index from here; keep it in step with the list above.
const LOG_COLUMN_COUNT: usize = 22;

fn map_log_row(r: &duckdb::Row<'_>) -> Result<LogRow, duckdb::Error> {
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
}

/// One bounded keyset page under the shared filter.
pub fn query_page(
    engine: &EngineConnection,
    files: &[PathBuf],
    filter: &CompiledFilter,
    window: &ResolvedWindow,
    request: &PageRequest,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<QueryPage, QueryError> {
    let limit = request.limit.clamp(1, MAX_PAGE_SIZE);
    if files.is_empty() {
        return Ok(QueryPage {
            rows: vec![],
            next_cursor: None,
            prev_cursor: None,
            has_more: false,
            limit,
        });
    }
    let mut params: Vec<Value> = Vec::new();
    let win = window_sql(window, &mut params);
    let keyset = match &request.cursor {
        Some(c) => {
            let pos = decode_cursor(c)?;
            keyset_sql(&pos, request.backward, &mut params)
        }
        None => "true".to_string(),
    };
    params.extend(filter.params.iter().cloned());
    let order = if request.backward {
        ORDER_BACKWARD
    } else {
        ORDER_FORWARD
    };
    let sql = format!(
        "SELECT {LOG_COLUMNS} FROM {files} WHERE {win} AND {keyset} AND {expr} {order} LIMIT {fetch}",
        files = files_expr(files),
        expr = filter.where_sql,
        fetch = limit as i64 + 1,
    );
    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let _guard = install_temp_tables(conn, filter)?;
        let mut stmt = conn.prepare(&sql)?;
        let mut rows: Vec<LogRow> = stmt
            .query_map(duckdb::params_from_iter(params.iter()), map_log_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = rows.len() as u32 > limit;
        rows.truncate(limit as usize);
        if request.backward {
            rows.reverse();
        }
        let next_cursor = rows
            .last()
            .map(|r| encode_cursor(r.event_time, &r.record_id, &r.dataset_id));
        let prev_cursor = rows
            .first()
            .map(|r| encode_cursor(r.event_time, &r.record_id, &r.dataset_id));
        Ok(QueryPage {
            rows,
            next_cursor,
            prev_cursor,
            has_more,
            limit,
        })
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterCounts {
    pub matching: i64,
    /// Records matching the query but excluded from a bounded window
    /// because they have no valid event timestamp.
    pub omitted_untimestamped: i64,
}

/// Exact match count plus the omitted-untimestamped count for bounded
/// windows (the honesty counter behind "N results, M without timestamps").
pub fn query_counts(
    engine: &EngineConnection,
    files: &[PathBuf],
    filter: &CompiledFilter,
    window: &ResolvedWindow,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<FilterCounts, QueryError> {
    if files.is_empty() {
        return Ok(FilterCounts {
            matching: 0,
            omitted_untimestamped: 0,
        });
    }
    let mut params: Vec<Value> = Vec::new();
    let win = window_sql(window, &mut params);
    params.extend(filter.params.iter().cloned());
    let bounded = window.start.is_some() || window.end.is_some();
    let omitted_select = if bounded {
        ", count(*) FILTER (WHERE event_time IS NULL) "
    } else {
        ", 0 "
    };
    // For bounded windows the omitted count intentionally ignores the
    // window predicate (untimestamped records never satisfy it).
    let sql = if bounded {
        format!(
            "SELECT count(*) FILTER (WHERE {win}) {omitted_select} \
             FROM {files} WHERE {expr}",
            files = files_expr(files),
            expr = filter.where_sql,
        )
    } else {
        format!(
            "SELECT count(*) {omitted_select} FROM {files} WHERE {win} AND {expr}",
            files = files_expr(files),
            expr = filter.where_sql,
        )
    };
    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let _guard = install_temp_tables(conn, filter)?;
        let mut stmt = conn.prepare(&sql)?;
        let (matching, omitted): (i64, i64) = stmt
            .query_row(duckdb::params_from_iter(params.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        Ok(FilterCounts {
            matching,
            omitted_untimestamped: omitted,
        })
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramBin {
    /// Inclusive bin start (UTC nanos).
    pub start: i64,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Histogram {
    pub bins: Vec<HistogramBin>,
    pub bin_width_nanos: i64,
    /// Interval actually binned, half-open.
    pub start: i64,
    pub end: i64,
    pub total_in_range: i64,
    pub untimestamped_count: i64,
    /// True when the window had no timestamped data to bin.
    pub empty: bool,
}

/// Nice bin widths from 1 ms up to 30 days.
const NICE_WIDTHS_NANOS: &[i64] = &[
    1_000_000,
    5_000_000,
    25_000_000,
    100_000_000,
    250_000_000,
    1_000_000_000,
    5_000_000_000,
    15_000_000_000,
    30_000_000_000,
    60_000_000_000,
    300_000_000_000,
    900_000_000_000,
    1_800_000_000_000,
    3_600_000_000_000,
    10_800_000_000_000,
    21_600_000_000_000,
    43_200_000_000_000,
    86_400_000_000_000,
    604_800_000_000_000,
    2_592_000_000_000_000,
];

fn pick_bin_width(span: i64, max_bins: u32) -> i64 {
    let max_bins = max_bins.clamp(10, MAX_HISTOGRAM_BINS) as i64;
    for w in NICE_WIDTHS_NANOS {
        if span / w < max_bins {
            return *w;
        }
    }
    span / max_bins + 1
}

/// Histogram over the exact same filter + window as the table. Bin
/// boundaries are aligned to multiples of the width (stable under pans).
pub fn query_histogram(
    engine: &EngineConnection,
    files: &[PathBuf],
    filter: &CompiledFilter,
    window: &ResolvedWindow,
    max_bins: u32,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<Histogram, QueryError> {
    let empty = Histogram {
        bins: vec![],
        bin_width_nanos: 1,
        start: 0,
        end: 0,
        total_in_range: 0,
        untimestamped_count: 0,
        empty: true,
    };
    if files.is_empty() {
        return Ok(empty);
    }
    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    // Resolve the binning interval: the window when bounded, otherwise the
    // filtered data extent.
    let (range, untimestamped) = {
        let mut params: Vec<Value> = Vec::new();
        let win = window_sql(window, &mut params);
        params.extend(filter.params.iter().cloned());
        let sql = format!(
            "SELECT min(event_time), max(event_time), \
                    count(*) FILTER (WHERE event_time IS NULL) \
             FROM {files} WHERE {win} AND {expr}",
            files = files_expr(files),
            expr = filter.where_sql,
        );
        run_bounded(cancel, budget, || {
            let conn = engine.raw();
            let _guard = install_temp_tables(conn, filter)?;
            let mut stmt = conn.prepare(&sql)?;
            let (min, max, nulls): (Option<i64>, Option<i64>, i64) = stmt
                .query_row(duckdb::params_from_iter(params.iter()), |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
            Ok((min.zip(max), nulls))
        })?
    };
    let (start, end) = match (window.start, window.end) {
        (Some(s), Some(e)) => (s, e),
        _ => match range {
            Some((min, max)) => (min, max.saturating_add(1)),
            None => {
                return Ok(Histogram {
                    untimestamped_count: untimestamped,
                    ..empty
                })
            }
        },
    };
    if end <= start {
        return Ok(Histogram {
            untimestamped_count: untimestamped,
            ..empty
        });
    }
    let width = pick_bin_width(end - start, max_bins);
    let aligned_start = start.div_euclid(width) * width;
    let bin_count = ((end - aligned_start) as f64 / width as f64).ceil() as i64;

    let mut params: Vec<Value> = Vec::new();
    let win = window_sql(window, &mut params);
    params.extend(filter.params.iter().cloned());
    // The binning constants are engine-derived i64 (never user text); they
    // are inlined as literals so positional binding stays unambiguous.
    let sql = format!(
        "SELECT (event_time - {aligned_start}) // {width} AS bin, count(*) \
         FROM {files} WHERE {win} AND {expr} AND event_time IS NOT NULL \
           AND event_time >= {aligned_start} AND event_time < {end} \
         GROUP BY bin ORDER BY bin",
        files = files_expr(files),
        expr = filter.where_sql,
    );

    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let _guard = install_temp_tables(conn, filter)?;
        let mut stmt = conn.prepare(&sql)?;
        let mut filled: Vec<i64> = vec![0; bin_count.max(0) as usize];
        let mapped = stmt.query_map(duckdb::params_from_iter(params.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?;
        let mut total = 0i64;
        for row in mapped {
            let (bin, count) = row?;
            if bin >= 0 && (bin as usize) < filled.len() {
                filled[bin as usize] = count;
                total += count;
            }
        }
        let bins = filled
            .into_iter()
            .enumerate()
            .map(|(i, count)| HistogramBin {
                start: aligned_start + (i as i64) * width,
                count,
            })
            .collect();
        Ok(Histogram {
            bins,
            bin_width_nanos: width,
            start: aligned_start,
            end,
            total_in_range: total,
            untimestamped_count: untimestamped,
            empty: false,
        })
    })
}

/// What a facet/summary computation targets — produced from catalog
/// resolution only (never raw user text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldTarget {
    Canonical { field: CanonicalField },
    Attr { path: Vec<String> },
}

/// SQL value expression for a facet/summary target.
fn target_expr(target: &FieldTarget, params: &mut Vec<Value>) -> String {
    match target {
        FieldTarget::Canonical { field } => match field {
            CanonicalField::Severity => "(CASE \
                 WHEN severity_number BETWEEN 1 AND 4 THEN 'TRACE' \
                 WHEN severity_number BETWEEN 5 AND 8 THEN 'DEBUG' \
                 WHEN severity_number BETWEEN 9 AND 12 THEN 'INFO' \
                 WHEN severity_number BETWEEN 13 AND 16 THEN 'WARN' \
                 WHEN severity_number BETWEEN 17 AND 20 THEN 'ERROR' \
                 WHEN severity_number BETWEEN 21 AND 24 THEN 'FATAL' \
                 ELSE upper(severity_text) END)"
                .to_string(),
            CanonicalField::Timestamp => "event_time".to_string(),
            CanonicalField::ObservedTimestamp => "observed_time".to_string(),
            CanonicalField::SeverityText => "severity_text".to_string(),
            CanonicalField::SeverityNumber => "severity_number".to_string(),
            CanonicalField::Message => "display_message".to_string(),
            CanonicalField::EventName => "event_name".to_string(),
            CanonicalField::TraceId => "trace_id".to_string(),
            CanonicalField::SpanId => "span_id".to_string(),
            CanonicalField::Operation => "operation".to_string(),
            CanonicalField::Outcome => "outcome".to_string(),
            CanonicalField::EventType => "event_type".to_string(),
            CanonicalField::RequestId => "request_id".to_string(),
            CanonicalField::TransactionId => "transaction_id".to_string(),
            CanonicalField::MessageId => "message_id".to_string(),
            CanonicalField::EntityId => "entity_id".to_string(),
            CanonicalField::Dataset => "dataset_id".to_string(),
            CanonicalField::RecordId => "record_id".to_string(),
        },
        FieldTarget::Attr { path } => {
            params.push(Value::Text(attr_value_path(path)));
            "json_extract_string(attributes_json, ?)".to_string()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacetValue {
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FacetResult {
    pub display: String,
    pub values: Vec<FacetValue>,
    pub missing_count: i64,
    /// More distinct values exist beyond the returned top-K.
    pub truncated: bool,
}

/// Bounded top-K facets under the shared filter. One query per field, all
/// inside one budget.
#[allow(clippy::too_many_arguments)]
pub fn query_facets(
    engine: &EngineConnection,
    files: &[PathBuf],
    filter: &CompiledFilter,
    window: &ResolvedWindow,
    fields: &[(String, FieldTarget)],
    top_k: u32,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<Vec<FacetResult>, QueryError> {
    if fields.len() > MAX_FACET_FIELDS {
        return Err(QueryError::InvalidParameter(format!(
            "at most {MAX_FACET_FIELDS} facet fields per request"
        )));
    }
    let top_k = top_k.clamp(1, MAX_FACET_TOP_K);
    if files.is_empty() {
        return Ok(fields
            .iter()
            .map(|(display, _)| FacetResult {
                display: display.clone(),
                values: vec![],
                missing_count: 0,
                truncated: false,
            })
            .collect());
    }
    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let _guard = install_temp_tables(conn, filter)?;
        let mut out = Vec::with_capacity(fields.len());
        for (display, target) in fields {
            if cancel.was_cancelled() {
                return Err(QueryError::Cancelled);
            }
            // Parameters bind in SQL-text order: the target expression is
            // in the inner SELECT (first), then window, then filter.
            let mut params: Vec<Value> = Vec::new();
            let expr = target_expr(target, &mut params);
            let win = window_sql(window, &mut params);
            params.extend(filter.params.iter().cloned());
            let inner = format!(
                "SELECT {expr} AS v FROM {files} WHERE {win} AND {fexpr}",
                files = files_expr(files),
                fexpr = filter.where_sql,
            );
            let sql = format!(
                "SELECT CAST(v AS VARCHAR), count(*) AS c FROM ({inner}) \
                 WHERE v IS NOT NULL \
                 GROUP BY 1 ORDER BY c DESC, 1 LIMIT {fetch}",
                fetch = top_k as i64 + 1,
            );
            let mut stmt = conn.prepare(&sql)?;
            let mapped = stmt.query_map(duckdb::params_from_iter(params.iter()), |r| {
                Ok(FacetValue {
                    value: r.get::<_, String>(0)?,
                    count: r.get::<_, i64>(1)?,
                })
            })?;
            let mut values = mapped.collect::<Result<Vec<_>, _>>()?;
            let truncated = values.len() as u32 > top_k;
            values.truncate(top_k as usize);

            let missing_sql = format!("SELECT count(*) FROM ({inner}) WHERE v IS NULL");
            let mut stmt2 = conn.prepare(&missing_sql)?;
            let missing: i64 =
                stmt2.query_row(duckdb::params_from_iter(params.iter()), |r| r.get(0))?;

            out.push(FacetResult {
                display: display.clone(),
                values,
                missing_count: missing,
                truncated,
            });
        }
        Ok(out)
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSummary {
    pub display: String,
    pub present_count: i64,
    pub missing_count: i64,
    pub distinct_count: i64,
    /// False = `distinct_count` comes from `approx_count_distinct`.
    pub distinct_is_exact: bool,
    pub top_values: Vec<FacetValue>,
    /// Numeric/timestamp extent when type-compatible.
    pub min_numeric: Option<f64>,
    pub max_numeric: Option<f64>,
    pub high_cardinality: bool,
    pub types: Vec<AttrType>,
}

/// Threshold above which a field is flagged high-cardinality and distinct
/// counts switch to the approximate algorithm.
pub const HIGH_CARDINALITY_THRESHOLD: i64 = 10_000;

/// Field distribution summary under the shared filter.
#[allow(clippy::too_many_arguments)]
pub fn query_field_summary(
    engine: &EngineConnection,
    files: &[PathBuf],
    filter: &CompiledFilter,
    window: &ResolvedWindow,
    display: &str,
    target: &FieldTarget,
    numeric: bool,
    types: Vec<AttrType>,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<FieldSummary, QueryError> {
    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    if files.is_empty() {
        return Ok(FieldSummary {
            display: display.to_string(),
            present_count: 0,
            missing_count: 0,
            distinct_count: 0,
            distinct_is_exact: true,
            top_values: vec![],
            min_numeric: None,
            max_numeric: None,
            high_cardinality: false,
            types,
        });
    }
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let _guard = install_temp_tables(conn, filter)?;

        // Single expression occurrence inside a subquery keeps parameter
        // binding order correct (expression params first, then window,
        // then filter).
        let mut params: Vec<Value> = Vec::new();
        let expr = target_expr(target, &mut params);
        let win = window_sql(window, &mut params);
        params.extend(filter.params.iter().cloned());
        let inner = format!(
            "SELECT {expr} AS v FROM {files} WHERE {win} AND {fexpr}",
            files = files_expr(files),
            fexpr = filter.where_sql,
        );
        let numeric_expr = if numeric {
            "TRY_CAST(v AS DOUBLE)"
        } else {
            "NULL"
        };
        let sql = format!(
            "SELECT count(*), count(v), approx_count_distinct(v), \
                    min({numeric_expr}), max({numeric_expr}) FROM ({inner})"
        );
        let mut stmt = conn.prepare(&sql)?;
        #[allow(clippy::type_complexity)]
        let (total, present, approx_distinct, min_n, max_n): (
            i64,
            i64,
            i64,
            Option<f64>,
            Option<f64>,
        ) = stmt.query_row(duckdb::params_from_iter(params.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?;

        // Exact distinct + top values only below the cardinality threshold.
        let high_cardinality = approx_distinct > HIGH_CARDINALITY_THRESHOLD;
        let (distinct_count, distinct_is_exact, top_values) = if high_cardinality {
            (approx_distinct, false, vec![])
        } else {
            let sql2 = format!(
                "SELECT CAST(v AS VARCHAR), count(*) AS c FROM ({inner}) \
                 WHERE v IS NOT NULL GROUP BY 1 ORDER BY c DESC, 1 LIMIT 10"
            );
            let mut stmt2 = conn.prepare(&sql2)?;
            let top = stmt2
                .query_map(duckdb::params_from_iter(params.iter()), |r| {
                    Ok(FacetValue {
                        value: r.get::<_, String>(0)?,
                        count: r.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let sql3 = format!("SELECT count(DISTINCT v) FROM ({inner})");
            let mut stmt3 = conn.prepare(&sql3)?;
            let exact_distinct: i64 =
                stmt3.query_row(duckdb::params_from_iter(params.iter()), |r| r.get(0))?;
            (exact_distinct, true, top)
        };

        Ok(FieldSummary {
            display: display.to_string(),
            present_count: present,
            missing_count: total - present,
            distinct_count,
            distinct_is_exact,
            top_values,
            min_numeric: min_n,
            max_numeric: max_n,
            high_cardinality,
            types,
        })
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceContext {
    /// Neighbors in source order (including the anchor record), ascending
    /// by record number.
    pub records: Vec<LogRow>,
    pub anchor_record_id: String,
    /// Neighbors are canonical workspace copies; raw source availability is
    /// reported separately by the caller (file status lives in metadata).
    pub before_requested: u32,
    pub after_requested: u32,
    /// Requested record-number range (source order, inclusive).
    pub range_low: u64,
    pub range_high: u64,
}

/// Bounded source-order context: records adjacent by original record
/// number within the same physical origin (file/archive entry) — not
/// merely nearby timestamps.
#[allow(clippy::too_many_arguments)]
pub fn query_source_context(
    engine: &EngineConnection,
    files: &[PathBuf],
    dataset_id: &str,
    origin_id: &str,
    anchor_record_id: &str,
    anchor_record_number: u64,
    before: u32,
    after: u32,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<SourceContext, QueryError> {
    let before = before.min(MAX_CONTEXT_NEIGHBORS);
    let after = after.min(MAX_CONTEXT_NEIGHBORS);
    let low = anchor_record_number.saturating_sub(before as u64);
    let high = anchor_record_number.saturating_add(after as u64);
    if files.is_empty() {
        return Ok(SourceContext {
            records: vec![],
            anchor_record_id: anchor_record_id.to_string(),
            before_requested: before,
            after_requested: after,
            range_low: low,
            range_high: high,
        });
    }
    let sql = format!(
        "SELECT {LOG_COLUMNS} FROM {files} \
         WHERE dataset_id = ? AND origin_id = ? AND record_number BETWEEN ? AND ? \
         ORDER BY record_number, record_id",
        files = files_expr(files),
    );
    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<LogRow> = stmt
            .query_map(
                duckdb::params![dataset_id, origin_id, low, high],
                map_log_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SourceContext {
            records: rows,
            anchor_record_id: anchor_record_id.to_string(),
            before_requested: before,
            after_requested: after,
            range_low: low,
            range_high: high,
        })
    })
}

/// Streams the complete ordered result (bounded by `max_rows`) row by row
/// under the shared filter — the export path. Rows arrive in exactly the
/// table's total order; `on_row` returns `false` to stop early (byte caps).
/// The result set is never materialized on this side; DuckDB manages the
/// sort internally.
#[allow(clippy::too_many_arguments)]
pub fn stream_query(
    engine: &EngineConnection,
    files: &[PathBuf],
    filter: &CompiledFilter,
    window: &ResolvedWindow,
    max_rows: u64,
    cancel: &QueryCancelHandle,
    budget: Duration,
    mut on_row: impl FnMut(LogRow) -> Result<bool, QueryError>,
) -> Result<u64, QueryError> {
    if files.is_empty() {
        return Ok(0);
    }
    let mut params: Vec<Value> = Vec::new();
    let win = window_sql(window, &mut params);
    params.extend(filter.params.iter().cloned());
    let sql = format!(
        "SELECT {LOG_COLUMNS} FROM {files} WHERE {win} AND {expr} {ORDER_FORWARD} LIMIT {fetch}",
        files = files_expr(files),
        expr = filter.where_sql,
        fetch = max_rows.min(i64::MAX as u64),
    );
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let _guard = install_temp_tables(conn, filter)?;
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(duckdb::params_from_iter(params.iter()))?;
        let mut n = 0u64;
        while let Some(row) = rows.next()? {
            if n.is_multiple_of(1024) && cancel.was_cancelled() {
                return Err(QueryError::Cancelled);
            }
            let mapped = map_log_row(row)?;
            if !on_row(mapped)? {
                break;
            }
            n += 1;
        }
        Ok(n)
    })
}

/// Full-detail record: the hot row plus the cold columns the detail panel
/// distinguishes (typed body, scope, timestamp provenance).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordDetail {
    pub row: LogRow,
    pub scope_id: String,
    pub body_json: Option<String>,
    pub event_name: Option<String>,
    pub original_timestamp_text: Option<String>,
    pub timezone_assumption_json: Option<String>,
    pub observed_time: i64,
}

/// Fetches one record with every detail column.
pub fn fetch_record_detail(
    engine: &EngineConnection,
    files: &[PathBuf],
    dataset_id: &str,
    record_id: &str,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<Option<RecordDetail>, QueryError> {
    if files.is_empty() {
        return Ok(None);
    }
    let sql = format!(
        "SELECT {LOG_COLUMNS}, scope_id, body_json, \
                original_timestamp_text, timezone_assumption_json, observed_time \
         FROM {files} WHERE dataset_id = ? AND record_id = ? LIMIT 1",
        files = files_expr(files),
    );
    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(duckdb::params![dataset_id, record_id], |r| {
                let row = map_log_row(r)?;
                Ok(RecordDetail {
                    event_name: row.event_name.clone(),
                    row,
                    scope_id: r.get(LOG_COLUMN_COUNT)?,
                    body_json: r.get(LOG_COLUMN_COUNT + 1)?,
                    original_timestamp_text: r.get(LOG_COLUMN_COUNT + 2)?,
                    timezone_assumption_json: r.get(LOG_COLUMN_COUNT + 3)?,
                    observed_time: r.get(LOG_COLUMN_COUNT + 4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.pop())
    })
}

/// Fetches one complete record by identity.
pub fn fetch_record(
    engine: &EngineConnection,
    files: &[PathBuf],
    dataset_id: &str,
    record_id: &str,
    cancel: &QueryCancelHandle,
    budget: Option<Duration>,
) -> Result<Option<LogRow>, QueryError> {
    if files.is_empty() {
        return Ok(None);
    }
    let sql = format!(
        "SELECT {LOG_COLUMNS} FROM {files} WHERE dataset_id = ? AND record_id = ? LIMIT 1",
        files = files_expr(files),
    );
    let budget = budget.unwrap_or(Duration::from_millis(DEFAULT_BUDGET_MS));
    run_bounded(cancel, budget, || {
        let conn = engine.raw();
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(duckdb::params![dataset_id, record_id], map_log_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows.pop())
    })
}
