//! Application services for LogScope.
//!
//! The typed service layer behind the Tauri command boundary. The desktop
//! shell, future CLI, and Agent API all call these services so UI and
//! automation share exactly the same semantics.

pub mod dto;
pub mod explorer;
pub mod export;
pub mod import;

pub use export::{
    run_export, ExportFormat, ExportOutcome, ExportSpec, DEFAULT_CSV_COLUMNS, DEFAULT_EXPORT_BYTES,
    DEFAULT_EXPORT_ROWS, MAX_EXPORT_BYTES, MAX_EXPORT_ROWS,
};
pub use import::{run_import, ImportOutcome, ImportRequest};

/// Product version stamped into workspaces and manifests.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
