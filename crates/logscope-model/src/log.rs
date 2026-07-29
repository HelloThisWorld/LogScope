//! Canonical normalized log record.

use serde::{Deserialize, Serialize};

use crate::hashing::stable_id;
use crate::provenance::IngestProvenance;
use crate::time::{TimezoneAssumption, UnixNanos};
use crate::trace_ids::{SpanId, TraceId};
use crate::value::{digest_attrs, AnyValue, AttrMap};
use crate::MODEL_VERSION;

/// A fully normalized log record.
///
/// `observed_time` is the time the collection system (importer or receiver)
/// first saw the record; for file imports it is wall-clock and therefore
/// **excluded** from the deterministic record hash, as is everything else
/// that varies between identical re-imports (dataset ID, source IDs,
/// ingest time).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    /// `log-<32 hex>`: deterministic content hash, see [`LogRecord::compute_record_id`].
    pub record_id: String,
    /// Event timestamp (UTC nanos). Absent when the source had none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_time: Option<UnixNanos>,
    /// When the collection system observed the record.
    pub observed_time: UnixNanos,
    /// Exact timestamp text from the source, if textual.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_timestamp_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone_assumption: Option<TimezoneAssumption>,
    /// Severity text exactly as it appeared in the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_text: Option<String>,
    /// OTLP severity number (1..=24) when known/mapped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_number: Option<i32>,
    /// Typed body (string for text logs, map for structured bodies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<AnyValue>,
    /// Derived single-line display message.
    pub display_message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<TraceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<SpanId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_flags: Option<u32>,
    pub resource_id: String,
    pub scope_id: String,
    // Generic optional correlation fields (populated by profiles, never by
    // organization-specific logic in core).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<String>,
    /// Complete typed attributes (every unmapped source field lands here).
    pub attributes: AttrMap,
    #[serde(default)]
    pub dropped_attributes_count: u32,
    pub provenance: IngestProvenance,
}

impl LogRecord {
    /// Computes the deterministic record ID from canonical content plus the
    /// stable provenance parts (raw hash, locator, parser/profile/normalizer
    /// versions, model version). Repeated imports of the same source with the
    /// same versions produce identical IDs.
    pub fn compute_record_id(&self) -> String {
        stable_id("log", |d| {
            d.str("log.v1");
            d.str(MODEL_VERSION);
            d.opt_i64(self.event_time.map(|t| t.0));
            d.opt_str(self.original_timestamp_text.as_deref());
            d.opt_str(self.severity_text.as_deref());
            d.opt_i32(self.severity_number);
            match &self.body {
                None => {
                    d.tag(0);
                }
                Some(b) => {
                    d.tag(1);
                    b.digest_into(d);
                }
            }
            d.str(&self.display_message);
            d.opt_str(self.event_name.as_deref());
            d.opt_str(self.trace_id.as_ref().map(|t| t.as_str()));
            d.opt_str(self.span_id.as_ref().map(|s| s.as_str()));
            d.opt_u32(self.trace_flags);
            d.str(&self.resource_id);
            d.str(&self.scope_id);
            d.opt_str(self.operation.as_deref());
            d.opt_str(self.outcome.as_deref());
            d.opt_str(self.event_type.as_deref());
            d.opt_str(self.request_id.as_deref());
            d.opt_str(self.transaction_id.as_deref());
            d.opt_str(self.message_id.as_deref());
            d.opt_str(self.entity_id.as_deref());
            digest_attrs(&self.attributes, d);
            d.u32(self.dropped_attributes_count);
            self.provenance.digest_stable_into(d);
        })
    }

    /// Fills `record_id` from content (call after all fields are final).
    pub fn seal(mut self) -> Self {
        self.record_id = self.compute_record_id();
        self
    }
}
