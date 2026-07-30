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

use rusqlite::{params, OptionalExtension, Transaction};
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
