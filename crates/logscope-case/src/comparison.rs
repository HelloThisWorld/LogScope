//! Deterministic baseline-versus-suspect classification (v0.4 WP3,
//! `cmp-rules` v1) — the pure decision table behind window comparison.
//!
//! Contract (design pass; enforced by the tests below):
//! - All arithmetic is integer arithmetic. Rates are never materialized
//!   as floats: rate comparisons cross-multiply counts by the OTHER
//!   window's duration (`count_s · dur_b` vs `count_b · dur_s`, in
//!   u128), and relative change is expressed in basis points computed
//!   with integer division. No float can enter a classification, an
//!   identity, or a serialized result.
//! - Windows of different durations therefore classify on rates by
//!   construction; raw counts are still reported but never the hidden
//!   basis of a classification.
//! - Zero baselines never divide: the relative change is the explicit
//!   string state `"undefined"`, and `New` additionally requires the
//!   configured minimum suspect count. Zero suspects mirror this with
//!   `Disappeared` and the minimum baseline count.
//! - Below the minimum counts on both sides the honest answer is
//!   `InsufficientData`, not `Unchanged`.

use serde::{Deserialize, Serialize};

use crate::CaseError;

/// Rule identity for the classification table.
pub const COMPARISON_RULE_ID: &str = "cmp-rules";
pub const COMPARISON_RULE_VERSION: i64 = 1;

/// Versioned thresholds. Integers only; `rel_threshold_bp` is basis
/// points of relative RATE change (10000 = +100%).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComparisonThresholds {
    /// Minimum combined-side evidence before any Increase/Decrease/
    /// Unchanged claim (per side max is compared).
    pub min_count: u64,
    /// Minimum suspect count before a zero-baseline key is `New`.
    pub min_new_count: u64,
    /// Minimum baseline count before a zero-suspect key is `Disappeared`.
    pub min_gone_count: u64,
    /// Relative rate-change threshold, basis points.
    pub rel_threshold_bp: u64,
    /// Absolute count-difference threshold (documented as raw counts).
    pub abs_threshold: u64,
}

impl Default for ComparisonThresholds {
    fn default() -> Self {
        ComparisonThresholds {
            min_count: 5,
            min_new_count: 5,
            min_gone_count: 5,
            rel_threshold_bp: 5_000, // ±50 % rate change
            abs_threshold: 10,
        }
    }
}

impl ComparisonThresholds {
    /// Parses thresholds JSON (`{}` = defaults); unknown keys and
    /// non-object shapes are structured refusals.
    pub fn parse(thresholds_json: &str) -> Result<ComparisonThresholds, CaseError> {
        let trimmed = thresholds_json.trim();
        if trimmed.is_empty() || trimmed == "{}" {
            return Ok(ComparisonThresholds::default());
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| CaseError::Invalid(format!("thresholds do not parse: {e}")))?;
        if !value.is_object() {
            return Err(CaseError::Invalid(
                "thresholds must be a JSON object".into(),
            ));
        }
        serde_json::from_value(value)
            .map_err(|e| CaseError::Invalid(format!("thresholds do not parse: {e}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    New,
    Disappeared,
    Increased,
    Decreased,
    Unchanged,
    InsufficientData,
}

impl Classification {
    pub fn as_str(self) -> &'static str {
        match self {
            Classification::New => "new",
            Classification::Disappeared => "disappeared",
            Classification::Increased => "increased",
            Classification::Decreased => "decreased",
            Classification::Unchanged => "unchanged",
            Classification::InsufficientData => "insufficient_data",
        }
    }
}

/// One classified key: every input and derived quantity that fed the
/// decision, ready to serialize into the result row. Relative change is
/// a string: basis points as a decimal string, or `"undefined"` for a
/// zero baseline — never a float, never infinity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classified {
    pub classification: Classification,
    pub rule_id: &'static str,
    pub rule_version: i64,
    pub baseline_count: u64,
    pub suspect_count: u64,
    pub baseline_duration_nanos: i64,
    pub suspect_duration_nanos: i64,
    /// suspect − baseline raw counts (may be negative).
    pub count_change: i64,
    /// Relative RATE change in basis points as a decimal string
    /// (`"2500"` = +25 %, `"-10000"` = −100 %), or `"undefined"`.
    pub rate_change_bp: String,
}

/// Classifies one key. Total and pure: any counts and positive
/// durations in, exactly one classification out.
pub fn classify(
    baseline_count: u64,
    suspect_count: u64,
    baseline_duration_nanos: i64,
    suspect_duration_nanos: i64,
    t: &ComparisonThresholds,
) -> Result<Classified, CaseError> {
    if baseline_duration_nanos <= 0 || suspect_duration_nanos <= 0 {
        return Err(CaseError::Invalid(
            "comparison windows must have positive duration".into(),
        ));
    }
    let dur_b = baseline_duration_nanos as u128;
    let dur_s = suspect_duration_nanos as u128;
    // Cross-multiplied rate positions (never a division):
    // suspect_rate ⋛ baseline_rate ⟺ count_s·dur_b ⋛ count_b·dur_s.
    let lhs = suspect_count as u128 * dur_b;
    let rhs = baseline_count as u128 * dur_s;

    let rate_change_bp = if baseline_count == 0 {
        "undefined".to_string()
    } else {
        // ((lhs − rhs) · 10000) / rhs, signed, integer division
        // truncating toward zero (documented).
        let diff = lhs as i128 - rhs as i128;
        ((diff * 10_000) / rhs as i128).to_string()
    };
    let count_change = suspect_count as i64 - baseline_count as i64;

    let classification = if baseline_count == 0 && suspect_count == 0 {
        Classification::InsufficientData
    } else if baseline_count == 0 {
        if suspect_count >= t.min_new_count {
            Classification::New
        } else {
            Classification::InsufficientData
        }
    } else if suspect_count == 0 {
        if baseline_count >= t.min_gone_count {
            Classification::Disappeared
        } else {
            Classification::InsufficientData
        }
    } else if baseline_count.max(suspect_count) < t.min_count {
        Classification::InsufficientData
    } else {
        // Threshold test on rates (relative, bp) OR raw counts (absolute).
        let diff = lhs as i128 - rhs as i128;
        let rel_hit = diff.unsigned_abs() * 10_000 >= rhs * t.rel_threshold_bp as u128;
        let abs_hit = count_change.unsigned_abs() >= t.abs_threshold;
        if rel_hit || abs_hit {
            if diff > 0 {
                Classification::Increased
            } else if diff < 0 {
                Classification::Decreased
            } else {
                Classification::Unchanged
            }
        } else {
            Classification::Unchanged
        }
    };

    Ok(Classified {
        classification,
        rule_id: COMPARISON_RULE_ID,
        rule_version: COMPARISON_RULE_VERSION,
        baseline_count,
        suspect_count,
        baseline_duration_nanos,
        suspect_duration_nanos,
        count_change,
        rate_change_bp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> ComparisonThresholds {
        ComparisonThresholds::default()
    }

    const H: i64 = 3_600_000_000_000; // one hour in nanos

    #[test]
    fn zero_baseline_is_new_or_insufficient_never_divided() {
        let c = classify(0, 20, H, H, &t()).unwrap();
        assert_eq!(c.classification, Classification::New);
        assert_eq!(c.rate_change_bp, "undefined");
        let c = classify(0, 3, H, H, &t()).unwrap();
        assert_eq!(c.classification, Classification::InsufficientData);
        let c = classify(0, 0, H, H, &t()).unwrap();
        assert_eq!(c.classification, Classification::InsufficientData);
    }

    #[test]
    fn zero_suspect_mirrors_with_disappeared() {
        let c = classify(20, 0, H, H, &t()).unwrap();
        assert_eq!(c.classification, Classification::Disappeared);
        assert_eq!(c.rate_change_bp, "-10000");
        let c = classify(3, 0, H, H, &t()).unwrap();
        assert_eq!(c.classification, Classification::InsufficientData);
    }

    #[test]
    fn different_durations_classify_on_rates_not_counts() {
        // 100 in 1 h baseline vs 100 in 2 h suspect: counts equal, rate
        // halved → Decreased at the default 50 % threshold.
        let c = classify(100, 100, H, 2 * H, &t()).unwrap();
        assert_eq!(c.classification, Classification::Decreased);
        assert_eq!(c.rate_change_bp, "-5000");
        assert_eq!(c.count_change, 0);
        // 50 in 1 h vs 200 in 2 h: rate doubled → Increased, +100 %.
        let c = classify(50, 200, H, 2 * H, &t()).unwrap();
        assert_eq!(c.classification, Classification::Increased);
        assert_eq!(c.rate_change_bp, "10000");
    }

    #[test]
    fn thresholds_gate_honestly_and_exactly() {
        let mut th = t();
        th.abs_threshold = 1_000_000; // out of reach: isolate the relative test
                                      // +49.99…% is below the 50 % bar → Unchanged.
        let c = classify(10_000, 14_999, H, H, &th).unwrap();
        assert_eq!(c.classification, Classification::Unchanged);
        // exactly +50 % hits it.
        let c = classify(10_000, 15_000, H, H, &th).unwrap();
        assert_eq!(c.classification, Classification::Increased);
        // Absolute threshold alone can trigger on small relative moves.
        let mut th = t();
        th.rel_threshold_bp = 1_000_000;
        th.abs_threshold = 50;
        let c = classify(10_000, 10_050, H, H, &th).unwrap();
        assert_eq!(c.classification, Classification::Increased);
        // Both-sides-small is insufficient, not unchanged.
        let c = classify(2, 3, H, H, &t()).unwrap();
        assert_eq!(c.classification, Classification::InsufficientData);
    }

    #[test]
    fn serialized_results_never_contain_floats_or_infinities() {
        for (b, s) in [(0u64, 100u64), (100, 0), (1, u64::MAX / 2), (7, 7)] {
            let c = classify(b, s, H, 3 * H, &t()).unwrap();
            let json = serde_json::to_string(&c).unwrap();
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            fn no_floats(v: &serde_json::Value) {
                match v {
                    serde_json::Value::Number(n) => {
                        assert!(n.as_i64().is_some() || n.as_u64().is_some())
                    }
                    serde_json::Value::Array(a) => a.iter().for_each(no_floats),
                    serde_json::Value::Object(m) => m.values().for_each(no_floats),
                    _ => {}
                }
            }
            no_floats(&v);
        }
        assert!(classify(1, 1, 0, H, &t()).is_err(), "zero-duration refused");
    }

    #[test]
    fn thresholds_parse_strictly() {
        assert_eq!(ComparisonThresholds::parse("{}").unwrap(), t());
        let custom = ComparisonThresholds::parse("{\"min_count\":2}").unwrap();
        assert_eq!(custom.min_count, 2);
        assert_eq!(custom.rel_threshold_bp, t().rel_threshold_bp);
        assert!(ComparisonThresholds::parse("[]").is_err());
        assert!(ComparisonThresholds::parse("{\"surprise\":1}").is_err());
        assert!(ComparisonThresholds::parse("{\"min_count\":0.5}").is_err());
    }
}
