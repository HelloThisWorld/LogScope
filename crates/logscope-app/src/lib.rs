//! Application services for LogScope.
//!
//! The typed service layer behind the Tauri command boundary. The desktop
//! shell, future CLI, and Agent API all call these services so UI and
//! automation share exactly the same semantics.

pub mod dto;
pub mod import;

pub use import::{run_import, ImportOutcome, ImportRequest};

/// Product version stamped into workspaces and manifests.
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");
