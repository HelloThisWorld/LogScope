//! Storage-layer errors with stable codes.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),
    #[error("fts index error: {0}")]
    Fts(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl StoreError {
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        StoreError::Io {
            path: path.into(),
            source,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            StoreError::Arrow(_) => "store/arrow",
            StoreError::Parquet(_) => "store/parquet",
            StoreError::Fts(_) => "store/fts",
            StoreError::Json(_) => "store/json",
            StoreError::Io { .. } => "store/io",
        }
    }
}
