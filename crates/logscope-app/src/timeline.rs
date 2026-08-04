//! Investigation timeline read model.
//!
//! Merges manual markers and pinned evidence into one deterministic
//! chronology with an explicit undated section. Nothing is inferred: an
//! entry's instant comes from a documented source (`time_source`), and
//! anything without a trustworthy instant lands in the undated section
//! with the reason stated — never silently dropped, never guessed.

use logscope_case::envelope::{self, DecodeOutcome, EvidenceReference, QueryContext};
use logscope_jobs::JobError;
use logscope_workspace::Workspace;
use serde::Serialize;

/// One merged timeline entry.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    /// `marker` or `evidence`.
    pub entry_kind: String,
    /// Marker id or evidence id.
    pub id: String,
    /// Marker kind (`deployment|config_change|operator_action|custom`)
    /// or evidence kind (`event|selection|query|...`).
    pub detail_kind: String,
    pub title: String,
    /// UTC instant; `None` = undated section.
    pub at_nanos: Option<i64>,
    /// Interval end (exclusive) for interval-shaped entries.
    pub end_nanos: Option<i64>,
    /// Where the instant came from: `marker` | `event_time` |
    /// `interval_bounds` | `resolved_window` | `none`.
    pub time_source: String,
    /// Stated reason when the entry is undated.
    pub undated_reason: Option<String>,
    /// Marker annotations preserved exactly as entered.
    pub description: Option<String>,
    pub original_time_text: Option<String>,
    pub original_tz_offset_min: Option<i64>,
}

/// The merged timeline: dated entries in deterministic order, then the
/// explicit undated section.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineModel {
    pub dated: Vec<TimelineEntry>,
    pub undated: Vec<TimelineEntry>,
    /// Archived markers/evidence excluded from this view (count only —
    /// they remain fully readable in their own panels).
    pub archived_excluded: i64,
}

fn ws_err(e: logscope_workspace::WorkspaceError) -> JobError {
    JobError::new(e.code(), e.to_string())
}

/// Parses a user-entered marker timestamp (RFC 3339, offset or `Z`).
/// Returns the UTC instant in nanoseconds and the original zone offset
/// in minutes — the offset is preserved so the marker can always be
/// shown as the user wrote it.
pub fn parse_marker_time(text: &str) -> Result<(i64, i64), JobError> {
    let dt = chrono::DateTime::parse_from_rfc3339(text.trim()).map_err(|e| {
        JobError::new(
            "case/invalid-timestamp",
            format!(
                "not an RFC 3339 timestamp: {e} \
                 (expected e.g. 2026-08-04T10:00:00Z or 2026-08-04T12:00:00+02:00)"
            ),
        )
    })?;
    let nanos = dt.timestamp_nanos_opt().ok_or_else(|| {
        JobError::new(
            "case/invalid-timestamp",
            "timestamp is outside the representable range",
        )
    })?;
    Ok((nanos, i64::from(dt.offset().local_minus_utc() / 60)))
}

/// Instant/interval derivation from a decoded reference's query context:
/// a scope only counts as time-bounded when both resolved bounds were
/// captured at pin time.
fn from_context(ctx: &QueryContext) -> (Option<i64>, Option<i64>, &'static str, Option<String>) {
    match (ctx.resolved_start, ctx.resolved_end) {
        (Some(s), Some(e)) => (Some(s), Some(e), "resolved_window", None),
        _ => (
            None,
            None,
            "none",
            Some("scope was unbounded (all time) at pin time".to_string()),
        ),
    }
}

/// Builds the merged timeline for one investigation.
pub fn timeline(ws: &Workspace, investigation_id: &str) -> Result<TimelineModel, JobError> {
    // Refuses unknown investigations with the standard error.
    ws.meta
        .get_investigation(investigation_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("investigation {investigation_id} does not exist"),
            )
        })?;

    let mut entries: Vec<TimelineEntry> = Vec::new();
    let mut archived_excluded = 0i64;

    let all_markers = ws
        .meta
        .list_markers(investigation_id, true)
        .map_err(ws_err)?;
    for m in all_markers {
        if m.archived {
            archived_excluded += 1;
            continue;
        }
        entries.push(TimelineEntry {
            entry_kind: "marker".into(),
            id: m.marker_id,
            detail_kind: m.kind,
            title: m.label,
            at_nanos: m.at_nanos,
            end_nanos: m.end_nanos,
            time_source: if m.at_nanos.is_some() {
                "marker"
            } else {
                "none"
            }
            .into(),
            undated_reason: if m.at_nanos.is_some() {
                None
            } else {
                Some("marker was entered without a timestamp".into())
            },
            description: m.description,
            original_time_text: m.original_time_text,
            original_tz_offset_min: m.original_tz_offset_min,
        });
    }

    let all_evidence = ws
        .meta
        .list_evidence(investigation_id, true)
        .map_err(ws_err)?;
    for ev in all_evidence {
        if ev.archived {
            archived_excluded += 1;
            continue;
        }
        let (at, end, source, reason): (Option<i64>, Option<i64>, &str, Option<String>) =
            match envelope::decode_reference(ev.envelope_version, &ev.reference_json) {
                DecodeOutcome::Decoded(reference) => match reference {
                    EvidenceReference::Event(e) => match e.event_time {
                        Some(t) => (Some(t), None, "event_time", None),
                        None => (
                            None,
                            None,
                            "none",
                            Some("the pinned event has no timestamp".into()),
                        ),
                    },
                    EvidenceReference::HistogramInterval(i) => {
                        (Some(i.start), Some(i.end), "interval_bounds", None)
                    }
                    EvidenceReference::Selection(s) => from_context(&s.context),
                    EvidenceReference::Query(q) => from_context(&q.context),
                    EvidenceReference::ExplorerGroup(g) => from_context(&g.context),
                    EvidenceReference::ItemRef(_) => (
                        None,
                        None,
                        "none",
                        Some("manual item — carries no event time".into()),
                    ),
                },
                DecodeOutcome::UnsupportedVersion { stored, supported } => (
                    None,
                    None,
                    "none",
                    Some(format!(
                        "reference unreadable: envelope {stored} (supported {supported})"
                    )),
                ),
                DecodeOutcome::Undecodable { .. } => {
                    (None, None, "none", Some("reference unreadable".into()))
                }
            };
        entries.push(TimelineEntry {
            entry_kind: "evidence".into(),
            id: ev.evidence_id,
            detail_kind: ev.kind,
            title: ev.title,
            at_nanos: at,
            end_nanos: end,
            time_source: source.into(),
            undated_reason: reason,
            description: ev.annotation,
            original_time_text: None,
            original_tz_offset_min: None,
        });
    }

    // Deterministic order. Dated: by instant, then bounded-before-open at
    // the same instant (an instant sorts before an interval starting
    // there), then id as the final total-order tiebreak. Undated: by id —
    // stable and independent of insertion order.
    let (mut dated, mut undated): (Vec<_>, Vec<_>) =
        entries.into_iter().partition(|e| e.at_nanos.is_some());
    dated.sort_by(|a, b| {
        (a.at_nanos, a.end_nanos.is_some(), a.end_nanos, &a.id).cmp(&(
            b.at_nanos,
            b.end_nanos.is_some(),
            b.end_nanos,
            &b.id,
        ))
    });
    undated.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(TimelineModel {
        dated,
        undated,
        archived_excluded,
    })
}
