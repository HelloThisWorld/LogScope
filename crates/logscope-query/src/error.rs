//! Structured query-service errors with stable codes.

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("query engine error: {0}")]
    Engine(#[from] duckdb::Error),
    #[error("full-text index error: {0}")]
    Fts(#[from] rusqlite::Error),
    #[error("query was cancelled")]
    Cancelled,
    #[error("query exceeded the configured execution budget")]
    Timeout,
    #[error("invalid query parameter: {0}")]
    InvalidParameter(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl QueryError {
    /// Stable machine-readable code for the Rust/Tauri boundary.
    pub fn code(&self) -> &'static str {
        match self {
            QueryError::Engine(_) => "query/engine",
            QueryError::Fts(_) => "query/fts",
            QueryError::Cancelled => "query/cancelled",
            QueryError::Timeout => "query/timeout",
            QueryError::InvalidParameter(_) => "query/invalid-parameter",
            QueryError::Io(_) => "query/io",
        }
    }

    /// True when the engine reported an interrupt (maps to `Cancelled`).
    pub fn is_interrupt(err: &duckdb::Error) -> bool {
        err.to_string().to_ascii_lowercase().contains("interrupt")
    }
}

/// Serializable error envelope used across the command boundary.
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope {
    pub code: String,
    pub message: String,
}

impl From<&QueryError> for ErrorEnvelope {
    fn from(e: &QueryError) -> Self {
        ErrorEnvelope {
            code: e.code().to_string(),
            message: e.to_string(),
        }
    }
}
