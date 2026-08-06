//! Baseline-versus-suspect comparison execution (v0.4 WP3) on the WP1
//! run lifecycle and the WP3 pure decision table.
//!
//! One compiled base query serves BOTH windows — only the concrete time
//! predicate differs, so the two sides can never drift semantically.
//! Each side is one bounded streaming pass into an order-independent
//! per-key count map; the candidate domain is the exact bounded union
//! of both sides (never two unrelated top-K lists); classification is
//! pure integer arithmetic in `logscope-case::comparison`. Results
//! publish as a deterministic parquet under `derived/analysis/<run>/`
//! before the two-phase run completes. Untimestamped records can never
//! enter a bounded window; they are counted explicitly in a third
//! count-only pass rather than silently vanishing.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;

use logscope_case::analysis::comparison_result_id;
use logscope_case::comparison::{
    classify, Classified, ComparisonThresholds, COMPARISON_RULE_ID, COMPARISON_RULE_VERSION,
};
use logscope_case::stack::{parse_stack, StackQuality};
use logscope_case::template::{normalize_message, MaskSet};
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

pub const RESULTS_FILE: &str = "comparison.parquet";
pub const RESULTS_SCHEMA_VERSION: i64 = 1;

/// Parsed comparison configuration (`config_json`). Windows are
/// explicit concrete UTC nanos, half-open; overlap is refused.
#[derive(Debug, Clone)]
pub struct ComparisonConfig {
    pub dimension: String,
    pub attribute: Option<String>,
    pub baseline_start: i64,
    pub baseline_end: i64,
    pub suspect_start: i64,
    pub suspect_end: i64,
    pub top_k: usize,
    pub max_keys_per_side: usize,
    pub max_records: u64,
    pub budget_seconds: u64,
}

const DIMENSIONS: &[&str] = &[
    "message_pattern",
    "stack_fingerprint",
    "severity",
    "resource",
    "attribute",
];

/// Strict wire shape: unknown keys are refused (a typo'd limit must
/// never silently become a default), mirroring the mask-set and
/// thresholds parsers.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComparisonConfig {
    dimension: String,
    #[serde(default)]
    attribute: Option<String>,
    baseline_start: i64,
    baseline_end: i64,
    suspect_start: i64,
    suspect_end: i64,
    #[serde(default)]
    top_k: Option<i64>,
    #[serde(default)]
    max_keys_per_side: Option<i64>,
    #[serde(default)]
    max_records: Option<i64>,
    #[serde(default)]
    budget_seconds: Option<i64>,
}

impl ComparisonConfig {
    pub fn parse(config_json: &str) -> Result<ComparisonConfig, JobError> {
        let raw: RawComparisonConfig = serde_json::from_str(config_json)
            .map_err(|e| invalid(format!("comparison config does not parse: {e}")))?;
        if !DIMENSIONS.contains(&raw.dimension.as_str()) {
            return Err(invalid(format!(
                "unknown comparison dimension {:?} (expected one of {})",
                raw.dimension,
                DIMENSIONS.join("|")
            )));
        }
        if raw.dimension == "attribute" && raw.attribute.as_deref().unwrap_or("").trim().is_empty()
        {
            return Err(invalid(
                "the attribute dimension requires config.attribute naming a field",
            ));
        }
        let cfg = ComparisonConfig {
            dimension: raw.dimension,
            attribute: raw.attribute,
            baseline_start: raw.baseline_start,
            baseline_end: raw.baseline_end,
            suspect_start: raw.suspect_start,
            suspect_end: raw.suspect_end,
            top_k: raw.top_k.unwrap_or(100).clamp(1, 1_000) as usize,
            max_keys_per_side: raw.max_keys_per_side.unwrap_or(10_000).clamp(1, 200_000) as usize,
            max_records: raw.max_records.unwrap_or(5_000_000).max(1) as u64,
            budget_seconds: raw.budget_seconds.unwrap_or(3_600).clamp(1, 24 * 3600) as u64,
        };
        if cfg.baseline_end <= cfg.baseline_start || cfg.suspect_end <= cfg.suspect_start {
            return Err(invalid(
                "comparison windows must be half-open with end > start",
            ));
        }
        // Overlap is refused (v1: an overlapping comparison is a design
        // choice the UI must make explicit before this is widened).
        if cfg.baseline_start < cfg.suspect_end && cfg.suspect_start < cfg.baseline_end {
            return Err(invalid(format!(
                "baseline [{}, {}) and suspect [{}, {}) overlap; comparison windows must be disjoint",
                cfg.baseline_start, cfg.baseline_end, cfg.suspect_start, cfg.suspect_end
            )));
        }
        Ok(cfg)
    }

    pub fn bounds_json(&self) -> serde_json::Value {
        serde_json::json!({
            "baseline": { "start": self.baseline_start, "end": self.baseline_end },
            "suspect": { "start": self.suspect_start, "end": self.suspect_end },
        })
    }
}

/// One classified comparison row (also the parquet schema, version 1).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ComparisonResult {
    pub result_id: String,
    pub dimension: String,
    pub key: String,
    pub classification: String,
    pub baseline_count: u64,
    pub suspect_count: u64,
    pub count_change: i64,
    /// Basis points as a decimal string, or "undefined" (zero baseline).
    pub rate_change_bp: String,
    pub rule_id: String,
    pub rule_version: i64,
    pub calculation_json: String,
}

struct SideCounts {
    counts: BTreeMap<String, u64>,
    accepted: u64,
    excluded_missing_field: u64,
    stack_malformed: u64,
    keys_truncated: bool,
    excluded_over_key_limit: u64,
}

enum KeySource {
    Message(MaskSet),
    Stack(String),
    Severity,
    Resource,
    Attribute(String),
}

fn key_source(
    def_field_selection: &str,
    cfg: &ComparisonConfig,
    masks: MaskSet,
) -> Result<KeySource, JobError> {
    match cfg.dimension.as_str() {
        "message_pattern" => Ok(KeySource::Message(masks)),
        "stack_fingerprint" => {
            let sel: serde_json::Value =
                serde_json::from_str(def_field_selection).unwrap_or_default();
            match sel.get("stack_field").and_then(|x| x.as_str()) {
                Some(f) => Ok(KeySource::Stack(f.to_string())),
                None => Err(invalid(
                    "the stack_fingerprint dimension requires field_selection_json.stack_field",
                )),
            }
        }
        "severity" => Ok(KeySource::Severity),
        "resource" => Ok(KeySource::Resource),
        _ => Ok(KeySource::Attribute(
            cfg.attribute.clone().unwrap_or_default(),
        )),
    }
}

fn attr_str(row: &LogRow, field: &str) -> Option<String> {
    let attrs = logscope_model::attrs_from_canonical_json(&row.attributes_json).ok()?;
    match attrs.get(field) {
        Some(logscope_model::AnyValue::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn extract_key(row: &LogRow, source: &KeySource, side: &mut SideCounts) -> Option<String> {
    match source {
        KeySource::Message(masks) => Some(normalize_message(&row.display_message, masks).template),
        KeySource::Stack(field) => match attr_str(row, field) {
            None => {
                side.excluded_missing_field += 1;
                None
            }
            Some(text) => {
                let s = parse_stack(&text);
                if s.quality == StackQuality::Malformed {
                    side.stack_malformed += 1;
                    None
                } else {
                    // The key is the normalized identity text (type +
                    // frames), stable and human-inspectable.
                    Some(if s.frames.is_empty() {
                        s.exception_type
                    } else {
                        format!("{} @ {}", s.exception_type, s.frames.join(" < "))
                    })
                }
            }
        },
        KeySource::Severity => Some(row.severity_text.clone().unwrap_or_else(|| "(none)".into())),
        KeySource::Resource => Some(row.resource_id.clone()),
        KeySource::Attribute(field) => match attr_str(row, field) {
            None => {
                side.excluded_missing_field += 1;
                None
            }
            Some(v) => Some(v),
        },
    }
}

struct CompiledBase {
    files: Vec<PathBuf>,
    filter: logscope_query::CompiledFilter,
    strategy: TimeStrategy,
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
    Ok(CompiledBase {
        files,
        filter,
        strategy,
    })
}

#[allow(clippy::too_many_arguments)]
fn stream_side(
    engine: &EngineConnection,
    base: &CompiledBase,
    start: i64,
    end: i64,
    source: &KeySource,
    cfg: &ComparisonConfig,
    ctx: &JobContext,
    cancel: &QueryCancelHandle,
) -> Result<SideCounts, JobError> {
    let mut side = SideCounts {
        counts: BTreeMap::new(),
        accepted: 0,
        excluded_missing_field: 0,
        stack_malformed: 0,
        keys_truncated: false,
        excluded_over_key_limit: 0,
    };
    let window = ResolvedWindow {
        strategy: base.strategy.clone(),
        start: Some(start),
        end: Some(end),
        empty_anchor: false,
    };
    let mut cancelled = false;
    let mut seen = 0u64;
    stream_query(
        engine,
        &base.files,
        &base.filter,
        &window,
        cfg.max_records,
        cancel,
        std::time::Duration::from_secs(cfg.budget_seconds),
        |row| {
            seen += 1;
            if seen.is_multiple_of(4096) && ctx.control.checkpoint().is_err() {
                cancelled = true;
                cancel.cancel();
                return Ok(false);
            }
            if let Some(key) = extract_key(&row, source, &mut side) {
                match side.counts.get_mut(&key) {
                    Some(n) => *n += 1,
                    None => {
                        if side.counts.len() >= cfg.max_keys_per_side {
                            side.keys_truncated = true;
                            side.excluded_over_key_limit += 1;
                        } else {
                            side.counts.insert(key, 1);
                        }
                    }
                }
                side.accepted += 1;
            }
            Ok(true)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    if cancelled {
        return Err(JobError::new("job/cancelled", "the job was cancelled"));
    }
    Ok(side)
}

/// Counts records the bounded windows can never see (no event time)
/// so the exclusion is explicit rather than silent.
fn count_untimestamped(
    engine: &EngineConnection,
    base: &CompiledBase,
    cfg: &ComparisonConfig,
    cancel: &QueryCancelHandle,
) -> Result<u64, JobError> {
    let window = ResolvedWindow {
        strategy: TimeStrategy::All,
        start: None,
        end: None,
        empty_anchor: false,
    };
    let mut untimestamped = 0u64;
    stream_query(
        engine,
        &base.files,
        &base.filter,
        &window,
        cfg.max_records,
        cancel,
        std::time::Duration::from_secs(cfg.budget_seconds),
        |row| {
            if row.event_time.is_none() {
                untimestamped += 1;
            }
            Ok(true)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    Ok(untimestamped)
}

/// Runs a comparison analysis end to end on the two-phase lifecycle.
/// The returned row is always terminal.
pub fn run_comparison_analysis(
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
    if def.kind != "comparison" {
        return Err(invalid(format!(
            "kind {:?} is not a comparison analysis",
            def.kind
        )));
    }
    let cfg = ComparisonConfig::parse(&def.config_json)?;
    let thresholds = ComparisonThresholds::parse(&def.thresholds_json).map_err(invalid)?;
    let masks = MaskSet::parse(&def.masking_profile_json).map_err(invalid)?;
    let source = key_source(&def.field_selection_json, &cfg, masks)?;

    let run = analysis::begin_run_with_bounds(ws, definition_id, cfg.bounds_json())?;
    analysis::mark_running(ws, &run.run_id)?;
    match execute(ws, engine, &run, &cfg, &thresholds, &source, ctx) {
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
    cfg: &ComparisonConfig,
    thresholds: &ComparisonThresholds,
    source: &KeySource,
    ctx: &JobContext,
) -> Result<AnalysisRunRow, JobError> {
    if ctx.control.checkpoint().is_err() {
        return Err(JobError::new("job/cancelled", "the job was cancelled"));
    }
    let base = compiled_base(ws, run)?;
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());

    let baseline = stream_side(
        engine,
        &base,
        cfg.baseline_start,
        cfg.baseline_end,
        source,
        cfg,
        ctx,
        &cancel,
    )?;
    let suspect = stream_side(
        engine,
        &base,
        cfg.suspect_start,
        cfg.suspect_end,
        source,
        cfg,
        ctx,
        &cancel,
    )?;
    let untimestamped = count_untimestamped(engine, &base, cfg, &cancel)?;

    // Exact bounded union of both sides — never two unrelated top-Ks.
    let mut keys: BTreeMap<&String, (u64, u64)> = BTreeMap::new();
    for (k, n) in &baseline.counts {
        keys.entry(k).or_insert((0, 0)).0 = *n;
    }
    for (k, n) in &suspect.counts {
        keys.entry(k).or_insert((0, 0)).1 = *n;
    }
    let dur_b = cfg.baseline_end - cfg.baseline_start;
    let dur_s = cfg.suspect_end - cfg.suspect_start;
    let mut rows: Vec<(ComparisonResult, u64)> = Vec::with_capacity(keys.len());
    for (key, (b, s)) in &keys {
        let c: Classified = classify(*b, *s, dur_b, dur_s, thresholds).map_err(invalid)?;
        let result_id = comparison_result_id(
            &run.semantic_fingerprint,
            &cfg.dimension,
            key,
            COMPARISON_RULE_ID,
            COMPARISON_RULE_VERSION,
        );
        rows.push((
            ComparisonResult {
                result_id,
                dimension: cfg.dimension.clone(),
                key: (*key).clone(),
                classification: c.classification.as_str().to_string(),
                baseline_count: c.baseline_count,
                suspect_count: c.suspect_count,
                count_change: c.count_change,
                rate_change_bp: c.rate_change_bp.clone(),
                rule_id: c.rule_id.to_string(),
                rule_version: c.rule_version,
                calculation_json: serde_json::to_string(&c).unwrap_or_else(|_| "{}".into()),
            },
            b + s,
        ));
    }
    // Deterministic order: combined count desc, then key asc; the
    // remainder beyond top_k is aggregated honestly, never dropped.
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.key.cmp(&b.0.key)));
    let total_keys = rows.len();
    let remainder: Vec<_> = rows.split_off(rows.len().min(cfg.top_k));
    let remainder_baseline: u64 = remainder.iter().map(|(r, _)| r.baseline_count).sum();
    let remainder_suspect: u64 = remainder.iter().map(|(r, _)| r.suspect_count).sum();
    let results: Vec<ComparisonResult> = rows.into_iter().map(|(r, _)| r).collect();

    let dir = ws.layout.derived_analysis_dir(&run.run_id);
    std::fs::create_dir_all(&dir).map_err(|e| engine_err(format!("{}: {e}", dir.display())))?;
    let out_path = dir.join(RESULTS_FILE);
    write_results_parquet(engine, &results, &out_path)?;
    let (byte_size, sha256) = hash_file(&out_path)?;
    ws.meta
        .record_derived_artifact(&DerivedArtifactRow {
            artifact_id: format!("dart-{}", uuid::Uuid::new_v4()),
            run_id: run.run_id.clone(),
            kind: "comparison_domain".into(),
            rel_path: format!("derived/analysis/{}/{RESULTS_FILE}", run.run_id),
            row_count: results.len() as i64,
            byte_size,
            sha256: sha256.clone(),
            schema_version: RESULTS_SCHEMA_VERSION,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
        .map_err(ws_err)?;

    let counts = serde_json::json!({
        "baseline_accepted": baseline.accepted,
        "suspect_accepted": suspect.accepted,
        "excluded_missing_field": baseline.excluded_missing_field + suspect.excluded_missing_field,
        "stack_malformed": baseline.stack_malformed + suspect.stack_malformed,
        "untimestamped_excluded": untimestamped,
        "keys_truncated": baseline.keys_truncated || suspect.keys_truncated,
        "excluded_over_key_limit":
            baseline.excluded_over_key_limit + suspect.excluded_over_key_limit,
    });
    let manifest = serde_json::json!({
        "dimension": cfg.dimension,
        "distinct_keys": total_keys,
        "top_k": cfg.top_k,
        "remainder": {
            "keys": remainder.len(),
            "baseline_count": remainder_baseline,
            "suspect_count": remainder_suspect,
        },
        "results": {
            "file": RESULTS_FILE,
            "rows": results.len(),
            "sha256": sha256,
            "schema_version": RESULTS_SCHEMA_VERSION,
        },
    });
    analysis::complete_run(ws, &run.run_id, &counts.to_string(), &manifest.to_string())
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

fn write_results_parquet(
    engine: &EngineConnection,
    results: &[ComparisonResult],
    out_path: &std::path::Path,
) -> Result<(), JobError> {
    let conn = engine.raw();
    conn.execute_batch(
        "CREATE OR REPLACE TEMP TABLE __ls_cmp_results(
            result_id VARCHAR, dimension VARCHAR, key VARCHAR, classification VARCHAR,
            baseline_count UBIGINT, suspect_count UBIGINT, count_change BIGINT,
            rate_change_bp VARCHAR, rule_id VARCHAR, rule_version BIGINT,
            calculation_json VARCHAR, sort_no UBIGINT); BEGIN",
    )
    .map_err(engine_err)?;
    {
        let mut stmt = conn
            .prepare("INSERT INTO __ls_cmp_results VALUES (?,?,?,?,?,?,?,?,?,?,?,?)")
            .map_err(engine_err)?;
        for (i, r) in results.iter().enumerate() {
            stmt.execute(duckdb::params![
                r.result_id,
                r.dimension,
                r.key,
                r.classification,
                r.baseline_count,
                r.suspect_count,
                r.count_change,
                r.rate_change_bp,
                r.rule_id,
                r.rule_version,
                r.calculation_json,
                i as u64,
            ])
            .map_err(engine_err)?;
        }
    }
    conn.execute_batch(&format!(
        "COMMIT; COPY (SELECT result_id, dimension, key, classification, baseline_count,
            suspect_count, count_change, rate_change_bp, rule_id, rule_version,
            calculation_json FROM __ls_cmp_results ORDER BY sort_no)
         TO '{}' (FORMAT PARQUET);
         DROP TABLE __ls_cmp_results;",
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

/// One page of comparison results in the stored deterministic order.
pub fn list_comparison_results(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    offset: u64,
    limit: u64,
) -> Result<Vec<ComparisonResult>, JobError> {
    let run = require_completed_run(ws, run_id)?;
    let path = ws
        .layout
        .derived_analysis_dir(&run.run_id)
        .join(RESULTS_FILE);
    if !path.exists() {
        return Err(JobError::new(
            "analysis/derived",
            "results file is missing; delete and re-run the analysis (rebuild lands in WP8)",
        ));
    }
    let limit = limit.clamp(1, 1_000);
    let conn = engine.raw();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT result_id, dimension, key, classification, baseline_count, suspect_count,
                    count_change, rate_change_bp, rule_id, rule_version, calculation_json
             FROM read_parquet('{}') LIMIT {limit} OFFSET {offset}",
            sql_quote(&path)
        ))
        .map_err(engine_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ComparisonResult {
                result_id: r.get(0)?,
                dimension: r.get(1)?,
                key: r.get(2)?,
                classification: r.get(3)?,
                baseline_count: r.get(4)?,
                suspect_count: r.get(5)?,
                count_change: r.get(6)?,
                rate_change_bp: r.get(7)?,
                rule_id: r.get(8)?,
                rule_version: r.get(9)?,
                calculation_json: r.get(10)?,
            })
        })
        .map_err(engine_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(engine_err)?;
    Ok(rows)
}

/// Deterministic drill-down to one side's contributing records for a
/// key: re-streams the run's OWN frozen window and re-derives the key
/// with the same versioned configuration. Refused when the run's inputs
/// moved.
pub fn comparison_records(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    key: &str,
    side: &str,
    limit: usize,
) -> Result<Vec<LogRow>, JobError> {
    let run = require_completed_run(ws, run_id)?;
    if let Some(reason) = analysis::check_run_current(ws, &run)? {
        return Err(JobError::new(
            "analysis/stale-run",
            format!("drill-down refused: {reason}; re-run the analysis"),
        ));
    }
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
    let cfg = ComparisonConfig::parse(&def.config_json)?;
    let masks = MaskSet::parse(&def.masking_profile_json).map_err(invalid)?;
    let source = key_source(&def.field_selection_json, &cfg, masks)?;
    let (start, end) = match side {
        "baseline" => (cfg.baseline_start, cfg.baseline_end),
        "suspect" => (cfg.suspect_start, cfg.suspect_end),
        other => {
            return Err(invalid(format!(
                "unknown side {other:?} (baseline|suspect)"
            )))
        }
    };
    let base = compiled_base(ws, &run)?;
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let window = ResolvedWindow {
        strategy: base.strategy.clone(),
        start: Some(start),
        end: Some(end),
        empty_anchor: false,
    };
    let limit = limit.clamp(1, 10_000);
    let mut hits: Vec<LogRow> = Vec::new();
    let mut scratch = SideCounts {
        counts: BTreeMap::new(),
        accepted: 0,
        excluded_missing_field: 0,
        stack_malformed: 0,
        keys_truncated: false,
        excluded_over_key_limit: 0,
    };
    stream_query(
        engine,
        &base.files,
        &base.filter,
        &window,
        cfg.max_records,
        &cancel,
        std::time::Duration::from_secs(cfg.budget_seconds),
        |row| {
            if extract_key(&row, &source, &mut scratch).as_deref() == Some(key) {
                hits.push(row);
                if hits.len() >= limit {
                    return Ok(false);
                }
            }
            Ok(true)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    Ok(hits)
}
