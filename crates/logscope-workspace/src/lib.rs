//! Workspace and metadata layer for LogScope.
//!
//! Owns the on-disk workspace layout, `manifest.json`, the SQLite metadata
//! database (`workspace.db`), forward-only transactional migrations, and the
//! crash-safe staging/recovery protocol.

pub mod case_meta;
pub mod db;
pub mod error;
pub mod explorer_meta;
pub mod layout;
pub mod manifest;
pub mod meta;
pub mod workspace;

pub use case_meta::{
    HistoryRow, HypothesisRow, InvestigationEdit, InvestigationRow, ItemRow, NewHypothesis,
    NewInvestigation, NewItem, NewScopeRef, ScopeRefRow,
};
pub use error::WorkspaceError;
pub use explorer_meta::{
    ColumnSetRow, ExportJobRow, FieldStatRow, IndexStateRow, RecentSearchRow, SavedSearchRow,
    MAX_RECENT_SEARCHES,
};
pub use layout::WorkspaceLayout;
pub use manifest::Manifest;
pub use meta::{
    DatasetRow, JobRow, LedgerCounts, LedgerEntry, MetaDb, PublishVersions, RejectedRecordRow,
    SegmentRow, SegmentToPublish, Signal, SourceFileRow, SourceRow, MAX_REJECT_EXCERPT_BYTES,
};
pub use workspace::{RecoveryReport, Workspace};
