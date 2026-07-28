//! Canonical, versioned telemetry model for LogScope.
//!
//! This crate defines the draft v0 canonical types for resources, scopes,
//! log records, metric records, span records, and ingest provenance, together
//! with deterministic canonical serialization and stable content-derived IDs.
//!
//! Determinism contract: canonical IDs, record hashes, and normalized output
//! are deterministic across repeated imports of the same source content with
//! the same model / parser / profile / normalizer versions. Wall-clock values
//! (ingest time, observed time for file imports) and workspace-instance IDs
//! (dataset ID, source IDs) are stored but excluded from every hash.

pub mod hashing;
pub mod log;
pub mod metric;
pub mod provenance;
pub mod resource;
pub mod scope;
pub mod severity;
pub mod span;
pub mod time;
pub mod trace_ids;
pub mod value;

pub use hashing::{hash_bytes_hex, stable_id, Digest};
pub use log::LogRecord;
pub use metric::{
    Exemplar, ExpBuckets, ExponentialHistogramPoint, HistogramPoint, MetricData, MetricRecord,
    NumberPoint, NumberValue, PointCommon, QuantileValue, SummaryPoint, Temporality,
};
pub use provenance::{
    IngestProvenance, OtlpBatchLocator, PhysicalOrigin, QualityFlag, RecordLocator, SourceProtocol,
};
pub use resource::{DerivedResource, ResourceDescriptor};
pub use scope::ScopeDescriptor;
pub use span::{SpanEvent, SpanKind, SpanLink, SpanRecord, SpanStatus, StatusCode};
pub use time::{TimestampPrecision, TimezoneAssumption, UnixNanos};
pub use trace_ids::{IdError, SpanId, TraceId};
pub use value::{
    attrs_canonical_json, attrs_from_canonical_json, digest_attrs, AnyValue, AttrMap, ByteValue,
    F64,
};

/// Version of the canonical model. Part of every deterministic record hash.
pub const MODEL_VERSION: &str = "0.0.1";
