//! Disclosure projection (v0.3 W6).
//!
//! One pure projection shared by report preview, report generation, and
//! (in W7) case bundles: the preview IS the final bytes because both run
//! the same code over the same inputs. The projection never mutates
//! canonical data — it shapes what leaves the workspace, nothing else.
//!
//! Rules are ordered and typed; posture is default-closed where it
//! matters (provenance paths are omitted unless the profile widens
//! them). Every removal, mask, replacement, pseudonymization, and
//! truncation is counted, and the counts are rendered into the artifact
//! so an omission can never look like completeness.

use logscope_jobs::JobError;
use serde::{Deserialize, Serialize};

/// Fixed masking token — one token, so a mask can never smuggle data.
pub const MASK_TOKEN: &str = "[REDACTED]";
/// Bounds on user regex rules (linear-time engine; still bounded).
pub const MAX_REGEX_PATTERN_CHARS: usize = 512;
pub const MAX_REGEX_COMPILED_BYTES: usize = 1 << 20;
pub const MAX_RULES: usize = 128;
/// Default bound on any single projected text block.
pub const DEFAULT_MAX_TEXT_CHARS: usize = 16 * 1024;

/// Ordered, typed redaction rules (`rules_json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RedactionRule {
    /// Removes a named field entirely wherever it appears.
    OmitField { field: String },
    /// Replaces a named field's value with the fixed token.
    MaskField { field: String },
    /// Exact substring replacement in every projected text.
    ReplaceExact { find: String, replace: String },
    /// Bounded, linear-time regex replacement in every projected text.
    ReplaceRegex { pattern: String, replace: String },
    /// Deterministic pseudonym for a named field's value, labeled as
    /// such: identical inputs yield identical tokens, so correlation
    /// survives while the value does not.
    Pseudonymize {
        field: String,
        #[serde(default = "default_pseudo_prefix")]
        prefix: String,
    },
}

fn default_pseudo_prefix() -> String {
    "subject".into()
}

/// Provenance path policy — default-closed: paths leave the workspace
/// only when the profile explicitly widens this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PathPolicy {
    #[default]
    Omit,
    Basename,
    Include,
}

/// Posture (`posture_json`). Deny always wins over allow; a non-empty
/// allow list turns field projection into allowlist mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionPosture {
    #[serde(default)]
    pub path_policy: PathPolicy,
    #[serde(default)]
    pub field_allow: Vec<String>,
    #[serde(default)]
    pub field_deny: Vec<String>,
    #[serde(default = "default_max_text")]
    pub max_text_chars: usize,
}

fn default_max_text() -> usize {
    DEFAULT_MAX_TEXT_CHARS
}

impl Default for RedactionPosture {
    fn default() -> Self {
        RedactionPosture {
            path_policy: PathPolicy::default(),
            field_allow: Vec::new(),
            field_deny: Vec::new(),
            max_text_chars: DEFAULT_MAX_TEXT_CHARS,
        }
    }
}

/// Honest application counts, rendered into the artifact.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RedactionSummary {
    pub fields_omitted: u64,
    pub fields_masked: u64,
    pub text_replacements: u64,
    pub pseudonymized: u64,
    pub truncated_blocks: u64,
    pub paths_redacted: u64,
}

impl RedactionSummary {
    pub fn total(&self) -> u64 {
        self.fields_omitted
            + self.fields_masked
            + self.text_replacements
            + self.pseudonymized
            + self.truncated_blocks
            + self.paths_redacted
    }
}

enum CompiledRule {
    OmitField(String),
    MaskField(String),
    ReplaceExact { find: String, replace: String },
    ReplaceRegex { re: regex::Regex, replace: String },
    Pseudonymize { field: String, prefix: String },
}

/// A compiled, validated projection.
pub struct Projection {
    rules: Vec<CompiledRule>,
    posture: RedactionPosture,
}

fn invalid(msg: impl std::fmt::Display) -> JobError {
    JobError::new("redaction/invalid-profile", msg.to_string())
}

impl Projection {
    /// Parses and validates a profile. Unknown rule kinds, unknown
    /// posture keys, oversized rule lists, and oversized or invalid
    /// regexes are structured refusals — never silently skipped.
    pub fn compile(rules_json: &str, posture_json: &str) -> Result<Projection, JobError> {
        let rules: Vec<RedactionRule> = serde_json::from_str(rules_json)
            .map_err(|e| invalid(format!("rules_json does not parse: {e}")))?;
        if rules.len() > MAX_RULES {
            return Err(invalid(format!(
                "{} rules exceed the bound of {MAX_RULES}",
                rules.len()
            )));
        }
        let posture: RedactionPosture = if posture_json.trim().is_empty() {
            RedactionPosture::default()
        } else {
            serde_json::from_str(posture_json)
                .map_err(|e| invalid(format!("posture_json does not parse: {e}")))?
        };
        let mut compiled = Vec::with_capacity(rules.len());
        for rule in rules {
            compiled.push(match rule {
                RedactionRule::OmitField { field } => CompiledRule::OmitField(field),
                RedactionRule::MaskField { field } => CompiledRule::MaskField(field),
                RedactionRule::ReplaceExact { find, replace } => {
                    if find.is_empty() {
                        return Err(invalid("replace_exact.find must not be empty"));
                    }
                    CompiledRule::ReplaceExact { find, replace }
                }
                RedactionRule::ReplaceRegex { pattern, replace } => {
                    if pattern.chars().count() > MAX_REGEX_PATTERN_CHARS {
                        return Err(invalid(format!(
                            "regex pattern exceeds {MAX_REGEX_PATTERN_CHARS} chars"
                        )));
                    }
                    let re = regex::RegexBuilder::new(&pattern)
                        .size_limit(MAX_REGEX_COMPILED_BYTES)
                        .build()
                        .map_err(|e| invalid(format!("regex does not compile: {e}")))?;
                    CompiledRule::ReplaceRegex { re, replace }
                }
                RedactionRule::Pseudonymize { field, prefix } => {
                    CompiledRule::Pseudonymize { field, prefix }
                }
            });
        }
        Ok(Projection {
            rules: compiled,
            posture,
        })
    }

    /// Projects free text: ordered exact/regex replacements, then the
    /// posture's block bound. Field-scoped rules do not apply here.
    pub fn text(&self, input: &str, summary: &mut RedactionSummary) -> String {
        let mut out = input.to_string();
        for rule in &self.rules {
            match rule {
                CompiledRule::ReplaceExact { find, replace } => {
                    let hits = out.matches(find.as_str()).count() as u64;
                    if hits > 0 {
                        summary.text_replacements += hits;
                        out = out.replace(find.as_str(), replace);
                    }
                }
                CompiledRule::ReplaceRegex { re, replace } => {
                    let hits = re.find_iter(&out).count() as u64;
                    if hits > 0 {
                        summary.text_replacements += hits;
                        out = re.replace_all(&out, replace.as_str()).into_owned();
                    }
                }
                _ => {}
            }
        }
        if out.chars().count() > self.posture.max_text_chars {
            summary.truncated_blocks += 1;
            let cut: String = out.chars().take(self.posture.max_text_chars).collect();
            out = format!("{cut} …[truncated by disclosure profile]");
        }
        out
    }

    /// Field disposition by name: deny/allow lists, then field rules.
    /// `None` = the field is omitted entirely.
    fn field_value(
        &self,
        name: &str,
        value: &str,
        summary: &mut RedactionSummary,
    ) -> Option<String> {
        if self.posture.field_deny.iter().any(|d| d == name) {
            summary.fields_omitted += 1;
            return None;
        }
        if !self.posture.field_allow.is_empty()
            && !self.posture.field_allow.iter().any(|a| a == name)
        {
            summary.fields_omitted += 1;
            return None;
        }
        for rule in &self.rules {
            match rule {
                CompiledRule::OmitField(f) if f == name => {
                    summary.fields_omitted += 1;
                    return None;
                }
                CompiledRule::MaskField(f) if f == name => {
                    summary.fields_masked += 1;
                    return Some(MASK_TOKEN.to_string());
                }
                CompiledRule::Pseudonymize { field, prefix } if field == name => {
                    summary.pseudonymized += 1;
                    return Some(pseudonym(prefix, value));
                }
                _ => {}
            }
        }
        Some(self.text(value, summary))
    }

    /// Projects an evidence snapshot JSON structurally: `fields` arrays
    /// are matched by their `name` entry, ordinary object keys by key
    /// name, provenance-path keys by the posture's path policy, and
    /// every remaining string leaf goes through the text rules.
    pub fn snapshot_json(&self, snapshot_json: &str, summary: &mut RedactionSummary) -> String {
        match serde_json::from_str::<serde_json::Value>(snapshot_json) {
            Ok(mut v) => {
                self.walk(&mut v, summary);
                serde_json::to_string(&v).unwrap_or_else(|_| "{}".into())
            }
            // An unparseable snapshot is projected as opaque text so raw
            // bytes still cannot bypass the rules.
            Err(_) => self.text(snapshot_json, summary),
        }
    }

    fn is_path_key(key: &str) -> bool {
        matches!(key, "path" | "source_path" | "file_path" | "original_path")
    }

    fn walk(&self, v: &mut serde_json::Value, summary: &mut RedactionSummary) {
        match v {
            serde_json::Value::Object(map) => {
                let keys: Vec<String> = map.keys().cloned().collect();
                // A {name, value} pair is a projected display field.
                let is_field_entry = map.contains_key("name") && map.contains_key("value");
                if is_field_entry {
                    let name = map
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let value_str = match map.get("value") {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => String::new(),
                    };
                    match self.field_value(&name, &value_str, summary) {
                        None => {
                            map.clear();
                            map.insert("name".into(), serde_json::Value::String(name));
                            map.insert(
                                "omitted_by_disclosure_profile".into(),
                                serde_json::Value::Bool(true),
                            );
                            return;
                        }
                        Some(projected) => {
                            map.insert("value".into(), serde_json::Value::String(projected));
                        }
                    }
                    return;
                }
                for key in keys {
                    // Path policy applies before generic field rules.
                    if Self::is_path_key(&key) {
                        if let Some(serde_json::Value::String(p)) = map.get(&key) {
                            let projected = match self.posture.path_policy {
                                PathPolicy::Include => None,
                                PathPolicy::Basename => {
                                    summary.paths_redacted += 1;
                                    Some(basename(p))
                                }
                                PathPolicy::Omit => {
                                    summary.paths_redacted += 1;
                                    Some("[path omitted]".to_string())
                                }
                            };
                            if let Some(np) = projected {
                                map.insert(key.clone(), serde_json::Value::String(np));
                                continue;
                            }
                        }
                    }
                    let field_result = match map.get(&key) {
                        Some(serde_json::Value::String(s)) => {
                            Some(self.field_value(&key, s, summary))
                        }
                        _ => None,
                    };
                    match field_result {
                        Some(None) => {
                            map.insert(
                                key.clone(),
                                serde_json::Value::String("[omitted by disclosure profile]".into()),
                            );
                        }
                        Some(Some(projected)) => {
                            map.insert(key.clone(), serde_json::Value::String(projected));
                        }
                        None => {
                            if let Some(child) = map.get_mut(&key) {
                                self.walk(child, summary);
                            }
                        }
                    }
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    self.walk(item, summary);
                }
            }
            serde_json::Value::String(s) => {
                *s = self.text(s, summary);
            }
            _ => {}
        }
    }
}

/// Deterministic, labeled pseudonym: identical values map to identical
/// tokens (correlation survives, the value does not). No secret is
/// involved and none is implied — the label says "pseudonym", never
/// "anonymous".
pub fn pseudonym(prefix: &str, value: &str) -> String {
    let hash = blake3::hash(value.as_bytes());
    let hex: String = hash.as_bytes()[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{prefix}-{hex}")
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}
