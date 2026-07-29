//! Structured workspace errors with stable codes.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace directory already contains a workspace: {0}")]
    AlreadyExists(String),
    #[error("not a workspace (missing or unreadable manifest): {0}")]
    NotAWorkspace(String),
    #[error("workspace manifest version {found} is newer than supported {supported}")]
    ManifestTooNew { found: u32, supported: u32 },
    #[error("workspace schema version {found} is newer than supported {supported}")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("metadata database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("manifest serialization error: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("invalid argument: {0}")]
    Invalid(String),
}

impl WorkspaceError {
    pub fn io(path: impl Into<String>, source: std::io::Error) -> Self {
        WorkspaceError::Io {
            path: path.into(),
            source,
        }
    }

    /// Stable machine-readable code for the Rust/Tauri boundary.
    pub fn code(&self) -> &'static str {
        match self {
            WorkspaceError::AlreadyExists(_) => "workspace/already-exists",
            WorkspaceError::NotAWorkspace(_) => "workspace/not-a-workspace",
            WorkspaceError::ManifestTooNew { .. } => "workspace/manifest-too-new",
            WorkspaceError::SchemaTooNew { .. } => "workspace/schema-too-new",
            WorkspaceError::Db(_) => "workspace/db",
            WorkspaceError::Io { .. } => "workspace/io",
            WorkspaceError::Manifest(_) => "workspace/manifest",
            WorkspaceError::Invalid(_) => "workspace/invalid-argument",
        }
    }
}
