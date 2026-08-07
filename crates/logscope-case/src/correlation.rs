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
}

impl Default for CorrelationLimits {
    fn default() -> Self {
        CorrelationLimits {
            min_group_size: 2,
            max_events_per_group: 256,
            max_groups: 10_000,
            max_edges_per_event: 32,
            max_total_edges: 250_000,
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

    #[test]
    fn limits_are_bounded_and_refuse_meaningless_values() {
        let d = CorrelationLimits::default();
        assert_eq!(d.min_group_size, 2);
        assert_eq!(d.max_events_per_group, 256);
        assert_eq!(d.max_groups, 10_000);
        assert_eq!(CorrelationLimits::parse("{}").unwrap(), d);
        let custom = CorrelationLimits::parse("{\"max_groups\":5}").unwrap();
        assert_eq!(custom.max_groups, 5);
        assert_eq!(custom.min_group_size, d.min_group_size);
        assert!(CorrelationLimits::parse("{\"min_group_size\":1}").is_err());
        assert!(CorrelationLimits::parse("{\"surprise\":1}").is_err());
        assert!(CorrelationLimits::parse("[]").is_err());
    }
}
