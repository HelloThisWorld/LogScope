//! Correlation run execution (v0.4 WP4a/WP4b) on the WP1 run lifecycle
//! and the `corr-rules` / `sig-rules` v1 models in `logscope-case`.
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
//!
//! WP4b adds two things on top of that sequence.
//!
//! **Signals** (`sig-rules` v1) are computed per group during the same
//! pass: retry and gap over consecutive pairs, clock skew over source
//! order, operational duplicates by bucketing members on a message
//! digest. Bucketing is what keeps the duplicate rule linear — comparing
//! every member with every other member inside a 256-member group would
//! be the pairwise join this module exists to avoid.
//!
//! **Probable neighborhoods** are deliberately *not* materialised here.
//! A neighborhood is anchored to one record a person selected, so it is
//! a bounded drill-down query ([`probable_neighborhood`]) against the
//! run's frozen scope, not a cross product written to disk.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::PathBuf;

use logscope_case::analysis::correlation_id;
use logscope_case::correlation::{
    evaluate_probable, explain_edge, explain_group, explain_neighborhood, group_key,
    neighbor_order, sequence_position, CompatibleField, CorrelationFacts, CorrelationLimits,
    KeyNormalization, KeySelector, NeighborFacts, ProbableRule, CORRELATION_RULE_ID,
    CORRELATION_RULE_VERSION,
};
use logscope_case::signals::{
    classify_clock_skew, classify_duplicate, classify_gap, classify_retry, DuplicateClass,
    DuplicateFacts, RetryFacts, SignalEvidence, SignalKind, SignalThresholds, SourcePosition,
    TimePoint, TimeQuality, SIGNAL_RULE_SET_ID, SIGNAL_RULE_SET_VERSION,
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
pub const SIGNALS_FILE: &str = "correlation_signals.parquet";
/// Bumped to 2 by WP4b: a run now carries a third artifact. Listings
/// refuse an older run rather than silently returning "no signals",
/// which would read as "none were found".
pub const RESULTS_SCHEMA_VERSION: i64 = 2;

/// Parsed correlation configuration (`config_json`).
#[derive(Debug, Clone)]
pub struct CorrelationConfig {
    pub selector: KeySelector,
    pub normalization: KeyNormalization,
    pub limits: CorrelationLimits,
    pub max_records: u64,
    pub budget_seconds: u64,
    /// Signals to evaluate over each group's sequence.
    pub signals: Vec<SignalKind>,
    pub thresholds: SignalThresholds,
    /// Attribute holding an explicit attempt counter. Without it the
    /// retry rule can never reach `Documented` — which is the point:
    /// documented retries require the source to have said so.
    pub attempt_attribute: Option<String>,
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
    /// Omitted means all four signals. An explicit empty list means
    /// none — a different request, and honoured as written.
    #[serde(default)]
    signals: Option<Vec<String>>,
    #[serde(default)]
    thresholds: SignalThresholds,
    #[serde(default)]
    attempt_attribute: Option<String>,
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
        raw.thresholds.validate().map_err(invalid)?;
        let signals = match &raw.signals {
            None => vec![
                SignalKind::Retry,
                SignalKind::OperationalDuplicate,
                SignalKind::ClockSkew,
                SignalKind::Gap,
            ],
            Some(names) => {
                let mut out = Vec::new();
                for name in names {
                    let kind = SignalKind::parse(name).map_err(invalid)?;
                    if out.contains(&kind) {
                        return Err(invalid(format!("signal {name:?} is listed twice")));
                    }
                    out.push(kind);
                }
                out
            }
        };
        let attempt_attribute = match raw.attempt_attribute {
            Some(f) if f.trim().is_empty() => {
                return Err(invalid(
                    "attempt_attribute must name a field, or be omitted entirely",
                ))
            }
            other => other,
        };
        Ok(CorrelationConfig {
            selector,
            normalization: raw.normalization,
            limits,
            max_records: raw.max_records.unwrap_or(5_000_000).max(1) as u64,
            budget_seconds: raw.budget_seconds.unwrap_or(3_600).clamp(1, 24 * 3600) as u64,
            signals,
            thresholds: raw.thresholds,
            attempt_attribute,
        })
    }

    fn wants(&self, kind: SignalKind) -> bool {
        self.signals.contains(&kind)
    }

    /// Whether any configured signal needs per-member detail beyond the
    /// (time, record ID) pair the sequence itself requires.
    fn needs_member_detail(&self) -> bool {
        !self.signals.is_empty()
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

/// One behavioural signal observed inside a group (signals parquet
/// schema, version 2).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorrelationSignal {
    pub signal_id: String,
    pub group_id: String,
    /// `retry` | `operational_duplicate` | `clock_skew` | `gap`.
    pub kind: String,
    pub rule_id: String,
    pub rule_version: i64,
    /// `documented` | `corroborated` | `indicative`.
    pub strength: String,
    /// True for everything the source did not state itself. Stored as a
    /// column so a caller can filter leads out without parsing prose.
    pub investigative_lead: bool,
    pub from_record_id: String,
    pub to_record_id: String,
    /// Both event times exactly as recorded. Never adjusted.
    pub from_event_time: i64,
    pub to_event_time: i64,
    pub delta_nanos: i64,
    /// The threshold the delta was judged against; null for signals that
    /// are not threshold-based.
    pub tolerance_nanos: Option<i64>,
    /// Typed fields that supported the conclusion (JSON array).
    pub matched_json: String,
    /// Fields the rule looks for that were absent (JSON array).
    pub missing_json: String,
    pub reason: String,
}

/// One member of a group, carrying exactly the fields the configured
/// signals need. Bounded by `max_events_per_group`.
struct Member {
    event_time: i64,
    record_id: String,
    source_id: String,
    dataset_id: String,
    record_number: Option<i64>,
    time_quality: TimeQuality,
    /// Import already recognised this record as a repeat. It can never
    /// become an operational duplicate — that is gate 25, decided with
    /// import's own verdict rather than a second guess at it.
    ingest_duplicate: bool,
    /// Import already saw this record's time move backwards in-source.
    out_of_order_at_ingest: bool,
    operation: Option<String>,
    outcome: Option<String>,
    message_id: Option<String>,
    attempt: Option<i64>,
    /// SHA-256 of the canonical message. Equality of digests stands in
    /// for equality of messages so a 256-member group never has to hold
    /// 256 full message bodies; the comparison stays exact, and no
    /// similarity or fuzzy match is involved anywhere.
    message_digest: [u8; 32],
}

/// Per-group accumulation during the streaming pass.
struct GroupAcc {
    dated: Vec<(i64, String)>,
    /// Signal inputs, in the same order as `dated` after both are
    /// sorted. Empty when no signal is configured.
    members: Vec<Member>,
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

/// Reads an integer attempt counter. Only a genuine integer counts:
/// a string that happens to look like a number is not the source
/// declaring a typed attempt.
fn attr_i64(row: &LogRow, field: &str) -> Option<i64> {
    let attrs = logscope_model::attrs_from_canonical_json(&row.attributes_json).ok()?;
    match attrs.get(field) {
        Some(logscope_model::AnyValue::Int(i)) => Some(*i),
        _ => None,
    }
}

/// Just the quality flags out of provenance. Provenance carries a lot
/// more, and correlation has no business reading the rest of it.
#[derive(Default, serde::Deserialize)]
struct ProvenanceFlags {
    #[serde(default)]
    flags: Vec<logscope_model::QualityFlag>,
}

/// The ingest-side facts WP4b depends on, read once per member.
///
/// These are not re-derived here. Import already decided whether a
/// timestamp was reconstructed and whether a record duplicates one it
/// had seen; re-deciding it in analysis would let the two disagree.
struct IngestQuality {
    time: TimeQuality,
    /// Import recognised this record as a repeat of one already stored.
    ingest_duplicate: bool,
    /// Import saw this record's time move backwards within its source.
    out_of_order: bool,
}

fn ingest_quality(row: &LogRow) -> IngestQuality {
    use logscope_model::QualityFlag as Q;
    let parsed: ProvenanceFlags = serde_json::from_str(&row.provenance_json).unwrap_or_default();
    let mut inferred = false;
    let mut ingest_duplicate = false;
    let mut out_of_order = false;
    for flag in &parsed.flags {
        match flag {
            // The recorded instant rests on an assumption import had to
            // make, so it cannot corroborate a claim about time.
            Q::TimezoneAssumed | Q::TimestampUnparsed => inferred = true,
            Q::DuplicateRecord => ingest_duplicate = true,
            Q::OutOfOrderTimestamp | Q::ClockSkewSuspected => out_of_order = true,
            _ => {}
        }
    }
    IngestQuality {
        time: match (row.event_time, inferred) {
            (None, _) => TimeQuality::Missing,
            (Some(_), true) => TimeQuality::Inferred,
            (Some(_), false) => TimeQuality::Observed,
        },
        ingest_duplicate,
        out_of_order,
    }
}

fn member_of(row: &LogRow, event_time: i64, attempt_field: &Option<String>) -> Member {
    let mut hasher = Sha256::new();
    hasher.update(row.display_message.as_bytes());
    let digest: [u8; 32] = hasher.finalize().into();
    let quality = ingest_quality(row);
    Member {
        event_time,
        record_id: row.record_id.clone(),
        source_id: row.source_id.clone(),
        dataset_id: row.dataset_id.clone(),
        // Record numbers are source ordinals; one that cannot be
        // represented as i64 is not an ordinal we can reason about.
        record_number: row.record_number.and_then(|n| i64::try_from(n).ok()),
        time_quality: quality.time,
        ingest_duplicate: quality.ingest_duplicate,
        out_of_order_at_ingest: quality.out_of_order,
        operation: row.operation.clone(),
        outcome: row.outcome.clone(),
        message_id: row.message_id.clone(),
        attempt: attempt_field.as_deref().and_then(|f| attr_i64(row, f)),
        message_digest: digest,
    }
}

/// What the signal pass observed for one group, for the run counters.
#[derive(Default)]
struct SignalCounts {
    per_kind: BTreeMap<&'static str, u64>,
    /// Members import had already marked as repeats, excluded from the
    /// operational duplicate rule and reported separately so the
    /// distinction is visible rather than merely respected.
    ingest_duplicates_excluded: u64,
    dropped_over_limit: u64,
}

/// Builds one signal row. Kept in one place so every signal — whatever
/// rule produced it — carries the same identity, columns, and prose.
#[allow(clippy::too_many_arguments)]
fn signal_row(
    run: &AnalysisRunRow,
    group_id: &str,
    key_label: &str,
    evidence: &SignalEvidence,
    from: (&str, i64),
    to: (&str, i64),
    tolerance_nanos: Option<i64>,
    reason: String,
) -> CorrelationSignal {
    CorrelationSignal {
        signal_id: correlation_id(
            &run.semantic_fingerprint,
            SIGNAL_RULE_SET_ID,
            SIGNAL_RULE_SET_VERSION,
            &format!("{key_label}#{}", evidence.kind.as_str()),
            &[from.0.to_string(), to.0.to_string()],
        ),
        group_id: group_id.to_string(),
        kind: evidence.kind.as_str().to_string(),
        rule_id: evidence.rule_id().to_string(),
        rule_version: SIGNAL_RULE_SET_VERSION,
        strength: evidence.strength.as_str().to_string(),
        investigative_lead: evidence.strength.is_investigative_lead(),
        from_record_id: from.0.to_string(),
        to_record_id: to.0.to_string(),
        from_event_time: from.1,
        to_event_time: to.1,
        delta_nanos: to.1 - from.1,
        tolerance_nanos,
        matched_json: serde_json::to_string(&evidence.matched).unwrap_or_else(|_| "[]".into()),
        missing_json: serde_json::to_string(&evidence.missing).unwrap_or_else(|_| "[]".into()),
        reason,
    }
}

/// Evaluates the configured signals over one group's ordered members.
///
/// Retry, gap and skew read consecutive pairs. Duplicates bucket members
/// by message digest rather than comparing every pair: inside a
/// 256-member group the pairwise form would be 32,640 comparisons for
/// the same answer, and pairwise growth is exactly what this module
/// refuses everywhere else.
fn signals_for_group(
    cfg: &CorrelationConfig,
    run: &AnalysisRunRow,
    group_id: &str,
    key_label: &str,
    members: &[Member],
    out: &mut Vec<CorrelationSignal>,
    counts: &mut SignalCounts,
) {
    let mut push = |row: CorrelationSignal, counts: &mut SignalCounts| {
        if out.len() >= cfg.limits.max_total_signals {
            counts.dropped_over_limit += 1;
            return;
        }
        *counts.per_kind.entry(kind_label(&row.kind)).or_insert(0) += 1;
        out.push(row);
    };

    for pair in members.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);

        if cfg.wants(SignalKind::Retry) {
            let earlier = RetryFacts {
                attempt: a.attempt,
                operation: a.operation.as_deref(),
                outcome: a.outcome.as_deref(),
            };
            let later = RetryFacts {
                attempt: b.attempt,
                operation: b.operation.as_deref(),
                outcome: b.outcome.as_deref(),
            };
            if let Some(evidence) = classify_retry(&earlier, &later) {
                let reason = evidence.explain();
                push(
                    signal_row(
                        run,
                        group_id,
                        key_label,
                        &evidence,
                        (&a.record_id, a.event_time),
                        (&b.record_id, b.event_time),
                        None,
                        reason,
                    ),
                    counts,
                );
            }
        }

        if cfg.wants(SignalKind::Gap) {
            let earlier = TimePoint {
                record_id: &a.record_id,
                event_time: a.event_time,
                quality: a.time_quality,
                record_number: a.record_number,
            };
            let later = TimePoint {
                record_id: &b.record_id,
                event_time: b.event_time,
                quality: b.time_quality,
                record_number: b.record_number,
            };
            if let Some(obs) = classify_gap(&earlier, &later, cfg.thresholds.gap_threshold_nanos) {
                let reason = obs.explain();
                push(
                    signal_row(
                        run,
                        group_id,
                        key_label,
                        &obs.evidence,
                        (&a.record_id, a.event_time),
                        (&b.record_id, b.event_time),
                        Some(obs.tolerance_nanos),
                        reason,
                    ),
                    counts,
                );
            }
        }
    }

    // Skew is about the order the *source* wrote records in, which is
    // not the group's time order — so it gets its own pass over members
    // sorted by source position, per source.
    if cfg.wants(SignalKind::ClockSkew) {
        let mut by_source: BTreeMap<(&str, &str), Vec<&Member>> = BTreeMap::new();
        for m in members {
            if m.record_number.is_some() {
                by_source
                    .entry((m.dataset_id.as_str(), m.source_id.as_str()))
                    .or_default()
                    .push(m);
            }
        }
        for (_, mut in_source) in by_source {
            in_source.sort_by_key(|m| (m.record_number, m.record_id.as_str()));
            for pair in in_source.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let first = TimePoint {
                    record_id: &a.record_id,
                    event_time: a.event_time,
                    quality: a.time_quality,
                    record_number: a.record_number,
                };
                let second = TimePoint {
                    record_id: &b.record_id,
                    event_time: b.event_time,
                    quality: b.time_quality,
                    record_number: b.record_number,
                };
                if let Some(obs) =
                    classify_clock_skew(&first, &second, cfg.thresholds.clock_skew_tolerance_nanos)
                {
                    let mut reason = obs.explain();
                    if b.out_of_order_at_ingest {
                        reason.push_str(
                            " Import recorded the same observation for this record at ingest.",
                        );
                    }
                    push(
                        signal_row(
                            run,
                            group_id,
                            key_label,
                            &obs.evidence,
                            (&a.record_id, a.event_time),
                            (&b.record_id, b.event_time),
                            Some(obs.tolerance_nanos),
                            reason,
                        ),
                        counts,
                    );
                }
            }
        }
    }

    if cfg.wants(SignalKind::OperationalDuplicate) {
        let mut by_message: BTreeMap<[u8; 32], Vec<&Member>> = BTreeMap::new();
        for m in members {
            if m.ingest_duplicate {
                // Import already called this a repeat of a record it
                // held. That is a statement about ingestion, so it never
                // becomes a statement about the system.
                counts.ingest_duplicates_excluded += 1;
                continue;
            }
            by_message.entry(m.message_digest).or_default().push(m);
        }
        for (digest, bucket) in by_message {
            if bucket.len() < 2 {
                continue;
            }
            // The rule compares message identity for exact equality. A
            // digest *is* that identity, so it is what gets compared —
            // rendered once per shared bucket rather than per member.
            let identity = hex_digest(&digest);
            // Consecutive members of the bucket only: n-1 rows rather
            // than n(n-1)/2, and every repeat still appears in one.
            for pair in bucket.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                let fa = DuplicateFacts {
                    position: SourcePosition {
                        dataset_id: &a.dataset_id,
                        source_id: &a.source_id,
                        record_number: a.record_number,
                    },
                    message: &identity,
                    message_id: a.message_id.as_deref(),
                    operation: a.operation.as_deref(),
                };
                let fb = DuplicateFacts {
                    position: SourcePosition {
                        dataset_id: &b.dataset_id,
                        source_id: &b.source_id,
                        record_number: b.record_number,
                    },
                    message: &identity,
                    message_id: b.message_id.as_deref(),
                    operation: b.operation.as_deref(),
                };
                match classify_duplicate(&fa, &fb) {
                    DuplicateClass::Operational(evidence) => {
                        let reason = evidence.explain();
                        push(
                            signal_row(
                                run,
                                group_id,
                                key_label,
                                &evidence,
                                (&a.record_id, a.event_time),
                                (&b.record_id, b.event_time),
                                None,
                                reason,
                            ),
                            counts,
                        );
                    }
                    DuplicateClass::Ingestion { .. } => {
                        counts.ingest_duplicates_excluded += 1;
                    }
                    DuplicateClass::Distinct => {}
                }
            }
        }
    }
}

fn hex_digest(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Static label for a signal kind string, so counters can key on
/// `&'static str` without leaking.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "retry" => "retry",
        "operational_duplicate" => "operational_duplicate",
        "clock_skew" => "clock_skew",
        _ => "gap",
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
    let attempt_field = cfg.attempt_attribute.clone();
    let wants_detail = cfg.needs_member_detail();
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
                        members: Vec::new(),
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
                        if wants_detail {
                            acc.members
                                .push(member_of(&row, event_time, &attempt_field));
                        }
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
    let mut signal_rows: Vec<CorrelationSignal> = Vec::new();
    let mut signal_counts = SignalCounts::default();
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
        // Members carry the same ordering, so a signal reading
        // consecutive pairs reads the same sequence the edges do.
        acc.members
            .sort_by(|a, b| (a.event_time, &a.record_id).cmp(&(b.event_time, &b.record_id)));
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

        signals_for_group(
            cfg,
            run,
            &group_id,
            &format!("{}={key}", cfg.selector.as_str()),
            &acc.members,
            &mut signal_rows,
            &mut signal_counts,
        );

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
    signal_rows.sort_by(|a, b| {
        a.group_id
            .cmp(&b.group_id)
            .then(a.kind.cmp(&b.kind))
            .then(a.from_event_time.cmp(&b.from_event_time))
            .then(a.from_record_id.cmp(&b.from_record_id))
            .then(a.to_record_id.cmp(&b.to_record_id))
    });

    let dir = ws.layout.derived_analysis_dir(&run.run_id);
    std::fs::create_dir_all(&dir).map_err(|e| engine_err(format!("{}: {e}", dir.display())))?;
    let groups_path = dir.join(GROUPS_FILE);
    let edges_path = dir.join(EDGES_FILE);
    let signals_path = dir.join(SIGNALS_FILE);
    write_groups_parquet(engine, &group_rows, &groups_path)?;
    write_edges_parquet(engine, &edge_rows, &edges_path)?;
    write_signals_parquet(engine, &signal_rows, &signals_path)?;
    let (groups_bytes, groups_sha) = hash_file(&groups_path)?;
    let (edges_bytes, edges_sha) = hash_file(&edges_path)?;
    let (signals_bytes, signals_sha) = hash_file(&signals_path)?;
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
        (
            "correlation_signals",
            SIGNALS_FILE,
            signal_rows.len(),
            signals_bytes,
            &signals_sha,
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
        "signals": signal_rows.len(),
        "signals_by_kind": signal_counts.per_kind,
        "signals_dropped_over_limit": signal_counts.dropped_over_limit,
        // Kept as its own counter: an ingestion duplicate excluded here
        // is a fact about import, and collapsing it into the operational
        // duplicate count would erase exactly the distinction the rule
        // exists to draw.
        "ingest_duplicates_excluded": signal_counts.ingest_duplicates_excluded,
    });
    let manifest = serde_json::json!({
        "key_selector": cfg.selector.as_str(),
        "confidence": confidence.as_str(),
        "normalization": cfg.normalization,
        "limits": cfg.limits,
        "signal_rules": {
            "rule_set": SIGNAL_RULE_SET_ID,
            "rule_set_version": SIGNAL_RULE_SET_VERSION,
            "enabled": cfg.signals.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            "thresholds": cfg.thresholds,
            "attempt_attribute": cfg.attempt_attribute,
        },
        "signals": {
            "file": SIGNALS_FILE,
            "rows": signal_rows.len(),
            "sha256": signals_sha,
            "schema_version": RESULTS_SCHEMA_VERSION,
        },
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

fn write_signals_parquet(
    engine: &EngineConnection,
    rows: &[CorrelationSignal],
    out_path: &std::path::Path,
) -> Result<(), JobError> {
    let conn = engine.raw();
    conn.execute_batch(
        "CREATE OR REPLACE TEMP TABLE __ls_corr_signals(
            signal_id VARCHAR, group_id VARCHAR, kind VARCHAR, rule_id VARCHAR,
            rule_version BIGINT, strength VARCHAR, investigative_lead BOOLEAN,
            from_record_id VARCHAR, to_record_id VARCHAR, from_event_time BIGINT,
            to_event_time BIGINT, delta_nanos BIGINT, tolerance_nanos BIGINT,
            matched_json VARCHAR, missing_json VARCHAR, reason VARCHAR,
            sort_no UBIGINT); BEGIN",
    )
    .map_err(engine_err)?;
    {
        let mut stmt = conn
            .prepare("INSERT INTO __ls_corr_signals VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
            .map_err(engine_err)?;
        for (i, s) in rows.iter().enumerate() {
            stmt.execute(duckdb::params![
                s.signal_id,
                s.group_id,
                s.kind,
                s.rule_id,
                s.rule_version,
                s.strength,
                s.investigative_lead,
                s.from_record_id,
                s.to_record_id,
                s.from_event_time,
                s.to_event_time,
                s.delta_nanos,
                s.tolerance_nanos,
                s.matched_json,
                s.missing_json,
                s.reason,
                i as u64,
            ])
            .map_err(engine_err)?;
        }
    }
    conn.execute_batch(&format!(
        "COMMIT; COPY (SELECT signal_id, group_id, kind, rule_id, rule_version, strength,
            investigative_lead, from_record_id, to_record_id, from_event_time,
            to_event_time, delta_nanos, tolerance_nanos, matched_json, missing_json,
            reason FROM __ls_corr_signals ORDER BY sort_no)
         TO '{}' (FORMAT PARQUET);
         DROP TABLE __ls_corr_signals;",
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

/// The behavioural signals observed inside one group.
///
/// A run produced before WP4b has no signals artifact and is refused by
/// [`derived_file`] with the re-run instruction, rather than returning
/// an empty page — "none were computed" and "none were found" are
/// different answers and must not look alike.
pub fn list_correlation_signals(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    group_id: &str,
    limit: u64,
) -> Result<Vec<CorrelationSignal>, JobError> {
    let run = require_completed_run(ws, run_id)?;
    let path = derived_file(ws, &run, SIGNALS_FILE)?;
    let limit = limit.clamp(1, 1_000);
    let conn = engine.raw();
    let mut stmt = conn
        .prepare(&format!(
            "SELECT signal_id, group_id, kind, rule_id, rule_version, strength,
                    investigative_lead, from_record_id, to_record_id, from_event_time,
                    to_event_time, delta_nanos, tolerance_nanos, matched_json,
                    missing_json, reason
             FROM read_parquet('{}') WHERE group_id = ? LIMIT {limit}",
            sql_quote(&path)
        ))
        .map_err(engine_err)?;
    let rows = stmt
        .query_map(duckdb::params![group_id], |r| {
            Ok(CorrelationSignal {
                signal_id: r.get(0)?,
                group_id: r.get(1)?,
                kind: r.get(2)?,
                rule_id: r.get(3)?,
                rule_version: r.get(4)?,
                strength: r.get(5)?,
                investigative_lead: r.get(6)?,
                from_record_id: r.get(7)?,
                to_record_id: r.get(8)?,
                from_event_time: r.get(9)?,
                to_event_time: r.get(10)?,
                delta_nanos: r.get(11)?,
                tolerance_nanos: r.get(12)?,
                matched_json: r.get(13)?,
                missing_json: r.get(14)?,
                reason: r.get(15)?,
            })
        })
        .map_err(engine_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(engine_err)?;
    Ok(rows)
}

/// One record admitted to a probable neighborhood.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbableNeighborRow {
    pub record_id: String,
    /// Exactly as recorded.
    pub event_time: i64,
    /// Neighbour time minus anchor time.
    pub delta_nanos: i64,
    pub matched_fields: Vec<String>,
    pub time_quality: String,
}

/// A neighborhood anchored to one selected record.
///
/// Everything gate 21 asks a probable relationship to expose is a field
/// here rather than something a caller has to assemble: the rule and its
/// version, the fields, the constraints, the tolerance, the timestamp
/// quality, and the limitation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbableNeighborhood {
    pub anchor_record_id: String,
    pub anchor_event_time: i64,
    pub anchor_time_quality: String,
    pub rule_id: String,
    pub rule_version: i64,
    /// Always `probable`. A neighborhood has no other confidence.
    pub confidence: String,
    pub compatible_fields: Vec<String>,
    pub constraints: String,
    pub tolerance_nanos: i64,
    pub neighbors: Vec<ProbableNeighborRow>,
    pub admitted: u64,
    /// Met the rule but fell outside the neighborhood limit. Nearest
    /// records are kept, so what is dropped is the least relevant.
    pub truncated: u64,
    pub scanned: u64,
    pub reason: String,
}

/// The anchor's own values, owned so they outlive the streaming closure
/// that compares candidates against them.
struct AnchorCapture {
    event_time: i64,
    quality: TimeQuality,
    dataset_id: String,
    source_id: String,
    resource_id: String,
    operation: Option<String>,
    outcome: Option<String>,
    event_name: Option<String>,
    event_type: Option<String>,
    severity: Option<String>,
}

fn neighbor_facts<'a>(row: &'a LogRow, quality: TimeQuality) -> NeighborFacts<'a> {
    NeighborFacts {
        record_id: &row.record_id,
        event_time: row.event_time,
        time_quality: quality,
        dataset_id: &row.dataset_id,
        source_id: &row.source_id,
        resource_id: Some(&row.resource_id),
        operation: row.operation.as_deref(),
        outcome: row.outcome.as_deref(),
        event_name: row.event_name.as_deref(),
        event_type: row.event_type.as_deref(),
        severity: row.severity_text.as_deref(),
    }
}

/// Builds a probable neighborhood around one record inside a completed
/// run's frozen scope.
///
/// Two bounded passes: the first locates the anchor, the second narrows
/// the run's window to the anchor's tolerance interval so the engine's
/// own time filter does the work. Without that narrowing this would
/// scan the whole run to find a handful of nearby records.
pub fn probable_neighborhood(
    ws: &Workspace,
    engine: &EngineConnection,
    run_id: &str,
    anchor_record_id: &str,
    compatible_fields: &[String],
    tolerance_nanos: i64,
    max_neighbors: u64,
) -> Result<ProbableNeighborhood, JobError> {
    let run = require_completed_run(ws, run_id)?;
    if run.state == "stale" {
        return Err(JobError::new(
            "analysis/stale-run",
            "the run's inputs changed after it completed; re-run before drilling down",
        ));
    }
    let fields: Vec<CompatibleField> = compatible_fields
        .iter()
        .map(|f| CompatibleField::parse(f).map_err(invalid))
        .collect::<Result<_, _>>()?;
    let rule = ProbableRule {
        compatible_fields: fields,
        tolerance_nanos,
    };
    rule.validate().map_err(invalid)?;
    let max_neighbors = max_neighbors.clamp(1, 1_000);

    let base = compiled_base(ws, &run)?;
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let budget = std::time::Duration::from_secs(300);

    // Pass 1: the anchor itself. Its values are captured as owned data
    // so pass 2 can compare against them without re-reading the row.
    let mut anchor: Option<AnchorCapture> = None;
    stream_query(
        engine,
        &base.files,
        &base.filter,
        &base.window(),
        5_000_000,
        &cancel,
        budget,
        |row| {
            if row.record_id != anchor_record_id {
                return Ok(true);
            }
            anchor = row.event_time.map(|event_time| AnchorCapture {
                event_time,
                quality: ingest_quality(&row).time,
                dataset_id: row.dataset_id.clone(),
                source_id: row.source_id.clone(),
                resource_id: row.resource_id.clone(),
                operation: row.operation.clone(),
                outcome: row.outcome.clone(),
                event_name: row.event_name.clone(),
                event_type: row.event_type.clone(),
                severity: row.severity_text.clone(),
            });
            Ok(false)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;

    let anchor = anchor.ok_or_else(|| {
        invalid(format!(
            "record {anchor_record_id} is not in this run's scope, or has no event time: a \
             neighborhood is defined by distance in time, so an undated record cannot anchor one"
        ))
    })?;
    let (anchor_time, anchor_quality) = (anchor.event_time, anchor.quality);

    // Pass 2: only the tolerance interval, intersected with the run's
    // own frozen window so a drill-down can never widen the run.
    let window = ResolvedWindow {
        strategy: base.strategy.clone(),
        start: Some(match base.start {
            Some(s) => s.max(anchor_time.saturating_sub(tolerance_nanos)),
            None => anchor_time.saturating_sub(tolerance_nanos),
        }),
        end: Some(match base.end {
            Some(e) => e.min(anchor_time.saturating_add(tolerance_nanos)),
            None => anchor_time.saturating_add(tolerance_nanos),
        }),
        empty_anchor: false,
    };

    let anchor_facts = NeighborFacts {
        record_id: anchor_record_id,
        event_time: Some(anchor.event_time),
        time_quality: anchor.quality,
        dataset_id: &anchor.dataset_id,
        source_id: &anchor.source_id,
        resource_id: Some(&anchor.resource_id),
        operation: anchor.operation.as_deref(),
        outcome: anchor.outcome.as_deref(),
        event_name: anchor.event_name.as_deref(),
        event_type: anchor.event_type.as_deref(),
        severity: anchor.severity.as_deref(),
    };

    let mut hits = Vec::new();
    let mut scanned: u64 = 0;
    let mut truncated: u64 = 0;
    stream_query(
        engine,
        &base.files,
        &base.filter,
        &window,
        5_000_000,
        &cancel,
        budget,
        |row| {
            scanned += 1;
            let quality = ingest_quality(&row).time;
            let facts = neighbor_facts(&row, quality);
            // `evaluate_probable` rejects the anchor as its own
            // neighbour, so no separate skip is needed here.
            if let Some(hit) = evaluate_probable(&anchor_facts, &facts, &rule) {
                hits.push(hit);
            }
            Ok(true)
        },
    )
    .map_err(|e| JobError::new(e.code(), e.to_string()))?;

    hits.sort_by(|a, b| neighbor_order(a).cmp(&neighbor_order(b)));
    let admitted = hits.len() as u64;
    if admitted > max_neighbors {
        truncated = admitted - max_neighbors;
        hits.truncate(max_neighbors as usize);
    }

    let reason = explain_neighborhood(
        &rule,
        anchor_record_id,
        anchor_quality,
        hits.len(),
        truncated as usize,
    );
    Ok(ProbableNeighborhood {
        anchor_record_id: anchor_record_id.to_string(),
        anchor_event_time: anchor_time,
        anchor_time_quality: anchor_quality.as_str().to_string(),
        rule_id: CORRELATION_RULE_ID.to_string(),
        rule_version: CORRELATION_RULE_VERSION,
        confidence: "probable".to_string(),
        compatible_fields: rule
            .compatible_fields
            .iter()
            .map(|f| f.as_str().to_string())
            .collect(),
        constraints: rule.describe_constraints(),
        tolerance_nanos,
        neighbors: hits
            .into_iter()
            .map(|h| ProbableNeighborRow {
                record_id: h.record_id,
                event_time: h.neighbor_event_time,
                delta_nanos: h.delta_nanos,
                matched_fields: h.matched_fields.iter().map(|s| s.to_string()).collect(),
                time_quality: h.time_quality.as_str().to_string(),
            })
            .collect(),
        admitted,
        truncated,
        scanned,
        reason,
    })
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
