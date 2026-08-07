//! Correlation run execution (v0.4 WP4a) on the WP1 run lifecycle and
//! the `corr-rules` v1 model in `logscope-case::correlation`.
//!
//! Candidate generation is **key-partitioned, never pairwise**: one
//! bounded streaming pass buckets records by their validated group key,
//! so cost is linear in records scanned and no join can blow up on a
//! hot key. Every cap — events per group, groups per run, edges per
//! event, total edges, records scanned — reports what it dropped.
//!
//! Within a group, order is canonical event time then record ID (a
//! deterministic content address, so the tie-break is stable across
//! runs and machines). Records with no event time are counted in an
//! explicit undated bucket and never placed in the sequence: ordering
//! them by import time would manufacture a plausible story the data
//! does not support. Edges connect consecutive records only — the
//! bounded honest representation of "previous/next in this group" — and
//! carry the measured time delta without interpreting it.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;

use logscope_case::analysis::correlation_id;
use logscope_case::correlation::{
    explain_edge, explain_group, group_key, sequence_position, CorrelationFacts, CorrelationLimits,
    KeyNormalization, KeySelector, CORRELATION_RULE_ID, CORRELATION_RULE_VERSION,
};
use logscope_jobs::{JobContext, JobError};
use logscope_query::{
    stream_query, EngineConnection, LogRow, QueryCancelHandle, ResolvedWindow, TimeStrategy,
};
use logscope_workspace::{AnalysisRunRow, DerivedArtifactRow, Workspace};
use sha2::{Digest, Sha256};

use crate::analysis;
use crate::explorer;

fn ws_err(e: logscope_workspace::WorkspaceError) -> JobError {
    JobError::new(e.code(), e.to_string())
}

fn invalid(msg: impl std::fmt::Display) -> JobError {
    JobError::new("analysis/invalid-definition", msg.to_string())
}

fn engine_err(e: impl std::fmt::Display) -> JobError {
    JobError::new(
        "analysis/derived",
        format!("derived-data write failed: {e}"),
    )
}

pub const GROUPS_FILE: &str = "correlation_groups.parquet";
pub const EDGES_FILE: &str = "correlation_edges.parquet";
pub const RESULTS_SCHEMA_VERSION: i64 = 1;

/// Parsed correlation configuration (`config_json`).
#[derive(Debug, Clone)]
pub struct CorrelationConfig {
    pub selector: KeySelector,
    pub normalization: KeyNormalization,
    pub limits: CorrelationLimits,
    pub max_records: u64,
    pub budget_seconds: u64,
}

/// Strict wire shape: unknown keys are refused, so a typo'd limit can
/// never silently become a default.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCorrelationConfig {
    key: String,
    #[serde(default)]
    attribute: Option<String>,
    #[serde(default)]
    normalization: KeyNormalization,
    #[serde(default)]
    max_records: Option<i64>,
    #[serde(default)]
    budget_seconds: Option<i64>,
}

impl CorrelationConfig {
    /// Parses config and limits together: the selector, its
    /// normalization, and the bounds are one decision.
    pub fn parse(config_json: &str, limits_json: &str) -> Result<CorrelationConfig, JobError> {
        let raw: RawCorrelationConfig = serde_json::from_str(config_json)
            .map_err(|e| invalid(format!("correlation config does not parse: {e}")))?;
        let selector = KeySelector::parse(&raw.key, raw.attribute.as_deref()).map_err(invalid)?;
        raw.normalization.validate_for(&selector).map_err(invalid)?;
        let limits = CorrelationLimits::parse(limits_json).map_err(invalid)?;
        Ok(CorrelationConfig {
            selector,
            normalization: raw.normalization,
            limits,
            max_records: raw.max_records.unwrap_or(5_000_000).max(1) as u64,
            budget_seconds: raw.budget_seconds.unwrap_or(3_600).clamp(1, 24 * 3600) as u64,
        })
    }
}

/// One correlation group (also the groups parquet schema, version 1).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorrelationGroup {
    pub group_id: String,
    pub key_selector: String,
    pub key_value: String,
    pub confidence: String,
    /// Records placed in the ordered sequence.
    pub event_count: u64,
    /// Records that carry the key but have no event time. Counted, never
    /// ordered, never dropped silently.
    pub undated_count: u64,
    /// Records beyond `max_events_per_group` that this group could not
    /// hold.
    pub truncated_count: u64,
    pub first_event_time: Option<i64>,
    pub last_event_time: Option<i64>,
    /// Distinct resources the group spans (bounded, sorted JSON array).
    pub resources_json: String,
    pub edge_count: u64,
    pub rule_id: String,
    pub rule_version: i64,
    pub reason: String,
}

/// One edge between consecutive records of a group (edges parquet
/// schema, version 1).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorrelationEdge {
    pub edge_id: String,
    pub group_id: String,
    pub from_record_id: String,
    pub to_record_id: String,
    pub from_event_time: i64,
    pub to_event_time: i64,
    /// `to - from` in nanoseconds. Reported as measured; a negative
    /// value is a real observation about the data, not an error to fix.
    pub delta_nanos: i64,
    pub confidence: String,
    pub rule_id: String,
    pub rule_version: i64,
    pub reason: String,
}

/// Per-group accumulation during the streaming pass.
struct GroupAcc {
    dated: Vec<(i64, String)>,
    undated: u64,
    truncated: u64,
    resources: std::collections::BTreeSet<String>,
    resources_truncated: bool,
    /// Normalization steps that changed at least one member's value
    /// before it matched. Union across the group, reported in the fixed
    /// rule order.
    applied: std::collections::BTreeSet<&'static str>,
}

/// The order normalization steps are applied in, so a reported list
/// reads the way the rule ran rather than alphabetically.
const NORMALIZATION_ORDER: [&str; 3] = ["trim", "strip_prefix", "case_fold"];

const MAX_RESOURCES_PER_GROUP: usize = 32;

struct ScanCounts {
    scanned: u64,
    keyed: u64,
    rejected: BTreeMap<&'static str, u64>,
    groups_truncated: u64,
}

fn attr_str(row: &LogRow, field: &str) -> Option<String> {
    let attrs = logscope_model::attrs_from_canonical_json(&row.attributes_json).ok()?;
    match attrs.get(field) {
        Some(logscope_model::AnyValue::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Borrows the correlation-relevant fields out of a canonical row.
/// `attribute_value` is resolved by the caller because it may require
/// decoding the typed attribute JSON.
fn facts<'a>(row: &'a LogRow, attribute_value: Option<&'a str>) -> CorrelationFacts<'a> {
    CorrelationFacts {
        record_id: &row.record_id,
        event_time: row.event_time,
        trace_id: row.trace_id.as_deref(),
        span_id: row.span_id.as_deref(),
        request_id: row.request_id.as_deref(),
        transaction_id: row.transaction_id.as_deref(),
        message_id: row.message_id.as_deref(),
        entity_id: row.entity_id.as_deref(),
        attribute: attribute_value,
    }
}

struct CompiledBase {
    files: Vec<PathBuf>,
    filter: logscope_query::CompiledFilter,
    strategy: TimeStrategy,
    start: Option<i64>,
    end: Option<i64>,
}

fn compiled_base(ws: &Workspace, run: &AnalysisRunRow) -> Result<CompiledBase, JobError> {
    let def = ws
        .meta
        .get_analysis_definition(&run.definition_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("analysis definition {} does not exist", run.definition_id),
            )
        })?;
    let dataset_ids: Vec<String> =
        serde_json::from_str(&def.dataset_selection_json).unwrap_or_default();
    let selection = explorer::resolve_dataset_selection(ws, &dataset_ids).map_err(ws_err)?;
    let analysis_q = explorer::analyze_query(ws, &selection, &def.query_text);
    let filter = explorer::compile_for_execution(ws, &selection, &analysis_q)?;
    let files = explorer::segment_files_for(ws, &selection).map_err(ws_err)?;
    let strategy: TimeStrategy =
        serde_json::from_str(&def.time_strategy_json).unwrap_or(TimeStrategy::All);
    // The run froze its window; drill-down and execution both use it
    // rather than re-resolving against a moving latest event.
    let bounds: serde_json::Value = serde_json::from_str(&run.bounds_json).unwrap_or_default();
    Ok(CompiledBase {
        files,
        filter,
        strategy,
        start: bounds.get("start").and_then(|v| v.as_i64()),
        end: bounds.get("end").and_then(|v| v.as_i64()),
    })
}

impl CompiledBase {
    fn window(&self) -> ResolvedWindow {
        ResolvedWindow {
            strategy: self.strategy.clone(),
            start: self.start,
            end: self.end,
            empty_anchor: false,
        }
    }
}

/// Runs a correlation analysis end to end on the two-phase lifecycle.
/// The returned row is always terminal.
pub fn run_correlation_analysis(
    ws: &Workspace,
    engine: &EngineConnection,
    definition_id: &str,
    ctx: &JobContext,
) -> Result<AnalysisRunRow, JobError> {
    let def = ws
        .meta
        .get_analysis_definition(definition_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("analysis definition {definition_id} does not exist"),
            )
        })?;
    if def.kind != "correlation" {
        return Err(invalid(format!(
            "kind {:?} is not a correlation analysis",
            def.kind
        )));
    }
    let cfg = CorrelationConfig::parse(&def.config_json, &def.limits_json)?;

    let run = analysis::begin_run(ws, definition_id)?;
    analysis::mark_running(ws, &run.run_id)?;
    match execute(ws, engine, &run, &cfg, ctx) {
        Ok(row) => Ok(row),
        Err(e) => {
            let cancelled = e.code == "job/cancelled";
            let terminal = analysis::abort_run(ws, &run.run_id, cancelled, &e)?;
            if cancelled {
                Ok(terminal)
            } else {
                Err(e)
            }
        }
    }
}

fn execute(
    ws: &Workspace,
    engine: &EngineConnection,
    run: &AnalysisRunRow,
    cfg: &CorrelationConfig,
    ctx: &JobContext,
) -> Result<AnalysisRunRow, JobError> {
    // A job cancelled before its first checkpoint must still be
    // terminal, never an empty success.
    if ctx.control.checkpoint().is_err() {
        return Err(JobError::new("job/cancelled", "the job was cancelled"));
    }
    let base = compiled_base(ws, run)?;
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());

    let mut groups: BTreeMap<String, GroupAcc> = BTreeMap::new();
    let mut counts = ScanCounts {
        scanned: 0,
        keyed: 0,
        rejected: BTreeMap::new(),
        groups_truncated: 0,
    };
    let attribute_field = match &cfg.selector {
        KeySelector::Attribute(field) => Some(field.clone()),
        _ => None,
    };
    let mut cancelled = false;
    stream_query(
        engine,
        &base.files,
        &base.filter,
        &base.window(),
        cfg.max_records,
        &cancel,
        std::time::Duration::from_secs(cfg.budget_seconds),
        |row| {
            counts.scanned += 1;
            if counts.scanned.is_multiple_of(4096) && ctx.control.checkpoint().is_err() {
                cancelled = true;
                cancel.cancel();
                return Ok(false);
            }
            let attribute_value = attribute_field.as_deref().and_then(|f| attr_str(&row, f));
            let facts = facts(&row, attribute_value.as_deref());
            let (key, applied) = match group_key(&cfg.selector, &cfg.normalization, &facts) {
                Ok(k) => k,
                Err(rejection) => {
                    *counts.rejected.entry(rejection.as_str()).or_insert(0) += 1;
                    return Ok(true);
                }
            };
            counts.keyed += 1;
            let acc = match groups.get_mut(&key) {
                Some(acc) => acc,
                None => {
                    if groups.len() >= cfg.limits.max_groups {
                        // High-cardinality guard: the key domain is
                        // wider than the run may hold. Counted, not
                        // silently narrowed.
                        counts.groups_truncated += 1;
                        return Ok(true);
                    }
                    groups.entry(key.clone()).or_insert(GroupAcc {
                        dated: Vec::new(),
                        undated: 0,
                        truncated: 0,
                        resources: std::collections::BTreeSet::new(),
                        resources_truncated: false,
                        applied: std::collections::BTreeSet::new(),
                    })
                }
            };
            // Which steps actually changed a member's value before it
            // matched. Recomputing this from the stored key would say
            // "nothing changed", because the stored key is already the
            // normalized value.
            acc.applied.extend(applied);
            if acc.resources.len() < MAX_RESOURCES_PER_GROUP {
                acc.resources.insert(row.resource_id.clone());
            } else if !acc.resources.contains(&row.resource_id) {
                acc.resources_truncated = true;
            }
            match sequence_position(&facts) {
                None => acc.undated += 1,
                Some((event_time, record_id)) => {
                    if acc.dated.len() >= cfg.limits.max_events_per_group {
                        acc.truncated += 1;
                    } else {
                        acc.dated.push((event_time, record_id.to_string()));
                    }
                }
            }
            Ok(true)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    if cancelled {
        return Err(JobError::new("job/cancelled", "the job was cancelled"));
    }

    let confidence = cfg.selector.confidence();
    let mut group_rows: Vec<CorrelationGroup> = Vec::new();
    let mut edge_rows: Vec<CorrelationEdge> = Vec::new();
    let mut singleton_keys: u64 = 0;
    let mut edges_dropped_over_limit: u64 = 0;

    for (key, mut acc) in groups {
        let members = acc.dated.len() as u64 + acc.undated + acc.truncated;
        if members < cfg.limits.min_group_size as u64 {
            singleton_keys += 1;
            continue;
        }
        // Canonical order: event time, then the deterministic record ID.
        acc.dated.sort();
        let participants: Vec<String> = acc.dated.iter().map(|(_, id)| id.clone()).collect();
        let group_id = correlation_id(
            &run.semantic_fingerprint,
            CORRELATION_RULE_ID,
            CORRELATION_RULE_VERSION,
            &format!("{}={key}", cfg.selector.as_str()),
            &participants,
        );
        let applied: Vec<&str> = NORMALIZATION_ORDER
            .iter()
            .filter(|step| acc.applied.contains(*step))
            .copied()
            .collect();
        let reason = explain_group(&cfg.selector, &cfg.normalization, &key, &applied);

        let mut edges_here = 0u64;
        for pair in acc.dated.windows(2) {
            let (from_time, from_id) = &pair[0];
            let (to_time, to_id) = &pair[1];
            if edge_rows.len() >= cfg.limits.max_total_edges
                || edges_here >= cfg.limits.max_edges_per_event as u64
            {
                edges_dropped_over_limit += 1;
                continue;
            }
            let delta = to_time - from_time;
            edge_rows.push(CorrelationEdge {
                edge_id: correlation_id(
                    &run.semantic_fingerprint,
                    CORRELATION_RULE_ID,
                    CORRELATION_RULE_VERSION,
                    &format!("{}={key}#edge", cfg.selector.as_str()),
                    &[from_id.clone(), to_id.clone()],
                ),
                group_id: group_id.clone(),
                from_record_id: from_id.clone(),
                to_record_id: to_id.clone(),
                from_event_time: *from_time,
                to_event_time: *to_time,
                delta_nanos: delta,
                confidence: confidence.as_str().to_string(),
                rule_id: CORRELATION_RULE_ID.to_string(),
                rule_version: CORRELATION_RULE_VERSION,
                reason: explain_edge(&cfg.selector, &key, delta),
            });
            edges_here += 1;
        }

        let mut resources: Vec<&String> = acc.resources.iter().collect();
        resources.sort();
        group_rows.push(CorrelationGroup {
            group_id,
            key_selector: cfg.selector.as_str().to_string(),
            key_value: key,
            confidence: confidence.as_str().to_string(),
            event_count: acc.dated.len() as u64,
            undated_count: acc.undated,
            truncated_count: acc.truncated,
            first_event_time: acc.dated.first().map(|(t, _)| *t),
            last_event_time: acc.dated.last().map(|(t, _)| *t),
            resources_json: serde_json::to_string(&resources).unwrap_or_else(|_| "[]".into()),
            edge_count: edges_here,
            rule_id: CORRELATION_RULE_ID.to_string(),
            rule_version: CORRELATION_RULE_VERSION,
            reason,
        });
    }

    // Deterministic stored order: larger groups first, then key.
    group_rows.sort_by(|a, b| {
        (b.event_count + b.undated_count + b.truncated_count)
            .cmp(&(a.event_count + a.undated_count + a.truncated_count))
            .then(a.key_value.cmp(&b.key_value))
    });
    edge_rows.sort_by(|a, b| {
        a.group_id
            .cmp(&b.group_id)
            .then(a.from_event_time.cmp(&b.from_event_time))
            .then(a.from_record_id.cmp(&b.from_record_id))
    });

    let dir = ws.layout.derived_analysis_dir(&run.run_id);
    std::fs::create_dir_all(&dir).map_err(|e| engine_err(format!("{}: {e}", dir.display())))?;
    let groups_path = dir.join(GROUPS_FILE);
    let edges_path = dir.join(EDGES_FILE);
    write_groups_parquet(engine, &group_rows, &groups_path)?;
    write_edges_parquet(engine, &edge_rows, &edges_path)?;
    let (groups_bytes, groups_sha) = hash_file(&groups_path)?;
    let (edges_bytes, edges_sha) = hash_file(&edges_path)?;
    for (kind, file, rows, bytes, sha) in [
        (
            "correlation_groups",
            GROUPS_FILE,
            group_rows.len(),
            groups_bytes,
            &groups_sha,
        ),
        (
            "correlation_edges",
            EDGES_FILE,
            edge_rows.len(),
            edges_bytes,
            &edges_sha,
        ),
    ] {
        ws.meta
            .record_derived_artifact(&DerivedArtifactRow {
                artifact_id: format!("dart-{}", uuid::Uuid::new_v4()),
                run_id: run.run_id.clone(),
                kind: kind.into(),
                rel_path: format!("derived/analysis/{}/{file}", run.run_id),
                row_count: rows as i64,
                byte_size: bytes,
                sha256: sha.clone(),
                schema_version: RESULTS_SCHEMA_VERSION,
                created_at: chrono::Utc::now().to_rfc3339(),
            })
            .map_err(ws_err)?;
    }

    let undated_total: u64 = group_rows.iter().map(|g| g.undated_count).sum();
    let truncated_total: u64 = group_rows.iter().map(|g| g.truncated_count).sum();
    let counts_json = serde_json::json!({
        "scanned": counts.scanned,
        "keyed": counts.keyed,
        "rejected": counts.rejected,
        "groups": group_rows.len(),
        "edges": edge_rows.len(),
        "singleton_keys": singleton_keys,
        "undated_in_groups": undated_total,
        "events_truncated_in_groups": truncated_total,
        "records_over_group_limit": counts.groups_truncated,
        "edges_dropped_over_limit": edges_dropped_over_limit,
    });
    let manifest = serde_json::json!({
        "key_selector": cfg.selector.as_str(),
        "confidence": confidence.as_str(),
        "normalization": cfg.normalization,
        "limits": cfg.limits,
        "groups": {
            "file": GROUPS_FILE,
            "rows": group_rows.len(),
            "sha256": groups_sha,
            "schema_version": RESULTS_SCHEMA_VERSION,
        },
        "edges": {
            "file": EDGES_FILE,
            "rows": edge_rows.len(),
            "sha256": edges_sha,
            "schema_version": RESULTS_SCHEMA_VERSION,
        },
    });
    analysis::complete_run(
        ws,
        &run.run_id,
        &counts_json.to_string(),
        &manifest.to_string(),
    )
}

fn hash_file(path: &std::path::Path) -> Result<(i64, String), JobError> {
    let mut f =
        std::fs::File::open(path).map_err(|e| engine_err(format!("{}: {e}", path.display())))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: i64 = 0;
    loop {
        let n = f.read(&mut buf).map_err(engine_err)?;
        if n == 0 {
            break;
        }
        total += n as i64;
        hasher.update(&buf[..n]);
    }
    Ok((total, format!("{:x}", hasher.finalize())))
}

fn sql_quote(path: &std::path::Path) -> String {
    path.display()
        .to_string()
        .replace('\'', "''")
        .replace('\\', "/")
}

fn write_groups_parquet(
    engine: &EngineConnection,
    rows: &[CorrelationGroup],
    out_path: &std::path::Path,
) -> Result<(), JobError> {
    let conn = engine.raw();
    conn.execute_batch(
        "CREATE OR REPLACE TEMP TABLE __ls_corr_groups(
            group_id VARCHAR, key_selector VARCHAR, key_value VARCHAR, confidence VARCHAR,
            event_count UBIGINT, undated_count UBIGINT, truncated_count UBIGINT,
            first_event_time BIGINT, last_event_time BIGINT, resources_json VARCHAR,
            edge_count UBIGINT, rule_id VARCHAR, rule_version BIGINT, reason VARCHAR,
            sort_no UBIGINT); BEGIN",
    )
    .map_err(engine_err)?;
    {
        let mut stmt = conn
            .prepare("INSERT INTO __ls_corr_groups VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .map_err(engine_err)?;
        for (i, g) in rows.iter().enumerate() {
            stmt.execute(duckdb::params![
                g.group_id,
                g.key_selector,
                g.key_value,
                g.confidence,
                g.event_count,
                g.undated_count,
                g.truncated_count,
                g.first_event_time,
                g.last_event_time,
                g.resources_json,
                g.edge_count,
                g.rule_id,
                g.rule_version,
                g.reason,
                i as u64,
            ])
            .map_err(engine_err)?;
        }
    }
    conn.execute_batch(&format!(
        "COMMIT; COPY (SELECT group_id, key_selector, key_value, confidence, event_count,
            undated_count, truncated_count, first_event_time, last_event_time,
            resources_json, edge_count, rule_id, rule_version, reason
            FROM __ls_corr_groups ORDER BY sort_no)
         TO '{}' (FORMAT PARQUET);
         DROP TABLE __ls_corr_groups;",
        sql_quote(out_path)
    ))
    .map_err(engine_err)?;
    Ok(())
}

fn write_edges_parquet(
    engine: &EngineConnection,
    rows: &[CorrelationEdge],
    out_path: &std::path::Path,
) -> Result<(), JobError> {
    let conn = engine.raw();
    conn.execute_batch(
        "CREATE OR REPLACE TEMP TABLE __ls_corr_edges(
            edge_id VARCHAR, group_id VARCHAR, from_record_id VARCHAR, to_record_id VARCHAR,
            from_event_time BIGINT, to_event_time BIGINT, delta_nanos BIGINT,
            confidence VARCHAR, rule_id VARCHAR, rule_version BIGINT, reason VARCHAR,
            sort_no UBIGINT); BEGIN",
    )
    .map_err(engine_err)?;
    {
        let mut stmt = conn
            .prepare("INSERT INTO __ls_corr_edges VALUES (?,?,?,?,?,?,?,?,?,?,?,?)")
            .map_err(engine_err)?;
        for (i, e) in rows.iter().enumerate() {
            stmt.execute(duckdb::params![
                e.edge_id,
                e.group_id,
                e.from_record_id,
                e.to_record_id,
                e.from_event_time,
                e.to_event_time,
                e.delta_nanos,
                e.confidence,
                e.rule_id,
                e.rule_version,
                e.reason,
                i as u64,
            ])
            .map_err(engine_err)?;
        }
    }
    conn.execute_batch(&format!(
        "COMMIT; COPY (SELECT edge_id, group_id, from_record_id, to_record_id,
            from_event_time, to_event_time, delta_nanos, confidence, rule_id,
            rule_version, reason FROM __ls_corr_edges ORDER BY sort_no)
         TO '{}' (FORMAT PARQUET);
         DROP TABLE __ls_corr_edges;",
        sql_quote(out_path)
    ))
    .map_err(engine_err)?;
    Ok(())
}

fn require_completed_run(ws: &Workspace, run_id: &str) -> Result<AnalysisRunRow, JobError> {
    let run = ws
        .meta
        .get_analysis_run(run_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("analysis run {run_id} does not exist"),
            )
        })?;
    if run.state != "completed" && run.state != "stale" {
        return Err(invalid(format!(
            "run {run_id} is {}; only completed runs have results",
            run.state
        )));
    }
    Ok(run)
}

fn derived_file(ws: &Workspace, run: &AnalysisRunRow, file: &str) -> Result<PathBuf, JobError> {
    let path = ws.layout.derived_analysis_dir(&run.run_id).join(file);
    if !path.exists() {
        return Err(JobError::new(
            "analysis/derived",
            format!("{file} is missing; delete and re-run the analysis (rebuild lands in WP8)"),
        ));
    }
    Ok(path)
}

/// One page of correlation groups in the stored deterministic order.
pub fn list_correlation_groups(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    offset: u64,
    limit: u64,
) -> Result<Vec<CorrelationGroup>, JobError> {
    let run = require_completed_run(ws, run_id)?;
    let path = derived_file(ws, &run, GROUPS_FILE)?;
    let limit = limit.clamp(1, 1_000);
    let conn = engine.raw();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT group_id, key_selector, key_value, confidence, event_count, undated_count,
                    truncated_count, first_event_time, last_event_time, resources_json,
                    edge_count, rule_id, rule_version, reason
             FROM read_parquet('{}') LIMIT {limit} OFFSET {offset}",
            sql_quote(&path)
        ))
        .map_err(engine_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(CorrelationGroup {
                group_id: r.get(0)?,
                key_selector: r.get(1)?,
                key_value: r.get(2)?,
                confidence: r.get(3)?,
                event_count: r.get(4)?,
                undated_count: r.get(5)?,
                truncated_count: r.get(6)?,
                first_event_time: r.get(7)?,
                last_event_time: r.get(8)?,
                resources_json: r.get(9)?,
                edge_count: r.get(10)?,
                rule_id: r.get(11)?,
                rule_version: r.get(12)?,
                reason: r.get(13)?,
            })
        })
        .map_err(engine_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(engine_err)?;
    Ok(rows)
}

/// The bounded ordered edges of one group (previous/next relationships).
pub fn list_correlation_edges(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    group_id: &str,
    limit: u64,
) -> Result<Vec<CorrelationEdge>, JobError> {
    let run = require_completed_run(ws, run_id)?;
    let path = derived_file(ws, &run, EDGES_FILE)?;
    let limit = limit.clamp(1, 1_000);
    let conn = engine.raw();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT edge_id, group_id, from_record_id, to_record_id, from_event_time,
                    to_event_time, delta_nanos, confidence, rule_id, rule_version, reason
             FROM read_parquet('{}') WHERE group_id = ? LIMIT {limit}",
            sql_quote(&path)
        ))
        .map_err(engine_err)?;
    let rows = stmt
        .query_map(duckdb::params![group_id], |r| {
            Ok(CorrelationEdge {
                edge_id: r.get(0)?,
                group_id: r.get(1)?,
                from_record_id: r.get(2)?,
                to_record_id: r.get(3)?,
                from_event_time: r.get(4)?,
                to_event_time: r.get(5)?,
                delta_nanos: r.get(6)?,
                confidence: r.get(7)?,
                rule_id: r.get(8)?,
                rule_version: r.get(9)?,
                reason: r.get(10)?,
            })
        })
        .map_err(engine_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(engine_err)?;
    Ok(rows)
}

/// Deterministic drill-down to a group's member records: re-streams the
/// run's OWN frozen window and re-derives the key with the same
/// versioned configuration. Refused when the run's inputs moved.
pub fn correlation_records(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    group_id: &str,
    limit: usize,
) -> Result<Vec<LogRow>, JobError> {
    let run = require_completed_run(ws, run_id)?;
    if let Some(reason) = analysis::check_run_current(ws, &run)? {
        return Err(JobError::new(
            "analysis/stale-run",
            format!("drill-down refused: {reason}; re-run the analysis"),
        ));
    }
    let group = list_group_by_id(ws, engine, &run, group_id)?;
    let def = ws
        .meta
        .get_analysis_definition(&run.definition_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("analysis definition {} does not exist", run.definition_id),
            )
        })?;
    let cfg = CorrelationConfig::parse(&def.config_json, &def.limits_json)?;
    let base = compiled_base(ws, &run)?;
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let attribute_field = match &cfg.selector {
        KeySelector::Attribute(field) => Some(field.clone()),
        _ => None,
    };
    let limit = limit.clamp(1, 10_000);
    let mut hits: Vec<LogRow> = Vec::new();
    stream_query(
        engine,
        &base.files,
        &base.filter,
        &base.window(),
        cfg.max_records,
        &cancel,
        std::time::Duration::from_secs(cfg.budget_seconds),
        |row| {
            let attribute_value = attribute_field.as_deref().and_then(|f| attr_str(&row, f));
            let facts = facts(&row, attribute_value.as_deref());
            if let Ok((key, _)) = group_key(&cfg.selector, &cfg.normalization, &facts) {
                if key == group.key_value {
                    hits.push(row);
                    if hits.len() >= limit {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    // Same canonical order the group used; undated members sort last
    // rather than being interleaved by import time.
    hits.sort_by(|a, b| match (a.event_time, b.event_time) {
        (Some(x), Some(y)) => x.cmp(&y).then(a.record_id.cmp(&b.record_id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.record_id.cmp(&b.record_id),
    });
    Ok(hits)
}

fn list_group_by_id(
    ws: &Workspace,
    engine: &EngineConnection,
    run: &AnalysisRunRow,
    group_id: &str,
) -> Result<CorrelationGroup, JobError> {
    let path = derived_file(ws, run, GROUPS_FILE)?;
    let conn = engine.raw();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT group_id, key_selector, key_value, confidence, event_count, undated_count,
                    truncated_count, first_event_time, last_event_time, resources_json,
                    edge_count, rule_id, rule_version, reason
             FROM read_parquet('{}') WHERE group_id = ? LIMIT 1",
            sql_quote(&path)
        ))
        .map_err(engine_err)?;
    let mut rows = stmt
        .query_map(duckdb::params![group_id], |r| {
            Ok(CorrelationGroup {
                group_id: r.get(0)?,
                key_selector: r.get(1)?,
                key_value: r.get(2)?,
                confidence: r.get(3)?,
                event_count: r.get(4)?,
                undated_count: r.get(5)?,
                truncated_count: r.get(6)?,
                first_event_time: r.get(7)?,
                last_event_time: r.get(8)?,
                resources_json: r.get(9)?,
                edge_count: r.get(10)?,
                rule_id: r.get(11)?,
                rule_version: r.get(12)?,
                reason: r.get(13)?,
            })
        })
        .map_err(engine_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(engine_err)?;
    rows.pop().ok_or_else(|| {
        JobError::new(
            "workspace/missing-entity",
            format!("group {group_id} is not part of run {}", run.run_id),
        )
    })
}
