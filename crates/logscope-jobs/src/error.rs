//! Structured job errors with stable codes.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Serializable, structured failure for the command boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Error)]
#[error("[{code}] {message}")]
pub struct JobError {
    /// Stable machine-readable code, e.g. `import/io`, `job/panic`.
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
    /// Whether retrying the job could succeed without user changes.
    pub retryable: bool,
}

impl JobError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        JobError {
            code: code.into(),
            message: message.into(),
            detail: None,
            retryable: false,
        }
    }

    pub fn retryable(mut self) -> Self {
        self.retryable = true;
        self
    }

    pub fn with_detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

/// Marker returned by [`crate::JobControl::checkpoint`] when cancellation
/// was requested. Workers convert this into a clean unwind of their work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl From<Cancelled> for JobError {
    fn from(_: Cancelled) -> Self {
        JobError::new("job/cancelled", "the job was cancelled")
    }
}
