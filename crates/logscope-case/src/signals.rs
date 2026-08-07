//! Behavioural signals over correlated records (v0.4 WP4b,
//! `sig-rules` v1) — retry, operational duplicate, clock skew, gap.
//!
//! Each signal is a declarative rule with its own ID and version whose
//! result names the fields that matched **and the fields that would
//! have strengthened it but were absent**. A reader can therefore see
//! not just what the rule concluded but how much of the evidence it
//! wanted was actually there.
//!
//! Contract (enforced by the tests below):
//! - Every signal reports an [`EvidenceStrength`]. Only [`Documented`]
//!   evidence — the source itself stating the fact in a typed field —
//!   is reported as observed. Everything weaker is an investigative
//!   lead and says so in its own text.
//! - **Message text is never retry evidence.** Not fuzzily, not
//!   exactly. Two records saying the same thing is not a retry, and
//!   [`RetryFacts`] deliberately has nowhere to put a message.
//! - **An ingestion duplicate is not an operational duplicate.** The
//!   same source line imported twice says something about LogScope;
//!   only distinct source positions say something about the system
//!   being investigated. The rule classifies rather than filters, so
//!   the ingestion case stays visible instead of vanishing.
//! - Skew and gap rules report original timestamps, the measured
//!   delta, and the tolerance they were judged against. **No rule here
//!   returns an adjusted time**; there is no field to return one in.
//! - A gap is never evidence that nothing happened.
//! - No rule infers causation, and no generated string may claim it.
//!
//! [`Documented`]: EvidenceStrength::Documented

use serde::{Deserialize, Serialize};

use crate::CaseError;

/// Rule-set identity. Individual signals carry their own rule ID within
/// the set so a result can name exactly the rule that produced it.
pub const SIGNAL_RULE_SET_ID: &str = "sig-rules";
pub const SIGNAL_RULE_SET_VERSION: i64 = 1;

/// The four signals of `sig-rules` v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalKind {
    Retry,
    OperationalDuplicate,
    ClockSkew,
    Gap,
}

pub const SIGNAL_KINDS: &[&str] = &["retry", "operational_duplicate", "clock_skew", "gap"];

impl SignalKind {
    pub fn parse(name: &str) -> Result<SignalKind, CaseError> {
        match name {
            "retry" => Ok(SignalKind::Retry),
            "operational_duplicate" => Ok(SignalKind::OperationalDuplicate),
            "clock_skew" => Ok(SignalKind::ClockSkew),
            "gap" => Ok(SignalKind::Gap),
            other => Err(CaseError::Invalid(format!(
                "unknown signal {other:?} (expected one of {})",
                SIGNAL_KINDS.join("|")
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SignalKind::Retry => "retry",
            SignalKind::OperationalDuplicate => "operational_duplicate",
            SignalKind::ClockSkew => "clock_skew",
            SignalKind::Gap => "gap",
        }
    }

    /// Stable per-signal rule ID, reported with every result.
    pub fn rule_id(self) -> &'static str {
        match self {
            SignalKind::Retry => "retry-signal",
            SignalKind::OperationalDuplicate => "operational-duplicate",
            SignalKind::ClockSkew => "clock-skew",
            SignalKind::Gap => "gap",
        }
    }

    /// The standing limitation for this signal — the thing a reader is
    /// most likely to over-read from it.
    pub fn limitation(self) -> &'static str {
        match self {
            SignalKind::Retry => {
                "A retry signal describes repeated attempts at the same operation. It does \
                 not establish that the retry was triggered by the earlier outcome, that it \
                 succeeded, or that any client behaviour was configured."
            }
            SignalKind::OperationalDuplicate => {
                "Repeated identical messages from distinct source positions describe what the \
                 source emitted. They do not establish that the underlying work happened more \
                 than once."
            }
            SignalKind::ClockSkew => {
                "Timestamps that move backwards against the source's own record order describe \
                 the recorded times, not the true instants. No timestamp is adjusted, and the \
                 cause (clock correction, batching, concurrent writers) is not determined."
            }
            SignalKind::Gap => {
                "A gap between records is not evidence that nothing happened: collection \
                 failures, filtering, sampling, retention, and level changes all remove records \
                 without any change in system behaviour."
            }
        }
    }
}

/// How strong the evidence behind a signal is.
///
/// This is the ladder gate 24 turns on: only [`Documented`] is reported
/// as an observation. Everything below it is explicitly a lead.
///
/// [`Documented`]: EvidenceStrength::Documented
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    /// One weak but deterministic indicator. Investigative lead only.
    Indicative,
    /// Several independent typed fields agree. Investigative lead only.
    Corroborated,
    /// The source states the fact itself, in a typed field.
    Documented,
}

impl EvidenceStrength {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceStrength::Indicative => "indicative",
            EvidenceStrength::Corroborated => "corroborated",
            EvidenceStrength::Documented => "documented",
        }
    }

    /// Anything the source did not state itself is a lead, and every
    /// rendered explanation says so.
    pub fn is_investigative_lead(self) -> bool {
        self != EvidenceStrength::Documented
    }
}

/// The label carried by every non-documented signal.
pub const INVESTIGATIVE_LEAD: &str = "Investigative lead only.";

/// The evidence behind one signal result: the rule that produced it,
/// how strong it is, and — the part that keeps it honest — which fields
/// were absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalEvidence {
    pub kind: SignalKind,
    pub strength: EvidenceStrength,
    /// Typed fields that supported the conclusion, in rule order.
    pub matched: Vec<&'static str>,
    /// Typed fields the rule looks for that this pair did not carry.
    pub missing: Vec<&'static str>,
}

impl SignalEvidence {
    pub fn rule_id(&self) -> &'static str {
        self.kind.rule_id()
    }

    /// Renders the evidence ladder in words: rule, strength, what
    /// matched, what was missing, the lead label when it applies, and
    /// the signal's standing limitation.
    pub fn explain(&self) -> String {
        let mut text = format!(
            "{} ({} v{}): {} evidence.",
            self.kind.rule_id(),
            SIGNAL_RULE_SET_ID,
            SIGNAL_RULE_SET_VERSION,
            self.strength.as_str()
        );
        if self.matched.is_empty() {
            text.push_str(" No typed field matched.");
        } else {
            text.push_str(&format!(" Matched on {}.", self.matched.join(", ")));
        }
        if self.missing.is_empty() {
            text.push_str(" Every field this rule considers was present.");
        } else {
            text.push_str(&format!(" Absent: {}.", self.missing.join(", ")));
        }
        if self.strength.is_investigative_lead() {
            text.push(' ');
            text.push_str(INVESTIGATIVE_LEAD);
        }
        text.push(' ');
        text.push_str(self.kind.limitation());
        text
    }
}

/// How well a record's event time is known. Signals that reason about
/// time refuse to treat an inferred timestamp as an observed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeQuality {
    /// The source carried an explicit timestamp for this record.
    Observed,
    /// The time was derived (carried forward from a previous line, from
    /// a file-level date, or otherwise reconstructed at import).
    Inferred,
    /// No event time at all.
    #[default]
    Missing,
}

impl TimeQuality {
    pub fn as_str(self) -> &'static str {
        match self {
            TimeQuality::Observed => "observed",
            TimeQuality::Inferred => "inferred",
            TimeQuality::Missing => "missing",
        }
    }
}

/* ------------------------------------------------------------- retry */

/// What the retry rule is allowed to look at.
///
/// Note what is missing: **there is no message field.** Two records
/// carrying the same text are not a retry, and rather than accept a
/// message and then refuse to use it, the rule cannot see one at all.
#[derive(Debug, Clone, Copy, Default)]
pub struct RetryFacts<'a> {
    /// Explicit typed attempt counter from the source (`retry.count`,
    /// `attempt`, or whichever attribute the definition selected).
    pub attempt: Option<i64>,
    pub operation: Option<&'a str>,
    pub outcome: Option<&'a str>,
}

/// Outcome values that count as a failed attempt. Exact match on the
/// canonical lowercase vocabulary — never a substring test.
const FAILURE_OUTCOMES: &[&str] = &["failure", "failed", "error", "timeout", "rejected"];

fn is_failure(outcome: Option<&str>) -> bool {
    outcome.is_some_and(|o| FAILURE_OUTCOMES.contains(&o))
}

/// Classifies an ordered pair of records that already share a
/// correlation key. `earlier`/`later` come from the group's canonical
/// sequence, so this rule never has to decide ordering itself.
///
/// Returns `None` when nothing beyond the shared key is present: a
/// shared identifier on its own is a correlation, not a retry.
pub fn classify_retry(earlier: &RetryFacts<'_>, later: &RetryFacts<'_>) -> Option<SignalEvidence> {
    let mut matched = Vec::new();
    let mut missing = Vec::new();

    // The source counting attempts is the only thing that makes a retry
    // an observation rather than a reading.
    let counted = match (earlier.attempt, later.attempt) {
        (Some(a), Some(b)) if b > a => true,
        // The source counts attempts and this one did not advance: it is
        // telling us these are the same attempt, logged twice. Believe
        // it — a weaker rung here would contradict the better evidence.
        (Some(_), Some(_)) => return None,
        (None, Some(b)) if b >= 1 => true,
        _ => false,
    };
    if counted {
        matched.push("attempt");
    } else {
        missing.push("attempt");
    }

    let same_operation = match (earlier.operation, later.operation) {
        (Some(a), Some(b)) => {
            if a == b {
                matched.push("operation");
                true
            } else {
                // A different operation is positive evidence against a
                // retry, not a missing field.
                return None;
            }
        }
        _ => {
            missing.push("operation");
            false
        }
    };

    let failed_first = is_failure(earlier.outcome);
    if failed_first {
        matched.push("outcome");
    } else if earlier.outcome.is_none() {
        missing.push("outcome");
    }

    let strength = if counted {
        EvidenceStrength::Documented
    } else if same_operation && failed_first {
        EvidenceStrength::Corroborated
    } else if same_operation {
        EvidenceStrength::Indicative
    } else {
        return None;
    };

    Some(SignalEvidence {
        kind: SignalKind::Retry,
        strength,
        matched,
        missing,
    })
}

/* -------------------------------------------------- operational dup */

/// Where a record physically came from. Two records with the same
/// [`SourcePosition`] are the same source line seen twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition<'a> {
    pub dataset_id: &'a str,
    pub source_id: &'a str,
    /// Ordinal of the record within its source, assigned at import.
    pub record_number: Option<i64>,
}

/// What the duplicate rule sees.
#[derive(Debug, Clone, Copy)]
pub struct DuplicateFacts<'a> {
    pub position: SourcePosition<'a>,
    /// Canonical message text, compared for exact equality only.
    pub message: &'a str,
    /// Typed logical message identity, when the source carries one.
    pub message_id: Option<&'a str>,
    pub operation: Option<&'a str>,
}

/// The result of asking "are these two records a duplicate, and of what
/// kind?" — the distinction gate 25 exists to protect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateClass {
    /// Different content: not a duplicate at all.
    Distinct,
    /// The same physical source line represented twice. This is an
    /// import-side observation about LogScope's own ingestion, and is
    /// deliberately NOT an operational signal.
    Ingestion { reason: &'static str },
    /// Distinct source positions carrying the same message — the system
    /// under investigation emitted it more than once.
    Operational(SignalEvidence),
}

/// Ingestion duplicates are recognised by position, before content is
/// ever considered — otherwise the same line imported twice would be
/// reported as the system repeating itself.
pub fn classify_duplicate(a: &DuplicateFacts<'_>, b: &DuplicateFacts<'_>) -> DuplicateClass {
    let same_source = a.position.dataset_id == b.position.dataset_id
        && a.position.source_id == b.position.source_id;
    if same_source {
        match (a.position.record_number, b.position.record_number) {
            (Some(x), Some(y)) if x == y => {
                return DuplicateClass::Ingestion {
                    reason: "same dataset, source, and record number: one source line \
                             represented twice, which describes ingestion rather than the \
                             system under investigation",
                };
            }
            (None, None) => {
                return DuplicateClass::Ingestion {
                    reason: "same dataset and source with no record number on either record: \
                             the two cannot be shown to be distinct source lines, so this is \
                             not reported as an operational duplicate",
                };
            }
            _ => {}
        }
    }

    if a.message != b.message {
        return DuplicateClass::Distinct;
    }

    let mut matched = vec!["message"];
    let mut missing = Vec::new();

    let same_message_id = match (a.message_id, b.message_id) {
        (Some(x), Some(y)) if x == y => {
            matched.push("message_id");
            true
        }
        (Some(_), Some(_)) => {
            // The source gave them distinct logical identities. It is
            // telling us these are different messages; believe it.
            return DuplicateClass::Distinct;
        }
        _ => {
            missing.push("message_id");
            false
        }
    };

    match (a.operation, b.operation) {
        (Some(x), Some(y)) if x == y => matched.push("operation"),
        (Some(_), Some(_)) => return DuplicateClass::Distinct,
        _ => missing.push("operation"),
    }

    let strength = if same_message_id {
        EvidenceStrength::Documented
    } else if matched.len() > 1 {
        EvidenceStrength::Corroborated
    } else {
        EvidenceStrength::Indicative
    };

    DuplicateClass::Operational(SignalEvidence {
        kind: SignalKind::OperationalDuplicate,
        strength,
        matched,
        missing,
    })
}

/* ------------------------------------------------------- time rules */

/// A time observation exactly as recorded, with how well it is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimePoint<'a> {
    pub record_id: &'a str,
    pub event_time: i64,
    pub quality: TimeQuality,
    /// Ordinal within the source, used to know what order the source
    /// itself wrote these records in.
    pub record_number: Option<i64>,
}

/// A measured time relationship. Both originals are carried through
/// unmodified, alongside the delta and the tolerance it was judged
/// against — there is deliberately no "corrected" field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeObservation {
    pub earlier_record_id: String,
    pub later_record_id: String,
    /// The two event times exactly as stored.
    pub earlier_event_time: i64,
    pub later_event_time: i64,
    /// `later_event_time - earlier_event_time`, by source order.
    pub delta_nanos: i64,
    /// The threshold this delta was compared against.
    pub tolerance_nanos: i64,
    pub earlier_quality: TimeQuality,
    pub later_quality: TimeQuality,
    pub evidence: SignalEvidence,
}

impl TimeObservation {
    /// Both times as recorded, so an explanation can quote the originals
    /// rather than a derived value.
    pub fn originals(&self) -> (i64, i64) {
        (self.earlier_event_time, self.later_event_time)
    }

    pub fn explain(&self) -> String {
        format!(
            "{} Recorded times: {} and {} (delta {} ns against a tolerance of {} ns); \
             timestamp quality {} then {}. Both times are reported as stored — no timestamp \
             was adjusted.",
            self.evidence.explain(),
            self.earlier_event_time,
            self.later_event_time,
            self.delta_nanos,
            self.tolerance_nanos,
            self.earlier_quality.as_str(),
            self.later_quality.as_str(),
        )
    }
}

/// Time quality drops the ladder a rung: a reconstructed timestamp
/// cannot corroborate a claim about time.
fn time_strength(a: TimeQuality, b: TimeQuality) -> EvidenceStrength {
    if a == TimeQuality::Observed && b == TimeQuality::Observed {
        EvidenceStrength::Corroborated
    } else {
        EvidenceStrength::Indicative
    }
}

/// Detects a timestamp that moves backwards against the order the
/// source itself wrote the records in.
///
/// `first`/`second` must be consecutive **in source order** (ascending
/// record number within one source), which is why the rule can call a
/// negative delta skew rather than merely out-of-order arrival.
/// `tolerance_nanos` is the backwards movement tolerated before the
/// signal fires.
///
/// Never reaches [`EvidenceStrength::Documented`]: the arithmetic is
/// exact, but attributing it to a clock is a reading, not a record.
pub fn classify_clock_skew(
    first: &TimePoint<'_>,
    second: &TimePoint<'_>,
    tolerance_nanos: i64,
) -> Option<TimeObservation> {
    let (Some(a), Some(b)) = (first.record_number, second.record_number) else {
        // Without source ordering there is no "backwards" to measure.
        return None;
    };
    if b <= a {
        return None;
    }
    let delta = second.event_time.checked_sub(first.event_time)?;
    if delta >= 0 || delta.saturating_neg() <= tolerance_nanos {
        return None;
    }

    let mut matched = vec!["event_time", "record_number"];
    let mut missing = Vec::new();
    for (q, label) in [
        (first.quality, "earlier_timestamp_observed"),
        (second.quality, "later_timestamp_observed"),
    ] {
        if q == TimeQuality::Observed {
            matched.push(label);
        } else {
            missing.push(label);
        }
    }

    Some(TimeObservation {
        earlier_record_id: first.record_id.to_string(),
        later_record_id: second.record_id.to_string(),
        earlier_event_time: first.event_time,
        later_event_time: second.event_time,
        delta_nanos: delta,
        tolerance_nanos,
        earlier_quality: first.quality,
        later_quality: second.quality,
        evidence: SignalEvidence {
            kind: SignalKind::ClockSkew,
            strength: time_strength(first.quality, second.quality),
            matched,
            missing,
        },
    })
}

/// Detects a quiet interval between two consecutive records in a
/// sequence. `threshold_nanos` must be positive; a gap of zero is not a
/// question anyone is asking.
///
/// Never reaches [`EvidenceStrength::Documented`], because absence of
/// records is never a record of absence.
pub fn classify_gap(
    earlier: &TimePoint<'_>,
    later: &TimePoint<'_>,
    threshold_nanos: i64,
) -> Option<TimeObservation> {
    if threshold_nanos <= 0 {
        return None;
    }
    let delta = later.event_time.checked_sub(earlier.event_time)?;
    if delta <= threshold_nanos {
        return None;
    }

    let mut matched = vec!["event_time"];
    let mut missing = Vec::new();
    for (q, label) in [
        (earlier.quality, "earlier_timestamp_observed"),
        (later.quality, "later_timestamp_observed"),
    ] {
        if q == TimeQuality::Observed {
            matched.push(label);
        } else {
            missing.push(label);
        }
    }

    Some(TimeObservation {
        earlier_record_id: earlier.record_id.to_string(),
        later_record_id: later.record_id.to_string(),
        earlier_event_time: earlier.event_time,
        later_event_time: later.event_time,
        delta_nanos: delta,
        tolerance_nanos: threshold_nanos,
        earlier_quality: earlier.quality,
        later_quality: later.quality,
        evidence: SignalEvidence {
            kind: SignalKind::Gap,
            strength: time_strength(earlier.quality, later.quality),
            matched,
            missing,
        },
    })
}

/// Per-signal thresholds. All explicit, all versioned with the rule set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignalThresholds {
    /// Backwards movement tolerated before clock skew is reported.
    pub clock_skew_tolerance_nanos: i64,
    /// Quiet interval before a gap is reported. Default 5 minutes.
    pub gap_threshold_nanos: i64,
}

impl Default for SignalThresholds {
    fn default() -> Self {
        SignalThresholds {
            clock_skew_tolerance_nanos: 1_000_000, // 1 ms
            gap_threshold_nanos: 300_000_000_000,  // 5 minutes
        }
    }
}

impl SignalThresholds {
    pub fn validate(&self) -> Result<(), CaseError> {
        if self.clock_skew_tolerance_nanos < 0 {
            return Err(CaseError::Invalid(
                "clock_skew_tolerance_nanos cannot be negative: it is how far time may move \
                 backwards before the signal fires"
                    .into(),
            ));
        }
        if self.gap_threshold_nanos <= 0 {
            return Err(CaseError::Invalid(
                "gap_threshold_nanos must be positive: every pair of records is separated by \
                 at least zero, so a non-positive threshold reports every pair"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tp<'a>(id: &'a str, t: i64, n: Option<i64>, q: TimeQuality) -> TimePoint<'a> {
        TimePoint {
            record_id: id,
            event_time: t,
            quality: q,
            record_number: n,
        }
    }

    fn dup<'a>(source: &'a str, n: Option<i64>, message: &'a str) -> DuplicateFacts<'a> {
        DuplicateFacts {
            position: SourcePosition {
                dataset_id: "ds-1",
                source_id: source,
                record_number: n,
            },
            message,
            message_id: None,
            operation: None,
        }
    }

    #[test]
    fn retry_cannot_be_established_from_message_text() {
        // The rule has nowhere to put a message: two records saying the
        // same thing, with nothing else, produce no retry at all.
        let bare = RetryFacts::default();
        assert!(classify_retry(&bare, &bare).is_none());

        // Same operation alone is the weakest rung, never documented.
        let a = RetryFacts {
            operation: Some("checkout"),
            ..Default::default()
        };
        let ev = classify_retry(&a, &a).unwrap();
        assert_eq!(ev.strength, EvidenceStrength::Indicative);
        assert!(ev.strength.is_investigative_lead());
        assert!(ev.explain().contains(INVESTIGATIVE_LEAD));
        assert!(ev.missing.contains(&"attempt"));
    }

    #[test]
    fn retry_evidence_ladder_climbs_only_with_typed_fields() {
        let failed = RetryFacts {
            operation: Some("checkout"),
            outcome: Some("failure"),
            ..Default::default()
        };
        let plain = RetryFacts {
            operation: Some("checkout"),
            ..Default::default()
        };
        // failure then another attempt: corroborated, still a lead.
        let ev = classify_retry(&failed, &plain).unwrap();
        assert_eq!(ev.strength, EvidenceStrength::Corroborated);
        assert!(ev.strength.is_investigative_lead());
        assert_eq!(ev.matched, ["operation", "outcome"]);

        // The source counting attempts is what makes it documented.
        let first = RetryFacts {
            attempt: Some(1),
            operation: Some("checkout"),
            outcome: Some("timeout"),
        };
        let second = RetryFacts {
            attempt: Some(2),
            operation: Some("checkout"),
            outcome: None,
        };
        let ev = classify_retry(&first, &second).unwrap();
        assert_eq!(ev.strength, EvidenceStrength::Documented);
        assert!(!ev.strength.is_investigative_lead());
        assert!(!ev.explain().contains(INVESTIGATIVE_LEAD));
        assert_eq!(ev.matched, ["attempt", "operation", "outcome"]);

        // A counter that does not advance is not a retry at all: the
        // source is saying this is the same attempt, logged twice, and
        // that is better evidence than the operation match is.
        let same = RetryFacts {
            attempt: Some(2),
            ..second
        };
        assert!(classify_retry(&second, &same).is_none());
        // A counter that goes backwards is equally not a new attempt.
        let earlier_counter = RetryFacts {
            attempt: Some(1),
            ..second
        };
        assert!(classify_retry(&second, &earlier_counter).is_none());

        // A different operation is evidence against, not weak evidence for.
        let other = RetryFacts {
            operation: Some("refund"),
            ..Default::default()
        };
        assert!(classify_retry(&plain, &other).is_none());
    }

    #[test]
    fn ingestion_duplicates_are_never_operational_duplicates() {
        let line = dup("src-a", Some(7), "connection reset");
        // Same source position: the same line seen twice.
        match classify_duplicate(&line, &line) {
            DuplicateClass::Ingestion { reason } => {
                assert!(reason.contains("ingestion"), "{reason}");
            }
            other => panic!("expected an ingestion duplicate, got {other:?}"),
        }

        // Same source, no record numbers: cannot be shown distinct, so
        // it is refused rather than guessed.
        let a = dup("src-a", None, "connection reset");
        let b = dup("src-a", None, "connection reset");
        assert!(matches!(
            classify_duplicate(&a, &b),
            DuplicateClass::Ingestion { .. }
        ));

        // Distinct positions in the same source: the system repeated it.
        let a = dup("src-a", Some(7), "connection reset");
        let b = dup("src-a", Some(9), "connection reset");
        match classify_duplicate(&a, &b) {
            DuplicateClass::Operational(ev) => {
                assert_eq!(ev.kind, SignalKind::OperationalDuplicate);
                assert_eq!(ev.strength, EvidenceStrength::Indicative);
                assert!(ev.explain().contains(INVESTIGATIVE_LEAD));
                assert!(ev
                    .explain()
                    .contains("not establish that the underlying work"));
            }
            other => panic!("expected an operational duplicate, got {other:?}"),
        }

        // Different text is not a duplicate at all.
        let c = dup("src-a", Some(11), "connection established");
        assert_eq!(classify_duplicate(&a, &c), DuplicateClass::Distinct);
    }

    #[test]
    fn duplicate_ladder_uses_typed_identity_and_believes_the_source() {
        let mut a = dup("src-a", Some(1), "job finished");
        let mut b = dup("src-b", Some(2), "job finished");
        a.message_id = Some("m-1");
        b.message_id = Some("m-1");
        match classify_duplicate(&a, &b) {
            DuplicateClass::Operational(ev) => {
                assert_eq!(ev.strength, EvidenceStrength::Documented);
                assert!(ev.matched.contains(&"message_id"));
            }
            other => panic!("expected documented duplicate, got {other:?}"),
        }
        // Distinct logical identities: the source says these differ.
        b.message_id = Some("m-2");
        assert_eq!(classify_duplicate(&a, &b), DuplicateClass::Distinct);
    }

    #[test]
    fn skew_reports_originals_and_never_rewrites_time() {
        let first = tp("r1", 1_000_000_000, Some(1), TimeQuality::Observed);
        let second = tp("r2", 900_000_000, Some(2), TimeQuality::Observed);
        let obs = classify_clock_skew(&first, &second, 1_000_000).unwrap();
        assert_eq!(obs.originals(), (1_000_000_000, 900_000_000));
        assert_eq!(obs.delta_nanos, -100_000_000);
        assert_eq!(obs.tolerance_nanos, 1_000_000);
        assert_eq!(obs.evidence.strength, EvidenceStrength::Corroborated);
        let text = obs.explain();
        assert!(text.contains("1000000000") && text.contains("900000000"));
        assert!(text.contains("no timestamp was adjusted"));
        assert!(text.contains(INVESTIGATIVE_LEAD));

        // Inside tolerance: no signal.
        let close = tp("r2", 999_500_000, Some(2), TimeQuality::Observed);
        assert!(classify_clock_skew(&first, &close, 1_000_000).is_none());
        // Forward time is not skew.
        let later = tp("r2", 2_000_000_000, Some(2), TimeQuality::Observed);
        assert!(classify_clock_skew(&first, &later, 1_000_000).is_none());
        // Without source ordering there is no "backwards" to measure.
        let unordered = tp("r2", 900_000_000, None, TimeQuality::Observed);
        assert!(classify_clock_skew(&first, &unordered, 1_000_000).is_none());
        // An inferred timestamp cannot corroborate a claim about time.
        let inferred = tp("r2", 900_000_000, Some(2), TimeQuality::Inferred);
        let obs = classify_clock_skew(&first, &inferred, 1_000_000).unwrap();
        assert_eq!(obs.evidence.strength, EvidenceStrength::Indicative);
        assert!(obs.evidence.missing.contains(&"later_timestamp_observed"));
    }

    #[test]
    fn a_gap_is_never_evidence_that_nothing_happened() {
        let a = tp("r1", 0, Some(1), TimeQuality::Observed);
        let b = tp("r2", 600_000_000_000, Some(2), TimeQuality::Observed);
        let obs = classify_gap(&a, &b, 300_000_000_000).unwrap();
        assert_eq!(obs.delta_nanos, 600_000_000_000);
        assert_eq!(obs.originals(), (0, 600_000_000_000));
        // Highest a gap can ever reach: absence is not a record.
        assert_eq!(obs.evidence.strength, EvidenceStrength::Corroborated);
        assert!(obs.evidence.strength.is_investigative_lead());
        let text = obs.explain();
        assert!(text.contains("not evidence that nothing happened"));
        assert!(text.contains("retention"));
        assert!(text.contains("no timestamp was adjusted"));

        // Under threshold: nothing.
        let near = tp("r2", 100_000_000_000, Some(2), TimeQuality::Observed);
        assert!(classify_gap(&a, &near, 300_000_000_000).is_none());
        // A non-positive threshold would report every pair; refuse it.
        assert!(classify_gap(&a, &b, 0).is_none());
    }

    #[test]
    fn no_signal_explanation_may_claim_causation() {
        let retry = classify_retry(
            &RetryFacts {
                attempt: Some(1),
                operation: Some("checkout"),
                outcome: Some("failure"),
            },
            &RetryFacts {
                attempt: Some(2),
                operation: Some("checkout"),
                outcome: None,
            },
        )
        .unwrap();
        let a = tp("r1", 1_000_000_000, Some(1), TimeQuality::Observed);
        let b = tp("r2", 0, Some(2), TimeQuality::Observed);
        let skew = classify_clock_skew(&a, &b, 0).unwrap();
        let gap = classify_gap(&tp("r1", 0, Some(1), TimeQuality::Observed), &b_far(), 1).unwrap();
        let dupe = match classify_duplicate(&dup("s", Some(1), "x"), &dup("s", Some(2), "x")) {
            DuplicateClass::Operational(ev) => ev.explain(),
            other => panic!("{other:?}"),
        };

        for text in [retry.explain(), skew.explain(), gap.explain(), dupe] {
            let lowered = text.to_lowercase();
            for forbidden in [
                "caused by",
                "because of",
                "root cause",
                "therefore",
                "proves",
                "confirms that",
            ] {
                assert!(!lowered.contains(forbidden), "{forbidden:?} in {text}");
            }
        }
    }

    fn b_far<'a>() -> TimePoint<'a> {
        tp("r2", 10_000_000_000, Some(2), TimeQuality::Observed)
    }

    #[test]
    fn thresholds_refuse_values_that_would_report_everything() {
        let d = SignalThresholds::default();
        d.validate().unwrap();
        assert_eq!(d.gap_threshold_nanos, 300_000_000_000);
        let bad = SignalThresholds {
            gap_threshold_nanos: 0,
            ..d.clone()
        };
        let err = bad.validate().unwrap_err().to_string();
        assert!(err.contains("reports every pair"), "{err}");
        let bad = SignalThresholds {
            clock_skew_tolerance_nanos: -1,
            ..d
        };
        assert!(bad.validate().is_err());
        // Unknown keys are refused like every other config in v0.4.
        assert!(serde_json::from_str::<SignalThresholds>("{\"surprise\":1}").is_err());
    }

    #[test]
    fn every_signal_names_its_rule_and_its_limitation() {
        for name in SIGNAL_KINDS {
            let kind = SignalKind::parse(name).unwrap();
            assert_eq!(kind.as_str(), *name);
            assert!(!kind.rule_id().is_empty());
            assert!(
                kind.limitation().len() > 40,
                "{name} needs a real limitation"
            );
            let ev = SignalEvidence {
                kind,
                strength: EvidenceStrength::Indicative,
                matched: vec![],
                missing: vec!["operation"],
            };
            let text = ev.explain();
            assert!(text.contains(kind.rule_id()));
            assert!(text.contains(SIGNAL_RULE_SET_ID));
            assert!(text.contains("No typed field matched."));
            assert!(text.contains("Absent: operation."));
            assert!(text.contains(INVESTIGATIVE_LEAD));
        }
        assert!(SignalKind::parse("hunch").is_err());
    }
}
