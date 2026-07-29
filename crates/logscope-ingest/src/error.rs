//! Ingest errors with stable codes.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("unsupported or undetectable format: {0}")]
    UnsupportedFormat(String),
    #[error("invalid profile: {0}")]
    InvalidProfile(String),
    #[error("archive limit exceeded: {0}")]
    ArchiveLimit(String),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

impl IngestError {
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        IngestError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            IngestError::Io { .. } => "ingest/io",
            IngestError::UnsupportedFormat(_) => "ingest/unsupported-format",
            IngestError::InvalidProfile(_) => "ingest/invalid-profile",
            IngestError::ArchiveLimit(_) => "ingest/archive-limit",
            IngestError::Json(_) => "ingest/json",
        }
    }
}
