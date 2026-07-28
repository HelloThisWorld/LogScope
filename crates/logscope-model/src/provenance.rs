//! Ingest provenance: where every canonical record came from, exactly.

use serde::{Deserialize, Serialize};

use crate::hashing::Digest;
use crate::time::{TimestampPrecision, UnixNanos};

/// Physical origin of a record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PhysicalOrigin {
    /// A concrete source file registered in the workspace.
    File {
        file_id: String,
        /// Entry path inside an archive, when the file came from one.
        #[serde(skip_serializing_if = "Option::is_none")]
        archive_entry: Option<String>,
    },
    /// A live OTLP receiver session (experimental in v0.0).
    OtlpSession { session_id: String },
}

/// Position of an OTLP record inside its export request envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct OtlpBatchLocator {
    /// Index of the envelope within the session or file (0-based).
    pub batch_index: u64,
    pub resource_index: u32,
    pub scope_index: u32,
    pub record_index: u32,
}

/// Exact locator of a record within its physical origin. Which fields are
/// populated depends on the source format (row for CSV, line/byte ranges for
/// text and JSONL, JSON pointer for nested documents, batch locator for OTLP).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RecordLocator {
    /// 1-based data-record number (CSV rows, JSONL records).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_number: Option<u64>,
    /// 1-based line range in the source file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u64>,
    /// Byte range [start, end) in the (decompressed) source stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_end: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_pointer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otlp: Option<OtlpBatchLocator>,
}

impl RecordLocator {
    pub fn digest_into(&self, d: &mut Digest) {
        d.str("locator.v1");
        d.opt_u64(self.record_number);
        d.opt_u64(self.line_start);
        d.opt_u64(self.line_end);
        d.opt_u64(self.byte_start);
        d.opt_u64(self.byte_end);
        d.opt_str(self.json_pointer.as_deref());
        match &self.otlp {
            None => {
                d.tag(0);
            }
            Some(o) => {
                d.tag(1)
                    .u64(o.batch_index)
                    .u32(o.resource_index)
                    .u32(o.scope_index)
                    .u32(o.record_index);
            }
        }
    }
}

/// Protocol / content family the record arrived through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceProtocol {
    FileImport,
    OtlpHttpProtobuf,
    OtlpHttpJson,
    OtlpGrpc,
    OtlpJsonlFile,
}

/// Conformance / data-quality flags attached to a record during ingest or
/// derived during analysis. Flags make imperfect input visible instead of
/// silently "fixing" it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "flag", rename_all = "snake_case")]
pub enum QualityFlag {
    /// Source record had no parsable event timestamp.
    TimestampMissing,
    /// Timestamp text could not be parsed with the configured format.
    TimestampUnparsed,
    /// Timestamp had no zone info; the recorded assumption was applied.
    TimezoneAssumed,
    /// Severity text had no known mapping to an OTLP severity number.
    SeverityUnmapped,
    /// Attribute key appeared more than once; the last value won.
    DuplicateAttributeKey { key: String },
    /// OTLP JSON envelope carried fields unknown to this version; the raw
    /// envelope is retained for future reprocessing.
    UnknownFieldsRetained { count: u32 },
    /// Record was assembled from multiple physical lines.
    MultilineContinuationJoined { lines: u32 },
    /// Decoding replaced invalid byte sequences with U+FFFD.
    EncodingReplacementChars { count: u64 },
    /// Record was cut off at end of input (partial final record).
    TruncatedRecord,
    /// Identical record hash already present under the documented policy.
    DuplicateRecord,
    /// Event time is earlier than a preceding record from the same source.
    OutOfOrderTimestamp,
    /// Metric point carried unspecified aggregation temporality.
    MetricTemporalityUnspecified,
    /// Span had no end timestamp in the source.
    SpanMissingEndTime,
    /// Span references a parent that is not present in the dataset.
    SpanOrphanParent,
    /// The same span ID occurs on multiple spans in the trace.
    SpanDuplicateId,
    /// Child appears to start before its parent by more than tolerance.
    ClockSkewSuspected,
    /// The full raw envelope was retained alongside the parsed record.
    RawEnvelopeRetained,
}

/// Complete ingest provenance for one canonical record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestProvenance {
    /// Dataset the record was published into.
    pub dataset_id: String,
    /// Logical source (file set, archive, OTLP session) within the workspace.
    pub logical_source_id: String,
    pub origin: PhysicalOrigin,
    pub locator: RecordLocator,
    pub parser_id: String,
    pub parser_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_version: Option<String>,
    pub normalizer_version: String,
    pub protocol: SourceProtocol,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Wall-clock import/receive time. Never part of deterministic hashes.
    pub ingest_time: UnixNanos,
    /// BLAKE3 hex of the raw record bytes (or OTLP envelope slice).
    pub raw_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_timestamp_precision: Option<TimestampPrecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<QualityFlag>,
}

impl IngestProvenance {
    /// Digests only the deterministic parts of provenance: content identity
    /// (raw hash), position (locator), and the versions of the code that
    /// produced the canonical form. Dataset ID, source IDs, protocol, and
    /// ingest time are intentionally excluded so re-imports of the same
    /// content hash identically.
    pub fn digest_stable_into(&self, d: &mut Digest) {
        d.str("prov.v1");
        d.str(&self.raw_hash);
        self.locator.digest_into(d);
        d.str(&self.parser_id);
        d.str(&self.parser_version);
        d.opt_str(self.profile_id.as_deref());
        d.opt_str(self.profile_version.as_deref());
        d.str(&self.normalizer_version);
    }
}
