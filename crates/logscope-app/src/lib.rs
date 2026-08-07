//! Application services for LogScope.
//!
//! The typed service layer behind the Tauri command boundary. The desktop
//! shell, future CLI, and Agent API all call these services so UI and
//! automation share exactly the same semantics.

pub mod analysis;
pub mod bundle;
pub mod case;
pub mod comparison;
pub mod correlation;
pub mod dto;
pub mod explorer;
pub mod export;
pub mod import;
pub mod patterns;
pub mod redact;
pub mod report;
pub mod timeline;

pub use case::{
    dataset_revision, pin_event, pin_group, pin_interval, pin_item, pin_query, pin_selection,
    verify_evidence, EvidenceOutcome, PinCommon, PinEventRequest, PinGroupRequest,
    PinIntervalRequest, PinItemRequest, PinQueryRequest, PinSelectionRequest, QueryScope,
    VerificationReport,
};
pub use export::{
    run_export, ExportFormat, ExportOutcome, ExportSpec, DEFAULT_CSV_COLUMNS, DEFAULT_EXPORT_BYTES,
    DEFAULT_EXPORT_ROWS, MAX_EXPORT_BYTES, MAX_EXPORT_ROWS,
};
pub use import::{run_import, ImportOutcome, ImportRequest};

/// Product version stamped into workspaces and manifests.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
