//! Versioned declarative Import Profile contract (v0.0 draft).
//!
//! Profiles are pure data: they declare format, framing, timestamp,
//! severity, message, correlation, and alias rules. They are sandboxed by
//! construction — there is no code execution surface, only declarative
//! field selection. The full v0.1 contract adds encodings, multiline rules,
//! header rules, and redaction hints on top of this draft.

use std::collections::BTreeMap;

use logscope_model::{stable_id, Digest};
use logscope_normalize::{TimestampFormat, TimezonePolicy};
use serde::{Deserialize, Serialize};

/// Version of the profile *contract* (the shape of this struct family).
pub const PROFILE_CONTRACT_VERSION: u32 = 1;

/// Reference to a source field: by column/property name or by CSV index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "by", content = "key", rename_all = "snake_case")]
pub enum FieldRef {
    Name(String),
    Index(usize),
}

impl FieldRef {
    pub fn name(s: &str) -> Self {
        FieldRef::Name(s.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FormatSpec {
    Csv {
        /// ASCII delimiter byte (',' or '\t' or ';').
        delimiter: u8,
        has_headers: bool,
    },
    Jsonl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimestampRule {
    /// Candidate fields, first match wins.
    pub candidates: Vec<FieldRef>,
    pub format: TimestampFormat,
    pub timezone: TimezonePolicy,
}

/// Declarative Import Profile. Public, generic, organization-agnostic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportProfile {
    pub profile_id: String,
    /// Profile content version (bump on any rule change).
    pub version: String,
    pub contract_version: u32,
    pub display_name: String,
    pub format: FormatSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<TimestampRule>,
    /// Candidate severity fields, first match wins.
    #[serde(default)]
    pub severity: Vec<FieldRef>,
    /// Candidate message/body fields, first match wins.
    #[serde(default)]
    pub message: Vec<FieldRef>,
    #[serde(default)]
    pub trace_id: Vec<FieldRef>,
    #[serde(default)]
    pub span_id: Vec<FieldRef>,
    /// canonical generic field name (operation, outcome, event_type,
    /// request_id, transaction_id, message_id, entity_id) -> candidates.
    #[serde(default)]
    pub generic_fields: BTreeMap<String, Vec<FieldRef>>,
}

impl ImportProfile {
    /// Deterministic identity of the complete profile content. Two profiles
    /// with identical rules have identical fingerprints regardless of where
    /// they came from.
    pub fn fingerprint(&self) -> String {
        let canonical = serde_json::to_string(self).expect("profile serialization cannot fail");
        stable_id("prf", |d: &mut Digest| {
            d.str("profile.v1");
            d.str(&canonical);
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.profile_id.is_empty() {
            return Err("profile_id must not be empty".into());
        }
        if self.contract_version > PROFILE_CONTRACT_VERSION {
            return Err(format!(
                "profile contract version {} is newer than supported {}",
                self.contract_version, PROFILE_CONTRACT_VERSION
            ));
        }
        if let FormatSpec::Csv { delimiter, .. } = self.format {
            if !delimiter.is_ascii() {
                return Err("CSV delimiter must be ASCII".into());
            }
        }
        Ok(())
    }
}

/// Built-in public profiles (v0.0 proof set; v0.1 ships the full set).
pub mod builtin {
    use super::*;

    /// Generic JSON application logs (JSONL/NDJSON), RFC3339 timestamps.
    pub fn jsonl_generic() -> ImportProfile {
        ImportProfile {
            profile_id: "builtin.jsonl.generic".into(),
            version: "1".into(),
            contract_version: PROFILE_CONTRACT_VERSION,
            display_name: "Generic JSON lines application logs".into(),
            format: FormatSpec::Jsonl,
            timestamp: Some(TimestampRule {
                candidates: vec![
                    FieldRef::name("@timestamp"),
                    FieldRef::name("timestamp"),
                    FieldRef::name("time"),
                    FieldRef::name("ts"),
                ],
                format: TimestampFormat::Rfc3339,
                timezone: TimezonePolicy::AssumeUtc,
            }),
            severity: vec![
                FieldRef::name("log.level"),
                FieldRef::name("level"),
                FieldRef::name("severity"),
            ],
            message: vec![FieldRef::name("message"), FieldRef::name("msg")],
            trace_id: vec![FieldRef::name("trace_id"), FieldRef::name("trace.id")],
            span_id: vec![FieldRef::name("span_id"), FieldRef::name("span.id")],
            generic_fields: BTreeMap::new(),
        }
    }

    /// Elasticsearch export (JSONL of `_source` documents or plain hit
    /// objects; ECS-style field names). Generic and organization-agnostic:
    /// this maps only public Elasticsearch/ECS conventions.
    pub fn elasticsearch_export() -> ImportProfile {
        ImportProfile {
            profile_id: "builtin.elasticsearch.export".into(),
            version: "1".into(),
            contract_version: PROFILE_CONTRACT_VERSION,
            display_name: "Elasticsearch export (JSONL, ECS field names)".into(),
            format: FormatSpec::Jsonl,
            timestamp: Some(TimestampRule {
                candidates: vec![
                    FieldRef::name("@timestamp"),
                    FieldRef::name("timestamp"),
                    FieldRef::name("event.created"),
                ],
                format: TimestampFormat::Rfc3339,
                timezone: TimezonePolicy::AssumeUtc,
            }),
            severity: vec![
                FieldRef::name("log.level"),
                FieldRef::name("level"),
                FieldRef::name("severity"),
            ],
            message: vec![
                FieldRef::name("message"),
                FieldRef::name("event.original"),
                FieldRef::name("msg"),
            ],
            trace_id: vec![FieldRef::name("trace.id"), FieldRef::name("trace_id")],
            span_id: vec![FieldRef::name("span.id"), FieldRef::name("span_id")],
            generic_fields: BTreeMap::from([
                (
                    "event_type".to_string(),
                    vec![FieldRef::name("event.type"), FieldRef::name("event.action")],
                ),
                ("outcome".to_string(), vec![FieldRef::name("event.outcome")]),
                (
                    "request_id".to_string(),
                    vec![FieldRef::name("http.request.id")],
                ),
                (
                    "transaction_id".to_string(),
                    vec![FieldRef::name("transaction.id")],
                ),
            ]),
        }
    }

    /// Generic CSV with `timestamp,level,message` style headers.
    pub fn csv_basic() -> ImportProfile {
        ImportProfile {
            profile_id: "builtin.csv.basic".into(),
            version: "1".into(),
            contract_version: PROFILE_CONTRACT_VERSION,
            display_name: "Generic CSV logs (timestamp, level, message)".into(),
            format: FormatSpec::Csv {
                delimiter: b',',
                has_headers: true,
            },
            timestamp: Some(TimestampRule {
                candidates: vec![
                    FieldRef::name("timestamp"),
                    FieldRef::name("@timestamp"),
                    FieldRef::name("time"),
                ],
                format: TimestampFormat::Rfc3339,
                timezone: TimezonePolicy::AssumeUtc,
            }),
            severity: vec![FieldRef::name("level"), FieldRef::name("severity")],
            message: vec![FieldRef::name("message")],
            trace_id: vec![FieldRef::name("trace_id")],
            span_id: vec![FieldRef::name("span_id")],
            generic_fields: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_deterministic_and_content_sensitive() {
        let a = builtin::jsonl_generic();
        let b = builtin::jsonl_generic();
        assert_eq!(a.fingerprint(), b.fingerprint());
        let mut c = builtin::jsonl_generic();
        c.version = "2".into();
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn profiles_serialize_round_trip() {
        for p in [
            builtin::jsonl_generic(),
            builtin::csv_basic(),
            builtin::elasticsearch_export(),
        ] {
            p.validate().unwrap();
            let json = serde_json::to_string_pretty(&p).unwrap();
            let back: ImportProfile = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
    }
}
