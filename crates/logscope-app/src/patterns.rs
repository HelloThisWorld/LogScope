//! Pattern-analysis execution (v0.4 WP2): message templates and stack
//! fingerprints over the WP1 run lifecycle.
//!
//! The engine is a single bounded streaming pass over the frozen scope:
//! every record is normalized independently (rule-based masking — see
//! ADR-0020), so aggregation is order-independent by construction and
//! the determinism gate (repeated runs, randomized partition order,
//! parallel import layout) holds without coordination. Results publish
//! atomically through the two-phase run record: a summaries parquet is
//! written under `derived/analysis/<run>/`, cataloged with its checksum,
//! and only then is the run finished `completed` with its manifest.
//!
//! Drill-down does NOT depend on a membership file in WP2: it re-streams
//! the frozen scope and re-normalizes with the same versioned
//! configuration — deterministic, bounded, and refused when the run went
//! stale (a moved dataset would silently change the answer). A cached
//! membership parquet is a WP8 optimization over the same contract.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;

use logscope_case::analysis::pattern_id as make_pattern_id;
use logscope_case::analysis::stack_fingerprint_id;
use logscope_case::stack::{parse_stack, StackQuality, STACK_ALGORITHM_ID};
use logscope_case::template::{normalize_message, MaskSet, TEMPLATE_ALGORITHM_ID};
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

fn engine_err(e: impl std::fmt::Display) -> JobError {
    JobError::new(
        "analysis/derived",
        format!("derived-data write failed: {e}"),
    )
}

pub const SUMMARIES_FILE: &str = "summaries.parquet";
pub const SUMMARIES_SCHEMA_VERSION: i64 = 1;

/// Per-run pattern limits, parsed from the definition's `limits_json` /
/// `config_json` with documented defaults. Integers only (identity
/// inputs refuse floats globally).
#[derive(Debug, Clone)]
pub struct PatternLimits {
    pub max_records: u64,
    pub max_patterns: usize,
    pub bucket_seconds: i64,
    pub max_buckets_per_pattern: usize,
    pub max_services_per_pattern: usize,
    pub budget_seconds: u64,
}

impl PatternLimits {
    fn from_def(limits_json: &str, config_json: &str) -> Result<PatternLimits, JobError> {
        fn get(v: &serde_json::Value, key: &str, default: i64) -> i64 {
            v.get(key).and_then(|x| x.as_i64()).unwrap_or(default)
        }
        let limits: serde_json::Value = serde_json::from_str(limits_json).unwrap_or_default();
        let config: serde_json::Value = serde_json::from_str(config_json).unwrap_or_default();
        let out = PatternLimits {
            max_records: get(&limits, "max_records", 5_000_000).max(1) as u64,
            max_patterns: get(&limits, "max_patterns", 50_000).max(1) as usize,
            bucket_seconds: get(&config, "bucket_seconds", 60).max(1),
            max_buckets_per_pattern: get(&limits, "max_buckets_per_pattern", 4_096).max(1) as usize,
            max_services_per_pattern: get(&limits, "max_services_per_pattern", 256).max(1) as usize,
            budget_seconds: get(&limits, "budget_seconds", 3_600).clamp(1, 24 * 3600) as u64,
        };
        Ok(out)
    }
}

/// One example reference (deterministic selection, ties by record id).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PatternExample {
    pub role: String, // earliest|latest|peak|resource
    pub record_id: String,
    pub dataset_id: String,
    pub event_time: Option<i64>,
}

/// One pattern summary row (also the parquet schema, version 1).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PatternSummary {
    pub pattern_id: String,
    pub kind: String, // message_template|stack_fingerprint
    pub template: String,
    pub exception_type: Option<String>,
    pub count: u64,
    pub untimestamped: u64,
    pub first_seen: Option<i64>,
    pub last_seen: Option<i64>,
    pub peak_bucket_start: Option<i64>,
    pub peak_bucket_count: u64,
    pub buckets_truncated: bool,
    pub services_truncated: bool,
    pub parse_quality: Option<String>, // stack kind only
    pub services_json: String,
    pub examples_json: String,
}

#[derive(Debug, Clone)]
struct Extreme {
    time: i64,
    record_id: String,
    dataset_id: String,
}

#[derive(Debug, Default)]
struct PatternAgg {
    template: String,
    exception_type: Option<String>,
    parse_quality: Option<&'static str>,
    count: u64,
    untimestamped: u64,
    earliest: Option<Extreme>,
    latest: Option<Extreme>,
    /// bucket start nanos → (count, earliest record in bucket).
    buckets: BTreeMap<i64, (u64, Extreme)>,
    buckets_truncated: bool,
    services: BTreeMap<String, (u64, Extreme)>,
    services_truncated: bool,
}

/// Order-independent accumulator: every update is a pure fold over
/// (comparable) per-record values, so input order can never change the
/// outcome. Kept private to the runner; unit-tested through it.
#[derive(Debug, Default)]
struct Accumulator {
    patterns: BTreeMap<String, PatternAgg>,
    accepted: u64,
    excluded_missing_field: u64,
    excluded_over_pattern_limit: u64,
    stack_malformed: u64,
    patterns_truncated: bool,
}

fn better_earliest(current: &Option<Extreme>, cand: &Extreme) -> bool {
    match current {
        None => true,
        Some(c) => (cand.time, &cand.record_id) < (c.time, &c.record_id),
    }
}

fn better_latest(current: &Option<Extreme>, cand: &Extreme) -> bool {
    match current {
        None => true,
        Some(c) => (cand.time, &cand.record_id) > (c.time, &c.record_id),
    }
}

impl Accumulator {
    fn add(
        &mut self,
        limits: &PatternLimits,
        pattern_id: String,
        template: String,
        exception_type: Option<String>,
        parse_quality: Option<&'static str>,
        row: &LogRow,
    ) {
        let entry = match self.patterns.get_mut(&pattern_id) {
            Some(e) => e,
            None => {
                if self.patterns.len() >= limits.max_patterns {
                    // The tail is not silently dropped: the exclusion is
                    // counted and the run reports truncation.
                    self.patterns_truncated = true;
                    self.excluded_over_pattern_limit += 1;
                    return;
                }
                let agg = PatternAgg {
                    template,
                    exception_type,
                    parse_quality,
                    ..PatternAgg::default()
                };
                self.patterns.entry(pattern_id).or_insert(agg)
            }
        };
        entry.count += 1;
        match row.event_time {
            None => entry.untimestamped += 1,
            Some(t) => {
                let cand = Extreme {
                    time: t,
                    record_id: row.record_id.clone(),
                    dataset_id: row.dataset_id.clone(),
                };
                if better_earliest(&entry.earliest, &cand) {
                    entry.earliest = Some(cand.clone());
                }
                if better_latest(&entry.latest, &cand) {
                    entry.latest = Some(cand.clone());
                }
                let bucket = t.div_euclid(limits.bucket_seconds * 1_000_000_000)
                    * (limits.bucket_seconds * 1_000_000_000);
                match entry.buckets.get_mut(&bucket) {
                    Some((n, first)) => {
                        *n += 1;
                        if better_earliest(&Some(first.clone()), &cand) {
                            *first = cand.clone();
                        }
                    }
                    None => {
                        if entry.buckets.len() >= limits.max_buckets_per_pattern {
                            entry.buckets_truncated = true;
                        } else {
                            entry.buckets.insert(bucket, (1, cand.clone()));
                        }
                    }
                }
                match entry.services.get_mut(&row.resource_id) {
                    Some((n, first)) => {
                        *n += 1;
                        if better_earliest(&Some(first.clone()), &cand) {
                            *first = cand.clone();
                        }
                    }
                    None => {
                        if entry.services.len() >= limits.max_services_per_pattern {
                            entry.services_truncated = true;
                        } else {
                            entry.services.insert(row.resource_id.clone(), (1, cand));
                        }
                    }
                }
            }
        }
        self.accepted += 1;
    }

    fn into_summaries(self, kind: &str) -> Vec<PatternSummary> {
        let mut out: Vec<PatternSummary> = self
            .patterns
            .into_iter()
            .map(|(pattern_id, agg)| {
                // Peak bucket: highest count, ties to the earliest bucket
                // (BTreeMap iteration is ascending, `>` keeps the first).
                let mut peak: Option<(i64, u64, &Extreme)> = None;
                for (start, (n, first)) in &agg.buckets {
                    if peak.map(|(_, pn, _)| *n > pn).unwrap_or(true) {
                        peak = Some((*start, *n, first));
                    }
                }
                let mut examples: Vec<PatternExample> = Vec::new();
                if let Some(e) = &agg.earliest {
                    examples.push(PatternExample {
                        role: "earliest".into(),
                        record_id: e.record_id.clone(),
                        dataset_id: e.dataset_id.clone(),
                        event_time: Some(e.time),
                    });
                }
                if let Some(e) = &agg.latest {
                    examples.push(PatternExample {
                        role: "latest".into(),
                        record_id: e.record_id.clone(),
                        dataset_id: e.dataset_id.clone(),
                        event_time: Some(e.time),
                    });
                }
                if let Some((_, _, first)) = peak {
                    examples.push(PatternExample {
                        role: "peak".into(),
                        record_id: first.record_id.clone(),
                        dataset_id: first.dataset_id.clone(),
                        event_time: Some(first.time),
                    });
                }
                for (_service, (_n, first)) in agg.services.iter().take(4) {
                    examples.push(PatternExample {
                        role: "resource".into(),
                        record_id: first.record_id.clone(),
                        dataset_id: first.dataset_id.clone(),
                        event_time: Some(first.time),
                    });
                }
                let services: Vec<serde_json::Value> = agg
                    .services
                    .iter()
                    .map(|(s, (n, _))| serde_json::json!({"resource_id": s, "count": n}))
                    .collect();
                PatternSummary {
                    pattern_id,
                    kind: kind.into(),
                    template: agg.template,
                    exception_type: agg.exception_type,
                    count: agg.count,
                    untimestamped: agg.untimestamped,
                    first_seen: agg.earliest.as_ref().map(|e| e.time),
                    last_seen: agg.latest.as_ref().map(|e| e.time),
                    peak_bucket_start: peak.map(|(s, _, _)| s),
                    peak_bucket_count: peak.map(|(_, n, _)| n).unwrap_or(0),
                    buckets_truncated: agg.buckets_truncated,
                    services_truncated: agg.services_truncated,
                    parse_quality: agg.parse_quality.map(str::to_string),
                    services_json: serde_json::to_string(&services).unwrap_or_else(|_| "[]".into()),
                    examples_json: serde_json::to_string(&examples).unwrap_or_else(|_| "[]".into()),
                }
            })
            .collect();
        out.sort_by(|a, b| b.count.cmp(&a.count).then(a.pattern_id.cmp(&b.pattern_id)));
        out
    }
}

/// The frozen execution scope rebuilt from a run row: compiled filter,
/// files, and the run's OWN concrete bounds (never re-resolved).
struct FrozenScope {
    files: Vec<PathBuf>,
    filter: logscope_query::CompiledFilter,
    window: ResolvedWindow,
}

fn rebuild_scope(ws: &Workspace, run: &AnalysisRunRow) -> Result<FrozenScope, JobError> {
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
    let bounds: serde_json::Value = serde_json::from_str(&run.bounds_json).unwrap_or_default();
    let strategy: TimeStrategy =
        serde_json::from_str(&def.time_strategy_json).unwrap_or(TimeStrategy::All);
    let window = ResolvedWindow {
        strategy,
        start: bounds.get("start").and_then(|v| v.as_i64()),
        end: bounds.get("end").and_then(|v| v.as_i64()),
        empty_anchor: false,
    };
    Ok(FrozenScope {
        files,
        filter,
        window,
    })
}

/// Text selection per analysis kind.
enum TextSource {
    DisplayMessage,
    Attribute(String),
}

fn text_source(kind: &str, field_selection_json: &str) -> Result<TextSource, JobError> {
    let sel: serde_json::Value = serde_json::from_str(field_selection_json).unwrap_or_default();
    match kind {
        "message_pattern" => Ok(match sel.get("message_field").and_then(|v| v.as_str()) {
            Some(f) => TextSource::Attribute(f.to_string()),
            None => TextSource::DisplayMessage,
        }),
        "stack_fingerprint" => match sel.get("stack_field").and_then(|v| v.as_str()) {
            Some(f) => Ok(TextSource::Attribute(f.to_string())),
            None => Err(JobError::new(
                "analysis/invalid-definition",
                "stack_fingerprint requires field_selection_json.stack_field",
            )),
        },
        other => Err(JobError::new(
            "analysis/invalid-definition",
            format!("kind {other:?} is not a pattern analysis"),
        )),
    }
}

fn extract_text<'a>(row: &'a LogRow, source: &TextSource) -> Option<std::borrow::Cow<'a, str>> {
    match source {
        TextSource::DisplayMessage => Some(std::borrow::Cow::Borrowed(&row.display_message)),
        TextSource::Attribute(field) => {
            // Attributes are stored as the model's TYPE-TAGGED canonical
            // JSON; only a string-typed value is usable as text — other
            // types are honestly a missing field for this analysis.
            let attrs = logscope_model::attrs_from_canonical_json(&row.attributes_json).ok()?;
            match attrs.get(field) {
                Some(logscope_model::AnyValue::Str(s)) => Some(std::borrow::Cow::Owned(s.clone())),
                _ => None,
            }
        }
    }
}

/// Runs a `message_pattern` or `stack_fingerprint` analysis end to end:
/// begins the two-phase run, streams the frozen scope once, publishes
/// the summaries parquet, and completes the run — or finishes it
/// cancelled/failed. The returned row is always terminal.
pub fn run_pattern_analysis(
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
    // Validated up front so the failure happens before a run record.
    let source = text_source(&def.kind, &def.field_selection_json)?;
    let masks = MaskSet::parse(&def.masking_profile_json)
        .map_err(|e| JobError::new("analysis/invalid-definition", e.to_string()))?;
    let limits = PatternLimits::from_def(&def.limits_json, &def.config_json)?;

    let run = analysis::begin_run(ws, definition_id)?;
    analysis::mark_running(ws, &run.run_id)?;
    match execute(ws, engine, &def.kind, &run, &source, &masks, &limits, ctx) {
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

#[allow(clippy::too_many_arguments)]
fn execute(
    ws: &Workspace,
    engine: &EngineConnection,
    kind: &str,
    run: &AnalysisRunRow,
    source: &TextSource,
    masks: &MaskSet,
    limits: &PatternLimits,
    ctx: &JobContext,
) -> Result<AnalysisRunRow, JobError> {
    // A cancellation requested before the stream starts must cancel —
    // the same pre-execution rule the F-1 fix established for queries.
    if ctx.control.checkpoint().is_err() {
        return Err(JobError::new("job/cancelled", "the job was cancelled"));
    }
    let scope = rebuild_scope(ws, run)?;
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let mut acc = Accumulator::default();
    let masking_fp = logscope_case::analysis::config_fingerprint(
        &serde_json::to_string(masks).unwrap_or_else(|_| "{}".into()),
    )
    .map_err(|e| JobError::new("analysis/invalid-definition", e.to_string()))?;

    let mut cancelled = false;
    let mut seen: u64 = 0;
    let mut truncated_records = false;
    let algorithm_version = run.algorithm_version;
    let streamed = stream_query(
        engine,
        &scope.files,
        &scope.filter,
        &scope.window,
        limits.max_records + 1,
        &cancel,
        std::time::Duration::from_secs(limits.budget_seconds),
        |row| {
            seen += 1;
            if seen > limits.max_records {
                truncated_records = true;
                return Ok(false);
            }
            if seen.is_multiple_of(4096) && ctx.control.checkpoint().is_err() {
                cancelled = true;
                cancel.cancel();
                return Ok(false);
            }
            match extract_text(&row, source) {
                None => acc.excluded_missing_field += 1,
                Some(text) => match kind {
                    "message_pattern" => {
                        let t = normalize_message(&text, masks);
                        let id = make_pattern_id(
                            TEMPLATE_ALGORITHM_ID,
                            algorithm_version,
                            &masking_fp,
                            &t.template,
                        );
                        acc.add(limits, id, t.template, None, None, &row);
                    }
                    _ => {
                        let s = parse_stack(&text);
                        if s.quality == StackQuality::Malformed {
                            acc.stack_malformed += 1;
                        } else {
                            let id = stack_fingerprint_id(
                                STACK_ALGORITHM_ID,
                                algorithm_version,
                                &masking_fp,
                                &s.exception_type,
                                &s.frames,
                                &s.causes,
                                s.truncated,
                            );
                            let display = if s.frames.is_empty() {
                                s.exception_type.clone()
                            } else {
                                format!("{} @ {}", s.exception_type, s.frames.join(" < "))
                            };
                            acc.add(
                                limits,
                                id,
                                display,
                                Some(s.exception_type),
                                Some(match s.quality {
                                    StackQuality::Parsed => "parsed",
                                    StackQuality::Partial => "partial",
                                    StackQuality::Malformed => "malformed",
                                }),
                                &row,
                            );
                        }
                    }
                },
            }
            Ok(true)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;
    let _ = streamed;
    if cancelled {
        return Err(JobError::new("job/cancelled", "the job was cancelled"));
    }

    let counts = serde_json::json!({
        "accepted": acc.accepted,
        "excluded_missing_field": acc.excluded_missing_field,
        "excluded_over_pattern_limit": acc.excluded_over_pattern_limit,
        "stack_malformed": acc.stack_malformed,
        "records_truncated": truncated_records,
    });
    let patterns_truncated = acc.patterns_truncated;
    let summary_kind = if kind == "message_pattern" {
        "message_template"
    } else {
        "stack_fingerprint"
    };
    let summaries = acc.into_summaries(summary_kind);

    // ---- publish the summaries parquet, then complete the run.
    let dir = ws.layout.derived_analysis_dir(&run.run_id);
    std::fs::create_dir_all(&dir).map_err(|e| engine_err(format!("{}: {e}", dir.display())))?;
    let out_path = dir.join(SUMMARIES_FILE);
    write_summaries_parquet(engine, &summaries, &out_path)?;
    let bytes = {
        let mut f = std::fs::File::open(&out_path)
            .map_err(|e| engine_err(format!("{}: {e}", out_path.display())))?;
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
        (total, format!("{:x}", hasher.finalize()))
    };
    ws.meta
        .record_derived_artifact(&DerivedArtifactRow {
            artifact_id: format!("dart-{}", uuid::Uuid::new_v4()),
            run_id: run.run_id.clone(),
            kind: "pattern_summaries".into(),
            rel_path: format!("derived/analysis/{}/{SUMMARIES_FILE}", run.run_id),
            row_count: summaries.len() as i64,
            byte_size: bytes.0,
            sha256: bytes.1.clone(),
            schema_version: SUMMARIES_SCHEMA_VERSION,
            created_at: chrono::Utc::now().to_rfc3339(),
        })
        .map_err(ws_err)?;

    let manifest = serde_json::json!({
        "distinct_patterns": summaries.len(),
        "patterns_truncated": patterns_truncated,
        "summaries": {
            "file": SUMMARIES_FILE,
            "rows": summaries.len(),
            "sha256": bytes.1,
            "schema_version": SUMMARIES_SCHEMA_VERSION,
        },
    });
    analysis::complete_run(ws, &run.run_id, &counts.to_string(), &manifest.to_string())
}

fn sql_quote(path: &std::path::Path) -> String {
    path.display()
        .to_string()
        .replace('\'', "''")
        .replace('\\', "/")
}

fn write_summaries_parquet(
    engine: &EngineConnection,
    summaries: &[PatternSummary],
    out_path: &std::path::Path,
) -> Result<(), JobError> {
    let conn = engine.raw();
    conn.execute_batch(
        "CREATE OR REPLACE TEMP TABLE __ls_pattern_summaries(
            pattern_id VARCHAR, kind VARCHAR, template VARCHAR, exception_type VARCHAR,
            count UBIGINT, untimestamped UBIGINT, first_seen BIGINT, last_seen BIGINT,
            peak_bucket_start BIGINT, peak_bucket_count UBIGINT,
            buckets_truncated BOOLEAN, services_truncated BOOLEAN,
            parse_quality VARCHAR, services_json VARCHAR, examples_json VARCHAR); BEGIN",
    )
    .map_err(engine_err)?;
    {
        let mut stmt = conn
            .prepare("INSERT INTO __ls_pattern_summaries VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .map_err(engine_err)?;
        for s in summaries {
            stmt.execute(duckdb::params![
                s.pattern_id,
                s.kind,
                s.template,
                s.exception_type,
                s.count,
                s.untimestamped,
                s.first_seen,
                s.last_seen,
                s.peak_bucket_start,
                s.peak_bucket_count,
                s.buckets_truncated,
                s.services_truncated,
                s.parse_quality,
                s.services_json,
                s.examples_json,
            ])
            .map_err(engine_err)?;
        }
    }
    conn.execute_batch(&format!(
        "COMMIT; COPY (SELECT * FROM __ls_pattern_summaries
            ORDER BY count DESC, pattern_id)
         TO '{}' (FORMAT PARQUET);
         DROP TABLE __ls_pattern_summaries;",
        sql_quote(out_path)
    ))
    .map_err(engine_err)?;
    Ok(())
}

/// One page of pattern summaries from a completed run, ordered by count
/// descending then pattern id (the parquet's stored order).
pub fn list_patterns(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    offset: u64,
    limit: u64,
) -> Result<Vec<PatternSummary>, JobError> {
    let run = require_completed_run(ws, run_id)?;
    let path = ws
        .layout
        .derived_analysis_dir(&run.run_id)
        .join(SUMMARIES_FILE);
    if !path.exists() {
        return Err(JobError::new(
            "analysis/derived",
            "summaries file is missing; delete and re-run the analysis (rebuild lands in WP8)",
        ));
    }
    let limit = limit.clamp(1, 1_000);
    let conn = engine.raw();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT pattern_id, kind, template, exception_type, count, untimestamped,
                    first_seen, last_seen, peak_bucket_start, peak_bucket_count,
                    buckets_truncated, services_truncated, parse_quality,
                    services_json, examples_json
             FROM read_parquet('{}')
             ORDER BY count DESC, pattern_id LIMIT {limit} OFFSET {offset}",
            sql_quote(&path)
        ))
        .map_err(engine_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(PatternSummary {
                pattern_id: r.get(0)?,
                kind: r.get(1)?,
                template: r.get(2)?,
                exception_type: r.get(3)?,
                count: r.get(4)?,
                untimestamped: r.get(5)?,
                first_seen: r.get(6)?,
                last_seen: r.get(7)?,
                peak_bucket_start: r.get(8)?,
                peak_bucket_count: r.get(9)?,
                buckets_truncated: r.get(10)?,
                services_truncated: r.get(11)?,
                parse_quality: r.get(12)?,
                services_json: r.get(13)?,
                examples_json: r.get(14)?,
            })
        })
        .map_err(engine_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(engine_err)?;
    Ok(rows)
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
        return Err(JobError::new(
            "analysis/invalid-definition",
            format!(
                "run {run_id} is {}; only completed runs have results",
                run.state
            ),
        ));
    }
    Ok(run)
}

/// Deterministic drill-down: re-streams the run's frozen scope with the
/// same versioned configuration and returns the canonical records whose
/// identity matches `pattern_id` (bounded). Refused when the run's
/// inputs moved — a stale scope would silently change the answer.
pub fn pattern_records(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    pattern_id: &str,
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
    let source = text_source(&def.kind, &def.field_selection_json)?;
    let masks = MaskSet::parse(&def.masking_profile_json)
        .map_err(|e| JobError::new("analysis/invalid-definition", e.to_string()))?;
    let limits = PatternLimits::from_def(&def.limits_json, &def.config_json)?;
    let masking_fp = logscope_case::analysis::config_fingerprint(
        &serde_json::to_string(&masks).unwrap_or_else(|_| "{}".into()),
    )
    .map_err(|e| JobError::new("analysis/invalid-definition", e.to_string()))?;
    let scope = rebuild_scope(ws, &run)?;
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let limit = limit.clamp(1, 10_000);
    let mut hits: Vec<LogRow> = Vec::new();
    stream_query(
        engine,
        &scope.files,
        &scope.filter,
        &scope.window,
        limits.max_records,
        &cancel,
        std::time::Duration::from_secs(limits.budget_seconds),
        |row| {
            if let Some(text) = extract_text(&row, &source) {
                let id = match def.kind.as_str() {
                    "message_pattern" => {
                        let t = normalize_message(&text, &masks);
                        make_pattern_id(
                            TEMPLATE_ALGORITHM_ID,
                            run.algorithm_version,
                            &masking_fp,
                            &t.template,
                        )
                    }
                    _ => {
                        let s = parse_stack(&text);
                        if s.quality == StackQuality::Malformed {
                            return Ok(true);
                        }
                        stack_fingerprint_id(
                            STACK_ALGORITHM_ID,
                            run.algorithm_version,
                            &masking_fp,
                            &s.exception_type,
                            &s.frames,
                            &s.causes,
                            s.truncated,
                        )
                    }
                };
                if id == pattern_id {
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
    Ok(hits)
}
