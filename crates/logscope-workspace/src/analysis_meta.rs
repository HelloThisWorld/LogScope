//! Metadata repositories for the v0.4 deterministic-analysis control
//! plane (analysis definitions, two-phase runs, deterministic findings,
//! derived-artifact catalog).
//!
//! Contract (extends the case_meta discipline):
//! - Definition mutations run in one transaction, bump `revision`, and
//!   write a `case_history` row (entity_kind `analysis_definition`) in
//!   that same transaction; editing never mutates a completed run.
//! - Runs are two-phase and immutable once finished: a row is inserted
//!   `pending` before any derived byte exists, moves to `running`, and
//!   is finished exactly once (`completed|cancelled|failed`). A crash
//!   leaves an honest tombstone that recovery finishes as
//!   `failed (job/interrupted)` — completed, never deleted.
//! - Only a `completed` run is a usable result. `stale` applies only to
//!   completed runs whose inputs later changed; history is preserved.
//! - Deterministic findings are execution products keyed to their run;
//!   they are inserted whole and never updated in place. Manual
//!   findings live in `investigation_items` and are never touched here.
//! - Cleanup deletes only unreferenced runs (no findings; evidence /
//!   report / bundle reference guards extend this check in WP6).

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::case_meta::{now, payload, record_history, stale_or_missing};
use crate::error::WorkspaceError;
use crate::meta::MetaDb;

// ---- row types ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisDefinitionRow {
    pub definition_id: String,
    pub definition_schema_version: i64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub dataset_selection_json: String,
    pub query_text: String,
    pub query_language_version: i64,
    pub query_fingerprint: Option<String>,
    pub time_strategy_json: String,
    pub field_selection_json: String,
    pub algorithm_id: String,
    pub algorithm_version: i64,
    pub config_json: String,
    pub config_fingerprint: String,
    pub masking_profile_json: String,
    pub thresholds_json: String,
    pub limits_json: String,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
}

#[derive(Debug, Clone)]
pub struct NewAnalysisDefinition {
    pub definition_id: String,
    pub definition_schema_version: i64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub dataset_selection_json: String,
    pub query_text: String,
    pub query_language_version: i64,
    pub query_fingerprint: Option<String>,
    pub time_strategy_json: String,
    pub field_selection_json: String,
    pub algorithm_id: String,
    pub algorithm_version: i64,
    pub config_json: String,
    pub config_fingerprint: String,
    pub masking_profile_json: String,
    pub thresholds_json: String,
    pub limits_json: String,
}

/// Editable definition fields; identity/algorithm changes create a new
/// configuration fingerprint upstream before reaching this layer.
#[derive(Debug, Clone)]
pub struct AnalysisDefinitionEdit {
    pub definition_id: String,
    pub expected_revision: i64,
    pub name: String,
    pub description: Option<String>,
    pub dataset_selection_json: String,
    pub query_text: String,
    pub query_language_version: i64,
    pub query_fingerprint: Option<String>,
    pub time_strategy_json: String,
    pub field_selection_json: String,
    pub config_json: String,
    pub config_fingerprint: String,
    pub masking_profile_json: String,
    pub thresholds_json: String,
    pub limits_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRunRow {
    pub run_id: String,
    pub definition_id: String,
    pub definition_revision: i64,
    pub semantic_fingerprint: String,
    pub state: String,
    pub dataset_revs_json: String,
    pub query_fingerprint: Option<String>,
    pub query_language_version: i64,
    pub bounds_json: String,
    pub algorithm_id: String,
    pub algorithm_version: i64,
    pub config_fingerprint: String,
    pub progress_stage: Option<String>,
    pub counts_json: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub warnings_json: String,
    pub manifest_json: Option<String>,
    pub error_json: Option<String>,
    pub invalidation_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewAnalysisRun {
    pub run_id: String,
    pub definition_id: String,
    pub definition_revision: i64,
    pub semantic_fingerprint: String,
    pub dataset_revs_json: String,
    pub query_fingerprint: Option<String>,
    pub query_language_version: i64,
    pub bounds_json: String,
    pub algorithm_id: String,
    pub algorithm_version: i64,
    pub config_fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisFindingRow {
    pub finding_id: String,
    pub origin: String,
    pub finding_schema_version: i64,
    pub rule_id: String,
    pub rule_version: i64,
    pub run_id: String,
    pub subject_json: String,
    pub title: String,
    pub explanation: String,
    pub calculation_json: String,
    pub severity: String,
    pub severity_rule_json: String,
    pub confidence: Option<String>,
    pub contributing_json: String,
    pub examples_json: String,
    pub state_json: String,
    pub created_at: String,
    pub revision: i64,
}

#[derive(Debug, Clone)]
pub struct NewAnalysisFinding {
    pub finding_id: String,
    pub finding_schema_version: i64,
    pub rule_id: String,
    pub rule_version: i64,
    pub run_id: String,
    pub subject_json: String,
    pub title: String,
    pub explanation: String,
    pub calculation_json: String,
    pub severity: String,
    pub severity_rule_json: String,
    pub confidence: Option<String>,
    pub contributing_json: String,
    pub examples_json: String,
    pub state_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedArtifactRow {
    pub artifact_id: String,
    pub run_id: String,
    pub kind: String,
    pub rel_path: String,
    pub row_count: i64,
    pub byte_size: i64,
    pub sha256: String,
    pub schema_version: i64,
    pub created_at: String,
}

// ---- column lists ------------------------------------------------------------

const DEF_COLS: &str = "definition_id, definition_schema_version, kind, name, description, \
     dataset_selection_json, query_text, query_language_version, query_fingerprint, \
     time_strategy_json, field_selection_json, algorithm_id, algorithm_version, config_json, \
     config_fingerprint, masking_profile_json, thresholds_json, limits_json, created_at, \
     updated_at, revision";

const RUN_COLS: &str = "run_id, definition_id, definition_revision, semantic_fingerprint, state, \
     dataset_revs_json, query_fingerprint, query_language_version, bounds_json, algorithm_id, \
     algorithm_version, config_fingerprint, progress_stage, counts_json, started_at, finished_at, \
     warnings_json, manifest_json, error_json, invalidation_reason";

const FINDING_COLS: &str = "finding_id, origin, finding_schema_version, rule_id, rule_version, \
     run_id, subject_json, title, explanation, calculation_json, severity, severity_rule_json, \
     confidence, contributing_json, examples_json, state_json, created_at, revision";

const ARTIFACT_COLS: &str =
    "artifact_id, run_id, kind, rel_path, row_count, byte_size, sha256, schema_version, created_at";

fn map_definition(r: &rusqlite::Row<'_>) -> rusqlite::Result<AnalysisDefinitionRow> {
    Ok(AnalysisDefinitionRow {
        definition_id: r.get(0)?,
        definition_schema_version: r.get(1)?,
        kind: r.get(2)?,
        name: r.get(3)?,
        description: r.get(4)?,
        dataset_selection_json: r.get(5)?,
        query_text: r.get(6)?,
        query_language_version: r.get(7)?,
        query_fingerprint: r.get(8)?,
        time_strategy_json: r.get(9)?,
        field_selection_json: r.get(10)?,
        algorithm_id: r.get(11)?,
        algorithm_version: r.get(12)?,
        config_json: r.get(13)?,
        config_fingerprint: r.get(14)?,
        masking_profile_json: r.get(15)?,
        thresholds_json: r.get(16)?,
        limits_json: r.get(17)?,
        created_at: r.get(18)?,
        updated_at: r.get(19)?,
        revision: r.get(20)?,
    })
}

fn map_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<AnalysisRunRow> {
    Ok(AnalysisRunRow {
        run_id: r.get(0)?,
        definition_id: r.get(1)?,
        definition_revision: r.get(2)?,
        semantic_fingerprint: r.get(3)?,
        state: r.get(4)?,
        dataset_revs_json: r.get(5)?,
        query_fingerprint: r.get(6)?,
        query_language_version: r.get(7)?,
        bounds_json: r.get(8)?,
        algorithm_id: r.get(9)?,
        algorithm_version: r.get(10)?,
        config_fingerprint: r.get(11)?,
        progress_stage: r.get(12)?,
        counts_json: r.get(13)?,
        started_at: r.get(14)?,
        finished_at: r.get(15)?,
        warnings_json: r.get(16)?,
        manifest_json: r.get(17)?,
        error_json: r.get(18)?,
        invalidation_reason: r.get(19)?,
    })
}

fn map_finding(r: &rusqlite::Row<'_>) -> rusqlite::Result<AnalysisFindingRow> {
    Ok(AnalysisFindingRow {
        finding_id: r.get(0)?,
        origin: r.get(1)?,
        finding_schema_version: r.get(2)?,
        rule_id: r.get(3)?,
        rule_version: r.get(4)?,
        run_id: r.get(5)?,
        subject_json: r.get(6)?,
        title: r.get(7)?,
        explanation: r.get(8)?,
        calculation_json: r.get(9)?,
        severity: r.get(10)?,
        severity_rule_json: r.get(11)?,
        confidence: r.get(12)?,
        contributing_json: r.get(13)?,
        examples_json: r.get(14)?,
        state_json: r.get(15)?,
        created_at: r.get(16)?,
        revision: r.get(17)?,
    })
}

fn map_artifact(r: &rusqlite::Row<'_>) -> rusqlite::Result<DerivedArtifactRow> {
    Ok(DerivedArtifactRow {
        artifact_id: r.get(0)?,
        run_id: r.get(1)?,
        kind: r.get(2)?,
        rel_path: r.get(3)?,
        row_count: r.get(4)?,
        byte_size: r.get(5)?,
        sha256: r.get(6)?,
        schema_version: r.get(7)?,
        created_at: r.get(8)?,
    })
}

impl MetaDb {
    // ---- analysis definitions -----------------------------------------------

    pub fn create_analysis_definition(
        &self,
        new: &NewAnalysisDefinition,
    ) -> Result<AnalysisDefinitionRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let ts = now();
        tx.execute(
            "INSERT INTO analysis_definitions
               (definition_id, definition_schema_version, kind, name, description,
                dataset_selection_json, query_text, query_language_version, query_fingerprint,
                time_strategy_json, field_selection_json, algorithm_id, algorithm_version,
                config_json, config_fingerprint, masking_profile_json, thresholds_json,
                limits_json, created_at, updated_at, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                     ?17, ?18, ?19, ?19, 1)",
            params![
                new.definition_id,
                new.definition_schema_version,
                new.kind,
                new.name,
                new.description,
                new.dataset_selection_json,
                new.query_text,
                new.query_language_version,
                new.query_fingerprint,
                new.time_strategy_json,
                new.field_selection_json,
                new.algorithm_id,
                new.algorithm_version,
                new.config_json,
                new.config_fingerprint,
                new.masking_profile_json,
                new.thresholds_json,
                new.limits_json,
                ts,
            ],
        )?;
        let row = get_definition_tx(&tx, &new.definition_id)?;
        record_history(
            &tx,
            None,
            "analysis_definition",
            &new.definition_id,
            1,
            "created",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn update_analysis_definition(
        &self,
        edit: &AnalysisDefinitionEdit,
    ) -> Result<AnalysisDefinitionRow, WorkspaceError> {
        let mut conn = self.raw();
        let tx = conn.transaction()?;
        let n = tx.execute(
            "UPDATE analysis_definitions SET
                name = ?1, description = ?2, dataset_selection_json = ?3, query_text = ?4,
                query_language_version = ?5, query_fingerprint = ?6, time_strategy_json = ?7,
                field_selection_json = ?8, config_json = ?9, config_fingerprint = ?10,
                masking_profile_json = ?11, thresholds_json = ?12, limits_json = ?13,
                updated_at = ?14, revision = revision + 1
             WHERE definition_id = ?15 AND revision = ?16",
            params![
                edit.name,
                edit.description,
                edit.dataset_selection_json,
                edit.query_text,
                edit.query_language_version,
                edit.query_fingerprint,
                edit.time_strategy_json,
                edit.field_selection_json,
                edit.config_json,
                edit.config_fingerprint,
                edit.masking_profile_json,
                edit.thresholds_json,
                edit.limits_json,
                now(),
                edit.definition_id,
                edit.expected_revision,
            ],
        )?;
        if n == 0 {
            return Err(stale_or_missing(
                &tx,
                "analysis_definitions",
                "definition_id",
                "analysis_definition",
                &edit.definition_id,
                edit.expected_revision,
            ));
        }
        let row = get_definition_tx(&tx, &edit.definition_id)?;
        record_history(
            &tx,
            None,
            "analysis_definition",
            &edit.definition_id,
            row.revision,
            "updated",
            &payload(&row)?,
            "{}",
        )?;
        tx.commit()?;
        Ok(row)
    }

    pub fn get_analysis_definition(
        &self,
        id: &str,
    ) -> Result<Option<AnalysisDefinitionRow>, WorkspaceError> {
        let conn = self.raw();
        Ok(conn
            .query_row(
                &format!("SELECT {DEF_COLS} FROM analysis_definitions WHERE definition_id = ?1"),
                params![id],
                map_definition,
            )
            .optional()?)
    }

    pub fn list_analysis_definitions(&self) -> Result<Vec<AnalysisDefinitionRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {DEF_COLS} FROM analysis_definitions ORDER BY created_at, definition_id"
        ))?;
        let rows = stmt
            .query_map([], map_definition)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- analysis runs (two-phase, immutable once finished) -----------------

    /// Inserts the run record `pending` — BEFORE any derived byte is
    /// written, so an interruption always leaves an honest tombstone.
    pub fn start_analysis_run(
        &self,
        new: &NewAnalysisRun,
    ) -> Result<AnalysisRunRow, WorkspaceError> {
        let conn = self.raw();
        conn.execute(
            "INSERT INTO analysis_runs
               (run_id, definition_id, definition_revision, semantic_fingerprint, state,
                dataset_revs_json, query_fingerprint, query_language_version, bounds_json,
                algorithm_id, algorithm_version, config_fingerprint, counts_json,
                started_at, warnings_json)
             VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?6, ?7, ?8, ?9, ?10, ?11, '{}', ?12, '[]')",
            params![
                new.run_id,
                new.definition_id,
                new.definition_revision,
                new.semantic_fingerprint,
                new.dataset_revs_json,
                new.query_fingerprint,
                new.query_language_version,
                new.bounds_json,
                new.algorithm_id,
                new.algorithm_version,
                new.config_fingerprint,
                now(),
            ],
        )?;
        get_run_conn(&conn, &new.run_id)
    }

    /// pending → running (job body picked the run up).
    pub fn mark_analysis_run_running(&self, run_id: &str) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        let n = conn.execute(
            "UPDATE analysis_runs SET state = 'running' WHERE run_id = ?1 AND state = 'pending'",
            params![run_id],
        )?;
        if n == 0 {
            return Err(WorkspaceError::Invalid(format!(
                "run {run_id} is not pending; two-phase order is pending -> running -> finished"
            )));
        }
        Ok(())
    }

    /// Progress update; only meaningful (and only allowed) while running.
    pub fn update_analysis_run_progress(
        &self,
        run_id: &str,
        stage: &str,
        counts_json: &str,
    ) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        let n = conn.execute(
            "UPDATE analysis_runs SET progress_stage = ?1, counts_json = ?2
             WHERE run_id = ?3 AND state = 'running'",
            params![stage, counts_json, run_id],
        )?;
        if n == 0 {
            return Err(WorkspaceError::Invalid(format!(
                "run {run_id} is not running; progress applies only to running runs"
            )));
        }
        Ok(())
    }

    /// Finishes a run exactly once: `completed`, `cancelled`, or
    /// `failed`, only from `pending`/`running`. A finished run is
    /// immutable — finishing again is refused, never overwritten.
    pub fn finish_analysis_run(
        &self,
        run_id: &str,
        terminal_state: &str,
        counts_json: &str,
        manifest_json: Option<&str>,
        error_json: Option<&str>,
    ) -> Result<AnalysisRunRow, WorkspaceError> {
        if !matches!(terminal_state, "completed" | "cancelled" | "failed") {
            return Err(WorkspaceError::Invalid(format!(
                "invalid terminal state {terminal_state:?} (completed|cancelled|failed)"
            )));
        }
        if terminal_state == "completed" && manifest_json.is_none() {
            return Err(WorkspaceError::Invalid(
                "a completed run must record its result manifest".into(),
            ));
        }
        let conn = self.raw();
        let n = conn.execute(
            "UPDATE analysis_runs SET state = ?1, counts_json = ?2, manifest_json = ?3,
                error_json = ?4, finished_at = ?5, progress_stage = NULL
             WHERE run_id = ?6 AND state IN ('pending', 'running')",
            params![
                terminal_state,
                counts_json,
                manifest_json,
                error_json,
                now(),
                run_id
            ],
        )?;
        if n == 0 {
            return Err(WorkspaceError::Invalid(format!(
                "run {run_id} is not unfinished; finished runs are immutable"
            )));
        }
        get_run_conn(&conn, run_id)
    }

    /// completed → stale, recording why. History stays: the run and its
    /// results remain readable; it is no longer "currently matching".
    pub fn mark_analysis_run_stale(
        &self,
        run_id: &str,
        reason: &str,
    ) -> Result<AnalysisRunRow, WorkspaceError> {
        let conn = self.raw();
        let n = conn.execute(
            "UPDATE analysis_runs SET state = 'stale', invalidation_reason = ?1
             WHERE run_id = ?2 AND state = 'completed'",
            params![reason, run_id],
        )?;
        if n == 0 {
            return Err(WorkspaceError::Invalid(format!(
                "run {run_id} is not completed; only completed runs go stale"
            )));
        }
        get_run_conn(&conn, run_id)
    }

    pub fn get_analysis_run(&self, run_id: &str) -> Result<Option<AnalysisRunRow>, WorkspaceError> {
        let conn = self.raw();
        Ok(conn
            .query_row(
                &format!("SELECT {RUN_COLS} FROM analysis_runs WHERE run_id = ?1"),
                params![run_id],
                map_run,
            )
            .optional()?)
    }

    pub fn list_analysis_runs(
        &self,
        definition_id: Option<&str>,
    ) -> Result<Vec<AnalysisRunRow>, WorkspaceError> {
        let conn = self.raw();
        let rows = match definition_id {
            Some(def) => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {RUN_COLS} FROM analysis_runs WHERE definition_id = ?1
                     ORDER BY started_at DESC, run_id"
                ))?;
                let rows = stmt
                    .query_map(params![def], map_run)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
            None => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {RUN_COLS} FROM analysis_runs ORDER BY started_at DESC, run_id"
                ))?;
                let rows = stmt
                    .query_map([], map_run)?
                    .collect::<Result<Vec<_>, _>>()?;
                rows
            }
        };
        Ok(rows)
    }

    /// Crash recovery: finishes every `pending`/`running` run as
    /// `failed (job/interrupted)`. Idempotent — finished runs are no
    /// longer pending/running. Returns the finished run ids.
    pub fn fail_interrupted_analysis_runs(&self) -> Result<Vec<String>, WorkspaceError> {
        let ids: Vec<String> = {
            let conn = self.raw();
            let mut stmt = conn.prepare(
                "SELECT run_id FROM analysis_runs WHERE state IN ('pending', 'running')
                 ORDER BY run_id",
            )?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        // The guard is dropped: finish_analysis_run takes the
        // (non-reentrant) connection itself.
        let err = serde_json::json!({
            "code": "job/interrupted",
            "message": "analysis run was interrupted by application shutdown; staged derived data was discarded",
            "recovered": true,
        })
        .to_string();
        for run_id in &ids {
            self.finish_analysis_run(run_id, "failed", "{}", None, Some(&err))?;
        }
        Ok(ids)
    }

    /// Deletes an UNREFERENCED run (cleanup/retention): refused while
    /// findings reference it. Derived-artifact rows cascade; the caller
    /// removes the derived files. WP6 extends this guard to evidence,
    /// report, and bundle references before any UI exposes deletion.
    pub fn delete_unreferenced_analysis_run(&self, run_id: &str) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        let findings: i64 = conn.query_row(
            "SELECT COUNT(*) FROM analysis_findings WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )?;
        if findings > 0 {
            return Err(WorkspaceError::Invalid(format!(
                "run {run_id} is referenced by {findings} finding(s); referenced runs are never silently deleted"
            )));
        }
        let n = conn.execute(
            "DELETE FROM analysis_runs WHERE run_id = ?1",
            params![run_id],
        )?;
        if n == 0 {
            return Err(WorkspaceError::MissingEntity {
                kind: "analysis_run",
                id: run_id.to_string(),
            });
        }
        Ok(())
    }

    // ---- deterministic findings ---------------------------------------------

    /// Inserts a deterministic finding produced by a completed run.
    /// Findings are execution products: inserted whole, never updated.
    pub fn record_analysis_finding(
        &self,
        new: &NewAnalysisFinding,
    ) -> Result<AnalysisFindingRow, WorkspaceError> {
        let conn = self.raw();
        let run_state: Option<String> = conn
            .query_row(
                "SELECT state FROM analysis_runs WHERE run_id = ?1",
                params![new.run_id],
                |r| r.get(0),
            )
            .optional()?;
        match run_state.as_deref() {
            Some("completed") => {}
            Some(other) => {
                return Err(WorkspaceError::Invalid(format!(
                    "findings evaluate completed runs only; run {} is {other}",
                    new.run_id
                )))
            }
            None => {
                return Err(WorkspaceError::MissingEntity {
                    kind: "analysis_run",
                    id: new.run_id.clone(),
                })
            }
        }
        conn.execute(
            "INSERT INTO analysis_findings
               (finding_id, origin, finding_schema_version, rule_id, rule_version, run_id,
                subject_json, title, explanation, calculation_json, severity,
                severity_rule_json, confidence, contributing_json, examples_json, state_json,
                created_at, revision)
             VALUES (?1, 'deterministic', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, 1)",
            params![
                new.finding_id,
                new.finding_schema_version,
                new.rule_id,
                new.rule_version,
                new.run_id,
                new.subject_json,
                new.title,
                new.explanation,
                new.calculation_json,
                new.severity,
                new.severity_rule_json,
                new.confidence,
                new.contributing_json,
                new.examples_json,
                new.state_json,
                now(),
            ],
        )?;
        conn.query_row(
            &format!("SELECT {FINDING_COLS} FROM analysis_findings WHERE finding_id = ?1"),
            params![new.finding_id],
            map_finding,
        )
        .optional()?
        .ok_or_else(|| WorkspaceError::MissingEntity {
            kind: "analysis_finding",
            id: new.finding_id.clone(),
        })
    }

    pub fn list_analysis_findings(
        &self,
        run_id: &str,
    ) -> Result<Vec<AnalysisFindingRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FINDING_COLS} FROM analysis_findings WHERE run_id = ?1
             ORDER BY finding_id"
        ))?;
        let rows = stmt
            .query_map(params![run_id], map_finding)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // ---- derived artifact catalog -------------------------------------------

    pub fn record_derived_artifact(
        &self,
        artifact: &DerivedArtifactRow,
    ) -> Result<(), WorkspaceError> {
        let conn = self.raw();
        conn.execute(
            "INSERT INTO derived_artifacts
               (artifact_id, run_id, kind, rel_path, row_count, byte_size, sha256,
                schema_version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                artifact.artifact_id,
                artifact.run_id,
                artifact.kind,
                artifact.rel_path,
                artifact.row_count,
                artifact.byte_size,
                artifact.sha256,
                artifact.schema_version,
                artifact.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_derived_artifacts(
        &self,
        run_id: &str,
    ) -> Result<Vec<DerivedArtifactRow>, WorkspaceError> {
        let conn = self.raw();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ARTIFACT_COLS} FROM derived_artifacts WHERE run_id = ?1 ORDER BY rel_path"
        ))?;
        let rows = stmt
            .query_map(params![run_id], map_artifact)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn get_definition_tx(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
) -> Result<AnalysisDefinitionRow, WorkspaceError> {
    tx.query_row(
        &format!("SELECT {DEF_COLS} FROM analysis_definitions WHERE definition_id = ?1"),
        params![id],
        map_definition,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "analysis_definition",
        id: id.to_string(),
    })
}

/// Queries on an already-held connection guard — `MetaDb::raw()` is a
/// non-reentrant mutex, so helpers must never re-lock it.
fn get_run_conn(
    conn: &rusqlite::Connection,
    run_id: &str,
) -> Result<AnalysisRunRow, WorkspaceError> {
    conn.query_row(
        &format!("SELECT {RUN_COLS} FROM analysis_runs WHERE run_id = ?1"),
        params![run_id],
        map_run,
    )
    .optional()?
    .ok_or_else(|| WorkspaceError::MissingEntity {
        kind: "analysis_run",
        id: run_id.to_string(),
    })
}
