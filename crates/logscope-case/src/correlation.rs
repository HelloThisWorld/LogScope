//! Deterministic log-to-log relationship rules (v0.4 WP4,
//! `corr-rules` v1) — the pure rule model behind correlation runs.
//!
//! Contract (design pass; enforced by the tests below):
//! - Three confidence classes and no others: `Exact` (canonical
//!   telemetry identity validated at ingest), `Correlated` (a stable
//!   typed application/transport identifier matched exactly on its
//!   canonical value), `Probable` (a documented bounded rule over
//!   compatible fields and event-time proximity, always an
//!   investigative lead).
//! - **Span ID alone can never select a group.** There is deliberately
//!   no such variant: a span ID is not globally unique, so the type
//!   system — not a runtime check someone can forget — is what makes
//!   the mistake unrepresentable.
//! - Exact keys are never normalized. Normalization belongs to
//!   ingestion for canonical identifiers; asking for case folding or a
//!   prefix strip on a trace ID is a structured refusal, not a silent
//!   no-op.
//! - Every applied normalization step travels into the explanation, so
//!   a reader sees exactly what was compared.
//! - Sharing an identifier is never ordering and never causation. Every
//!   explanation says so; no rule in this module infers direction.
//! - Records without an event time never enter an ordered sequence.
//! - Rules are declarative data validated by this core. There is no
//!   executable user rule anywhere in the model.

use serde::{Deserialize, Serialize};

use crate::signals::{TimeQuality, INVESTIGATIVE_LEAD};
use crate::CaseError;

/// Rule identity for the correlation rule set.
pub const CORRELATION_RULE_ID: &str = "corr-rules";
pub const CORRELATION_RULE_VERSION: i64 = 1;

/// Relationship confidence. Ordered by strength of evidence; time
/// proximity alone can only ever reach `Probable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Exact,
    Correlated,
    Probable,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Correlated => "correlated",
            Confidence::Probable => "probable",
        }
    }
}

/// The identifier a group is built from.
///
/// Note what is absent: there is no `SpanId` variant. A span ID is
/// unique only within its trace, so it cannot select a group on its
/// own, and the only way to use one is inside [`KeySelector::TraceAndSpan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySelector {
    /// Canonical W3C trace ID, validated at ingest.
    TraceId,
    /// The (trace ID, span ID) pair — records of one span.
    TraceAndSpan,
    RequestId,
    TransactionId,
    MessageId,
    EntityId,
    /// An explicitly selected typed attribute (session ID, job-run ID,
    /// or any other stable identifier the source carries).
    Attribute(String),
}

/// Selector names accepted in configuration.
pub const KEY_SELECTORS: &[&str] = &[
    "trace_id",
    "trace_span",
    "request_id",
    "transaction_id",
    "message_id",
    "entity_id",
    "attribute",
];

impl KeySelector {
    /// Parses a configured selector. `span_id` is refused by name with
    /// its reason: the mistake is common enough to deserve a real
    /// answer rather than "unknown key".
    pub fn parse(name: &str, attribute: Option<&str>) -> Result<KeySelector, CaseError> {
        match name {
            "trace_id" => Ok(KeySelector::TraceId),
            "trace_span" => Ok(KeySelector::TraceAndSpan),
            "request_id" => Ok(KeySelector::RequestId),
            "transaction_id" => Ok(KeySelector::TransactionId),
            "message_id" => Ok(KeySelector::MessageId),
            "entity_id" => Ok(KeySelector::EntityId),
            "attribute" => {
                let field = attribute.unwrap_or("").trim();
                if field.is_empty() {
                    return Err(CaseError::Invalid(
                        "the attribute key requires config.attribute naming a field".into(),
                    ));
                }
                Ok(KeySelector::Attribute(field.to_string()))
            }
            "span_id" => Err(CaseError::Invalid(
                "span_id cannot select a correlation group: a span ID is unique only \
                 within its trace. Use trace_span to group the records of one span."
                    .into(),
            )),
            other => Err(CaseError::Invalid(format!(
                "unknown correlation key {other:?} (expected one of {})",
                KEY_SELECTORS.join("|")
            ))),
        }
    }

    /// Stable selector name for identity and display.
    pub fn as_str(&self) -> &str {
        match self {
            KeySelector::TraceId => "trace_id",
            KeySelector::TraceAndSpan => "trace_span",
            KeySelector::RequestId => "request_id",
            KeySelector::TransactionId => "transaction_id",
            KeySelector::MessageId => "message_id",
            KeySelector::EntityId => "entity_id",
            KeySelector::Attribute(field) => field,
        }
    }

    /// Human phrase used in explanations.
    pub fn describe(&self) -> String {
        match self {
            KeySelector::TraceId => "canonical trace ID".into(),
            KeySelector::TraceAndSpan => "canonical trace and span ID pair".into(),
            KeySelector::RequestId => "typed request ID".into(),
            KeySelector::TransactionId => "typed transaction ID".into(),
            KeySelector::MessageId => "typed message ID".into(),
            KeySelector::EntityId => "typed entity ID".into(),
            KeySelector::Attribute(field) => format!("selected attribute {field}"),
        }
    }

    /// The confidence a group built from this key can carry. Never
    /// higher than the evidence: only canonical telemetry identity
    /// reaches `Exact`.
    pub fn confidence(&self) -> Confidence {
        match self {
            KeySelector::TraceId | KeySelector::TraceAndSpan => Confidence::Exact,
            _ => Confidence::Correlated,
        }
    }
}

/// Explicit, versioned key normalization. Every field defaults to off:
/// the documented default is an exact match on the canonical typed
/// value, and any deviation has to be asked for and is then reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KeyNormalization {
    pub trim: bool,
    pub case_fold: bool,
    pub strip_prefix: Option<String>,
}

impl KeyNormalization {
    pub fn is_identity(&self) -> bool {
        !self.trim && !self.case_fold && self.strip_prefix.is_none()
    }

    /// Rejects normalization that would corrupt canonical identity.
    pub fn validate_for(&self, selector: &KeySelector) -> Result<(), CaseError> {
        if selector.confidence() == Confidence::Exact && !self.is_identity() {
            return Err(CaseError::Invalid(format!(
                "normalization cannot be applied to {}: canonical telemetry identifiers \
                 are validated and normalized at ingest, and altering them here would \
                 make an exact relationship mean something else",
                selector.describe()
            )));
        }
        if let Some(prefix) = &self.strip_prefix {
            if prefix.is_empty() {
                return Err(CaseError::Invalid(
                    "strip_prefix must name a non-empty prefix".into(),
                ));
            }
        }
        Ok(())
    }

    /// Applies the configured steps in a fixed order, returning the
    /// compared value and the steps that actually changed it.
    pub fn apply(&self, raw: &str) -> (String, Vec<&'static str>) {
        let mut applied = Vec::new();
        let mut value = raw.to_string();
        if self.trim {
            let trimmed = value.trim().to_string();
            if trimmed != value {
                applied.push("trim");
            }
            value = trimmed;
        }
        if let Some(prefix) = &self.strip_prefix {
            if let Some(rest) = value.strip_prefix(prefix.as_str()) {
                value = rest.to_string();
                applied.push("strip_prefix");
            }
        }
        if self.case_fold {
            let folded = value.to_lowercase();
            if folded != value {
                applied.push("case_fold");
            }
            value = folded;
        }
        (value, applied)
    }
}

/// The fields one canonical record contributes to correlation. The
/// caller resolves them from its own row type, so this crate stays free
/// of any query or storage dependency.
#[derive(Debug, Clone, Copy, Default)]
pub struct CorrelationFacts<'a> {
    pub record_id: &'a str,
    pub event_time: Option<i64>,
    pub trace_id: Option<&'a str>,
    pub span_id: Option<&'a str>,
    pub request_id: Option<&'a str>,
    pub transaction_id: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub entity_id: Option<&'a str>,
    /// Pre-resolved value for [`KeySelector::Attribute`].
    pub attribute: Option<&'a str>,
}

/// Why a record could not join any group. Every record that produces no
/// key lands in exactly one of these buckets, so the totals add up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRejection {
    /// The source never carried the field.
    MissingField,
    /// The field is present but is not a canonical identifier. It stays
    /// visible on the record for diagnostics; it just cannot produce an
    /// exact relationship.
    NotCanonical,
    /// `trace_span` needs both halves.
    IncompletePair,
    /// Present but empty (or whitespace under a trim rule).
    EmptyValue,
}

impl KeyRejection {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyRejection::MissingField => "missing_field",
            KeyRejection::NotCanonical => "not_canonical",
            KeyRejection::IncompletePair => "incomplete_pair",
            KeyRejection::EmptyValue => "empty_value",
        }
    }
}

/// A canonical trace ID is 32 lowercase hex characters and never
/// all-zero; a span ID is 16. Ingestion already enforces this (invalid
/// originals are kept as attributes and flagged, never written to the
/// canonical column), and re-checking here keeps the guarantee local to
/// the rule that depends on it.
fn is_canonical_hex_id(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && !value.bytes().all(|b| b == b'0')
}

/// Extracts the raw (pre-normalization) group key for one record.
pub fn raw_group_key<'a>(
    selector: &KeySelector,
    facts: &CorrelationFacts<'a>,
) -> Result<String, KeyRejection> {
    let present = |v: Option<&'a str>| -> Result<&'a str, KeyRejection> {
        match v {
            None => Err(KeyRejection::MissingField),
            Some("") => Err(KeyRejection::EmptyValue),
            Some(s) => Ok(s),
        }
    };
    match selector {
        KeySelector::TraceId => {
            let trace = present(facts.trace_id)?;
            if !is_canonical_hex_id(trace, 32) {
                return Err(KeyRejection::NotCanonical);
            }
            Ok(trace.to_string())
        }
        KeySelector::TraceAndSpan => {
            let trace = present(facts.trace_id)?;
            if !is_canonical_hex_id(trace, 32) {
                return Err(KeyRejection::NotCanonical);
            }
            let span = match facts.span_id {
                None | Some("") => return Err(KeyRejection::IncompletePair),
                Some(s) => s,
            };
            if !is_canonical_hex_id(span, 16) {
                return Err(KeyRejection::NotCanonical);
            }
            Ok(format!("{trace}/{span}"))
        }
        KeySelector::RequestId => present(facts.request_id).map(str::to_string),
        KeySelector::TransactionId => present(facts.transaction_id).map(str::to_string),
        KeySelector::MessageId => present(facts.message_id).map(str::to_string),
        KeySelector::EntityId => present(facts.entity_id).map(str::to_string),
        KeySelector::Attribute(_) => present(facts.attribute).map(str::to_string),
    }
}

/// Raw key plus normalization, in one step: the value groups are keyed
/// by, and the steps that changed it.
pub fn group_key(
    selector: &KeySelector,
    normalization: &KeyNormalization,
    facts: &CorrelationFacts<'_>,
) -> Result<(String, Vec<&'static str>), KeyRejection> {
    let raw = raw_group_key(selector, facts)?;
    let (value, applied) = normalization.apply(&raw);
    if value.is_empty() {
        return Err(KeyRejection::EmptyValue);
    }
    Ok((value, applied))
}

/// Canonical position of a record inside a group: event time ascending,
/// then record ID ascending. Record IDs are deterministic content
/// addresses, so the tie-break is stable across runs and machines.
///
/// Returns `None` for a record with no event time. Such records are
/// counted in an explicit undated bucket and never placed in the
/// sequence — ordering them by import time would invent a story the
/// data does not support.
pub fn sequence_position<'a>(facts: &CorrelationFacts<'a>) -> Option<(i64, &'a str)> {
    facts.event_time.map(|t| (t, facts.record_id))
}

/// Bounded execution limits. Correlation is key-partitioned, never a
/// pairwise join, and every cap reports what it dropped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CorrelationLimits {
    /// Smallest group that is a relationship at all. One record sharing
    /// a key with nobody is not a correlation.
    pub min_group_size: usize,
    pub max_events_per_group: usize,
    pub max_groups: usize,
    pub max_edges_per_event: usize,
    pub max_total_edges: usize,
    /// Signals are bounded separately from edges: they answer a
    /// different question, and sharing one budget would let a noisy
    /// group's signals silently consume the sequence's edges.
    pub max_total_signals: usize,
}

impl Default for CorrelationLimits {
    fn default() -> Self {
        CorrelationLimits {
            min_group_size: 2,
            max_events_per_group: 256,
            max_groups: 10_000,
            max_edges_per_event: 32,
            max_total_edges: 250_000,
            max_total_signals: 250_000,
        }
    }
}

impl CorrelationLimits {
    pub fn parse(limits_json: &str) -> Result<CorrelationLimits, CaseError> {
        let trimmed = limits_json.trim();
        if trimmed.is_empty() || trimmed == "{}" {
            return Ok(CorrelationLimits::default());
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| CaseError::Invalid(format!("limits do not parse: {e}")))?;
        if !value.is_object() {
            return Err(CaseError::Invalid("limits must be a JSON object".into()));
        }
        let limits: CorrelationLimits = serde_json::from_value(value)
            .map_err(|e| CaseError::Invalid(format!("limits do not parse: {e}")))?;
        if limits.min_group_size < 2 {
            return Err(CaseError::Invalid(
                "min_group_size must be at least 2: a single record sharing a key with \
                 nobody is not a relationship"
                    .into(),
            ));
        }
        if limits.max_events_per_group < 2 || limits.max_groups == 0 {
            return Err(CaseError::Invalid(
                "max_events_per_group must be at least 2 and max_groups at least 1".into(),
            ));
        }
        Ok(limits)
    }
}

/// The limitation every correlation result carries. Shared identity is
/// not order and not cause, and no amount of it becomes either.
pub const SHARED_KEY_LIMITATION: &str =
    "A shared identifier establishes that these records refer to the same thing. \
     It does not establish ordering, parent/child structure, completeness, or causation.";

/// Additional limitation for trace-based groups: LogScope correlates
/// logs, and never reconstructs a trace from them.
pub const TRACE_LIMITATION: &str =
    "This is an identifier match across log records, not a reconstructed trace: \
     span topology, trace completeness, and parent/child order are not derived.";

/// Builds the explanation for a group. The reason states the rule, the
/// key, every normalization step that changed the compared value, and
/// the standing limitation.
pub fn explain_group(
    selector: &KeySelector,
    normalization: &KeyNormalization,
    key: &str,
    applied: &[&str],
) -> String {
    let confidence = selector.confidence();
    let mut text = format!(
        "{}: records share the {} {key}.",
        match confidence {
            Confidence::Exact => "Exact",
            Confidence::Correlated => "Correlated",
            Confidence::Probable => "Probable",
        },
        selector.describe()
    );
    if confidence == Confidence::Exact {
        text.push_str(" The identifier was validated at ingest.");
    } else if normalization.is_identity() {
        text.push_str(" Matched exactly on the canonical value, with no normalization.");
    } else if applied.is_empty() {
        text.push_str(
            " Normalization was configured but changed nothing for this key; \
             the values matched as stored.",
        );
    } else {
        text.push_str(&format!(
            " Normalization applied before matching: {}.",
            applied.join(", ")
        ));
    }
    text.push(' ');
    text.push_str(SHARED_KEY_LIMITATION);
    if confidence == Confidence::Exact {
        text.push(' ');
        text.push_str(TRACE_LIMITATION);
    }
    text
}

/// Explanation for one edge between consecutive records in a group.
/// `delta_nanos` is reported, never interpreted: a later timestamp does
/// not make one record the consequence of the other.
pub fn explain_edge(selector: &KeySelector, key: &str, delta_nanos: i64) -> String {
    format!(
        "Consecutive records in the {} group {key}, {delta_nanos} ns apart by canonical \
         event time. Adjacency in time is not causation, and the gap is reported as \
         measured — no timestamp was adjusted.",
        selector.describe()
    )
}

/* ------------------------------------------ probable neighborhoods */

/// A field that two records must agree on for a probable neighborhood.
/// Every variant is a typed canonical column — there is no free-text or
/// similarity option, because "looks alike" is not a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatibleField {
    Resource,
    Dataset,
    Source,
    Operation,
    Outcome,
    EventName,
    EventType,
    Severity,
}

pub const COMPATIBLE_FIELDS: &[&str] = &[
    "resource",
    "dataset",
    "source",
    "operation",
    "outcome",
    "event_name",
    "event_type",
    "severity",
];

impl CompatibleField {
    pub fn parse(name: &str) -> Result<CompatibleField, CaseError> {
        match name {
            "resource" => Ok(CompatibleField::Resource),
            "dataset" => Ok(CompatibleField::Dataset),
            "source" => Ok(CompatibleField::Source),
            "operation" => Ok(CompatibleField::Operation),
            "outcome" => Ok(CompatibleField::Outcome),
            "event_name" => Ok(CompatibleField::EventName),
            "event_type" => Ok(CompatibleField::EventType),
            "severity" => Ok(CompatibleField::Severity),
            other => Err(CaseError::Invalid(format!(
                "unknown compatible field {other:?} (expected one of {})",
                COMPATIBLE_FIELDS.join("|")
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CompatibleField::Resource => "resource",
            CompatibleField::Dataset => "dataset",
            CompatibleField::Source => "source",
            CompatibleField::Operation => "operation",
            CompatibleField::Outcome => "outcome",
            CompatibleField::EventName => "event_name",
            CompatibleField::EventType => "event_type",
            CompatibleField::Severity => "severity",
        }
    }

    fn of<'a>(self, facts: &NeighborFacts<'a>) -> Option<&'a str> {
        match self {
            CompatibleField::Resource => facts.resource_id,
            CompatibleField::Dataset => Some(facts.dataset_id),
            CompatibleField::Source => Some(facts.source_id),
            CompatibleField::Operation => facts.operation,
            CompatibleField::Outcome => facts.outcome,
            CompatibleField::EventName => facts.event_name,
            CompatibleField::EventType => facts.event_type,
            CompatibleField::Severity => facts.severity,
        }
    }
}

/// The canonical columns a neighborhood rule may compare.
#[derive(Debug, Clone, Copy, Default)]
pub struct NeighborFacts<'a> {
    pub record_id: &'a str,
    pub event_time: Option<i64>,
    pub time_quality: TimeQuality,
    pub dataset_id: &'a str,
    pub source_id: &'a str,
    pub resource_id: Option<&'a str>,
    pub operation: Option<&'a str>,
    pub outcome: Option<&'a str>,
    pub event_name: Option<&'a str>,
    pub event_type: Option<&'a str>,
    pub severity: Option<&'a str>,
}

/// A bounded, documented proximity rule anchored to one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbableRule {
    /// Fields the anchor and neighbour must agree on. May be empty,
    /// which is proximity alone and is reported as such.
    pub compatible_fields: Vec<CompatibleField>,
    /// Half-width of the window around the anchor's event time.
    pub tolerance_nanos: i64,
}

impl ProbableRule {
    pub fn validate(&self) -> Result<(), CaseError> {
        if self.tolerance_nanos <= 0 {
            return Err(CaseError::Invalid(
                "tolerance_nanos must be positive: a neighborhood with no width selects \
                 only records sharing an exact instant"
                    .into(),
            ));
        }
        let mut seen = Vec::new();
        for field in &self.compatible_fields {
            if seen.contains(&field.as_str()) {
                return Err(CaseError::Invalid(format!(
                    "compatible field {} is listed twice",
                    field.as_str()
                )));
            }
            seen.push(field.as_str());
        }
        Ok(())
    }

    /// The constraints this rule imposes, rendered for display. Gate 21
    /// requires a neighborhood to state its own constraints, not just
    /// its results.
    pub fn describe_constraints(&self) -> String {
        let fields = if self.compatible_fields.is_empty() {
            "no field equality required (time proximity alone)".to_string()
        } else {
            format!(
                "equal {}",
                self.compatible_fields
                    .iter()
                    .map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(" and ")
            )
        };
        format!(
            "within {} ns of the anchor's event time, {fields}",
            self.tolerance_nanos
        )
    }
}

/// One record admitted to a neighborhood.
///
/// The confidence is not a field, and there is no constructor that sets
/// one: a neighborhood is [`Confidence::Probable`] and cannot be
/// anything else. Gate 22 is therefore a property of the type rather
/// than a check that could be forgotten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbableNeighbor {
    pub record_id: String,
    /// Neighbour time minus anchor time, exactly as recorded.
    pub delta_nanos: i64,
    pub anchor_event_time: i64,
    pub neighbor_event_time: i64,
    pub matched_fields: Vec<&'static str>,
    pub time_quality: TimeQuality,
}

impl ProbableNeighbor {
    /// Always `Probable`. Time proximity is evidence that two things
    /// were near each other, and nothing more.
    pub fn confidence(&self) -> Confidence {
        Confidence::Probable
    }
}

/// The limitation every neighborhood carries.
pub const PROXIMITY_LIMITATION: &str =
    "Records near each other in time are not thereby related: any busy system produces \
     unrelated records in the same interval. This neighborhood is a starting point for \
     investigation, not a relationship.";

/// Evaluates one candidate against the anchor.
///
/// Returns `None` when either record is undated (a neighborhood is
/// defined by time, and an undated record has no distance), when the
/// candidate is the anchor itself, when it falls outside the tolerance,
/// or when a required field is absent or unequal. A required field that
/// the candidate does not carry is a rejection, never a wildcard.
pub fn evaluate_probable(
    anchor: &NeighborFacts<'_>,
    candidate: &NeighborFacts<'_>,
    rule: &ProbableRule,
) -> Option<ProbableNeighbor> {
    if candidate.record_id == anchor.record_id {
        return None;
    }
    let (anchor_time, candidate_time) = (anchor.event_time?, candidate.event_time?);
    let delta = candidate_time.checked_sub(anchor_time)?;
    if delta.saturating_abs() > rule.tolerance_nanos {
        return None;
    }

    let mut matched = Vec::new();
    for field in &rule.compatible_fields {
        match (field.of(anchor), field.of(candidate)) {
            (Some(a), Some(b)) if a == b => matched.push(field.as_str()),
            _ => return None,
        }
    }

    Some(ProbableNeighbor {
        record_id: candidate.record_id.to_string(),
        delta_nanos: delta,
        anchor_event_time: anchor_time,
        neighbor_event_time: candidate_time,
        matched_fields: matched,
        time_quality: candidate.time_quality,
    })
}

/// Canonical neighborhood ordering: absolute distance from the anchor,
/// then event time, then record ID. Distance first is what makes a
/// truncated neighborhood useful — the records dropped by a limit are
/// the least relevant ones, not an arbitrary tail.
pub fn neighbor_order(n: &ProbableNeighbor) -> (i64, i64, &str) {
    (
        n.delta_nanos.saturating_abs(),
        n.neighbor_event_time,
        n.record_id.as_str(),
    )
}

/// Builds the explanation for a neighborhood. Gate 21 wants all six of
/// rule, fields, constraints, tolerance, quality, and limitation, so
/// each one is named here rather than left for a caller to remember.
pub fn explain_neighborhood(
    rule: &ProbableRule,
    anchor_record_id: &str,
    anchor_quality: TimeQuality,
    admitted: usize,
    truncated: usize,
) -> String {
    let mut text = format!(
        "Probable ({CORRELATION_RULE_ID} v{CORRELATION_RULE_VERSION}, neighborhood): \
         {admitted} record(s) anchored to {anchor_record_id} under the constraint {}. \
         Anchor timestamp quality: {}.",
        rule.describe_constraints(),
        anchor_quality.as_str(),
    );
    if truncated > 0 {
        text.push_str(&format!(
            " {truncated} further record(s) met the rule but were dropped by the \
             neighborhood limit, nearest first."
        ));
    }
    text.push(' ');
    text.push_str(INVESTIGATIVE_LEAD);
    text.push(' ');
    text.push_str(PROXIMITY_LIMITATION);
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN: &str = "00f067aa0ba902b7";

    fn facts_with_trace<'a>(record_id: &'a str, trace: Option<&'a str>) -> CorrelationFacts<'a> {
        CorrelationFacts {
            record_id,
            event_time: Some(1),
            trace_id: trace,
            ..Default::default()
        }
    }

    #[test]
    fn span_alone_can_never_select_a_group() {
        let err = KeySelector::parse("span_id", None).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("unique only"), "{message}");
        assert!(message.contains("trace_span"), "the answer names the fix");
        // And the variant does not exist: every selector that reaches
        // Exact carries a trace ID.
        for name in KEY_SELECTORS {
            let selector = KeySelector::parse(name, Some("session.id")).unwrap();
            if selector.confidence() == Confidence::Exact {
                assert!(matches!(
                    selector,
                    KeySelector::TraceId | KeySelector::TraceAndSpan
                ));
            }
        }
    }

    #[test]
    fn exact_requires_canonical_identifiers() {
        let selector = KeySelector::TraceId;
        assert_eq!(
            raw_group_key(&selector, &facts_with_trace("r1", Some(TRACE))).unwrap(),
            TRACE
        );
        // Invalid originals stay visible on the record but never group.
        for bad in [
            "not-a-trace",
            "4BF92F3577B34DA6A3CE929D0E0E4736",
            &"0".repeat(32),
        ] {
            assert_eq!(
                raw_group_key(&selector, &facts_with_trace("r1", Some(bad))),
                Err(KeyRejection::NotCanonical),
                "{bad} must not produce an exact key"
            );
        }
        assert_eq!(
            raw_group_key(&selector, &facts_with_trace("r1", None)),
            Err(KeyRejection::MissingField)
        );
    }

    #[test]
    fn trace_span_needs_both_halves() {
        let selector = KeySelector::TraceAndSpan;
        let both = CorrelationFacts {
            record_id: "r1",
            trace_id: Some(TRACE),
            span_id: Some(SPAN),
            ..Default::default()
        };
        assert_eq!(
            raw_group_key(&selector, &both).unwrap(),
            format!("{TRACE}/{SPAN}")
        );
        let trace_only = CorrelationFacts {
            record_id: "r1",
            trace_id: Some(TRACE),
            ..Default::default()
        };
        assert_eq!(
            raw_group_key(&selector, &trace_only),
            Err(KeyRejection::IncompletePair)
        );
        let span_only = CorrelationFacts {
            record_id: "r1",
            span_id: Some(SPAN),
            ..Default::default()
        };
        assert_eq!(
            raw_group_key(&selector, &span_only),
            Err(KeyRejection::MissingField),
            "a span without its trace is not a key"
        );
    }

    #[test]
    fn normalization_is_refused_on_canonical_identity_and_reported_otherwise() {
        let folding = KeyNormalization {
            case_fold: true,
            ..Default::default()
        };
        for exact in [KeySelector::TraceId, KeySelector::TraceAndSpan] {
            let err = folding.validate_for(&exact).unwrap_err().to_string();
            assert!(err.contains("validated and normalized at ingest"), "{err}");
        }
        // On a correlated key it is allowed, versioned, and visible.
        let selector = KeySelector::RequestId;
        folding.validate_for(&selector).unwrap();
        let normalization = KeyNormalization {
            trim: true,
            case_fold: true,
            strip_prefix: Some("req-".into()),
        };
        let (value, applied) = normalization.apply("  req-ABC123 ");
        assert_eq!(value, "abc123");
        assert_eq!(applied, ["trim", "strip_prefix", "case_fold"]);
        // Steps that change nothing are not claimed.
        let (value, applied) = normalization.apply("abc123");
        assert_eq!(value, "abc123");
        assert!(applied.is_empty());
        // The default compares the canonical value untouched.
        let default = KeyNormalization::default();
        assert!(default.is_identity());
        assert_eq!(default.apply(" Req-1 "), (" Req-1 ".to_string(), vec![]));
    }

    #[test]
    fn explanations_state_the_rule_and_refuse_to_imply_causation() {
        let text = explain_group(
            &KeySelector::TraceId,
            &KeyNormalization::default(),
            TRACE,
            &[],
        );
        assert!(text.starts_with("Exact:"));
        assert!(text.contains("validated at ingest"));
        assert!(text.contains("does not establish ordering"));
        assert!(text.contains("not a reconstructed trace"));

        let normalization = KeyNormalization {
            case_fold: true,
            ..Default::default()
        };
        let text = explain_group(
            &KeySelector::RequestId,
            &normalization,
            "abc",
            &["case_fold"],
        );
        assert!(text.starts_with("Correlated:"));
        assert!(text.contains("Normalization applied before matching: case_fold"));
        assert!(text.contains("does not establish ordering"));

        let text = explain_group(
            &KeySelector::RequestId,
            &KeyNormalization::default(),
            "abc",
            &[],
        );
        assert!(text.contains("no normalization"));

        let edge = explain_edge(&KeySelector::MessageId, "m-1", 4_000);
        assert!(edge.contains("4000 ns apart"));
        assert!(edge.contains("not causation"));
        assert!(edge.contains("no timestamp was adjusted"));

        // No explanation in this module may assert causation.
        for text in [
            explain_group(
                &KeySelector::TraceId,
                &KeyNormalization::default(),
                TRACE,
                &[],
            ),
            explain_edge(&KeySelector::TraceId, TRACE, -5),
        ] {
            let lowered = text.to_lowercase();
            for forbidden in ["caused by", "because of", "root cause", "therefore"] {
                assert!(!lowered.contains(forbidden), "{forbidden} in {text}");
            }
        }
    }

    #[test]
    fn undated_records_never_enter_the_sequence() {
        let dated = CorrelationFacts {
            record_id: "r2",
            event_time: Some(7),
            ..Default::default()
        };
        let undated = CorrelationFacts {
            record_id: "r1",
            event_time: None,
            ..Default::default()
        };
        assert_eq!(sequence_position(&dated), Some((7, "r2")));
        assert_eq!(sequence_position(&undated), None);
        // Equal timestamps order by the deterministic record id.
        let a = CorrelationFacts {
            record_id: "rb",
            event_time: Some(7),
            ..Default::default()
        };
        let b = CorrelationFacts {
            record_id: "ra",
            event_time: Some(7),
            ..Default::default()
        };
        assert!(sequence_position(&b) < sequence_position(&a));
    }

    fn neighbor<'a>(
        id: &'a str,
        time: Option<i64>,
        operation: Option<&'a str>,
    ) -> NeighborFacts<'a> {
        NeighborFacts {
            record_id: id,
            event_time: time,
            time_quality: TimeQuality::Observed,
            dataset_id: "ds-1",
            source_id: "src-1",
            operation,
            ..Default::default()
        }
    }

    #[test]
    fn a_neighborhood_can_never_be_more_than_probable() {
        let rule = ProbableRule {
            compatible_fields: vec![CompatibleField::Operation],
            tolerance_nanos: 1_000,
        };
        rule.validate().unwrap();
        let anchor = neighbor("r1", Some(10_000), Some("checkout"));
        let near = neighbor("r2", Some(10_400), Some("checkout"));
        let hit = evaluate_probable(&anchor, &near, &rule).unwrap();
        assert_eq!(hit.confidence(), Confidence::Probable);
        assert_eq!(hit.delta_nanos, 400);
        assert_eq!(hit.matched_fields, ["operation"]);
        // Originals are carried through, not just the delta.
        assert_eq!(
            (hit.anchor_event_time, hit.neighbor_event_time),
            (10_000, 10_400)
        );

        // Even trace-identical records reached through this rule stay
        // Probable: the rule is proximity, so the answer is proximity.
        let mut exact_twin = neighbor("r3", Some(10_100), Some("checkout"));
        exact_twin.resource_id = Some("res-1");
        assert_eq!(
            evaluate_probable(&anchor, &exact_twin, &rule)
                .unwrap()
                .confidence(),
            Confidence::Probable
        );
    }

    #[test]
    fn neighborhoods_reject_rather_than_widen() {
        let rule = ProbableRule {
            compatible_fields: vec![CompatibleField::Operation],
            tolerance_nanos: 1_000,
        };
        let anchor = neighbor("r1", Some(10_000), Some("checkout"));
        // Outside the window.
        assert!(evaluate_probable(
            &anchor,
            &neighbor("r2", Some(11_001), Some("checkout")),
            &rule
        )
        .is_none());
        // Boundary is inclusive and symmetric.
        assert!(evaluate_probable(
            &anchor,
            &neighbor("r2", Some(11_000), Some("checkout")),
            &rule
        )
        .is_some());
        assert!(evaluate_probable(
            &anchor,
            &neighbor("r2", Some(9_000), Some("checkout")),
            &rule
        )
        .is_some());
        // A required field the candidate does not carry is a rejection,
        // not a wildcard.
        assert!(evaluate_probable(&anchor, &neighbor("r2", Some(10_100), None), &rule).is_none());
        // Different value: rejected.
        assert!(evaluate_probable(
            &anchor,
            &neighbor("r2", Some(10_100), Some("refund")),
            &rule
        )
        .is_none());
        // Undated records have no distance and never join.
        assert!(
            evaluate_probable(&anchor, &neighbor("r2", None, Some("checkout")), &rule).is_none()
        );
        let undated_anchor = neighbor("r1", None, Some("checkout"));
        assert!(evaluate_probable(
            &undated_anchor,
            &neighbor("r2", Some(10_000), Some("checkout")),
            &rule
        )
        .is_none());
        // The anchor is not its own neighbour.
        assert!(evaluate_probable(&anchor, &anchor, &rule).is_none());
    }

    #[test]
    fn proximity_alone_is_allowed_but_says_so() {
        let rule = ProbableRule {
            compatible_fields: vec![],
            tolerance_nanos: 500,
        };
        rule.validate().unwrap();
        let anchor = neighbor("r1", Some(0), None);
        let hit = evaluate_probable(&anchor, &neighbor("r2", Some(100), None), &rule).unwrap();
        assert!(hit.matched_fields.is_empty());
        assert_eq!(hit.confidence(), Confidence::Probable);
        assert!(rule.describe_constraints().contains("time proximity alone"));

        let text = explain_neighborhood(&rule, "r1", TimeQuality::Observed, 1, 0);
        assert!(text.starts_with("Probable ("));
        assert!(text.contains("time proximity alone"));
        assert!(text.contains(INVESTIGATIVE_LEAD));
        assert!(text.contains("not thereby related"));
        assert!(text.contains("quality: observed"));
    }

    #[test]
    fn neighborhood_ordering_drops_the_least_relevant_first() {
        let rule = ProbableRule {
            compatible_fields: vec![],
            tolerance_nanos: 10_000,
        };
        let anchor = neighbor("r0", Some(1_000), None);
        let mut hits: Vec<_> = [("r3", 9_000i64), ("r1", 1_100), ("r2", 800)]
            .iter()
            .map(|(id, t)| {
                evaluate_probable(&anchor, &neighbor(id, Some(*t), None), &rule).unwrap()
            })
            .collect();
        hits.sort_by(|a, b| neighbor_order(a).cmp(&neighbor_order(b)));
        // Nearest in absolute time first, regardless of direction.
        assert_eq!(
            hits.iter()
                .map(|h| h.record_id.as_str())
                .collect::<Vec<_>>(),
            ["r1", "r2", "r3"]
        );
        assert_eq!(hits[0].delta_nanos, 100);
        assert_eq!(hits[1].delta_nanos, -200);

        let text = explain_neighborhood(&rule, "r0", TimeQuality::Inferred, 2, 1);
        assert!(text.contains("1 further record(s)"));
        assert!(text.contains("nearest first"));
        assert!(text.contains("quality: inferred"));
    }

    #[test]
    fn neighborhood_rules_refuse_degenerate_configuration() {
        let zero = ProbableRule {
            compatible_fields: vec![],
            tolerance_nanos: 0,
        };
        assert!(zero
            .validate()
            .unwrap_err()
            .to_string()
            .contains("no width"));
        let duplicated = ProbableRule {
            compatible_fields: vec![CompatibleField::Operation, CompatibleField::Operation],
            tolerance_nanos: 1,
        };
        assert!(duplicated
            .validate()
            .unwrap_err()
            .to_string()
            .contains("twice"));
        assert!(CompatibleField::parse("message").is_err());
        for name in COMPATIBLE_FIELDS {
            assert_eq!(CompatibleField::parse(name).unwrap().as_str(), *name);
        }
    }

    #[test]
    fn limits_are_bounded_and_refuse_meaningless_values() {
        let d = CorrelationLimits::default();
        assert_eq!(d.min_group_size, 2);
        assert_eq!(d.max_events_per_group, 256);
        assert_eq!(d.max_groups, 10_000);
        assert_eq!(d.max_total_signals, 250_000);
        assert_eq!(CorrelationLimits::parse("{}").unwrap(), d);
        let custom = CorrelationLimits::parse("{\"max_groups\":5}").unwrap();
        assert_eq!(custom.max_groups, 5);
        assert_eq!(custom.min_group_size, d.min_group_size);
        // Signals are budgeted apart from edges.
        let split = CorrelationLimits::parse("{\"max_total_signals\":7}").unwrap();
        assert_eq!(
            (split.max_total_signals, split.max_total_edges),
            (7, 250_000)
        );
        assert!(CorrelationLimits::parse("{\"min_group_size\":1}").is_err());
        assert!(CorrelationLimits::parse("{\"surprise\":1}").is_err());
        assert!(CorrelationLimits::parse("[]").is_err());
    }
}
