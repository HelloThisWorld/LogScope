//! Canonical normalized span record.
//!
//! The derived trace representation is a causal graph: parent-child edges
//! form the primary tree and links are first-class edges. Reconstruction
//! (in the query service) annotates orphan/missing-root/duplicate/sampled/
//! incomplete/out-of-order/clock-skew conditions and never fabricates spans.

use serde::{Deserialize, Serialize};

use crate::hashing::stable_id;
use crate::provenance::IngestProvenance;
use crate::time::UnixNanos;
use crate::trace_ids::{SpanId, TraceId};
use crate::value::{digest_attrs, AttrMap};
use crate::MODEL_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Unspecified,
    Internal,
    Server,
    Client,
    Producer,
    Consumer,
}

impl SpanKind {
    fn digest_tag(self) -> u8 {
        match self {
            SpanKind::Unspecified => 0,
            SpanKind::Internal => 1,
            SpanKind::Server => 2,
            SpanKind::Client => 3,
            SpanKind::Producer => 4,
            SpanKind::Consumer => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCode {
    Unset,
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanStatus {
    pub code: StatusCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanEvent {
    pub time: UnixNanos,
    pub name: String,
    pub attributes: AttrMap,
    #[serde(default)]
    pub dropped_attributes_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanLink {
    pub trace_id: TraceId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span_id: Option<SpanId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_state: Option<String>,
    pub attributes: AttrMap,
    #[serde(default)]
    pub dropped_attributes_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanRecord {
    /// `spn-<32 hex>` deterministic content hash.
    pub record_id: String,
    pub trace_id: TraceId,
    pub span_id: SpanId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_state: Option<String>,
    /// W3C trace flags + OTLP span flags word, exactly as received.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,
    pub name: String,
    pub kind: SpanKind,
    pub start_time: UnixNanos,
    /// Absent when the source never recorded an end (flagged, not invented).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_time: Option<UnixNanos>,
    pub status: SpanStatus,
    pub resource_id: String,
    pub scope_id: String,
    pub attributes: AttrMap,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SpanEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<SpanLink>,
    #[serde(default)]
    pub dropped_attributes_count: u32,
    #[serde(default)]
    pub dropped_events_count: u32,
    #[serde(default)]
    pub dropped_links_count: u32,
    pub provenance: IngestProvenance,
}

impl SpanRecord {
    /// End minus start, when an end time exists. Negative durations are
    /// preserved (they are evidence of clock problems, not data errors).
    pub fn duration_nanos(&self) -> Option<i64> {
        self.end_time.map(|e| e.0 - self.start_time.0)
    }

    pub fn compute_record_id(&self) -> String {
        stable_id("spn", |d| {
            d.str("span.v1");
            d.str(MODEL_VERSION);
            d.str(self.trace_id.as_str());
            d.str(self.span_id.as_str());
            d.opt_str(self.parent_span_id.as_ref().map(|s| s.as_str()));
            d.opt_str(self.trace_state.as_deref());
            d.opt_u32(self.flags);
            d.str(&self.name);
            d.u8(self.kind.digest_tag());
            d.i64(self.start_time.0);
            d.opt_i64(self.end_time.map(|t| t.0));
            d.u8(match self.status.code {
                StatusCode::Unset => 0,
                StatusCode::Ok => 1,
                StatusCode::Error => 2,
            });
            d.opt_str(self.status.message.as_deref());
            d.str(&self.resource_id);
            d.str(&self.scope_id);
            digest_attrs(&self.attributes, d);
            d.u64(self.events.len() as u64);
            for e in &self.events {
                d.i64(e.time.0).str(&e.name);
                digest_attrs(&e.attributes, d);
                d.u32(e.dropped_attributes_count);
            }
            d.u64(self.links.len() as u64);
            for l in &self.links {
                d.str(l.trace_id.as_str());
                d.opt_str(l.span_id.as_ref().map(|s| s.as_str()));
                d.opt_str(l.trace_state.as_deref());
                digest_attrs(&l.attributes, d);
                d.u32(l.dropped_attributes_count);
                d.opt_u32(l.flags);
            }
            d.u32(self.dropped_attributes_count);
            d.u32(self.dropped_events_count);
            d.u32(self.dropped_links_count);
            self.provenance.digest_stable_into(d);
        })
    }

    pub fn seal(mut self) -> Self {
        self.record_id = self.compute_record_id();
        self
    }
}
