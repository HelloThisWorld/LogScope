//! EXPERIMENTAL OTLP compatibility spike for LogScope (v0.0).
//!
//! Loopback-only, disabled-by-default receivers for OTLP/HTTP (protobuf and
//! JSON) and OTLP/gRPC, plus OTLP JSONL file import. Not a production Local
//! OTel Session feature; reliable live ingestion is a v0.7 concern.

pub mod convert;
pub mod error;
pub mod jsonl_file;
pub mod receiver;

pub use convert::{
    convert_logs, convert_metrics, convert_traces, ConvertContext, ConvertedBatch, OtlpReject,
};
pub use error::OtlpError;
pub use jsonl_file::{
    read_otlp_jsonl, stream_otlp_jsonl, JsonlEnvelope, JsonlImportResult, JsonlReject,
};
pub use receiver::{
    start, EnvelopeMeta, EnvelopePayload, OtlpReceiverConfig, OtlpReceiverHandle, ReceivedEnvelope,
};

/// Parser identity for OTLP conversion (part of deterministic hashes).
pub const OTLP_PARSER_VERSION: &str = "0.0.1";

/// Re-export so test support and benches build envelopes against the exact
/// schema version this crate consumes.
pub use opentelemetry_proto;
