//! Deterministic analysis identity (v0.4 WP1): canonical serialization,
//! semantic-input fingerprints, and content-addressed result IDs.
//!
//! Contract (binding for every v0.4 algorithm):
//! - Identity inputs are canonical JSON: object keys sorted (serde_json's
//!   default `Map` is a BTreeMap, so serialization is sorted by
//!   construction — asserted by test), arrays in documented order,
//!   compact encoding, and **no floating-point numbers** — thresholds
//!   and rates travel as decimal strings so no platform float formatting
//!   can enter an identity.
//! - The opaque execution id (`arun-<uuid>`) is distinct from the
//!   deterministic semantic-input fingerprint (`asem-<hex>`): rerunning
//!   identical inputs mints a new execution record with the SAME
//!   semantic fingerprint and the same semantic result identities.
//! - Result IDs are content-addressed BLAKE3 digests over canonical
//!   identity inputs. Counts, timestamps, example selection, display
//!   labels, machine state, and row order never participate.
//! - Full 64-hex digests are stored; displays may shorten, but the full
//!   ID stays available. Uniqueness is enforced by the store on the full
//!   ID: a genuine collision fails loudly and is never silently merged.

use serde_json::Value;

use crate::CaseError;

/// Analysis-definition schema version written to
/// `analysis_definitions.definition_schema_version`.
pub const ANALYSIS_DEFINITION_SCHEMA_VERSION: i64 = 1;

/// Serializes a JSON value canonically for identity input: compact, keys
/// sorted (by construction), floats refused.
pub fn canonical_identity_json(value: &Value) -> Result<String, CaseError> {
    refuse_floats(value, "$")?;
    serde_json::to_string(value)
        .map_err(|e| CaseError::Invalid(format!("canonical serialization failed: {e}")))
}

fn refuse_floats(value: &Value, path: &str) -> Result<(), CaseError> {
    match value {
        Value::Number(n) => {
            if n.as_i64().is_none() && n.as_u64().is_none() {
                return Err(CaseError::Invalid(format!(
                    "identity input contains a floating-point number at {path}; \
                     encode thresholds and rates as decimal strings"
                )));
            }
            Ok(())
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                refuse_floats(item, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (k, v) in map {
                refuse_floats(v, &format!("{path}.{k}"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn digest(prefix: &str, canonical: &str) -> String {
    format!("{prefix}-{}", blake3::hash(canonical.as_bytes()).to_hex())
}

/// Fingerprints a normalized configuration (`acfg-<hex>`). The input is
/// re-parsed so key order in the stored text never matters.
pub fn config_fingerprint(config_json: &str) -> Result<String, CaseError> {
    let value: Value = serde_json::from_str(config_json)
        .map_err(|e| CaseError::Invalid(format!("configuration is not valid JSON: {e}")))?;
    if !value.is_object() {
        return Err(CaseError::Invalid(
            "configuration must be a JSON object".into(),
        ));
    }
    Ok(digest("acfg", &canonical_identity_json(&value)?))
}

/// The deterministic semantic-input identity of one analysis execution.
/// Everything that changes results is here; nothing else is.
#[derive(Debug, Clone)]
pub struct SemanticIdentity<'a> {
    /// `(dataset_id, dsrev)` pairs; sorted internally by dataset_id.
    pub dataset_revs: &'a [(String, String)],
    /// `qry-<hex>`; `None` for an empty (all-data) query.
    pub query_fingerprint: Option<&'a str>,
    pub query_language_version: i64,
    /// Concrete UTC-nanos half-open bounds (already resolved; a relative
    /// strategy must be frozen before identity is computed).
    pub bounds: &'a Value,
    pub algorithm_id: &'a str,
    pub algorithm_version: i64,
    /// `acfg-<hex>` from [`config_fingerprint`].
    pub config_fingerprint: &'a str,
}

/// `asem-<hex>` — identical inputs always yield the identical value.
pub fn semantic_fingerprint(identity: &SemanticIdentity<'_>) -> Result<String, CaseError> {
    let mut revs: Vec<(&str, &str)> = identity
        .dataset_revs
        .iter()
        .map(|(d, r)| (d.as_str(), r.as_str()))
        .collect();
    revs.sort();
    let value = serde_json::json!({
        "dataset_revs": revs,
        "query_fingerprint": identity.query_fingerprint,
        "query_language_version": identity.query_language_version,
        "bounds": identity.bounds,
        "algorithm_id": identity.algorithm_id,
        "algorithm_version": identity.algorithm_version,
        "config_fingerprint": identity.config_fingerprint,
    });
    Ok(digest("asem", &canonical_identity_json(&value)?))
}

/// `pat-<hex>` — content-addressed from the algorithm identity, the
/// masking-configuration identity, and the normalized template alone.
pub fn pattern_id(
    algorithm_id: &str,
    algorithm_version: i64,
    masking_fingerprint: &str,
    normalized_template: &str,
) -> String {
    let value = serde_json::json!({
        "algorithm_id": algorithm_id,
        "algorithm_version": algorithm_version,
        "masking_fingerprint": masking_fingerprint,
        "template": normalized_template,
    });
    digest("pat", &value.to_string())
}

/// `stk-<hex>` — stack fingerprints are content-addressed like patterns
/// but over the exception type, ordered normalized frames, and the cause
/// chain; explicit truncation participates so a partial trace never
/// collides with its complete form.
pub fn stack_fingerprint_id(
    algorithm_id: &str,
    algorithm_version: i64,
    masking_fingerprint: &str,
    exception_type: &str,
    normalized_frames: &[String],
    cause_chain: &[String],
    truncated: bool,
) -> String {
    let value = serde_json::json!({
        "algorithm_id": algorithm_id,
        "algorithm_version": algorithm_version,
        "masking_fingerprint": masking_fingerprint,
        "exception_type": exception_type,
        "frames": normalized_frames,
        "causes": cause_chain,
        "truncated": truncated,
    });
    digest("stk", &value.to_string())
}

/// `acmp-<hex>` — a comparison result identity: the run's semantic
/// fingerprint plus dimension/key/rule identity.
pub fn comparison_result_id(
    semantic_fingerprint: &str,
    dimension: &str,
    key_identity: &str,
    rule_id: &str,
    rule_version: i64,
) -> String {
    let value = serde_json::json!({
        "semantic_fingerprint": semantic_fingerprint,
        "dimension": dimension,
        "key": key_identity,
        "rule_id": rule_id,
        "rule_version": rule_version,
    });
    digest("acmp", &value.to_string())
}

/// `acor-<hex>` — correlation edge/group identity. Participant order is
/// irrelevant by construction (sorted internally).
pub fn correlation_id(
    semantic_fingerprint: &str,
    rule_id: &str,
    rule_version: i64,
    normalized_key_or_rule: &str,
    participant_event_ids: &[String],
) -> String {
    let mut participants: Vec<&str> = participant_event_ids.iter().map(String::as_str).collect();
    participants.sort_unstable();
    let value = serde_json::json!({
        "semantic_fingerprint": semantic_fingerprint,
        "rule_id": rule_id,
        "rule_version": rule_version,
        "key": normalized_key_or_rule,
        "participants": participants,
    });
    digest("acor", &value.to_string())
}

/// `afind-<hex>` — deterministic finding identity: rule identity, the
/// subject's semantic identity, and the calculation identity. The opaque
/// execution run that produced a persisted evaluation is recorded on the
/// row, not in the ID.
pub fn finding_id(
    rule_id: &str,
    rule_version: i64,
    subject_semantic_id: &str,
    calculation_identity: &Value,
) -> Result<String, CaseError> {
    let value = serde_json::json!({
        "rule_id": rule_id,
        "rule_version": rule_version,
        "subject": subject_semantic_id,
        "calculation": calculation_identity,
    });
    Ok(digest("afind", &canonical_identity_json(&value)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_and_refuses_floats() {
        let v: Value = serde_json::from_str(r#"{"zeta":1,"alpha":{"b":2,"a":3}}"#).unwrap();
        assert_eq!(
            canonical_identity_json(&v).unwrap(),
            r#"{"alpha":{"a":3,"b":2},"zeta":1}"#
        );
        let f: Value = serde_json::from_str(r#"{"threshold":0.5}"#).unwrap();
        let err = canonical_identity_json(&f).unwrap_err();
        assert!(err.to_string().contains("$.threshold"));
    }

    #[test]
    fn semantic_fingerprint_is_input_order_independent() {
        let bounds = serde_json::json!({"start": 1, "end": 2});
        let a = vec![
            ("ds-b".to_string(), "dsrev-2".to_string()),
            ("ds-a".to_string(), "dsrev-1".to_string()),
        ];
        let mut b = a.clone();
        b.reverse();
        let fp = |revs: &[(String, String)]| {
            semantic_fingerprint(&SemanticIdentity {
                dataset_revs: revs,
                query_fingerprint: Some("qry-x"),
                query_language_version: 1,
                bounds: &bounds,
                algorithm_id: "template.v1",
                algorithm_version: 1,
                config_fingerprint: "acfg-abc",
            })
            .unwrap()
        };
        assert_eq!(fp(&a), fp(&b));
        // Any single component change changes the identity.
        let other = semantic_fingerprint(&SemanticIdentity {
            dataset_revs: &a,
            query_fingerprint: Some("qry-x"),
            query_language_version: 1,
            bounds: &bounds,
            algorithm_id: "template.v1",
            algorithm_version: 2,
            config_fingerprint: "acfg-abc",
        })
        .unwrap();
        assert_ne!(fp(&a), other);
    }

    #[test]
    fn correlation_identity_ignores_participant_order() {
        let a = correlation_id(
            "asem-x",
            "rule.trace",
            1,
            "trace:abc",
            &["log-2".into(), "log-1".into()],
        );
        let b = correlation_id(
            "asem-x",
            "rule.trace",
            1,
            "trace:abc",
            &["log-1".into(), "log-2".into()],
        );
        assert_eq!(a, b);
    }
}
