//! Deterministic findings (v0.4 WP5, `find-rules` v1) — the pure rule
//! model that turns completed pattern, comparison, and correlation runs
//! into ranked, explainable statements.
//!
//! A finding is the most dangerous object in this product. It is the
//! thing a reader is most likely to quote in an incident review, and
//! the thing most likely to be read as "the tool found the cause". So
//! the contract here is mostly about what a finding may not say:
//!
//! - **Severity is not impact.** It ranks findings by the canonical log
//!   severity involved and the size of the measured change, nothing
//!   else. There is no customer, financial, or security dimension in
//!   the type, so none can be reported. Every rendered finding carries
//!   [`SEVERITY_MEANING`] saying so.
//! - **A finding is never more confident than its evidence.**
//!   [`FindingConfidence`] is carried in from the analysis that produced
//!   the input; a rule cannot promote it.
//! - **Limitations are carried, never invented and never dropped.** The
//!   source rule's own limitation travels into the finding alongside the
//!   standing one.
//! - Every generated string is covered by a forbidden-wording test:
//!   nothing here may say `confirmed`, `root cause`, `caused by`,
//!   `therefore`, or their neighbours.
//! - Rules are declarative data validated by this core. There is no
//!   executable user rule anywhere in the model.

use serde::{Deserialize, Serialize};

use crate::signals::{EvidenceStrength, SignalKind};
use crate::CaseError;

/// Rule-set identity. Individual rules carry their own ID within the set.
pub const FINDING_RULE_SET_ID: &str = "find-rules";
pub const FINDING_RULE_SET_VERSION: i64 = 1;

/// The most supporting records a finding will carry. A finding points at
/// evidence; it does not reproduce a dataset.
pub const MAX_FINDING_RECORDS: usize = 20;

/// The seven rules of `find-rules` v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingRule {
    NewHighSeverityPattern,
    IncreasedErrorPattern,
    DisappearedPattern,
    RepeatedRetrySignal,
    OperationalDuplicate,
    ClockSkewWarning,
    ConcentratedDimensionChange,
}

pub const FINDING_RULES: &[&str] = &[
    "new_high_severity_pattern",
    "increased_error_pattern",
    "disappeared_pattern",
    "repeated_retry_signal",
    "operational_duplicate",
    "clock_skew_warning",
    "concentrated_dimension_change",
];

impl FindingRule {
    pub fn parse(name: &str) -> Result<FindingRule, CaseError> {
        match name {
            "new_high_severity_pattern" => Ok(FindingRule::NewHighSeverityPattern),
            "increased_error_pattern" => Ok(FindingRule::IncreasedErrorPattern),
            "disappeared_pattern" => Ok(FindingRule::DisappearedPattern),
            "repeated_retry_signal" => Ok(FindingRule::RepeatedRetrySignal),
            "operational_duplicate" => Ok(FindingRule::OperationalDuplicate),
            "clock_skew_warning" => Ok(FindingRule::ClockSkewWarning),
            "concentrated_dimension_change" => Ok(FindingRule::ConcentratedDimensionChange),
            other => Err(CaseError::Invalid(format!(
                "unknown finding rule {other:?} (expected one of {})",
                FINDING_RULES.join("|")
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            FindingRule::NewHighSeverityPattern => "new_high_severity_pattern",
            FindingRule::IncreasedErrorPattern => "increased_error_pattern",
            FindingRule::DisappearedPattern => "disappeared_pattern",
            FindingRule::RepeatedRetrySignal => "repeated_retry_signal",
            FindingRule::OperationalDuplicate => "operational_duplicate",
            FindingRule::ClockSkewWarning => "clock_skew_warning",
            FindingRule::ConcentratedDimensionChange => "concentrated_dimension_change",
        }
    }

    /// Stable per-rule ID reported with every finding.
    pub fn rule_id(self) -> &'static str {
        match self {
            FindingRule::NewHighSeverityPattern => "new-high-severity-pattern",
            FindingRule::IncreasedErrorPattern => "increased-error-pattern",
            FindingRule::DisappearedPattern => "disappeared-pattern",
            FindingRule::RepeatedRetrySignal => "repeated-retry-signal",
            FindingRule::OperationalDuplicate => "operational-duplicate",
            FindingRule::ClockSkewWarning => "clock-skew-warning",
            FindingRule::ConcentratedDimensionChange => "concentrated-dimension-change",
        }
    }

    /// The limitation specific to this rule — the thing a reader is most
    /// likely to over-read from it.
    pub fn limitation(self) -> &'static str {
        match self {
            FindingRule::NewHighSeverityPattern => {
                "A message shape absent from the baseline window may be genuinely new, or may \
                 simply not have occurred during that window. This does not establish when the \
                 behaviour began."
            }
            FindingRule::IncreasedErrorPattern => {
                "A higher rate in the suspect window describes the two windows compared. It does \
                 not establish a trend, a change in the system, or that anything was affected."
            }
            FindingRule::DisappearedPattern => {
                "A message shape absent from the suspect window may have stopped occurring, or \
                 may have stopped being logged. Absence of records is not absence of activity."
            }
            FindingRule::RepeatedRetrySignal => {
                "Repeated attempts describe what the source recorded about the same operation. \
                 They do not establish why the attempts repeated or whether any succeeded."
            }
            FindingRule::OperationalDuplicate => {
                "Repeated identical messages describe what the source emitted. They do not \
                 establish that the underlying work happened more than once."
            }
            FindingRule::ClockSkewWarning => {
                "Timestamps that move backwards against a source's own record order describe the \
                 recorded times, not the true instants. No timestamp is adjusted anywhere."
            }
            FindingRule::ConcentratedDimensionChange => {
                "Concentration describes where the measured change sits within the compared \
                 keys. It does not establish that those keys are related to each other or that \
                 the rest of the system was unchanged."
            }
        }
    }
}

/// The highest canonical log severity among the records a finding rests
/// on. Derived from OTLP severity numbers, so it means the same thing
/// whatever text the source used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityCeiling {
    /// No record carried a mapped severity.
    Unknown,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl SeverityCeiling {
    /// OTLP severity number bands (1–4 trace, 5–8 debug, 9–12 info,
    /// 13–16 warn, 17–20 error, 21–24 fatal). Anything outside the
    /// defined range is `Unknown` rather than guessed.
    pub fn from_number(n: Option<i32>) -> SeverityCeiling {
        match n {
            Some(1..=4) => SeverityCeiling::Trace,
            Some(5..=8) => SeverityCeiling::Debug,
            Some(9..=12) => SeverityCeiling::Info,
            Some(13..=16) => SeverityCeiling::Warn,
            Some(17..=20) => SeverityCeiling::Error,
            Some(21..=24) => SeverityCeiling::Fatal,
            _ => SeverityCeiling::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SeverityCeiling::Unknown => "unknown",
            SeverityCeiling::Trace => "trace",
            SeverityCeiling::Debug => "debug",
            SeverityCeiling::Info => "info",
            SeverityCeiling::Warn => "warn",
            SeverityCeiling::Error => "error",
            SeverityCeiling::Fatal => "fatal",
        }
    }
}

/// How large the measured change is, in bands rather than a raw number,
/// so the severity table has something discrete to key on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Magnitude {
    Small,
    Moderate,
    Large,
}

impl Magnitude {
    pub fn as_str(self) -> &'static str {
        match self {
            Magnitude::Small => "small",
            Magnitude::Moderate => "moderate",
            Magnitude::Large => "large",
        }
    }
}

/// Finding severity. Deliberately four generic bands with no business
/// vocabulary: there is no `critical`, no `customer_impacting`, no
/// `security`. The type cannot express an impact claim, so no rule can
/// make one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Informational,
    Low,
    Medium,
    High,
}

impl FindingSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingSeverity::Informational => "informational",
            FindingSeverity::Low => "low",
            FindingSeverity::Medium => "medium",
            FindingSeverity::High => "high",
        }
    }
}

/// What severity means, carried on every rendered finding. Without this
/// sentence "high" is an invitation to read impact into a log count.
pub const SEVERITY_MEANING: &str =
    "Severity ranks findings by the canonical log severity involved and the size of the \
     measured change. It is not a statement about customer, financial, or security impact.";

/// The limitation every finding carries, whatever rule produced it.
pub const FINDING_LIMITATION: &str =
    "A finding reports what the configured rule measured in the data the run scanned. It does \
     not establish a cause, an impact, or that any action is required, and it says nothing \
     about data outside the run's scope.";

/// **The severity table.** Documented here and in ADR-0022, and pinned
/// by a test that walks every cell — a mapping that lives only in an
/// `if` chain is a mapping nobody can review.
///
/// | ceiling ＼ magnitude | small | moderate | large |
/// |---|---|---|---|
/// | fatal | medium | high | high |
/// | error | low | medium | high |
/// | warn | low | low | medium |
/// | info / debug / trace / unknown | informational | informational | low |
pub fn severity_for(ceiling: SeverityCeiling, magnitude: Magnitude) -> FindingSeverity {
    use FindingSeverity as S;
    use Magnitude as M;
    use SeverityCeiling as C;
    match (ceiling, magnitude) {
        (C::Fatal, M::Small) => S::Medium,
        (C::Fatal, _) => S::High,
        (C::Error, M::Small) => S::Low,
        (C::Error, M::Moderate) => S::Medium,
        (C::Error, M::Large) => S::High,
        (C::Warn, M::Large) => S::Medium,
        (C::Warn, _) => S::Low,
        (_, M::Large) => S::Low,
        (_, _) => S::Informational,
    }
}

/// How much the evidence behind a finding is worth. Carried in from the
/// analysis that produced the input; a finding rule cannot promote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingConfidence {
    /// One weak but deterministic indicator.
    Indicative,
    /// Several independent typed fields agree.
    Corroborated,
    /// The underlying values are exact counts, or the source stated the
    /// fact itself. Note this is confidence in the *measurement*, never
    /// in an interpretation of it.
    Measured,
}

impl FindingConfidence {
    pub fn as_str(self) -> &'static str {
        match self {
            FindingConfidence::Indicative => "indicative",
            FindingConfidence::Corroborated => "corroborated",
            FindingConfidence::Measured => "measured",
        }
    }

    /// A signal's evidence ladder maps straight across; nothing is
    /// gained by passing through a finding rule.
    pub fn from_evidence(strength: EvidenceStrength) -> FindingConfidence {
        match strength {
            EvidenceStrength::Documented => FindingConfidence::Measured,
            EvidenceStrength::Corroborated => FindingConfidence::Corroborated,
            EvidenceStrength::Indicative => FindingConfidence::Indicative,
        }
    }
}

/// Where a finding's input came from. Gate 28's "origin".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingOrigin {
    /// `comparison` | `correlation` | `pattern`.
    pub analysis_kind: String,
    pub run_id: String,
    /// The rule set that produced the input (`cmp-rules`, `sig-rules`).
    pub source_rule_id: String,
    pub source_rule_version: i64,
}

/// One named value that fed a rule. Gate 28's "inputs": a reader can
/// recompute the decision from these alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingInput {
    pub name: String,
    pub value: String,
}

fn input(name: &str, value: impl std::fmt::Display) -> FindingInput {
    FindingInput {
        name: name.to_string(),
        value: value.to_string(),
    }
}

/// A finding. Every field gate 28 requires is present by construction:
/// rule, inputs, calculation, thresholds, severity, confidence, records,
/// limitations, origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: FindingRule,
    pub rule_id: String,
    pub rule_set_id: String,
    pub rule_set_version: i64,
    pub severity: FindingSeverity,
    pub ceiling: SeverityCeiling,
    pub magnitude: Magnitude,
    pub confidence: FindingConfidence,
    /// The subject the rule fired on (a template, a key, a group).
    pub subject: String,
    pub title: String,
    pub inputs: Vec<FindingInput>,
    /// How the decision was reached, in the same integer terms the rule
    /// used. Never a restatement of the title.
    pub calculation: String,
    /// The thresholds actually applied, so a reader can see what would
    /// have had to differ for the rule not to fire.
    pub thresholds: Vec<FindingInput>,
    /// Bounded supporting records.
    pub record_ids: Vec<String>,
    /// Supporting records beyond [`MAX_FINDING_RECORDS`].
    pub records_truncated: u64,
    /// The rule's own limitation, then the standing one. Carried, never
    /// invented, never dropped.
    pub limitations: Vec<String>,
    pub origin: FindingOrigin,
}

impl Finding {
    /// The full rendered statement. Everything a reader needs to judge
    /// the finding is in here, including what it does not say.
    pub fn explain(&self) -> String {
        let mut text = format!(
            "{} ({} v{}): {}. Severity {} (canonical severity ceiling {}, {} change); \
             confidence {}. {}",
            self.rule_id,
            self.rule_set_id,
            self.rule_set_version,
            self.title,
            self.severity.as_str(),
            self.ceiling.as_str(),
            self.magnitude.as_str(),
            self.confidence.as_str(),
            self.calculation,
        );
        if !self.thresholds.is_empty() {
            text.push_str(&format!(
                " Thresholds applied: {}.",
                self.thresholds
                    .iter()
                    .map(|t| format!("{}={}", t.name, t.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        text.push(' ');
        text.push_str(SEVERITY_MEANING);
        for limitation in &self.limitations {
            text.push(' ');
            text.push_str(limitation);
        }
        text
    }
}

/// Versioned thresholds, integers only, all explicit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FindingThresholds {
    /// Suspect-side count before a `new` key becomes a finding.
    pub min_new_count: u64,
    /// Baseline-side count before a `disappeared` key becomes a finding.
    pub min_gone_count: u64,
    /// Suspect-side count before an `increased` key becomes a finding.
    pub min_increase_count: u64,
    /// Occurrences of one signal kind in a group before it is a finding.
    pub min_signal_occurrences: u64,
    /// Share of total change, in basis points, that must sit in
    /// `concentration_max_keys` keys before concentration is reported.
    pub concentration_share_bp: u64,
    pub concentration_max_keys: usize,
    /// Rate-change bands, basis points. `10000` = +100 %.
    pub magnitude_moderate_bp: u64,
    pub magnitude_large_bp: u64,
}

impl Default for FindingThresholds {
    fn default() -> Self {
        FindingThresholds {
            min_new_count: 5,
            min_gone_count: 5,
            min_increase_count: 10,
            min_signal_occurrences: 3,
            concentration_share_bp: 8_000, // 80 % of the change
            concentration_max_keys: 3,
            magnitude_moderate_bp: 5_000, // +50 %
            magnitude_large_bp: 20_000,   // +200 %
        }
    }
}

impl FindingThresholds {
    pub fn parse(thresholds_json: &str) -> Result<FindingThresholds, CaseError> {
        let trimmed = thresholds_json.trim();
        if trimmed.is_empty() || trimmed == "{}" {
            return Ok(FindingThresholds::default());
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| CaseError::Invalid(format!("thresholds do not parse: {e}")))?;
        if !value.is_object() {
            return Err(CaseError::Invalid(
                "thresholds must be a JSON object".into(),
            ));
        }
        let parsed: FindingThresholds = serde_json::from_value(value)
            .map_err(|e| CaseError::Invalid(format!("thresholds do not parse: {e}")))?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn validate(&self) -> Result<(), CaseError> {
        if self.magnitude_large_bp <= self.magnitude_moderate_bp {
            return Err(CaseError::Invalid(
                "magnitude_large_bp must exceed magnitude_moderate_bp, otherwise the bands \
                 overlap and a change could be both"
                    .into(),
            ));
        }
        if self.concentration_share_bp > 10_000 {
            return Err(CaseError::Invalid(
                "concentration_share_bp cannot exceed 10000: a share above 100 % of the \
                 measured change is not a quantity that exists"
                    .into(),
            ));
        }
        if self.concentration_max_keys == 0 {
            return Err(CaseError::Invalid(
                "concentration_max_keys must be at least 1".into(),
            ));
        }
        Ok(())
    }

    /// Bands an absolute rate change (basis points) into a magnitude.
    /// Sign is dropped deliberately: a −90 % change is as large a
    /// movement as a +90 % one, and which direction matters is the
    /// individual rule's business.
    pub fn magnitude_for_bp(&self, bp: i64) -> Magnitude {
        let abs = bp.unsigned_abs();
        if abs >= self.magnitude_large_bp {
            Magnitude::Large
        } else if abs >= self.magnitude_moderate_bp {
            Magnitude::Moderate
        } else {
            Magnitude::Small
        }
    }

    /// Bands a count when there is no rate to speak of (a new or
    /// disappeared key has no comparable rate). Multiples of the
    /// entry threshold keep this in the same integer world.
    pub fn magnitude_for_count(&self, count: u64, entry_threshold: u64) -> Magnitude {
        let floor = entry_threshold.max(1);
        if count >= floor.saturating_mul(10) {
            Magnitude::Large
        } else if count >= floor.saturating_mul(3) {
            Magnitude::Moderate
        } else {
            Magnitude::Small
        }
    }
}

/// One classified comparison row, as a finding rule sees it.
#[derive(Debug, Clone)]
pub struct ComparisonFacts<'a> {
    pub key: &'a str,
    pub dimension: &'a str,
    /// `new | disappeared | increased | decreased | unchanged |
    /// insufficient_data`, exactly as `cmp-rules` classified it.
    pub classification: &'a str,
    pub baseline_count: u64,
    pub suspect_count: u64,
    /// Basis points as `cmp-rules` produced it, or `"undefined"`.
    pub rate_change_bp: &'a str,
    pub ceiling: SeverityCeiling,
    pub record_ids: &'a [String],
    pub total_record_matches: u64,
}

fn bounded_records(facts_records: &[String], total: u64) -> (Vec<String>, u64) {
    let kept: Vec<String> = facts_records
        .iter()
        .take(MAX_FINDING_RECORDS)
        .cloned()
        .collect();
    let truncated = total.saturating_sub(kept.len() as u64);
    (kept, truncated)
}

fn threshold_list(pairs: &[(&str, u64)]) -> Vec<FindingInput> {
    pairs.iter().map(|(n, v)| input(n, v)).collect()
}

/// `new-high-severity-pattern`: a key absent from the baseline window,
/// carrying a canonical severity of warn or above.
pub fn new_high_severity_pattern(
    facts: &ComparisonFacts<'_>,
    thresholds: &FindingThresholds,
    origin: &FindingOrigin,
) -> Option<Finding> {
    if facts.classification != "new" || facts.suspect_count < thresholds.min_new_count {
        return None;
    }
    // "High severity" means the canonical ceiling, not a judgement: a
    // new INFO template is a fact, but it is not this rule's subject.
    if facts.ceiling < SeverityCeiling::Warn {
        return None;
    }
    let magnitude = thresholds.magnitude_for_count(facts.suspect_count, thresholds.min_new_count);
    let (record_ids, records_truncated) =
        bounded_records(facts.record_ids, facts.total_record_matches);
    let rule = FindingRule::NewHighSeverityPattern;
    Some(Finding {
        rule,
        rule_id: rule.rule_id().to_string(),
        rule_set_id: FINDING_RULE_SET_ID.to_string(),
        rule_set_version: FINDING_RULE_SET_VERSION,
        severity: severity_for(facts.ceiling, magnitude),
        ceiling: facts.ceiling,
        magnitude,
        confidence: FindingConfidence::Measured,
        subject: facts.key.to_string(),
        title: format!(
            "{} {:?} appears in the suspect window and not in the baseline window",
            facts.dimension, facts.key
        ),
        inputs: vec![
            input("dimension", facts.dimension),
            input("key", facts.key),
            input("classification", facts.classification),
            input("baseline_count", facts.baseline_count),
            input("suspect_count", facts.suspect_count),
            input("severity_ceiling", facts.ceiling.as_str()),
        ],
        calculation: format!(
            "cmp-rules classified this key as new (baseline {}, suspect {}); {} \
             suspect-side records is at or above the min_new_count of {}",
            facts.baseline_count,
            facts.suspect_count,
            facts.suspect_count,
            thresholds.min_new_count
        ),
        thresholds: threshold_list(&[("min_new_count", thresholds.min_new_count)]),
        record_ids,
        records_truncated,
        limitations: vec![
            rule.limitation().to_string(),
            FINDING_LIMITATION.to_string(),
        ],
        origin: origin.clone(),
    })
}

/// `increased-error-pattern`: a key whose rate rose beyond the
/// configured band, carrying a canonical severity of error or above.
pub fn increased_error_pattern(
    facts: &ComparisonFacts<'_>,
    thresholds: &FindingThresholds,
    origin: &FindingOrigin,
) -> Option<Finding> {
    if facts.classification != "increased" || facts.suspect_count < thresholds.min_increase_count {
        return None;
    }
    if facts.ceiling < SeverityCeiling::Error {
        return None;
    }
    // `cmp-rules` emits basis points as a decimal string, or the literal
    // "undefined" for a zero baseline. An unparsable value is not
    // silently treated as zero — it means this rule has no rate to band.
    let bp: i64 = facts.rate_change_bp.parse().ok()?;
    let magnitude = thresholds.magnitude_for_bp(bp);
    let (record_ids, records_truncated) =
        bounded_records(facts.record_ids, facts.total_record_matches);
    let rule = FindingRule::IncreasedErrorPattern;
    Some(Finding {
        rule,
        rule_id: rule.rule_id().to_string(),
        rule_set_id: FINDING_RULE_SET_ID.to_string(),
        rule_set_version: FINDING_RULE_SET_VERSION,
        severity: severity_for(facts.ceiling, magnitude),
        ceiling: facts.ceiling,
        magnitude,
        confidence: FindingConfidence::Measured,
        subject: facts.key.to_string(),
        title: format!(
            "{} {:?} occurs at a higher rate in the suspect window",
            facts.dimension, facts.key
        ),
        inputs: vec![
            input("dimension", facts.dimension),
            input("key", facts.key),
            input("classification", facts.classification),
            input("baseline_count", facts.baseline_count),
            input("suspect_count", facts.suspect_count),
            input("rate_change_bp", facts.rate_change_bp),
            input("severity_ceiling", facts.ceiling.as_str()),
        ],
        calculation: format!(
            "cmp-rules measured a rate change of {} basis points (baseline {}, suspect {}); \
             |{}| falls in the {} band (moderate at {}, large at {})",
            facts.rate_change_bp,
            facts.baseline_count,
            facts.suspect_count,
            bp,
            magnitude.as_str(),
            thresholds.magnitude_moderate_bp,
            thresholds.magnitude_large_bp
        ),
        thresholds: threshold_list(&[
            ("min_increase_count", thresholds.min_increase_count),
            ("magnitude_moderate_bp", thresholds.magnitude_moderate_bp),
            ("magnitude_large_bp", thresholds.magnitude_large_bp),
        ]),
        record_ids,
        records_truncated,
        limitations: vec![
            rule.limitation().to_string(),
            FINDING_LIMITATION.to_string(),
        ],
        origin: origin.clone(),
    })
}

/// `disappeared-pattern`: a key present in the baseline window and
/// absent from the suspect window.
pub fn disappeared_pattern(
    facts: &ComparisonFacts<'_>,
    thresholds: &FindingThresholds,
    origin: &FindingOrigin,
) -> Option<Finding> {
    if facts.classification != "disappeared" || facts.baseline_count < thresholds.min_gone_count {
        return None;
    }
    let magnitude = thresholds.magnitude_for_count(facts.baseline_count, thresholds.min_gone_count);
    let (record_ids, records_truncated) =
        bounded_records(facts.record_ids, facts.total_record_matches);
    let rule = FindingRule::DisappearedPattern;
    Some(Finding {
        rule,
        rule_id: rule.rule_id().to_string(),
        rule_set_id: FINDING_RULE_SET_ID.to_string(),
        rule_set_version: FINDING_RULE_SET_VERSION,
        severity: severity_for(facts.ceiling, magnitude),
        ceiling: facts.ceiling,
        magnitude,
        confidence: FindingConfidence::Measured,
        subject: facts.key.to_string(),
        title: format!(
            "{} {:?} appears in the baseline window and not in the suspect window",
            facts.dimension, facts.key
        ),
        inputs: vec![
            input("dimension", facts.dimension),
            input("key", facts.key),
            input("classification", facts.classification),
            input("baseline_count", facts.baseline_count),
            input("suspect_count", facts.suspect_count),
            input("severity_ceiling", facts.ceiling.as_str()),
        ],
        calculation: format!(
            "cmp-rules classified this key as disappeared (baseline {}, suspect {}); {} \
             baseline-side records is at or above the min_gone_count of {}",
            facts.baseline_count,
            facts.suspect_count,
            facts.baseline_count,
            thresholds.min_gone_count
        ),
        thresholds: threshold_list(&[("min_gone_count", thresholds.min_gone_count)]),
        record_ids,
        records_truncated,
        limitations: vec![
            rule.limitation().to_string(),
            FINDING_LIMITATION.to_string(),
        ],
        origin: origin.clone(),
    })
}

/// The signals of one kind observed inside one correlation group.
#[derive(Debug, Clone)]
pub struct SignalFacts<'a> {
    pub kind: SignalKind,
    pub group_key: &'a str,
    pub key_selector: &'a str,
    pub occurrences: u64,
    /// The strongest evidence any of those occurrences reached. A
    /// finding inherits it and cannot exceed it.
    pub strongest: EvidenceStrength,
    pub ceiling: SeverityCeiling,
    pub record_ids: &'a [String],
    pub total_record_matches: u64,
}

/// The three signal-derived rules share a shape: enough occurrences of
/// one signal kind inside one group. They differ only in which kind they
/// watch and what they are called, so they share one builder rather than
/// three near-copies that could drift apart.
fn signal_finding(
    rule: FindingRule,
    expected: SignalKind,
    facts: &SignalFacts<'_>,
    thresholds: &FindingThresholds,
    origin: &FindingOrigin,
) -> Option<Finding> {
    if facts.kind != expected || facts.occurrences < thresholds.min_signal_occurrences {
        return None;
    }
    let magnitude =
        thresholds.magnitude_for_count(facts.occurrences, thresholds.min_signal_occurrences);
    let (record_ids, records_truncated) =
        bounded_records(facts.record_ids, facts.total_record_matches);
    let confidence = FindingConfidence::from_evidence(facts.strongest);
    Some(Finding {
        rule,
        rule_id: rule.rule_id().to_string(),
        rule_set_id: FINDING_RULE_SET_ID.to_string(),
        rule_set_version: FINDING_RULE_SET_VERSION,
        severity: severity_for(facts.ceiling, magnitude),
        ceiling: facts.ceiling,
        magnitude,
        confidence,
        subject: facts.group_key.to_string(),
        title: format!(
            "{} occurrences of the {} signal in the {} group {:?}",
            facts.occurrences,
            expected.as_str().replace('_', " "),
            facts.key_selector,
            facts.group_key
        ),
        inputs: vec![
            input("signal_kind", expected.as_str()),
            input("key_selector", facts.key_selector),
            input("group_key", facts.group_key),
            input("occurrences", facts.occurrences),
            input("strongest_evidence", facts.strongest.as_str()),
            input("severity_ceiling", facts.ceiling.as_str()),
        ],
        calculation: format!(
            "sig-rules reported {} {} signal(s) in this group, at or above the \
             min_signal_occurrences of {}; the strongest single occurrence reached {} \
             evidence, which is what this finding inherits",
            facts.occurrences,
            expected.as_str(),
            thresholds.min_signal_occurrences,
            facts.strongest.as_str()
        ),
        thresholds: threshold_list(&[(
            "min_signal_occurrences",
            thresholds.min_signal_occurrences,
        )]),
        record_ids,
        records_truncated,
        limitations: vec![
            rule.limitation().to_string(),
            expected.limitation().to_string(),
            FINDING_LIMITATION.to_string(),
        ],
        origin: origin.clone(),
    })
}

/// `repeated-retry-signal`.
pub fn repeated_retry_signal(
    facts: &SignalFacts<'_>,
    thresholds: &FindingThresholds,
    origin: &FindingOrigin,
) -> Option<Finding> {
    signal_finding(
        FindingRule::RepeatedRetrySignal,
        SignalKind::Retry,
        facts,
        thresholds,
        origin,
    )
}

/// `operational-duplicate`.
pub fn operational_duplicate(
    facts: &SignalFacts<'_>,
    thresholds: &FindingThresholds,
    origin: &FindingOrigin,
) -> Option<Finding> {
    signal_finding(
        FindingRule::OperationalDuplicate,
        SignalKind::OperationalDuplicate,
        facts,
        thresholds,
        origin,
    )
}

/// `clock-skew-warning`.
pub fn clock_skew_warning(
    facts: &SignalFacts<'_>,
    thresholds: &FindingThresholds,
    origin: &FindingOrigin,
) -> Option<Finding> {
    signal_finding(
        FindingRule::ClockSkewWarning,
        SignalKind::ClockSkew,
        facts,
        thresholds,
        origin,
    )
}

/// The shape of one dimension's measured change, for concentration.
#[derive(Debug, Clone)]
pub struct ConcentrationFacts<'a> {
    pub dimension: &'a str,
    /// Absolute count change per key, already sorted descending by the
    /// caller. Only the leading keys are read.
    pub sorted_abs_changes: &'a [(String, u64)],
    pub total_abs_change: u64,
    pub ceiling: SeverityCeiling,
    pub record_ids: &'a [String],
    pub total_record_matches: u64,
}

/// `concentrated-dimension-change`: most of a dimension's measured
/// change sits in a handful of its keys.
pub fn concentrated_dimension_change(
    facts: &ConcentrationFacts<'_>,
    thresholds: &FindingThresholds,
    origin: &FindingOrigin,
) -> Option<Finding> {
    if facts.total_abs_change == 0 {
        return None;
    }
    // More keys than the concentration window means there is nothing to
    // concentrate: every key would be "the top keys".
    if facts.sorted_abs_changes.len() <= thresholds.concentration_max_keys {
        return None;
    }
    let top: u64 = facts
        .sorted_abs_changes
        .iter()
        .take(thresholds.concentration_max_keys)
        .map(|(_, v)| *v)
        .sum();
    // Integer basis points, no float anywhere on the path.
    let share_bp = (u128::from(top) * 10_000u128 / u128::from(facts.total_abs_change)) as u64;
    if share_bp < thresholds.concentration_share_bp {
        return None;
    }
    let magnitude = if share_bp >= 9_500 {
        Magnitude::Large
    } else if share_bp >= 9_000 {
        Magnitude::Moderate
    } else {
        Magnitude::Small
    };
    let (record_ids, records_truncated) =
        bounded_records(facts.record_ids, facts.total_record_matches);
    let names: Vec<&str> = facts
        .sorted_abs_changes
        .iter()
        .take(thresholds.concentration_max_keys)
        .map(|(k, _)| k.as_str())
        .collect();
    let rule = FindingRule::ConcentratedDimensionChange;
    Some(Finding {
        rule,
        rule_id: rule.rule_id().to_string(),
        rule_set_id: FINDING_RULE_SET_ID.to_string(),
        rule_set_version: FINDING_RULE_SET_VERSION,
        severity: severity_for(facts.ceiling, magnitude),
        ceiling: facts.ceiling,
        magnitude,
        confidence: FindingConfidence::Measured,
        subject: facts.dimension.to_string(),
        title: format!(
            "{} of the measured change in {} sits in {} of its {} keys",
            fmt_bp_percent(share_bp),
            facts.dimension,
            thresholds.concentration_max_keys,
            facts.sorted_abs_changes.len()
        ),
        inputs: vec![
            input("dimension", facts.dimension),
            input("distinct_keys", facts.sorted_abs_changes.len()),
            input("top_keys", names.join(", ")),
            input("top_abs_change", top),
            input("total_abs_change", facts.total_abs_change),
            input("share_bp", share_bp),
            input("severity_ceiling", facts.ceiling.as_str()),
        ],
        calculation: format!(
            "the {} largest of {} keys account for {} of {} absolute count change, \
             = {share_bp} basis points, at or above the concentration_share_bp of {}",
            thresholds.concentration_max_keys,
            facts.sorted_abs_changes.len(),
            top,
            facts.total_abs_change,
            thresholds.concentration_share_bp
        ),
        thresholds: threshold_list(&[
            ("concentration_share_bp", thresholds.concentration_share_bp),
            (
                "concentration_max_keys",
                thresholds.concentration_max_keys as u64,
            ),
        ]),
        record_ids,
        records_truncated,
        limitations: vec![
            rule.limitation().to_string(),
            FINDING_LIMITATION.to_string(),
        ],
        origin: origin.clone(),
    })
}

/// Basis points as a percentage string, integer arithmetic only.
fn fmt_bp_percent(bp: u64) -> String {
    format!("{}.{:02}%", bp / 100, bp % 100)
}

/// Deterministic ranking: severity descending, then confidence
/// descending, then rule name, then subject. Stable across runs and
/// machines, and never dependent on discovery order.
pub fn finding_order(
    f: &Finding,
) -> (
    std::cmp::Reverse<FindingSeverity>,
    std::cmp::Reverse<FindingConfidence>,
    &str,
    &str,
) {
    (
        std::cmp::Reverse(f.severity),
        std::cmp::Reverse(f.confidence),
        f.rule.as_str(),
        f.subject.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> FindingOrigin {
        FindingOrigin {
            analysis_kind: "comparison".into(),
            run_id: "acmp-1".into(),
            source_rule_id: "cmp-rules".into(),
            source_rule_version: 1,
        }
    }

    fn cmp_facts<'a>(
        classification: &'a str,
        baseline: u64,
        suspect: u64,
        bp: &'a str,
        ceiling: SeverityCeiling,
        records: &'a [String],
    ) -> ComparisonFacts<'a> {
        ComparisonFacts {
            key: "connection reset",
            dimension: "message_pattern",
            classification,
            baseline_count: baseline,
            suspect_count: suspect,
            rate_change_bp: bp,
            ceiling,
            record_ids: records,
            total_record_matches: records.len() as u64,
        }
    }

    #[test]
    fn the_severity_table_is_pinned_cell_by_cell() {
        use FindingSeverity as S;
        use Magnitude::*;
        use SeverityCeiling as C;
        let expected = [
            (C::Fatal, Small, S::Medium),
            (C::Fatal, Moderate, S::High),
            (C::Fatal, Large, S::High),
            (C::Error, Small, S::Low),
            (C::Error, Moderate, S::Medium),
            (C::Error, Large, S::High),
            (C::Warn, Small, S::Low),
            (C::Warn, Moderate, S::Low),
            (C::Warn, Large, S::Medium),
            (C::Info, Small, S::Informational),
            (C::Info, Moderate, S::Informational),
            (C::Info, Large, S::Low),
            (C::Debug, Large, S::Low),
            (C::Trace, Small, S::Informational),
            (C::Unknown, Small, S::Informational),
            (C::Unknown, Large, S::Low),
        ];
        for (ceiling, magnitude, want) in expected {
            assert_eq!(
                severity_for(ceiling, magnitude),
                want,
                "{:?} × {:?}",
                ceiling,
                magnitude
            );
        }
        // Nothing below warn can reach medium or high, whatever the
        // magnitude: severity is capped by the evidence, not by size.
        for ceiling in [C::Unknown, C::Trace, C::Debug, C::Info] {
            for magnitude in [Small, Moderate, Large] {
                assert!(severity_for(ceiling, magnitude) <= S::Low);
            }
        }
    }

    #[test]
    fn otlp_severity_numbers_band_to_the_canonical_ceiling() {
        assert_eq!(
            SeverityCeiling::from_number(Some(1)),
            SeverityCeiling::Trace
        );
        assert_eq!(SeverityCeiling::from_number(Some(9)), SeverityCeiling::Info);
        assert_eq!(
            SeverityCeiling::from_number(Some(13)),
            SeverityCeiling::Warn
        );
        assert_eq!(
            SeverityCeiling::from_number(Some(17)),
            SeverityCeiling::Error
        );
        assert_eq!(
            SeverityCeiling::from_number(Some(24)),
            SeverityCeiling::Fatal
        );
        // Out of range and absent are Unknown, never guessed.
        for n in [None, Some(0), Some(25), Some(-3)] {
            assert_eq!(SeverityCeiling::from_number(n), SeverityCeiling::Unknown);
        }
    }

    #[test]
    fn a_new_pattern_finding_needs_both_the_count_and_the_severity() {
        let t = FindingThresholds::default();
        let records = vec!["r1".to_string(), "r2".to_string()];
        // Enough records and an error ceiling: fires.
        let f = new_high_severity_pattern(
            &cmp_facts("new", 0, 40, "undefined", SeverityCeiling::Error, &records),
            &t,
            &origin(),
        )
        .unwrap();
        assert_eq!(f.rule_id, "new-high-severity-pattern");
        // 40 is at least 3× but below 10× the min_new_count of 5.
        assert_eq!(f.magnitude, Magnitude::Moderate);
        assert_eq!(f.severity, FindingSeverity::Medium);
        assert_eq!(f.confidence, FindingConfidence::Measured);
        // Ten times the threshold reaches the top band.
        let big = new_high_severity_pattern(
            &cmp_facts("new", 0, 50, "undefined", SeverityCeiling::Error, &records),
            &t,
            &origin(),
        )
        .unwrap();
        assert_eq!(big.magnitude, Magnitude::Large);
        assert_eq!(big.severity, FindingSeverity::High);

        // Below the count threshold: nothing.
        assert!(new_high_severity_pattern(
            &cmp_facts("new", 0, 4, "undefined", SeverityCeiling::Error, &records),
            &t,
            &origin()
        )
        .is_none());
        // Info-level: a fact, but not this rule's subject.
        assert!(new_high_severity_pattern(
            &cmp_facts("new", 0, 40, "undefined", SeverityCeiling::Info, &records),
            &t,
            &origin()
        )
        .is_none());
        // A different classification never reaches this rule.
        assert!(new_high_severity_pattern(
            &cmp_facts("increased", 5, 40, "5000", SeverityCeiling::Error, &records),
            &t,
            &origin()
        )
        .is_none());
    }

    #[test]
    fn an_increase_bands_by_rate_and_refuses_an_unusable_rate() {
        let t = FindingThresholds::default();
        let records: Vec<String> = vec![];
        let at = |bp: &str| {
            increased_error_pattern(
                &cmp_facts("increased", 10, 100, bp, SeverityCeiling::Error, &records),
                &t,
                &origin(),
            )
        };
        assert_eq!(at("1000").unwrap().magnitude, Magnitude::Small);
        assert_eq!(at("5000").unwrap().magnitude, Magnitude::Moderate);
        assert_eq!(at("20000").unwrap().magnitude, Magnitude::Large);
        assert_eq!(at("20000").unwrap().severity, FindingSeverity::High);
        // Sign is dropped when banding: the movement is what is measured.
        assert_eq!(at("-20000").unwrap().magnitude, Magnitude::Large);
        // A zero baseline has no comparable rate, and `undefined` is not
        // quietly read as zero.
        assert!(at("undefined").is_none());
        assert!(at("").is_none());
    }

    #[test]
    fn a_finding_never_outranks_the_evidence_it_came_from() {
        let t = FindingThresholds::default();
        let records: Vec<String> = vec![];
        for (strength, want) in [
            (EvidenceStrength::Documented, FindingConfidence::Measured),
            (
                EvidenceStrength::Corroborated,
                FindingConfidence::Corroborated,
            ),
            (EvidenceStrength::Indicative, FindingConfidence::Indicative),
        ] {
            let facts = SignalFacts {
                kind: SignalKind::Retry,
                group_key: "rq-1",
                key_selector: "request_id",
                occurrences: 9,
                strongest: strength,
                ceiling: SeverityCeiling::Error,
                record_ids: &records,
                total_record_matches: 0,
            };
            let f = repeated_retry_signal(&facts, &t, &origin()).unwrap();
            assert_eq!(f.confidence, want);
            // The signal's own limitation travels with the finding.
            assert!(f
                .limitations
                .iter()
                .any(|l| l.contains("do not establish why the attempts repeated")
                    || l.contains("not establish that the retry was triggered")));
        }
    }

    #[test]
    fn signal_rules_only_answer_for_their_own_kind() {
        let t = FindingThresholds::default();
        let records: Vec<String> = vec![];
        let facts = |kind| SignalFacts {
            kind,
            group_key: "rq-1",
            key_selector: "request_id",
            occurrences: 9,
            strongest: EvidenceStrength::Documented,
            ceiling: SeverityCeiling::Warn,
            record_ids: &records,
            total_record_matches: 0,
        };
        assert!(repeated_retry_signal(&facts(SignalKind::Retry), &t, &origin()).is_some());
        assert!(repeated_retry_signal(&facts(SignalKind::Gap), &t, &origin()).is_none());
        assert!(
            operational_duplicate(&facts(SignalKind::OperationalDuplicate), &t, &origin())
                .is_some()
        );
        assert!(operational_duplicate(&facts(SignalKind::Retry), &t, &origin()).is_none());
        assert!(clock_skew_warning(&facts(SignalKind::ClockSkew), &t, &origin()).is_some());
        assert!(clock_skew_warning(&facts(SignalKind::Retry), &t, &origin()).is_none());
        // Below the occurrence threshold nothing fires at all.
        let mut sparse = facts(SignalKind::Retry);
        sparse.occurrences = 2;
        assert!(repeated_retry_signal(&sparse, &t, &origin()).is_none());
    }

    #[test]
    fn concentration_is_integer_only_and_needs_something_to_concentrate() {
        let t = FindingThresholds::default();
        let records: Vec<String> = vec![];
        let changes: Vec<(String, u64)> = [("a", 90u64), ("b", 5), ("c", 3), ("d", 1), ("e", 1)]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect();
        let facts = ConcentrationFacts {
            dimension: "operation",
            sorted_abs_changes: &changes,
            total_abs_change: 100,
            ceiling: SeverityCeiling::Error,
            record_ids: &records,
            total_record_matches: 0,
        };
        let f = concentrated_dimension_change(&facts, &t, &origin()).unwrap();
        // 90 + 5 + 3 = 98 of 100 = 9800 bp.
        assert!(f.calculation.contains("9800"));
        assert!(f.title.contains("98.00%"));
        assert_eq!(f.magnitude, Magnitude::Large);

        // Evenly spread: no concentration.
        let even: Vec<(String, u64)> = (0..10).map(|i| (format!("k{i}"), 10u64)).collect();
        let spread = ConcentrationFacts {
            sorted_abs_changes: &even,
            total_abs_change: 100,
            ..facts.clone()
        };
        assert!(concentrated_dimension_change(&spread, &t, &origin()).is_none());

        // Fewer keys than the window: nothing to concentrate into.
        let two: Vec<(String, u64)> = vec![("a".into(), 60), ("b".into(), 40)];
        let tiny = ConcentrationFacts {
            sorted_abs_changes: &two,
            total_abs_change: 100,
            ..facts.clone()
        };
        assert!(concentrated_dimension_change(&tiny, &t, &origin()).is_none());

        // No change at all is not a finding, and never a divide by zero.
        let zero = ConcentrationFacts {
            total_abs_change: 0,
            ..facts.clone()
        };
        assert!(concentrated_dimension_change(&zero, &t, &origin()).is_none());
    }

    #[test]
    fn records_are_bounded_and_the_remainder_is_counted() {
        let t = FindingThresholds::default();
        let many: Vec<String> = (0..50).map(|i| format!("r{i}")).collect();
        let mut facts = cmp_facts("new", 0, 40, "undefined", SeverityCeiling::Error, &many);
        facts.total_record_matches = 500;
        let f = new_high_severity_pattern(&facts, &t, &origin()).unwrap();
        assert_eq!(f.record_ids.len(), MAX_FINDING_RECORDS);
        assert_eq!(f.records_truncated, 480);
        assert_eq!(f.record_ids[0], "r0");
    }

    #[test]
    fn thresholds_refuse_configurations_that_cannot_mean_anything() {
        let d = FindingThresholds::default();
        d.validate().unwrap();
        assert_eq!(FindingThresholds::parse("{}").unwrap(), d);
        let custom = FindingThresholds::parse("{\"min_new_count\":9}").unwrap();
        assert_eq!(custom.min_new_count, 9);
        assert_eq!(custom.min_gone_count, d.min_gone_count);

        // Overlapping magnitude bands would make a change both.
        assert!(FindingThresholds::parse(
            "{\"magnitude_moderate_bp\":5000,\"magnitude_large_bp\":5000}"
        )
        .is_err());
        // A share above 100 % is not a quantity that exists.
        assert!(FindingThresholds::parse("{\"concentration_share_bp\":10001}").is_err());
        assert!(FindingThresholds::parse("{\"concentration_max_keys\":0}").is_err());
        assert!(FindingThresholds::parse("{\"surprise\":1}").is_err());
        assert!(FindingThresholds::parse("[]").is_err());
    }

    #[test]
    fn every_rule_is_named_parseable_and_carries_a_real_limitation() {
        for name in FINDING_RULES {
            let rule = FindingRule::parse(name).unwrap();
            assert_eq!(rule.as_str(), *name);
            assert!(!rule.rule_id().is_empty());
            assert!(
                rule.limitation().len() > 60,
                "{name} needs a limitation worth reading"
            );
        }
        assert!(FindingRule::parse("looks_bad").is_err());
    }

    #[test]
    fn ranking_is_deterministic_and_severity_led() {
        let t = FindingThresholds::default();
        let records: Vec<String> = vec![];
        let high = new_high_severity_pattern(
            &cmp_facts("new", 0, 90, "undefined", SeverityCeiling::Error, &records),
            &t,
            &origin(),
        )
        .unwrap();
        let low = new_high_severity_pattern(
            &cmp_facts("new", 0, 6, "undefined", SeverityCeiling::Warn, &records),
            &t,
            &origin(),
        )
        .unwrap();
        let mut findings = vec![low.clone(), high.clone()];
        findings.sort_by(|a, b| finding_order(a).cmp(&finding_order(b)));
        assert_eq!(findings[0].severity, FindingSeverity::High);
        assert_eq!(findings[1].severity, low.severity);
        // Sorting twice changes nothing.
        let once = findings.clone();
        findings.sort_by(|a, b| finding_order(a).cmp(&finding_order(b)));
        assert_eq!(once, findings);
    }

    #[test]
    fn no_finding_string_claims_a_cause_an_impact_or_a_resolution() {
        let t = FindingThresholds::default();
        let records: Vec<String> = vec!["r1".into()];
        let changes: Vec<(String, u64)> = vec![
            ("a".into(), 90),
            ("b".into(), 5),
            ("c".into(), 3),
            ("d".into(), 2),
        ];
        let mut texts: Vec<String> = Vec::new();

        for ceiling in [SeverityCeiling::Fatal, SeverityCeiling::Error] {
            for f in [
                new_high_severity_pattern(
                    &cmp_facts("new", 0, 40, "undefined", ceiling, &records),
                    &t,
                    &origin(),
                ),
                increased_error_pattern(
                    &cmp_facts("increased", 10, 100, "20000", ceiling, &records),
                    &t,
                    &origin(),
                ),
                disappeared_pattern(
                    &cmp_facts("disappeared", 40, 0, "-10000", ceiling, &records),
                    &t,
                    &origin(),
                ),
            ]
            .into_iter()
            .flatten()
            {
                texts.push(f.explain());
                texts.push(f.title.clone());
                texts.push(f.calculation.clone());
                texts.extend(f.limitations.clone());
            }
        }
        for kind in [
            SignalKind::Retry,
            SignalKind::OperationalDuplicate,
            SignalKind::ClockSkew,
        ] {
            let facts = SignalFacts {
                kind,
                group_key: "rq-1",
                key_selector: "request_id",
                occurrences: 9,
                strongest: EvidenceStrength::Documented,
                ceiling: SeverityCeiling::Error,
                record_ids: &records,
                total_record_matches: 1,
            };
            for f in [
                repeated_retry_signal(&facts, &t, &origin()),
                operational_duplicate(&facts, &t, &origin()),
                clock_skew_warning(&facts, &t, &origin()),
            ]
            .into_iter()
            .flatten()
            {
                texts.push(f.explain());
            }
        }
        texts.push(
            concentrated_dimension_change(
                &ConcentrationFacts {
                    dimension: "operation",
                    sorted_abs_changes: &changes,
                    total_abs_change: 100,
                    ceiling: SeverityCeiling::Error,
                    record_ids: &records,
                    total_record_matches: 1,
                },
                &t,
                &origin(),
            )
            .unwrap()
            .explain(),
        );

        assert!(texts.len() > 20, "the corpus must actually produce text");

        // Two lists, because a disclaimer has to be allowed to name the
        // thing it disclaims. "It is not a statement about customer,
        // financial, or security impact" contains `impact` and
        // `customer` and is precisely the sentence that keeps the
        // finding honest — banning those words everywhere would delete
        // the guard rather than enforce it.

        // (a) Claims about cause or certainty: never acceptable
        //     anywhere, in any grammatical form.
        for text in &texts {
            let lowered = text.to_lowercase();
            for forbidden in [
                "confirmed",
                "root cause",
                "caused by",
                "therefore",
                "because of",
                "proves",
                "resulted in",
                "must be fixed",
                "we recommend",
            ] {
                assert!(!lowered.contains(forbidden), "{forbidden:?} in {text}");
            }
        }

        // (b) Business-impact vocabulary: banned from the text that
        //     *asserts* something — the title, the subject, and the
        //     calculation — where it could only be a claim.
        let mut claims: Vec<String> = Vec::new();
        for ceiling in [SeverityCeiling::Fatal, SeverityCeiling::Error] {
            for f in [
                new_high_severity_pattern(
                    &cmp_facts("new", 0, 40, "undefined", ceiling, &records),
                    &t,
                    &origin(),
                ),
                increased_error_pattern(
                    &cmp_facts("increased", 10, 100, "20000", ceiling, &records),
                    &t,
                    &origin(),
                ),
                disappeared_pattern(
                    &cmp_facts("disappeared", 40, 0, "-10000", ceiling, &records),
                    &t,
                    &origin(),
                ),
            ]
            .into_iter()
            .flatten()
            {
                claims.push(f.title.clone());
                claims.push(f.calculation.clone());
                claims.push(f.subject.clone());
                claims.push(f.severity.as_str().to_string());
            }
        }
        assert!(!claims.is_empty());
        for text in &claims {
            let lowered = text.to_lowercase();
            for forbidden in [
                "impact", "customer", "revenue", "breach", "outage", "critical", "urgent",
                "resolved", "mitigat", "incident",
            ] {
                assert!(
                    !lowered.contains(forbidden),
                    "{forbidden:?} in asserting text {text:?}"
                );
            }
        }
        // And the two standing statements are actually present — the
        // guards above only prove the absence of bad wording, which an
        // empty string would also satisfy.
        let rendered = new_high_severity_pattern(
            &cmp_facts("new", 0, 40, "undefined", SeverityCeiling::Error, &records),
            &t,
            &origin(),
        )
        .unwrap()
        .explain();
        assert!(rendered.contains("not a statement about customer, financial, or security"));
        assert!(rendered.contains("does not establish a cause"));
    }
}
