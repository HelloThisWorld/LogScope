//! Platform-neutral investigation-workbench domain for LogScope.
//!
//! Owns the typed vocabulary (statuses, kinds, states), opaque id
//! minting, and per-entity validation shared by the application services
//! and the desktop boundary. Storage lives in `logscope-workspace`
//! (`case_meta`); query execution stays in `logscope-query`. This crate
//! deliberately has no database, DuckDB, or UI dependency so it remains
//! viable for the future macOS target.

pub mod envelope;
pub mod ids;
pub mod vocab;

pub use ids::new_id;
pub use vocab::*;

use thiserror::Error;

/// Entity schema version written to `investigations.entity_version`.
pub const INVESTIGATION_ENTITY_VERSION: i64 = 1;
/// Evidence envelope schema version written to `evidence.envelope_version`.
pub const EVIDENCE_ENVELOPE_VERSION: i64 = 1;

#[derive(Debug, Error)]
pub enum CaseError {
    #[error("invalid {field}: {value:?} (expected one of {expected})")]
    InvalidValue {
        field: &'static str,
        value: String,
        expected: &'static str,
    },
    #[error("{0}")]
    Invalid(String),
}

impl CaseError {
    /// Stable machine-readable code for the Rust/Tauri boundary.
    pub fn code(&self) -> &'static str {
        match self {
            CaseError::InvalidValue { .. } => "case/invalid-value",
            CaseError::Invalid(_) => "case/invalid",
        }
    }
}

/// Parses a user-supplied tag list from its stored JSON form: an array of
/// non-empty strings, bounded to keep rows and history payloads small.
pub const MAX_TAGS: usize = 32;
pub const MAX_TAG_LEN: usize = 128;

pub fn validate_tags_json(tags_json: &str) -> Result<Vec<String>, CaseError> {
    let tags: Vec<String> = serde_json::from_str(tags_json)
        .map_err(|_| CaseError::Invalid("tags must be a JSON array of strings".into()))?;
    if tags.len() > MAX_TAGS {
        return Err(CaseError::Invalid(format!(
            "too many tags: {} (max {MAX_TAGS})",
            tags.len()
        )));
    }
    for t in &tags {
        if t.trim().is_empty() || t.len() > MAX_TAG_LEN {
            return Err(CaseError::Invalid(format!(
                "tag must be non-empty and at most {MAX_TAG_LEN} bytes: {t:?}"
            )));
        }
    }
    Ok(tags)
}

/// Validates the kind/status shape of a typed investigation item: notes
/// and findings carry no status, tasks carry only a task status, and
/// questions carry only a question status.
pub fn validate_item_shape(
    kind: ItemKind,
    task_status: Option<&str>,
    question_status: Option<&str>,
) -> Result<(), CaseError> {
    let parsed_task = match task_status {
        None => None,
        Some(s) => Some(TaskStatus::parse(s).ok_or(CaseError::InvalidValue {
            field: "task_status",
            value: s.to_string(),
            expected: TaskStatus::EXPECTED,
        })?),
    };
    let parsed_question = match question_status {
        None => None,
        Some(s) => Some(QuestionStatus::parse(s).ok_or(CaseError::InvalidValue {
            field: "question_status",
            value: s.to_string(),
            expected: QuestionStatus::EXPECTED,
        })?),
    };
    match kind {
        ItemKind::Task => {
            if parsed_task.is_none() || parsed_question.is_some() {
                return Err(CaseError::Invalid(
                    "a task carries a task status and no question status".into(),
                ));
            }
        }
        ItemKind::Question => {
            if parsed_question.is_none() || parsed_task.is_some() {
                return Err(CaseError::Invalid(
                    "a question carries a question status and no task status".into(),
                ));
            }
        }
        ItemKind::Note | ItemKind::Finding => {
            if parsed_task.is_some() || parsed_question.is_some() {
                return Err(CaseError::Invalid(
                    "notes and findings carry no status fields".into(),
                ));
            }
        }
    }
    Ok(())
}
