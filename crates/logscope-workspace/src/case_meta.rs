//! Metadata repositories for the v0.3 investigation workbench control
//! plane (investigations, scope refs, hypotheses, typed items, and the
//! shared non-destructive history ledger).
//!
//! Contract:
//! - Every mutation runs in one transaction, bumps the entity `revision`,
//!   and inserts a `case_history` row (full post-action payload) in that
//!   same transaction — prior meaning is always retrievable.
//! - Updates are optimistic: callers pass the revision they last saw; a
//!   mismatch returns `WorkspaceError::StaleRevision` (structured
//!   conflict), never silent last-write-wins.
//! - Enum-valued columns store service-validated strings; the typed
//!   vocabulary lives in `logscope-case` above this layer.
//! - No OS identity is captured anywhere. `owner_text` is exactly what
//!   the user typed, or NULL.
//! - There is no hard-delete API for investigations, hypotheses, items,
//!   or evidence: normal removal archives (with a history record).
//!   Scope refs — plain pointers — may be removed, with their final
//!   payload retained in history.

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::error::WorkspaceError;
use crate::meta::MetaDb;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ---- rows -----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvestigationRow {
    pub investigation_id: String,
    pub entity_version: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub severity: Option<String>,
    pub owner_text: Option<String>,
    pub tags_json: String,
    pub created_at: String,
    pub updated_at: String,
    pub status_changed_at: Option<String>,
    pub incident_started_at: Option<i64>,
    pub mitigated_at: Option<i64>,
    pub resolved_at: Option<i64>,
    pub window_start: Option<i64>,
    pub window_end: Option<i64>,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeRefRow {
    pub scope_ref_id: String,
    pub investigation_id: String,
    pub kind: String,
    pub dataset_id: Option<String>,
    pub dataset_revision: Option<String>,
    pub selector_json: Option<String>,
    pub saved_search_id: Option<String>,
    pub query_json: Option<String>,
    pub window_start: Option<i64>,
    pub window_end: Option<i64>,
    pub label: Option<String>,
    pub position: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HypothesisRow {
    pub hypothesis_id: String,
    pub investigation_id: String,
    pub statement: String,
    pub rationale: Option<String>,
    pub state: String,
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRow {
    pub item_id: String,
    pub investigation_id: String,
    pub kind: String,
    pub content: String,
    pub task_status: Option<String>,
    pub question_status: Option<String>,
    pub authored_by_user: bool,
    pub finding_provenance_json: Option<String>,
    pub position: i64,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRow {
    pub evidence_id: String,
    pub investigation_id: String,
    pub envelope_version: i64,
    pub kind: String,
    pub signal: String,
    pub title: String,
    pub annotation: Option<String>,
    pub relevance: Option<String>,
    pub captured_investigation_revision: i64,
    pub group_id: Option<String>,
    pub position: i64,
    pub supersedes_evidence_id: Option<String>,
    pub archived: bool,
    pub resolver_state: String,
    pub resolver_detail_json: String,
    pub last_verified_at: Option<String>,
    pub reference_json: String,
    pub snapshot_json: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceGroupRow {
    pub group_id: String,
    pub investigation_id: String,
    pub name: String,
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRow {
    pub history_id: i64,
    pub investigation_id: Option<String>,
    pub entity_kind: String,
    pub entity_id: String,
    pub revision: i64,
    pub action: String,
    pub payload_json: String,
    pub detail_json: String,
    pub created_at: String,
}

// ---- inputs ----------------------------------------------------------------

/// Creation input. IDs are minted by the service layer (`inv-<uuid>`),
/// never derived from titles, paths, or row numbers.
#[derive(Debug, Clone)]
pub struct NewInvestigation {
    pub investigation_id: String,
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<String>,
    pub owner_text: Option<String>,
    pub tags_json: String,
    pub incident_started_at: Option<i64>,
    pub window_start: Option<i64>,
    pub window_end: Option<i64>,
}

/// Full editable-field update (status changes go through
/// [`MetaDb::set_investigation_status`]).
#[derive(Debug, Clone)]
pub struct InvestigationEdit {
    pub investigation_id: String,
    pub expected_revision: i64,
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<String>,
    pub owner_text: Option<String>,
    pub tags_json: String,
    pub incident_started_at: Option<i64>,
    pub mitigated_at: Option<i64>,
    pub resolved_at: Option<i64>,
    pub window_start: Option<i64>,
    pub window_end: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewScopeRef {
    pub scope_ref_id: String,
    pub investigation_id: String,
    pub kind: String,
    pub dataset_id: Option<String>,
    pub dataset_revision: Option<String>,
    pub selector_json: Option<String>,
    pub saved_search_id: Option<String>,
    pub query_json: Option<String>,
    pub window_start: Option<i64>,
    pub window_end: Option<i64>,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewHypothesis {
    pub hypothesis_id: String,
    pub investigation_id: String,
    pub statement: String,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewItem {
    pub item_id: String,
    pub investigation_id: String,
    pub kind: String,
    pub content: String,
    pub task_status: Option<String>,
    pub question_status: Option<String>,
}

/// Pin input. `reference_json`/`snapshot_json` are produced by the
/// service layer from the typed `logscope-case` envelope (validated and
/// bounded there); the repository stores them verbatim.
#[derive(Debug, Clone)]
pub struct NewEvidence {
    pub evidence_id: String,
    pub investigation_id: String,
    pub envelope_version: i64,
    pub kind: String,
    pub signal: String,
    pub title: String,
    pub annotation: Option<String>,
    pub relevance: Option<String>,
    pub captured_investigation_revision: i64,
    pub group_id: Option<String>,
    pub supersedes_evidence_id: Option<String>,
    pub reference_json: String,
    pub snapshot_json: String,
}

/// Manual timeline marker. Markers are never inferred from log content;
/// the original timestamp text and zone offset are preserved exactly as
/// the user entered them, alongside the normalized UTC instant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkerRow {
    pub marker_id: String,
    pub investigation_id: String,
    pub kind: String,
    pub label: String,
    pub description: Option<String>,
    /// UTC instant; `None` places the marker in the undated section.
    pub at_nanos: Option<i64>,
    /// Optional bounded-interval end (exclusive). Requires `at_nanos`.
    pub end_nanos: Option<i64>,
    pub original_tz_offset_min: Option<i64>,
    pub original_time_text: Option<String>,
    pub position: i64,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

#[derive(Debug, Clone)]
pub struct NewMarker {
    pub marker_id: String,
    pub investigation_id: String,
    pub kind: String,
    pub label: String,
    pub description: Option<String>,
    pub at_nanos: Option<i64>,
    pub end_nanos: Option<i64>,
    pub original_tz_offset_min: Option<i64>,
    pub original_time_text: Option<String>,
}

/// Full editable-field update for a marker.
#[derive(Debug, Clone)]
pub struct MarkerEdit {
    pub marker_id: String,
    pub expected_revision: i64,
    pub kind: String,
    pub label: String,
    pub description: Option<String>,
    pub at_nanos: Option<i64>,
    pub end_nanos: Option<i64>,
    pub original_tz_offset_min: Option<i64>,
    pub original_time_text: Option<String>,
}

/// Report definition: which sections, which evidence at which exact
/// revisions, and rendering options. Narrative content is user-authored
/// on the definition itself — generation never synthesizes text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportDefRow {
    pub report_def_id: String,
    pub investigation_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub sections_json: String,
    pub selected_evidence_json: String,
    pub selected_markers_json: String,
    pub redaction_profile_id: Option<String>,
    pub options_json: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

#[derive(Debug, Clone)]
pub struct NewReportDef {
    pub report_def_id: String,
    pub investigation_id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub sections_json: String,
    pub selected_evidence_json: String,
    pub selected_markers_json: String,
    pub options_json: String,
}

#[derive(Debug, Clone)]
pub struct ReportDefEdit {
    pub report_def_id: String,
    pub expected_revision: i64,
    pub title: String,
    pub subtitle: Option<String>,
    pub sections_json: String,
    pub selected_evidence_json: String,
    pub selected_markers_json: String,
    pub options_json: String,
}

/// Disclosure profile: ordered typed rules plus posture. Applying a
/// profile never mutates canonical data — it is an export-time
/// projection only. `profile_version` bumps on every rule or posture
/// change so generated artifacts can name exactly what shaped them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactionProfileRow {
    pub profile_id: String,
    pub name: String,
    pub profile_version: i64,
    pub rules_json: String,
    pub posture_json: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

/// Immutable bundle-export record (same two-phase discipline as report
/// artifacts: `running` inserted before any byte, finished exactly once).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleExportRow {
    pub bundle_id: String,
    pub investigation_id: String,
    pub destination_path: String,
    pub manifest_json: Option<String>,
    pub checksum_sha256: Option<String>,
    pub byte_size: Option<i64>,
    pub status: String,
    pub error_json: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

/// Import provenance recorded in the DESTINATION workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleImportRow {
    pub import_id: String,
    pub original_bundle_path: String,
    pub bundle_checksum: String,
    pub manifest_json: String,
    pub imported_at: String,
    pub detail_json: String,
}

/// Immutable generation record. A row is inserted `running` before any
/// byte is written and finished exactly once; a crash leaves the
/// `running` tombstone as honest evidence of the interrupted attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportArtifactRow {
    pub artifact_id: String,
    pub report_def_id: String,
    pub investigation_id: String,
    pub format: String,
    pub destination_path: String,
    pub snapshot_json: String,
    pub checksum_sha256: Option<String>,
    pub byte_size: Option<i64>,
    pub status: String,
    pub error_json: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

// ---- shared helpers ---------------------------------------------------------

// The arguments mirror the case_history columns one-to-one; a params
// struct would only rename the same eight fields.
#[allow(clippy::too_many_arguments)]
fn record_history(
    tx: &Transaction<'_>,
    investigation_id: Option<&str>,
    entity_kind: &str,
    entity_id: &str,
    revision: i64,
    action: &str,
    payload_json: &str,
    detail_json: &str,
) -> Result<(), WorkspaceError> {
    tx.execute(
        "INSERT INTO case_history
           (investigation_id, entity_kind, entity_id, revision, action,
            payload_json, detail_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            investigation_id,
            entity_kind,
            entity_id,
            revision,
            action,
            payload_json,
            detail_json,
            now()
        ],
    )?;
    Ok(())
}

fn payload<T: Serialize>(row: &T) -> Result<String, WorkspaceError> {
    Ok(serde_json::to_string(row)?)
}

/// Distinguishes "someone else changed it" from "it does not exist" after
/// a guarded UPDATE affected zero rows.
fn stale_or_missing(
    tx: &Transaction<'_>,
    table: &str,
    id_col: &str,
    kind: &'static str,
    id: &str,
    expected: i64,
) -> WorkspaceError {
    let exists = tx
        .query_row(
            &format!("SELECT 1 FROM {table} WHERE {id_col} = ?1"),
            params![id],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some();
    if exists {
        WorkspaceError::StaleRevision {
            kind,
            id: id.to_string(),
            expected,
        }
    } else {
        WorkspaceError::MissingEntity {
            kind,
            id: id.to_string(),
        }
    }
}

fn map_investigation(r: &rusqlite::Row<'_>) -> rusqlite::Result<InvestigationRow> {
    Ok(InvestigationRow {
        investigation_id: r.get(0)?,
        entity_version: r.get(1)?,
        title: r.get(2)?,
        description: r.get(3)?,
        status: r.get(4)?,
        severity: r.get(5)?,
        owner_text: r.get(6)?,
        tags_json: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
        status_changed_at: r.get(10)?,
        incident_started_at: r.get(11)?,
        mitigated_at: r.get(12)?,
        resolved_at: r.get(13)?,
        window_start: r.get(14)?,
        window_end: r.get(15)?,
        revision: r.get(16)?,
    })
}

const INVESTIGATION_COLS: &str = "investigation_id, entity_version, title, description, status, \
     severity, owner_text, tags_json, created_at, updated_at, status_changed_at, \
     incident_started_at, mitigated_at, resolved_at, window_start, window_end, revision";

fn get_investigation_tx(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<InvestigationRow, WorkspaceError> {
    tx.query_row(
        &format!("SELECT {INVESTIGATION_COLS} FROM investigations WHERE investigation_id = ?1"),
        params![id],
        map_investigation,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "investigation",
        id: id.to_string(),
    })
}

fn map_hypothesis(r: &rusqlite::Row<'_>) -> rusqlite::Result<HypothesisRow> {
    Ok(HypothesisRow {
        hypothesis_id: r.get(0)?,
        investigation_id: r.get(1)?,
        statement: r.get(2)?,
        rationale: r.get(3)?,
        state: r.get(4)?,
        position: r.get(5)?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
        revision: r.get(8)?,
    })
}

const HYPOTHESIS_COLS: &str = "hypothesis_id, investigation_id, statement, rationale, state, \
     position, created_at, updated_at, revision";

fn get_hypothesis_tx(tx: &Transaction<'_>, id: &str) -> Result<HypothesisRow, WorkspaceError> {
    tx.query_row(
        &format!("SELECT {HYPOTHESIS_COLS} FROM hypotheses WHERE hypothesis_id = ?1"),
        params![id],
        map_hypothesis,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "hypothesis",
        id: id.to_string(),
    })
}

fn map_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<ItemRow> {
    Ok(ItemRow {
        item_id: r.get(0)?,
        investigation_id: r.get(1)?,
        kind: r.get(2)?,
        content: r.get(3)?,
        task_status: r.get(4)?,
        question_status: r.get(5)?,
        authored_by_user: r.get::<_, i64>(6)? != 0,
        finding_provenance_json: r.get(7)?,
        position: r.get(8)?,
        archived: r.get::<_, i64>(9)? != 0,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        revision: r.get(12)?,
    })
}

const ITEM_COLS: &str = "item_id, investigation_id, kind, content, task_status, question_status, \
     authored_by_user, finding_provenance_json, position, archived, created_at, updated_at, \
     revision";

fn get_item_tx(tx: &Transaction<'_>, id: &str) -> Result<ItemRow, WorkspaceError> {
    tx.query_row(
        &format!("SELECT {ITEM_COLS} FROM investigation_items WHERE item_id = ?1"),
        params![id],
        map_item,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "item",
        id: id.to_string(),
    })
}

fn map_marker(r: &rusqlite::Row<'_>) -> rusqlite::Result<MarkerRow> {
    Ok(MarkerRow {
        marker_id: r.get(0)?,
        investigation_id: r.get(1)?,
        kind: r.get(2)?,
        label: r.get(3)?,
        description: r.get(4)?,
        at_nanos: r.get(5)?,
        end_nanos: r.get(6)?,
        original_tz_offset_min: r.get(7)?,
        original_time_text: r.get(8)?,
        position: r.get(9)?,
        archived: r.get::<_, i64>(10)? != 0,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        revision: r.get(13)?,
    })
}

const MARKER_COLS: &str = "marker_id, investigation_id, kind, label, description, at_nanos, \
     end_nanos, original_tz_offset_min, original_time_text, position, archived, created_at, \
     updated_at, revision";

fn get_marker_tx(tx: &Transaction<'_>, id: &str) -> Result<MarkerRow, WorkspaceError> {
    tx.query_row(
        &format!("SELECT {MARKER_COLS} FROM timeline_markers WHERE marker_id = ?1"),
        params![id],
        map_marker,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "marker",
        id: id.to_string(),
    })
}

fn map_report_def(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReportDefRow> {
    Ok(ReportDefRow {
        report_def_id: r.get(0)?,
        investigation_id: r.get(1)?,
        title: r.get(2)?,
        subtitle: r.get(3)?,
        sections_json: r.get(4)?,
        selected_evidence_json: r.get(5)?,
        selected_markers_json: r.get(6)?,
        redaction_profile_id: r.get(7)?,
        options_json: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        revision: r.get(11)?,
    })
}

const REPORT_DEF_COLS: &str = "report_def_id, investigation_id, title, subtitle, sections_json, \
     selected_evidence_json, selected_markers_json, redaction_profile_id, options_json, \
     created_at, updated_at, revision";

fn get_report_def_tx(tx: &Transaction<'_>, id: &str) -> Result<ReportDefRow, WorkspaceError> {
    tx.query_row(
        &format!("SELECT {REPORT_DEF_COLS} FROM report_definitions WHERE report_def_id = ?1"),
        params![id],
        map_report_def,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "report_def",
        id: id.to_string(),
    })
}

fn map_artifact(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReportArtifactRow> {
    Ok(ReportArtifactRow {
        artifact_id: r.get(0)?,
        report_def_id: r.get(1)?,
        investigation_id: r.get(2)?,
        format: r.get(3)?,
        destination_path: r.get(4)?,
        snapshot_json: r.get(5)?,
        checksum_sha256: r.get(6)?,
        byte_size: r.get(7)?,
        status: r.get(8)?,
        error_json: r.get(9)?,
        created_at: r.get(10)?,
        finished_at: r.get(11)?,
    })
}

const ARTIFACT_COLS: &str = "artifact_id, report_def_id, investigation_id, format, \
     destination_path, snapshot_json, checksum_sha256, byte_size, status, error_json, \
     created_at, finished_at";

/// Artifact fetch on an already-held connection (see `raw()` reentrancy).
fn get_artifact_conn(conn: &Connection, id: &str) -> Result<ReportArtifactRow, WorkspaceError> {
    conn.query_row(
        &format!("SELECT {ARTIFACT_COLS} FROM report_artifacts WHERE artifact_id = ?1"),
        params![id],
        map_artifact,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "report_artifact",
        id: id.to_string(),
    })
}

fn map_redaction_profile(r: &rusqlite::Row<'_>) -> rusqlite::Result<RedactionProfileRow> {
    Ok(RedactionProfileRow {
        profile_id: r.get(0)?,
        name: r.get(1)?,
        profile_version: r.get(2)?,
        rules_json: r.get(3)?,
        posture_json: r.get(4)?,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
        revision: r.get(7)?,
    })
}

const REDACTION_COLS: &str = "profile_id, name, profile_version, rules_json, posture_json, \
     created_at, updated_at, revision";

fn get_redaction_profile_tx(
    tx: &Transaction<'_>,
    id: &str,
) -> Result<RedactionProfileRow, WorkspaceError> {
    tx.query_row(
        &format!("SELECT {REDACTION_COLS} FROM redaction_profiles WHERE profile_id = ?1"),
        params![id],
        map_redaction_profile,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "redaction_profile",
        id: id.to_string(),
    })
}

fn map_bundle_export(r: &rusqlite::Row<'_>) -> rusqlite::Result<BundleExportRow> {
    Ok(BundleExportRow {
        bundle_id: r.get(0)?,
        investigation_id: r.get(1)?,
        destination_path: r.get(2)?,
        manifest_json: r.get(3)?,
        checksum_sha256: r.get(4)?,
        byte_size: r.get(5)?,
        status: r.get(6)?,
        error_json: r.get(7)?,
        created_at: r.get(8)?,
        finished_at: r.get(9)?,
    })
}

const BUNDLE_EXPORT_COLS: &str = "bundle_id, investigation_id, destination_path, manifest_json, \
     checksum_sha256, byte_size, status, error_json, created_at, finished_at";

/// Fetch on an already-held connection (see `raw()` reentrancy).
fn get_bundle_export_conn(conn: &Connection, id: &str) -> Result<BundleExportRow, WorkspaceError> {
    conn.query_row(
        &format!("SELECT {BUNDLE_EXPORT_COLS} FROM bundle_exports WHERE bundle_id = ?1"),
        params![id],
        map_bundle_export,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "bundle_export",
        id: id.to_string(),
    })
}

fn map_bundle_import(r: &rusqlite::Row<'_>) -> rusqlite::Result<BundleImportRow> {
    Ok(BundleImportRow {
        import_id: r.get(0)?,
        original_bundle_path: r.get(1)?,
        bundle_checksum: r.get(2)?,
        manifest_json: r.get(3)?,
        imported_at: r.get(4)?,
        detail_json: r.get(5)?,
    })
}

const BUNDLE_IMPORT_COLS: &str = "import_id, original_bundle_path, bundle_checksum, \
     manifest_json, imported_at, detail_json";

/// A bounded marker interval must have a start, and must end after it.
fn check_marker_bounds(at: Option<i64>, end: Option<i64>) -> Result<(), WorkspaceError> {
    match (at, end) {
        (None, Some(_)) => Err(WorkspaceError::Invalid(
            "a marker interval end requires a start instant".into(),
        )),
        (Some(a), Some(e)) if e <= a => Err(WorkspaceError::Invalid(
            "a marker interval must end after it starts (end is exclusive)".into(),
        )),
        _ => Ok(()),
    }
}

fn next_position(
    tx: &Transaction<'_>,
    table: &str,
    investigation_id: &str,
) -> Result<i64, WorkspaceError> {
    Ok(tx.query_row(
        &format!("SELECT COALESCE(MAX(position) + 1, 0) FROM {table} WHERE investigation_id = ?1"),
        params![investigation_id],
        |r| r.get(0),
    )?)
}

// ---- repositories -----------------------------------------------------------

impl MetaDb {
    // ---- investigations ---------------------------------------------------

    pub fn create_investigation(
        &self,
        new: &NewInvestigation,
    ) -> Result<InvestigationRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let ts = now();
        tx.execute(
            "INSERT INTO investigations
               (investigation_id, entity_version, title, description, status,
                severity, owner_text, tags_json, created_at, updated_at,
                incident_started_at, window_start, window_end, revision)
             VALUES (?1, 1, ?2, ?3, 'open', ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, 1)",
            params![
                new.investigation_id,
                new.title,
                new.description,
                new.severity,
                new.owner_text,
                new.tags_json,
                ts,
                new.incident_started_at,
                new.window_start,
                new.window_end,
            ],
        )?;
        let row = get_investigation_tx(&tx, &new.investigation_id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "investigation",
            &row.investigation_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn get_investigation(&self, id: &str) -> Result<Option<InvestigationRow>, WorkspaceError> {
        let conn = self.raw();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {INVESTIGATION_COLS} FROM investigations WHERE investigation_id = ?1"
                ),
                params![id],
                map_investigation,
            )
            .optional()?)
    }

    /// Archived investigations stay readable; they are excluded from the
    /// default listing but always retrievable explicitly.
    pub fn list_investigations(
        &self,
        include_archived: bool,
    ) -> Result<Vec<InvestigationRow>, WorkspaceError> {
        let conn = self.raw();
        let sql = format!(
            "SELECT {INVESTIGATION_COLS} FROM investigations {} ORDER BY updated_at DESC",
            if include_archived {
                ""
            } else {
                "WHERE status != 'archived'"
            }
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], map_investigation)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn update_investigation(
        &self,
        edit: &InvestigationEdit,
    ) -> Result<InvestigationRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE investigations SET
                title = ?1, description = ?2, severity = ?3, owner_text = ?4,
                tags_json = ?5, incident_started_at = ?6, mitigated_at = ?7,
                resolved_at = ?8, window_start = ?9, window_end = ?10,
                updated_at = ?11, revision = revision + 1
             WHERE investigation_id = ?12 AND revision = ?13",
            params![
                edit.title,
                edit.description,
                edit.severity,
                edit.owner_text,
                edit.tags_json,
                edit.incident_started_at,
                edit.mitigated_at,
                edit.resolved_at,
                edit.window_start,
                edit.window_end,
                now(),
                edit.investigation_id,
                edit.expected_revision,
            ],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "investigations",
                "investigation_id",
                "investigation",
                &edit.investigation_id,
                edit.expected_revision,
            ));
        }
        let row = get_investigation_tx(&tx, &edit.investigation_id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "investigation",
            &row.investigation_id,
            row.revision,
            "edited",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Explicit status transition (`status_changed`), archive (`archived`),
    /// or restore (`restored`). Status never changes implicitly.
    pub fn set_investigation_status(
        &self,
        id: &str,
        expected_revision: i64,
        new_status: &str,
        action: &str,
    ) -> Result<InvestigationRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT status FROM investigations WHERE investigation_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let ts = now();
        let n = tx.execute(
            "UPDATE investigations SET status = ?1, status_changed_at = ?2,
                updated_at = ?2, revision = revision + 1
             WHERE investigation_id = ?3 AND revision = ?4",
            params![new_status, ts, id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "investigations",
                "investigation_id",
                "investigation",
                id,
                expected_revision,
            ));
        }
        let row = get_investigation_tx(&tx, id)?;
        let detail = serde_json::json!({
            "from": prior,
            "to": new_status,
        })
        .to_string();
        record_history(
            &tx,
            Some(id),
            "investigation",
            id,
            row.revision,
            action,
            &payload(&row)?,
            &detail,
        )?;
        tx.commit()?;
        Ok(row)
    }

    // ---- scope refs ---------------------------------------------------------

    pub fn add_scope_ref(&self, new: &NewScopeRef) -> Result<ScopeRefRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let position = next_position(&tx, "investigation_scope_refs", &new.investigation_id)?;
        let ts = now();
        tx.execute(
            "INSERT INTO investigation_scope_refs
               (scope_ref_id, investigation_id, kind, dataset_id, dataset_revision,
                selector_json, saved_search_id, query_json, window_start, window_end,
                label, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                new.scope_ref_id,
                new.investigation_id,
                new.kind,
                new.dataset_id,
                new.dataset_revision,
                new.selector_json,
                new.saved_search_id,
                new.query_json,
                new.window_start,
                new.window_end,
                new.label,
                position,
                ts,
            ],
        )?;
        let row = self.get_scope_ref_tx(&tx, &new.scope_ref_id)?;
        record_history(
            &tx,
            Some(&new.investigation_id),
            "scope_ref",
            &new.scope_ref_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    fn get_scope_ref_tx(
        &self,
        tx: &Transaction<'_>,
        id: &str,
    ) -> Result<ScopeRefRow, WorkspaceError> {
        tx.query_row(
            "SELECT scope_ref_id, investigation_id, kind, dataset_id, dataset_revision,
                    selector_json, saved_search_id, query_json, window_start, window_end,
                    label, position, created_at
             FROM investigation_scope_refs WHERE scope_ref_id = ?1",
            params![id],
            |r| {
                Ok(ScopeRefRow {
                    scope_ref_id: r.get(0)?,
                    investigation_id: r.get(1)?,
                    kind: r.get(2)?,
                    dataset_id: r.get(3)?,
                    dataset_revision: r.get(4)?,
                    selector_json: r.get(5)?,
                    saved_search_id: r.get(6)?,
                    query_json: r.get(7)?,
                    window_start: r.get(8)?,
                    window_end: r.get(9)?,
                    label: r.get(10)?,
                    position: r.get(11)?,
                    created_at: r.get(12)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| WorkspaceError::MissingEntity {
            kind: "scope_ref",
            id: id.to_string(),
        })
    }

    pub fn list_scope_refs(
        &self,
        investigation_id: &str,
    ) -> Result<Vec<ScopeRefRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT scope_ref_id, investigation_id, kind, dataset_id, dataset_revision,
                    selector_json, saved_search_id, query_json, window_start, window_end,
                    label, position, created_at
             FROM investigation_scope_refs WHERE investigation_id = ?1
             ORDER BY position, created_at",
        )?;
        let rows = stmt
            .query_map(params![investigation_id], |r| {
                Ok(ScopeRefRow {
                    scope_ref_id: r.get(0)?,
                    investigation_id: r.get(1)?,
                    kind: r.get(2)?,
                    dataset_id: r.get(3)?,
                    dataset_revision: r.get(4)?,
                    selector_json: r.get(5)?,
                    saved_search_id: r.get(6)?,
                    query_json: r.get(7)?,
                    window_start: r.get(8)?,
                    window_end: r.get(9)?,
                    label: r.get(10)?,
                    position: r.get(11)?,
                    created_at: r.get(12)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Scope refs are pointers, not evidence: removal is allowed, with the
    /// final payload preserved in history.
    pub fn remove_scope_ref(&self, scope_ref_id: &str) -> Result<(), WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let row = self.get_scope_ref_tx(&tx, scope_ref_id)?;
        tx.execute(
            "DELETE FROM investigation_scope_refs WHERE scope_ref_id = ?1",
            params![scope_ref_id],
        )?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "scope_ref",
            scope_ref_id,
            1,
            "removed",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(())
    }

    // ---- hypotheses ----------------------------------------------------------

    pub fn create_hypothesis(&self, new: &NewHypothesis) -> Result<HypothesisRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let position = next_position(&tx, "hypotheses", &new.investigation_id)?;
        let ts = now();
        tx.execute(
            "INSERT INTO hypotheses
               (hypothesis_id, investigation_id, statement, rationale, state,
                position, created_at, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, 'unverified', ?5, ?6, ?6, 1)",
            params![
                new.hypothesis_id,
                new.investigation_id,
                new.statement,
                new.rationale,
                position,
                ts,
            ],
        )?;
        let row = get_hypothesis_tx(&tx, &new.hypothesis_id)?;
        record_history(
            &tx,
            Some(&new.investigation_id),
            "hypothesis",
            &new.hypothesis_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn update_hypothesis(
        &self,
        id: &str,
        expected_revision: i64,
        statement: &str,
        rationale: Option<&str>,
    ) -> Result<HypothesisRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE hypotheses SET statement = ?1, rationale = ?2, updated_at = ?3,
                revision = revision + 1
             WHERE hypothesis_id = ?4 AND revision = ?5",
            params![statement, rationale, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "hypotheses",
                "hypothesis_id",
                "hypothesis",
                id,
                expected_revision,
            ));
        }
        let row = get_hypothesis_tx(&tx, id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "hypothesis",
            id,
            row.revision,
            "edited",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Manual, audited state transition. `Supported` and `Confirmed` are
    /// distinct states; nothing here is ever set automatically.
    pub fn set_hypothesis_state(
        &self,
        id: &str,
        expected_revision: i64,
        new_state: &str,
    ) -> Result<HypothesisRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT state FROM hypotheses WHERE hypothesis_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        let n = tx.execute(
            "UPDATE hypotheses SET state = ?1, updated_at = ?2, revision = revision + 1
             WHERE hypothesis_id = ?3 AND revision = ?4",
            params![new_state, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "hypotheses",
                "hypothesis_id",
                "hypothesis",
                id,
                expected_revision,
            ));
        }
        let row = get_hypothesis_tx(&tx, id)?;
        let detail = serde_json::json!({"from": prior, "to": new_state}).to_string();
        record_history(
            &tx,
            Some(&row.investigation_id),
            "hypothesis",
            id,
            row.revision,
            "state_changed",
            &payload(&row)?,
            &detail,
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn list_hypotheses(
        &self,
        investigation_id: &str,
    ) -> Result<Vec<HypothesisRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {HYPOTHESIS_COLS} FROM hypotheses WHERE investigation_id = ?1
             ORDER BY position, created_at"
        ))?;
        let rows = stmt
            .query_map(params![investigation_id], map_hypothesis)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn link_hypothesis_evidence(
        &self,
        hypothesis_id: &str,
        expected_revision: i64,
        evidence_id: &str,
    ) -> Result<HypothesisRow, WorkspaceError> {
        self.change_hypothesis_link(hypothesis_id, expected_revision, evidence_id, true)
    }

    pub fn unlink_hypothesis_evidence(
        &self,
        hypothesis_id: &str,
        expected_revision: i64,
        evidence_id: &str,
    ) -> Result<HypothesisRow, WorkspaceError> {
        self.change_hypothesis_link(hypothesis_id, expected_revision, evidence_id, false)
    }

    fn change_hypothesis_link(
        &self,
        hypothesis_id: &str,
        expected_revision: i64,
        evidence_id: &str,
        link: bool,
    ) -> Result<HypothesisRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE hypotheses SET updated_at = ?1, revision = revision + 1
             WHERE hypothesis_id = ?2 AND revision = ?3",
            params![now(), hypothesis_id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "hypotheses",
                "hypothesis_id",
                "hypothesis",
                hypothesis_id,
                expected_revision,
            ));
        }
        if link {
            tx.execute(
                "INSERT OR IGNORE INTO hypothesis_evidence (hypothesis_id, evidence_id, linked_at)
                 VALUES (?1, ?2, ?3)",
                params![hypothesis_id, evidence_id, now()],
            )?;
        } else {
            tx.execute(
                "DELETE FROM hypothesis_evidence WHERE hypothesis_id = ?1 AND evidence_id = ?2",
                params![hypothesis_id, evidence_id],
            )?;
        }
        let row = get_hypothesis_tx(&tx, hypothesis_id)?;
        let detail = serde_json::json!({"evidence_id": evidence_id}).to_string();
        record_history(
            &tx,
            Some(&row.investigation_id),
            "hypothesis",
            hypothesis_id,
            row.revision,
            if link { "linked" } else { "unlinked" },
            &payload(&row)?,
            &detail,
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn linked_evidence_ids(&self, hypothesis_id: &str) -> Result<Vec<String>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT evidence_id FROM hypothesis_evidence WHERE hypothesis_id = ?1
             ORDER BY linked_at",
        )?;
        let rows = stmt
            .query_map(params![hypothesis_id], |r| r.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- items (notes, tasks, findings, questions) ---------------------------

    pub fn create_item(&self, new: &NewItem) -> Result<ItemRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let position = next_position(&tx, "investigation_items", &new.investigation_id)?;
        let ts = now();
        tx.execute(
            "INSERT INTO investigation_items
               (item_id, investigation_id, kind, content, task_status, question_status,
                authored_by_user, finding_provenance_json, position, archived,
                created_at, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, NULL, ?7, 0, ?8, ?8, 1)",
            params![
                new.item_id,
                new.investigation_id,
                new.kind,
                new.content,
                new.task_status,
                new.question_status,
                position,
                ts,
            ],
        )?;
        let row = get_item_tx(&tx, &new.item_id)?;
        record_history(
            &tx,
            Some(&new.investigation_id),
            "item",
            &new.item_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn update_item_content(
        &self,
        id: &str,
        expected_revision: i64,
        content: &str,
    ) -> Result<ItemRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE investigation_items SET content = ?1, updated_at = ?2,
                revision = revision + 1
             WHERE item_id = ?3 AND revision = ?4",
            params![content, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "investigation_items",
                "item_id",
                "item",
                id,
                expected_revision,
            ));
        }
        let row = get_item_tx(&tx, id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "item",
            id,
            row.revision,
            "edited",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Updates the kind-specific status column (tasks and questions).
    pub fn set_item_status(
        &self,
        id: &str,
        expected_revision: i64,
        task_status: Option<&str>,
        question_status: Option<&str>,
    ) -> Result<ItemRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let prior: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT task_status, question_status FROM investigation_items WHERE item_id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let n = tx.execute(
            "UPDATE investigation_items SET task_status = ?1, question_status = ?2,
                updated_at = ?3, revision = revision + 1
             WHERE item_id = ?4 AND revision = ?5",
            params![task_status, question_status, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "investigation_items",
                "item_id",
                "item",
                id,
                expected_revision,
            ));
        }
        let row = get_item_tx(&tx, id)?;
        let detail = serde_json::json!({
            "from": prior.map(|(t, q)| serde_json::json!({"task": t, "question": q})),
            "to": {"task": task_status, "question": question_status},
        })
        .to_string();
        record_history(
            &tx,
            Some(&row.investigation_id),
            "item",
            id,
            row.revision,
            "status_changed",
            &payload(&row)?,
            &detail,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Normal removal path: archive with history (`archived`/`restored`),
    /// never a hard delete.
    pub fn set_item_archived(
        &self,
        id: &str,
        expected_revision: i64,
        archived: bool,
    ) -> Result<ItemRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE investigation_items SET archived = ?1, updated_at = ?2,
                revision = revision + 1
             WHERE item_id = ?3 AND revision = ?4",
            params![archived as i64, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "investigation_items",
                "item_id",
                "item",
                id,
                expected_revision,
            ));
        }
        let row = get_item_tx(&tx, id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "item",
            id,
            row.revision,
            if archived { "archived" } else { "restored" },
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn list_items(
        &self,
        investigation_id: &str,
        include_archived: bool,
    ) -> Result<Vec<ItemRow>, WorkspaceError> {
        let conn = self.raw();
        let sql = format!(
            "SELECT {ITEM_COLS} FROM investigation_items WHERE investigation_id = ?1 {}
             ORDER BY position, created_at",
            if include_archived {
                ""
            } else {
                "AND archived = 0"
            }
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![investigation_id], map_item)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- timeline markers -----------------------------------------------------

    pub fn create_marker(&self, new: &NewMarker) -> Result<MarkerRow, WorkspaceError> {
        check_marker_bounds(new.at_nanos, new.end_nanos)?;
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let position = next_position(&tx, "timeline_markers", &new.investigation_id)?;
        let ts = now();
        tx.execute(
            "INSERT INTO timeline_markers
               (marker_id, investigation_id, kind, label, description, at_nanos,
                end_nanos, original_tz_offset_min, original_time_text, position,
                archived, created_at, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, ?11, ?11, 1)",
            params![
                new.marker_id,
                new.investigation_id,
                new.kind,
                new.label,
                new.description,
                new.at_nanos,
                new.end_nanos,
                new.original_tz_offset_min,
                new.original_time_text,
                position,
                ts,
            ],
        )?;
        let row = get_marker_tx(&tx, &new.marker_id)?;
        record_history(
            &tx,
            Some(&new.investigation_id),
            "marker",
            &new.marker_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn update_marker(&self, edit: &MarkerEdit) -> Result<MarkerRow, WorkspaceError> {
        check_marker_bounds(edit.at_nanos, edit.end_nanos)?;
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE timeline_markers SET kind = ?1, label = ?2, description = ?3,
                at_nanos = ?4, end_nanos = ?5, original_tz_offset_min = ?6,
                original_time_text = ?7, updated_at = ?8, revision = revision + 1
             WHERE marker_id = ?9 AND revision = ?10",
            params![
                edit.kind,
                edit.label,
                edit.description,
                edit.at_nanos,
                edit.end_nanos,
                edit.original_tz_offset_min,
                edit.original_time_text,
                now(),
                edit.marker_id,
                edit.expected_revision,
            ],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "timeline_markers",
                "marker_id",
                "marker",
                &edit.marker_id,
                edit.expected_revision,
            ));
        }
        let row = get_marker_tx(&tx, &edit.marker_id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "marker",
            &edit.marker_id,
            row.revision,
            "edited",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn set_marker_archived(
        &self,
        id: &str,
        expected_revision: i64,
        archived: bool,
    ) -> Result<MarkerRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE timeline_markers SET archived = ?1, updated_at = ?2,
                revision = revision + 1
             WHERE marker_id = ?3 AND revision = ?4",
            params![archived as i64, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "timeline_markers",
                "marker_id",
                "marker",
                id,
                expected_revision,
            ));
        }
        let row = get_marker_tx(&tx, id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "marker",
            id,
            row.revision,
            if archived { "archived" } else { "restored" },
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn list_markers(
        &self,
        investigation_id: &str,
        include_archived: bool,
    ) -> Result<Vec<MarkerRow>, WorkspaceError> {
        let conn = self.raw();
        let sql = format!(
            "SELECT {MARKER_COLS} FROM timeline_markers WHERE investigation_id = ?1 {}
             ORDER BY position, created_at",
            if include_archived {
                ""
            } else {
                "AND archived = 0"
            }
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![investigation_id], map_marker)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- redaction profiles ----------------------------------------------------

    pub fn create_redaction_profile(
        &self,
        profile_id: &str,
        name: &str,
        rules_json: &str,
        posture_json: &str,
    ) -> Result<RedactionProfileRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let ts = now();
        tx.execute(
            "INSERT INTO redaction_profiles
               (profile_id, name, profile_version, rules_json, posture_json,
                created_at, updated_at, revision)
             VALUES (?1, ?2, 1, ?3, ?4, ?5, ?5, 1)",
            params![profile_id, name, rules_json, posture_json, ts],
        )?;
        let row = get_redaction_profile_tx(&tx, profile_id)?;
        record_history(
            &tx,
            None,
            "redaction_profile",
            profile_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Any rule or posture change bumps `profile_version`, so artifacts
    /// generated before and after can never claim the same profile
    /// identity. A pure rename keeps the version.
    pub fn update_redaction_profile(
        &self,
        profile_id: &str,
        expected_revision: i64,
        name: &str,
        rules_json: &str,
        posture_json: &str,
    ) -> Result<RedactionProfileRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let semantic_change: bool = tx
            .query_row(
                "SELECT rules_json != ?1 OR posture_json != ?2
                 FROM redaction_profiles WHERE profile_id = ?3",
                params![rules_json, posture_json, profile_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        let n = tx.execute(
            "UPDATE redaction_profiles SET name = ?1, rules_json = ?2, posture_json = ?3,
                profile_version = profile_version + ?4, updated_at = ?5,
                revision = revision + 1
             WHERE profile_id = ?6 AND revision = ?7",
            params![
                name,
                rules_json,
                posture_json,
                i64::from(semantic_change),
                now(),
                profile_id,
                expected_revision,
            ],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "redaction_profiles",
                "profile_id",
                "redaction_profile",
                profile_id,
                expected_revision,
            ));
        }
        let row = get_redaction_profile_tx(&tx, profile_id)?;
        record_history(
            &tx,
            None,
            "redaction_profile",
            profile_id,
            row.revision,
            "edited",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn get_redaction_profile(
        &self,
        id: &str,
    ) -> Result<Option<RedactionProfileRow>, WorkspaceError> {
        let conn = self.raw();
        Ok(conn
            .query_row(
                &format!("SELECT {REDACTION_COLS} FROM redaction_profiles WHERE profile_id = ?1"),
                params![id],
                map_redaction_profile,
            )
            .optional()?)
    }

    pub fn list_redaction_profiles(&self) -> Result<Vec<RedactionProfileRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {REDACTION_COLS} FROM redaction_profiles ORDER BY name, profile_id"
        ))?;
        let rows = stmt
            .query_map([], map_redaction_profile)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Attaches or detaches a disclosure profile on a report definition.
    pub fn set_report_def_redaction(
        &self,
        report_def_id: &str,
        expected_revision: i64,
        profile_id: Option<&str>,
    ) -> Result<ReportDefRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE report_definitions SET redaction_profile_id = ?1, updated_at = ?2,
                revision = revision + 1
             WHERE report_def_id = ?3 AND revision = ?4",
            params![profile_id, now(), report_def_id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "report_definitions",
                "report_def_id",
                "report_def",
                report_def_id,
                expected_revision,
            ));
        }
        let row = get_report_def_tx(&tx, report_def_id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "report_def",
            report_def_id,
            row.revision,
            "redaction_changed",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    // ---- report definitions + artifacts ---------------------------------------

    pub fn create_report_def(&self, new: &NewReportDef) -> Result<ReportDefRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let ts = now();
        tx.execute(
            "INSERT INTO report_definitions
               (report_def_id, investigation_id, title, subtitle, sections_json,
                selected_evidence_json, selected_markers_json, redaction_profile_id,
                options_json, created_at, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, ?9, 1)",
            params![
                new.report_def_id,
                new.investigation_id,
                new.title,
                new.subtitle,
                new.sections_json,
                new.selected_evidence_json,
                new.selected_markers_json,
                new.options_json,
                ts,
            ],
        )?;
        let row = get_report_def_tx(&tx, &new.report_def_id)?;
        record_history(
            &tx,
            Some(&new.investigation_id),
            "report_def",
            &new.report_def_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn update_report_def(&self, edit: &ReportDefEdit) -> Result<ReportDefRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE report_definitions SET title = ?1, subtitle = ?2, sections_json = ?3,
                selected_evidence_json = ?4, selected_markers_json = ?5, options_json = ?6,
                updated_at = ?7, revision = revision + 1
             WHERE report_def_id = ?8 AND revision = ?9",
            params![
                edit.title,
                edit.subtitle,
                edit.sections_json,
                edit.selected_evidence_json,
                edit.selected_markers_json,
                edit.options_json,
                now(),
                edit.report_def_id,
                edit.expected_revision,
            ],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "report_definitions",
                "report_def_id",
                "report_def",
                &edit.report_def_id,
                edit.expected_revision,
            ));
        }
        let row = get_report_def_tx(&tx, &edit.report_def_id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "report_def",
            &edit.report_def_id,
            row.revision,
            "edited",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn get_report_def(&self, id: &str) -> Result<Option<ReportDefRow>, WorkspaceError> {
        let conn = self.raw();
        Ok(conn
            .query_row(
                &format!(
                    "SELECT {REPORT_DEF_COLS} FROM report_definitions WHERE report_def_id = ?1"
                ),
                params![id],
                map_report_def,
            )
            .optional()?)
    }

    pub fn list_report_defs(
        &self,
        investigation_id: &str,
    ) -> Result<Vec<ReportDefRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {REPORT_DEF_COLS} FROM report_definitions
             WHERE investigation_id = ?1 ORDER BY created_at, report_def_id"
        ))?;
        let rows = stmt
            .query_map(params![investigation_id], map_report_def)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Inserts the immutable `running` record before any byte is written.
    pub fn start_report_artifact(
        &self,
        artifact_id: &str,
        report_def_id: &str,
        investigation_id: &str,
        format: &str,
        destination_path: &str,
        snapshot_json: &str,
    ) -> Result<ReportArtifactRow, WorkspaceError> {
        // One guard for the whole call: `raw()` is a non-reentrant mutex,
        // so re-entering through another repository method would deadlock.
        let conn = self.raw();
        conn.execute(
            "INSERT INTO report_artifacts
               (artifact_id, report_def_id, investigation_id, format, destination_path,
                snapshot_json, checksum_sha256, byte_size, status, error_json,
                created_at, finished_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, 'running', NULL, ?7, NULL)",
            params![
                artifact_id,
                report_def_id,
                investigation_id,
                format,
                destination_path,
                snapshot_json,
                now(),
            ],
        )?;
        get_artifact_conn(&conn, artifact_id)
    }

    /// Finishes a generation record exactly once. Refuses to finish a row
    /// that is not `running`, so an artifact's outcome can never be
    /// rewritten after the fact.
    pub fn finish_report_artifact(
        &self,
        artifact_id: &str,
        status: &str,
        checksum_sha256: Option<&str>,
        byte_size: Option<i64>,
        error_json: Option<&str>,
    ) -> Result<ReportArtifactRow, WorkspaceError> {
        if !matches!(status, "completed" | "failed" | "cancelled") {
            return Err(WorkspaceError::Invalid(format!(
                "invalid terminal artifact status {status:?}"
            )));
        }
        let conn = self.raw();
        let n = conn.execute(
            "UPDATE report_artifacts SET status = ?1, checksum_sha256 = ?2,
                byte_size = ?3, error_json = ?4, finished_at = ?5
             WHERE artifact_id = ?6 AND status = 'running'",
            params![
                status,
                checksum_sha256,
                byte_size,
                error_json,
                now(),
                artifact_id
            ],
        )?;
        if n == 0 {
            return Err(WorkspaceError::Invalid(format!(
                "artifact {artifact_id} is not running (already finished or unknown)"
            )));
        }
        get_artifact_conn(&conn, artifact_id)
    }

    pub fn get_report_artifact(
        &self,
        id: &str,
    ) -> Result<Option<ReportArtifactRow>, WorkspaceError> {
        let conn = self.raw();
        Ok(conn
            .query_row(
                &format!("SELECT {ARTIFACT_COLS} FROM report_artifacts WHERE artifact_id = ?1"),
                params![id],
                map_artifact,
            )
            .optional()?)
    }

    pub fn list_report_artifacts(
        &self,
        investigation_id: &str,
    ) -> Result<Vec<ReportArtifactRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ARTIFACT_COLS} FROM report_artifacts
             WHERE investigation_id = ?1 ORDER BY created_at DESC, artifact_id"
        ))?;
        let rows = stmt
            .query_map(params![investigation_id], map_artifact)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- bundle exports + imports ----------------------------------------------

    pub fn start_bundle_export(
        &self,
        bundle_id: &str,
        investigation_id: &str,
        destination_path: &str,
    ) -> Result<BundleExportRow, WorkspaceError> {
        let conn = self.raw();
        conn.execute(
            "INSERT INTO bundle_exports
               (bundle_id, investigation_id, destination_path, manifest_json,
                checksum_sha256, byte_size, status, error_json, created_at, finished_at)
             VALUES (?1, ?2, ?3, NULL, NULL, NULL, 'running', NULL, ?4, NULL)",
            params![bundle_id, investigation_id, destination_path, now()],
        )?;
        get_bundle_export_conn(&conn, bundle_id)
    }

    /// Finishes an export record exactly once (`running` rows only).
    pub fn finish_bundle_export(
        &self,
        bundle_id: &str,
        status: &str,
        manifest_json: Option<&str>,
        checksum_sha256: Option<&str>,
        byte_size: Option<i64>,
        error_json: Option<&str>,
    ) -> Result<BundleExportRow, WorkspaceError> {
        if !matches!(status, "completed" | "failed" | "cancelled") {
            return Err(WorkspaceError::Invalid(format!(
                "invalid terminal bundle status {status:?}"
            )));
        }
        let conn = self.raw();
        let n = conn.execute(
            "UPDATE bundle_exports SET status = ?1, manifest_json = ?2,
                checksum_sha256 = ?3, byte_size = ?4, error_json = ?5, finished_at = ?6
             WHERE bundle_id = ?7 AND status = 'running'",
            params![
                status,
                manifest_json,
                checksum_sha256,
                byte_size,
                error_json,
                now(),
                bundle_id
            ],
        )?;
        if n == 0 {
            return Err(WorkspaceError::Invalid(format!(
                "bundle export {bundle_id} is not running (already finished or unknown)"
            )));
        }
        get_bundle_export_conn(&conn, bundle_id)
    }

    pub fn list_bundle_exports(
        &self,
        investigation_id: &str,
    ) -> Result<Vec<BundleExportRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {BUNDLE_EXPORT_COLS} FROM bundle_exports
             WHERE investigation_id = ?1 ORDER BY created_at DESC, bundle_id"
        ))?;
        let rows = stmt
            .query_map(params![investigation_id], map_bundle_export)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Records import provenance in the destination workspace.
    pub fn record_bundle_import(
        &self,
        import_id: &str,
        original_bundle_path: &str,
        bundle_checksum: &str,
        manifest_json: &str,
        detail_json: &str,
    ) -> Result<BundleImportRow, WorkspaceError> {
        let conn = self.raw();
        conn.execute(
            "INSERT INTO bundle_imports
               (import_id, original_bundle_path, bundle_checksum, manifest_json,
                imported_at, detail_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                import_id,
                original_bundle_path,
                bundle_checksum,
                manifest_json,
                now(),
                detail_json
            ],
        )?;
        conn.query_row(
            &format!("SELECT {BUNDLE_IMPORT_COLS} FROM bundle_imports WHERE import_id = ?1"),
            params![import_id],
            map_bundle_import,
        )
        .optional()?
        .ok_or_else(|| WorkspaceError::MissingEntity {
            kind: "bundle_import",
            id: import_id.to_string(),
        })
    }

    pub fn list_bundle_imports(&self) -> Result<Vec<BundleImportRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {BUNDLE_IMPORT_COLS} FROM bundle_imports ORDER BY imported_at DESC, import_id"
        ))?;
        let rows = stmt
            .query_map([], map_bundle_import)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- reordering -----------------------------------------------------------

    /// Reorders children of one investigation. `entity_kind` selects the
    /// table. The ordered id list must exactly cover the investigation's
    /// current rows of that kind (archived included — reordering never
    /// drops anything). Positions change; stable IDs, provenance, and
    /// child revisions do not. The investigation revision is the guard,
    /// and one history row records the new order retrievably.
    pub fn reorder_children(
        &self,
        investigation_id: &str,
        expected_investigation_revision: i64,
        entity_kind: &str,
        ordered_ids: &[String],
    ) -> Result<InvestigationRow, WorkspaceError> {
        let (table, id_col) = match entity_kind {
            "hypothesis" => ("hypotheses", "hypothesis_id"),
            "item" => ("investigation_items", "item_id"),
            "evidence" => ("evidence", "evidence_id"),
            "evidence_group" => ("evidence_groups", "group_id"),
            "marker" => ("timeline_markers", "marker_id"),
            "scope_ref" => ("investigation_scope_refs", "scope_ref_id"),
            other => {
                return Err(WorkspaceError::Invalid(format!(
                    "unknown reorderable entity kind: {other}"
                )))
            }
        };
        let mut conn = self.raw();
        let tx = conn.transaction()?;

        let n = tx.execute(
            "UPDATE investigations SET updated_at = ?1, revision = revision + 1
             WHERE investigation_id = ?2 AND revision = ?3",
            params![now(), investigation_id, expected_investigation_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "investigations",
                "investigation_id",
                "investigation",
                investigation_id,
                expected_investigation_revision,
            ));
        }

        let mut existing: Vec<String> = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {id_col} FROM {table} WHERE investigation_id = ?1"
            ))?;
            let ids = stmt
                .query_map(params![investigation_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            ids
        };
        existing.sort();
        let mut requested = ordered_ids.to_vec();
        requested.sort();
        if existing != requested {
            return Err(WorkspaceError::Invalid(format!(
                "reorder id set does not match current {entity_kind} rows of {investigation_id}"
            )));
        }

        {
            let mut stmt = tx.prepare(&format!(
                "UPDATE {table} SET position = ?1 WHERE {id_col} = ?2"
            ))?;
            for (i, id) in ordered_ids.iter().enumerate() {
                stmt.execute(params![i as i64, id])?;
            }
        }

        let row = get_investigation_tx(&tx, investigation_id)?;
        let detail = serde_json::json!({
            "entity_kind": entity_kind,
            "ordered_ids": ordered_ids,
        })
        .to_string();
        record_history(
            &tx,
            Some(investigation_id),
            "investigation",
            investigation_id,
            row.revision,
            "reordered",
            &payload(&row)?,
            &detail,
        )?;
        tx.commit()?;
        Ok(row)
    }

    // ---- history ----------------------------------------------------------------

    pub fn list_entity_history(
        &self,
        entity_kind: &str,
        entity_id: &str,
    ) -> Result<Vec<HistoryRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT history_id, investigation_id, entity_kind, entity_id, revision,
                    action, payload_json, detail_json, created_at
             FROM case_history WHERE entity_kind = ?1 AND entity_id = ?2
             ORDER BY history_id",
        )?;
        let rows = stmt
            .query_map(params![entity_kind, entity_id], map_history)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Newest-first activity ledger for one investigation.
    pub fn list_investigation_activity(
        &self,
        investigation_id: &str,
        limit: u32,
    ) -> Result<Vec<HistoryRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT history_id, investigation_id, entity_kind, entity_id, revision,
                    action, payload_json, detail_json, created_at
             FROM case_history WHERE investigation_id = ?1
             ORDER BY history_id DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![investigation_id, limit], map_history)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn map_history(r: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryRow> {
    Ok(HistoryRow {
        history_id: r.get(0)?,
        investigation_id: r.get(1)?,
        entity_kind: r.get(2)?,
        entity_id: r.get(3)?,
        revision: r.get(4)?,
        action: r.get(5)?,
        payload_json: r.get(6)?,
        detail_json: r.get(7)?,
        created_at: r.get(8)?,
    })
}

// ---- evidence storage ---------------------------------------------------------

const EVIDENCE_COLS: &str = "evidence_id, investigation_id, envelope_version, kind, signal, \
     title, annotation, relevance, captured_investigation_revision, group_id, position, \
     supersedes_evidence_id, archived, resolver_state, resolver_detail_json, last_verified_at, \
     reference_json, snapshot_json, created_at, updated_at, revision";

fn map_evidence(r: &rusqlite::Row<'_>) -> rusqlite::Result<EvidenceRow> {
    Ok(EvidenceRow {
        evidence_id: r.get(0)?,
        investigation_id: r.get(1)?,
        envelope_version: r.get(2)?,
        kind: r.get(3)?,
        signal: r.get(4)?,
        title: r.get(5)?,
        annotation: r.get(6)?,
        relevance: r.get(7)?,
        captured_investigation_revision: r.get(8)?,
        group_id: r.get(9)?,
        position: r.get(10)?,
        supersedes_evidence_id: r.get(11)?,
        archived: r.get::<_, i64>(12)? != 0,
        resolver_state: r.get(13)?,
        resolver_detail_json: r.get(14)?,
        last_verified_at: r.get(15)?,
        reference_json: r.get(16)?,
        snapshot_json: r.get(17)?,
        created_at: r.get(18)?,
        updated_at: r.get(19)?,
        revision: r.get(20)?,
    })
}

fn get_evidence_tx(tx: &Transaction<'_>, id: &str) -> Result<EvidenceRow, WorkspaceError> {
    tx.query_row(
        &format!("SELECT {EVIDENCE_COLS} FROM evidence WHERE evidence_id = ?1"),
        params![id],
        map_evidence,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "evidence",
        id: id.to_string(),
    })
}

impl MetaDb {
    // ---- evidence groups ----------------------------------------------------

    pub fn create_evidence_group(
        &self,
        group_id: &str,
        investigation_id: &str,
        name: &str,
    ) -> Result<EvidenceGroupRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let position = next_position(&tx, "evidence_groups", investigation_id)?;
        let ts = now();
        tx.execute(
            "INSERT INTO evidence_groups
               (group_id, investigation_id, name, position, created_at, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, 1)",
            params![group_id, investigation_id, name, position, ts],
        )?;
        let row = get_group_tx(&tx, group_id)?;
        record_history(
            &tx,
            Some(investigation_id),
            "evidence_group",
            group_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn rename_evidence_group(
        &self,
        group_id: &str,
        expected_revision: i64,
        name: &str,
    ) -> Result<EvidenceGroupRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE evidence_groups SET name = ?1, updated_at = ?2, revision = revision + 1
             WHERE group_id = ?3 AND revision = ?4",
            params![name, now(), group_id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "evidence_groups",
                "group_id",
                "evidence_group",
                group_id,
                expected_revision,
            ));
        }
        let row = get_group_tx(&tx, group_id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "evidence_group",
            group_id,
            row.revision,
            "edited",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Deleting a group never touches its members' evidence rows beyond
    /// clearing the pointer (FK `ON DELETE SET NULL`); the final group
    /// payload stays in history.
    pub fn delete_evidence_group(&self, group_id: &str) -> Result<(), WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let row = get_group_tx(&tx, group_id)?;
        tx.execute(
            "DELETE FROM evidence_groups WHERE group_id = ?1",
            params![group_id],
        )?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "evidence_group",
            group_id,
            row.revision,
            "removed",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_evidence_groups(
        &self,
        investigation_id: &str,
    ) -> Result<Vec<EvidenceGroupRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(
            "SELECT group_id, investigation_id, name, position, created_at, updated_at, revision
             FROM evidence_groups WHERE investigation_id = ?1 ORDER BY position, created_at",
        )?;
        let rows = stmt
            .query_map(params![investigation_id], map_group)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- evidence -------------------------------------------------------------

    /// Stores a pinned evidence item (action `pinned`). The typed
    /// reference and bounded snapshot were validated by the service layer.
    pub fn insert_evidence(&self, new: &NewEvidence) -> Result<EvidenceRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let row = insert_evidence_tx(&tx, new, "pinned")?;
        tx.commit()?;
        Ok(row)
    }

    pub fn get_evidence(&self, id: &str) -> Result<Option<EvidenceRow>, WorkspaceError> {
        let conn = self.raw();
        Ok(conn
            .query_row(
                &format!("SELECT {EVIDENCE_COLS} FROM evidence WHERE evidence_id = ?1"),
                params![id],
                map_evidence,
            )
            .optional()?)
    }

    pub fn list_evidence(
        &self,
        investigation_id: &str,
        include_archived: bool,
    ) -> Result<Vec<EvidenceRow>, WorkspaceError> {
        let conn = self.raw();
        let sql = format!(
            "SELECT {EVIDENCE_COLS} FROM evidence WHERE investigation_id = ?1 {}
             ORDER BY position, created_at",
            if include_archived {
                ""
            } else {
                "AND archived = 0"
            }
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![investigation_id], map_evidence)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Edits display metadata (title/annotation/relevance). The reference
    /// and snapshot are immutable through this path.
    pub fn update_evidence_annotation(
        &self,
        id: &str,
        expected_revision: i64,
        title: &str,
        annotation: Option<&str>,
        relevance: Option<&str>,
    ) -> Result<EvidenceRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE evidence SET title = ?1, annotation = ?2, relevance = ?3,
                updated_at = ?4, revision = revision + 1
             WHERE evidence_id = ?5 AND revision = ?6",
            params![title, annotation, relevance, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "evidence",
                "evidence_id",
                "evidence",
                id,
                expected_revision,
            ));
        }
        let row = get_evidence_tx(&tx, id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "evidence",
            id,
            row.revision,
            "edited",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn set_evidence_group(
        &self,
        id: &str,
        expected_revision: i64,
        group_id: Option<&str>,
    ) -> Result<EvidenceRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE evidence SET group_id = ?1, updated_at = ?2, revision = revision + 1
             WHERE evidence_id = ?3 AND revision = ?4",
            params![group_id, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "evidence",
                "evidence_id",
                "evidence",
                id,
                expected_revision,
            ));
        }
        let row = get_evidence_tx(&tx, id)?;
        let detail = serde_json::json!({ "group_id": group_id }).to_string();
        record_history(
            &tx,
            Some(&row.investigation_id),
            "evidence",
            id,
            row.revision,
            "edited",
            &payload(&row)?,
            &detail,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Normal removal path: archive (tombstoned via history), restorable.
    pub fn set_evidence_archived(
        &self,
        id: &str,
        expected_revision: i64,
        archived: bool,
    ) -> Result<EvidenceRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE evidence SET archived = ?1, updated_at = ?2, revision = revision + 1
             WHERE evidence_id = ?3 AND revision = ?4",
            params![archived as i64, now(), id, expected_revision],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "evidence",
                "evidence_id",
                "evidence",
                id,
                expected_revision,
            ));
        }
        let row = get_evidence_tx(&tx, id)?;
        record_history(
            &tx,
            Some(&row.investigation_id),
            "evidence",
            id,
            row.revision,
            if archived { "archived" } else { "restored" },
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Pins `new` as a superseding item for `superseded_id` in one
    /// transaction. The earlier item and its revisions stay untouched and
    /// visible; a `superseded` history event records the link on the old
    /// entity.
    pub fn supersede_evidence(
        &self,
        new: &NewEvidence,
        superseded_id: &str,
    ) -> Result<EvidenceRow, WorkspaceError> {
        if new.supersedes_evidence_id.as_deref() != Some(superseded_id) {
            return Err(WorkspaceError::Invalid(
                "supersede_evidence requires new.supersedes_evidence_id to name the old item"
                    .into(),
            ));
        }
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let old = get_evidence_tx(&tx, superseded_id)?;
        let row = insert_evidence_tx(&tx, new, "pinned")?;
        let detail = serde_json::json!({ "superseded_by": new.evidence_id }).to_string();
        record_history(
            &tx,
            Some(&old.investigation_id),
            "evidence",
            superseded_id,
            old.revision,
            "superseded",
            &payload(&old)?,
            &detail,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// Records a verification outcome. This touches ONLY the resolver
    /// columns: it never bumps the content revision, never rewrites the
    /// captured snapshot or reference, and writes no per-item history
    /// (a batch verification records one investigation-level `verified`
    /// event via [`MetaDb::record_verification_run`]).
    pub fn update_evidence_resolution(
        &self,
        id: &str,
        resolver_state: &str,
        resolver_detail_json: &str,
        verified_at: &str,
    ) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        let n = conn.execute(
            "UPDATE evidence SET resolver_state = ?1, resolver_detail_json = ?2,
                last_verified_at = ?3
             WHERE evidence_id = ?4",
            params![resolver_state, resolver_detail_json, verified_at, id],
        )?;
        if n == 0 {
            return Err(WorkspaceError::MissingEntity {
                kind: "evidence",
                id: id.to_string(),
            });
        }
        Ok(())
    }

    /// One activity event summarizing a (batch) verification run.
    pub fn record_verification_run(
        &self,
        investigation_id: &str,
        detail_json: &str,
    ) -> Result<(), WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let row = get_investigation_tx(&tx, investigation_id)?;
        record_history(
            &tx,
            Some(investigation_id),
            "investigation",
            investigation_id,
            row.revision,
            "verified",
            &payload(&row)?,
            detail_json,
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn insert_evidence_tx(
    tx: &Transaction<'_>,
    new: &NewEvidence,
    action: &str,
) -> Result<EvidenceRow, WorkspaceError> {
    let position = next_position(tx, "evidence", &new.investigation_id)?;
    let ts = now();
    tx.execute(
        "INSERT INTO evidence
           (evidence_id, investigation_id, envelope_version, kind, signal, title,
            annotation, relevance, captured_investigation_revision, group_id, position,
            supersedes_evidence_id, archived, resolver_state, resolver_detail_json,
            last_verified_at, reference_json, snapshot_json, created_at, updated_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0, 'unverified', '{}',
                 NULL, ?13, ?14, ?15, ?15, 1)",
        params![
            new.evidence_id,
            new.investigation_id,
            new.envelope_version,
            new.kind,
            new.signal,
            new.title,
            new.annotation,
            new.relevance,
            new.captured_investigation_revision,
            new.group_id,
            position,
            new.supersedes_evidence_id,
            new.reference_json,
            new.snapshot_json,
            ts,
        ],
    )?;
    let row = get_evidence_tx(tx, &new.evidence_id)?;
    record_history(
        tx,
        Some(&new.investigation_id),
        "evidence",
        &new.evidence_id,
        1,
        action,
        &payload(&row)?,
        "{}",
    )?;
    Ok(row)
}

fn map_group(r: &rusqlite::Row<'_>) -> rusqlite::Result<EvidenceGroupRow> {
    Ok(EvidenceGroupRow {
        group_id: r.get(0)?,
        investigation_id: r.get(1)?,
        name: r.get(2)?,
        position: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        revision: r.get(6)?,
    })
}

fn get_group_tx(tx: &Transaction<'_>, id: &str) -> Result<EvidenceGroupRow, WorkspaceError> {
    tx.query_row(
        "SELECT group_id, investigation_id, name, position, created_at, updated_at, revision
         FROM evidence_groups WHERE group_id = ?1",
        params![id],
        map_group,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "evidence_group",
        id: id.to_string(),
    })
}
