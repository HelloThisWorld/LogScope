//! Workspace and metadata layer for LogScope.
//!
//! Owns the on-disk workspace layout, `manifest.json`, the SQLite metadata
//! database (`workspace.db`), forward-only transactional migrations, and the
//! crash-safe staging/recovery protocol.

pub mod db;
pub mod error;
pub mod layout;
pub mod manifest;
pub mod meta;
pub mod workspace;

pub use error::WorkspaceError;
pub use layout::WorkspaceLayout;
pub use manifest::Manifest;
pub use meta::{
    DatasetRow, JobRow, LedgerCounts, LedgerEntry, MetaDb, PublishVersions, RejectedRecordRow,
    SegmentRow, SegmentToPublish, Signal, SourceFileRow, SourceRow, MAX_REJECT_EXCERPT_BYTES,
};
pub use workspace::{RecoveryReport, Workspace};
