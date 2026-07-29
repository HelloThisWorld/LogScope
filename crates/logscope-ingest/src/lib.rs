//! Source adapters and Import Profiles for LogScope.
//!
//! Streaming, bounded readers for CSV and JSONL (v0.0 proof scope) that
//! produce canonical batches with exact source locators, plus the versioned
//! draft contracts for logical sources and declarative Import Profiles.

pub mod error;
pub mod log_normalizer;
pub mod profile;
pub mod reader;
pub mod source;

pub use error::IngestError;
pub use log_normalizer::{normalize_log, reject_from_malformed, NormalizeContext, NormalizeReject};
pub use profile::{builtin, FieldRef, FormatSpec, ImportProfile, TimestampRule};
pub use reader::{
    CsvReader, JsonlReader, MalformedRecord, ParsedFields, ParsedRecord, ReadItem, RecordReader,
};
pub use source::{
    fingerprint_file, CompatibilityTier, IngestCheckpoint, LogicalSourceSpec, ParserManifest,
    PhysicalSourceIdentity, SignalClassification, SOURCE_CONTRACT_VERSION,
};

/// Parser identifiers/versions for the built-in v0.0 readers.
pub const CSV_PARSER_ID: &str = "csv";
pub const JSONL_PARSER_ID: &str = "jsonl";
pub const PARSER_VERSION: &str = "0.0.1";
