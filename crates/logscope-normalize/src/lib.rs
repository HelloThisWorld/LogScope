//! Normalization and validation for LogScope.
//!
//! Timestamp parsing with explicit timezone assumptions, severity mapping,
//! attribute typing, and shared derivations used by all source adapters
//! (file-based and OTLP).

pub mod attrs;
pub mod severity;
pub mod timestamp;

pub use attrs::{attrs_from_json_object, attrs_from_pairs, derive_display_message};
pub use severity::map_severity_text;
pub use timestamp::{
    parse_timestamp, ParsedTimestamp, TimestampError, TimestampFormat, TimezonePolicy,
};

/// Version of the normalizer. Part of every deterministic record hash.
pub const NORMALIZER_VERSION: &str = "0.0.1";
