//! Evidence envelope v1: the versioned, typed contract between pinned
//! evidence and the canonical workspace.
//!
//! Every evidence item stores two separate payloads:
//! - a **live reference** ([`EvidenceReference`]) used to re-resolve
//!   current canonical data and navigate back to the Explorer;
//! - a **captured snapshot** ([`EvidenceSnapshot`]) — a bounded record of
//!   what was visible at pin time, kept readable even if the original
//!   source disappears. It is never a full query result or source copy,
//!   and verification never rewrites it.
//!
//! Decoding is gated by the stored `envelope_version`: payloads written
//! by a newer schema are refused (`UnsupportedReferenceVersion`) instead
//! of being reinterpreted. Future kinds (comparison, deterministic
//! finding, metric point/range, span, trace) arrive as new envelope
//! versions without changing what v1 log evidence means.

use serde::{Deserialize, Serialize};

use crate::{CaseError, EVIDENCE_ENVELOPE_VERSION};

/// Hard bound on ids captured in a bounded selection.
pub const MAX_SELECTION_IDS: usize = 500;
/// Bound on representative canonical ids stored for query/group/interval
/// evidence.
pub const MAX_REPRESENTATIVE_IDS: usize = 20;
/// Bound on rows captured inside any snapshot.
pub const MAX_SNAPSHOT_ROWS: usize = 50;
/// Bound on one captured field/message excerpt (bytes of UTF-8).
pub const MAX_SNAPSHOT_FIELD_BYTES: usize = 4096;
/// Bound on the whole serialized snapshot JSON (bytes).
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;

/// Dataset identity plus the revision fingerprint captured at pin time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetRevRef {
    pub dataset_id: String,
    /// `dsrev-<hex>` fingerprint of the dataset's published segment set.
    pub dataset_revision: String,
}

/// Result-count knowledge captured with query-shaped evidence. Distinct
/// states stay distinct; an unknown count is never presented as exact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CountState {
    Exact {
        count: i64,
    },
    Approximate {
        count: i64,
    },
    /// Counting stopped at a documented bound.
    Bounded {
        at_least: i64,
    },
    Unknown,
}

/// The exact query context a pin was made from: the authoritative
/// language text plus the concrete time bounds that were in effect.
/// `resolved_start/end` are the half-open UTC-nanos bounds computed at
/// pin time — a relative strategy is always pinned to concrete instants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryContext {
    pub query_text: String,
    pub language_version: i64,
    /// `qry-<hex>` canonical AST fingerprint, when the query validated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub dataset_ids: Vec<String>,
    /// Original strategy as JSON (`all` / `absolute` / `relative_to_latest`).
    pub time_strategy_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_end: Option<i64>,
    /// Records excluded because they carry no timestamp (bounded windows
    /// exclude them); kept so drift checks compare like with like.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub omitted_untimestamped: Option<i64>,
}

/// Live reference payloads, one per evidence kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceReference {
    Event(EventRef),
    Selection(SelectionRef),
    Query(QueryRef),
    ExplorerGroup(GroupRef),
    HistogramInterval(IntervalRef),
    ItemRef(ItemReference),
}

/// One canonical log event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRef {
    /// Deterministic canonical id (`log-<32hex>`).
    pub record_id: String,
    pub dataset_id: String,
    pub dataset_revision: String,
    /// Segment the record lived in at pin time (direct resolution aid).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment_id: Option<String>,
    /// Registered source file and its captured full-file BLAKE3 hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_content_hash: Option<String>,
    /// RecordLocator JSON (line/byte/record position within the source).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_locator_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_version: Option<String>,
    pub parser_id: String,
    pub parser_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_time: Option<i64>,
    /// Timestamp-quality flags copied from ingest provenance (names).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timestamp_quality: Vec<String>,
}

/// A bounded, ordered multi-row selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionRef {
    /// Ordered stable canonical ids (bounded by [`MAX_SELECTION_IDS`]).
    pub record_ids: Vec<String>,
    pub datasets: Vec<DatasetRevRef>,
    /// The query/time/dataset context the selection was made from.
    pub context: QueryContext,
    /// How many rows the user had selected (may exceed what was captured).
    pub selected_count: u32,
    pub max_allowed: u32,
    /// True when `record_ids` is a truncated prefix of the selection.
    pub truncated: bool,
}

/// A saved or ad hoc query result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRef {
    pub context: QueryContext,
    pub datasets: Vec<DatasetRevRef>,
    /// Present when pinned from a saved search (resolution reports if the
    /// saved definition later changes or disappears; it never substitutes
    /// a same-name query).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_search_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_search_fingerprint: Option<String>,
    /// Documented result order (the keyset triple).
    pub sort: String,
    pub count: CountState,
    /// Bounded representative canonical ids (first page / user-chosen).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub representative_ids: Vec<String>,
}

/// A visible facet / field-distribution group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRef {
    /// Base query context the facet was computed over.
    pub context: QueryContext,
    pub datasets: Vec<DatasetRevRef>,
    /// Grouping field (canonical or attr display identity).
    pub field: String,
    /// The visible group value, exactly as grouped (JSON-encoded scalar;
    /// `null` represents the missing-value group).
    pub value_json: String,
    /// The query-language predicate that selects this group, produced by
    /// the authoritative serializer.
    pub predicate_text: String,
    pub count: CountState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub representative_ids: Vec<String>,
}

/// One histogram interval selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntervalRef {
    pub context: QueryContext,
    pub datasets: Vec<DatasetRevRef>,
    /// Half-open interval [start, end) in UTC nanos — exact boundary
    /// semantics, matching every other time bound in the product.
    pub start: i64,
    pub end: i64,
    pub bucket_width_nanos: i64,
    /// Display timezone the user was looking at (IANA name).
    pub display_timezone: String,
    pub count: CountState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub representative_ids: Vec<String>,
}

/// A manual note/finding item captured as evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemReference {
    pub item_id: String,
    /// Item revision captured at pin time.
    pub item_revision: i64,
}

// ---- captured snapshots ------------------------------------------------------

/// Bounded captured snapshots, one shape per evidence kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceSnapshot {
    Event(EventSnapshot),
    Selection(SelectionSnapshot),
    Query(QuerySummarySnapshot),
    ExplorerGroup(GroupSnapshot),
    HistogramInterval(IntervalSnapshot),
    ItemRef(ItemSnapshot),
}

/// One bounded captured row: the display fields visible at pin time.
/// Values are pre-truncated to [`MAX_SNAPSHOT_FIELD_BYTES`], with the
/// truncation made explicit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRow {
    pub record_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_number: Option<i32>,
    pub display_message: String,
    pub display_message_truncated: bool,
    /// Selected display fields (name -> display-safe value excerpt).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<SnapshotField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotField {
    pub name: String,
    pub value: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSnapshot {
    pub row: SnapshotRow,
    /// Bounded raw source excerpt when it was visible at pin time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_excerpt: Option<String>,
    pub raw_excerpt_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSnapshot {
    /// Bounded captured rows (≤ [`MAX_SNAPSHOT_ROWS`]); may be fewer than
    /// the referenced ids.
    pub rows: Vec<SnapshotRow>,
    pub rows_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuerySummarySnapshot {
    pub count: CountState,
    /// Wall-clock execution duration observed at pin time (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    pub rows: Vec<SnapshotRow>,
    pub rows_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSnapshot {
    pub field: String,
    pub value_json: String,
    pub count: CountState,
    /// Share of the base result the group represented at pin time, in
    /// basis points (0..=10000), when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub share_bp: Option<i32>,
    pub rows: Vec<SnapshotRow>,
    pub rows_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntervalSnapshot {
    pub count: CountState,
    /// Neighbor buckets captured for context: (bucket_start, count).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub neighbor_buckets: Vec<(i64, i64)>,
    pub rows: Vec<SnapshotRow>,
    pub rows_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemSnapshot {
    pub item_kind: String,
    /// Item content exactly as it read at pin time (bounded).
    pub content: String,
    pub content_truncated: bool,
}

// ---- encode / decode / validate ----------------------------------------------

/// Serializes a reference for storage.
pub fn encode_reference(r: &EvidenceReference) -> Result<String, CaseError> {
    serde_json::to_string(r).map_err(|e| CaseError::Invalid(format!("encode reference: {e}")))
}

/// Serializes a snapshot for storage, enforcing the global size bound.
pub fn encode_snapshot(s: &EvidenceSnapshot) -> Result<String, CaseError> {
    let json = serde_json::to_string(s)
        .map_err(|e| CaseError::Invalid(format!("encode snapshot: {e}")))?;
    if json.len() > MAX_SNAPSHOT_BYTES {
        return Err(CaseError::Invalid(format!(
            "snapshot exceeds {MAX_SNAPSHOT_BYTES} bytes ({}); bound the capture first",
            json.len()
        )));
    }
    Ok(json)
}

/// Decode outcome for stored payloads: newer versions are refused, and a
/// v1 payload that does not parse is reported as undecodable rather than
/// silently dropped.
#[derive(Debug)]
pub enum DecodeOutcome<T> {
    Decoded(T),
    /// Stored by a newer build; must not be reinterpreted.
    UnsupportedVersion {
        stored: i64,
        supported: i64,
    },
    /// Version is supported but the payload does not parse.
    Undecodable {
        error: String,
    },
}

pub fn decode_reference(envelope_version: i64, json: &str) -> DecodeOutcome<EvidenceReference> {
    decode_versioned(envelope_version, json)
}

pub fn decode_snapshot(envelope_version: i64, json: &str) -> DecodeOutcome<EvidenceSnapshot> {
    decode_versioned(envelope_version, json)
}

fn decode_versioned<T: serde::de::DeserializeOwned>(
    envelope_version: i64,
    json: &str,
) -> DecodeOutcome<T> {
    if envelope_version > EVIDENCE_ENVELOPE_VERSION {
        return DecodeOutcome::UnsupportedVersion {
            stored: envelope_version,
            supported: EVIDENCE_ENVELOPE_VERSION,
        };
    }
    match serde_json::from_str(json) {
        Ok(v) => DecodeOutcome::Decoded(v),
        Err(e) => DecodeOutcome::Undecodable {
            error: e.to_string(),
        },
    }
}

/// Structural bounds every reference must satisfy before storage.
pub fn validate_reference(r: &EvidenceReference) -> Result<(), CaseError> {
    fn check_repr(ids: &[String]) -> Result<(), CaseError> {
        if ids.len() > MAX_REPRESENTATIVE_IDS {
            return Err(CaseError::Invalid(format!(
                "representative ids exceed the bound of {MAX_REPRESENTATIVE_IDS}"
            )));
        }
        Ok(())
    }
    match r {
        EvidenceReference::Event(e) => {
            if e.record_id.is_empty() || e.dataset_id.is_empty() || e.dataset_revision.is_empty() {
                return Err(CaseError::Invalid(
                    "event reference requires record, dataset, and dataset revision".into(),
                ));
            }
            Ok(())
        }
        EvidenceReference::Selection(s) => {
            if s.record_ids.is_empty() {
                return Err(CaseError::Invalid("selection has no record ids".into()));
            }
            if s.record_ids.len() > MAX_SELECTION_IDS {
                return Err(CaseError::Invalid(format!(
                    "selection captures {} ids; the bound is {MAX_SELECTION_IDS}",
                    s.record_ids.len()
                )));
            }
            if (s.record_ids.len() as u32) < s.selected_count && !s.truncated {
                return Err(CaseError::Invalid(
                    "a partial capture must set the explicit truncation state".into(),
                ));
            }
            Ok(())
        }
        EvidenceReference::Query(q) => check_repr(&q.representative_ids),
        EvidenceReference::ExplorerGroup(g) => check_repr(&g.representative_ids),
        EvidenceReference::HistogramInterval(i) => {
            if i.end <= i.start {
                return Err(CaseError::Invalid(
                    "interval must be half-open with end > start".into(),
                ));
            }
            check_repr(&i.representative_ids)
        }
        EvidenceReference::ItemRef(i) => {
            if i.item_id.is_empty() {
                return Err(CaseError::Invalid(
                    "item reference requires an item id".into(),
                ));
            }
            Ok(())
        }
    }
}

/// Truncates a captured value to the per-field bound at a char boundary,
/// reporting whether truncation happened.
pub fn bound_field(value: &str) -> (String, bool) {
    if value.len() <= MAX_SNAPSHOT_FIELD_BYTES {
        return (value.to_string(), false);
    }
    let mut end = MAX_SNAPSHOT_FIELD_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> QueryContext {
        QueryContext {
            query_text: "severity:ERROR".into(),
            language_version: 1,
            fingerprint: Some("qry-abc".into()),
            dataset_ids: vec!["ds-1".into()],
            time_strategy_json: "{\"kind\":\"all\"}".into(),
            resolved_start: None,
            resolved_end: Some(1_700_000_000_000_000_000),
            omitted_untimestamped: Some(3),
        }
    }

    #[test]
    fn reference_round_trips_through_json() {
        let r = EvidenceReference::Query(QueryRef {
            context: ctx(),
            datasets: vec![DatasetRevRef {
                dataset_id: "ds-1".into(),
                dataset_revision: "dsrev-1".into(),
            }],
            saved_search_id: None,
            saved_search_fingerprint: None,
            sort: "event_time DESC NULLS LAST, record_id DESC, dataset_id DESC".into(),
            count: CountState::Exact { count: 42 },
            representative_ids: vec!["log-a".into()],
        });
        let json = encode_reference(&r).unwrap();
        match decode_reference(1, &json) {
            DecodeOutcome::Decoded(back) => assert_eq!(back, r),
            other => panic!("expected decode, got {other:?}"),
        }
    }

    #[test]
    fn newer_envelope_versions_are_refused_not_reinterpreted() {
        let json = "{\"kind\":\"telepathy\",\"anything\":1}";
        match decode_reference(EVIDENCE_ENVELOPE_VERSION + 1, json) {
            DecodeOutcome::UnsupportedVersion { stored, supported } => {
                assert_eq!(stored, EVIDENCE_ENVELOPE_VERSION + 1);
                assert_eq!(supported, EVIDENCE_ENVELOPE_VERSION);
            }
            other => panic!("expected unsupported version, got {other:?}"),
        }
        // Supported version + garbage = undecodable, reported, not dropped.
        match decode_reference(1, "not json") {
            DecodeOutcome::Undecodable { .. } => {}
            other => panic!("expected undecodable, got {other:?}"),
        }
    }

    #[test]
    fn selection_bounds_and_truncation_are_enforced() {
        let mut sel = SelectionRef {
            record_ids: (0..10).map(|i| format!("log-{i}")).collect(),
            datasets: vec![],
            context: ctx(),
            selected_count: 25,
            max_allowed: 500,
            truncated: false,
        };
        // Partial capture without the explicit truncation state is invalid.
        assert!(validate_reference(&EvidenceReference::Selection(sel.clone())).is_err());
        sel.truncated = true;
        assert!(validate_reference(&EvidenceReference::Selection(sel.clone())).is_ok());
        // The hard id bound is enforced.
        sel.record_ids = (0..=MAX_SELECTION_IDS)
            .map(|i| format!("log-{i}"))
            .collect();
        assert!(validate_reference(&EvidenceReference::Selection(sel)).is_err());
    }

    #[test]
    fn snapshot_size_bound_is_enforced() {
        let big = "x".repeat(MAX_SNAPSHOT_BYTES);
        let snap = EvidenceSnapshot::ItemRef(ItemSnapshot {
            item_kind: "note".into(),
            content: big,
            content_truncated: false,
        });
        assert!(encode_snapshot(&snap).is_err());

        let (bounded, truncated) = bound_field(&"y".repeat(MAX_SNAPSHOT_FIELD_BYTES + 10));
        assert!(truncated);
        assert_eq!(bounded.len(), MAX_SNAPSHOT_FIELD_BYTES);
    }

    #[test]
    fn interval_boundaries_are_half_open_and_validated() {
        let bad = EvidenceReference::HistogramInterval(IntervalRef {
            context: ctx(),
            datasets: vec![],
            start: 10,
            end: 10,
            bucket_width_nanos: 5,
            display_timezone: "UTC".into(),
            count: CountState::Unknown,
            representative_ids: vec![],
        });
        assert!(validate_reference(&bad).is_err());
    }
}
