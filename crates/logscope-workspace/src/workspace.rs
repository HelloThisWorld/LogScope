//! Workspace lifecycle: create, open, recover, publish, close.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::WorkspaceError;
use crate::layout::WorkspaceLayout;
use crate::manifest::{Manifest, SUPPORTED_MANIFEST_VERSION};
use crate::meta::{LedgerEntry, MetaDb, PublishVersions, SegmentToPublish, Signal};

/// What the recovery sweep did while opening a workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecoveryReport {
    /// Staging directories discarded (jobs that never published).
    pub discarded_staging_dirs: Vec<String>,
    /// Data files removed because no committed segment row references them.
    pub removed_orphan_files: Vec<String>,
    /// Jobs found in a non-terminal state and marked failed/interrupted.
    pub interrupted_jobs: Vec<String>,
    /// Staging-status datasets with no published segments, deleted.
    pub discarded_staging_datasets: Vec<String>,
    /// Report-artifact / bundle-export / analysis-run records found
    /// unfinished and completed as failed (`kind:id`) — honest
    /// tombstones, never deleted.
    #[serde(default)]
    pub interrupted_case_records: Vec<String>,
}

impl RecoveryReport {
    pub fn is_clean(&self) -> bool {
        self.discarded_staging_dirs.is_empty()
            && self.removed_orphan_files.is_empty()
            && self.interrupted_jobs.is_empty()
            && self.discarded_staging_datasets.is_empty()
            && self.interrupted_case_records.is_empty()
    }
}

/// An open workspace.
pub struct Workspace {
    pub layout: WorkspaceLayout,
    pub manifest: Manifest,
    pub meta: MetaDb,
    pub recovery: RecoveryReport,
}

impl Workspace {
    /// Creates a new workspace in `root` (which must not already contain
    /// one), writes the manifest, initializes the metadata database, and
    /// returns the open workspace.
    pub fn create(
        root: &Path,
        name: &str,
        product_version: &str,
    ) -> Result<Workspace, WorkspaceError> {
        let layout = WorkspaceLayout::new(root);
        if layout.manifest_path().exists() {
            return Err(WorkspaceError::AlreadyExists(root.display().to_string()));
        }
        layout.ensure_dirs()?;
        let meta = MetaDb::open(&layout.db_path())?;
        let schema_version = meta.schema_version()?;
        let manifest = Manifest {
            manifest_version: SUPPORTED_MANIFEST_VERSION,
            workspace_id: format!("ws-{}", uuid::Uuid::new_v4()),
            name: name.to_string(),
            product_version: product_version.to_string(),
            schema_version,
            created_at: chrono::Utc::now().to_rfc3339(),
            available_signals: vec![],
        };
        manifest.save(&layout.manifest_path())?;
        meta.set_info("workspace_id", &manifest.workspace_id)?;
        meta.set_info("created_by_product_version", product_version)?;
        Ok(Workspace {
            layout,
            manifest,
            meta,
            recovery: RecoveryReport::default(),
        })
    }

    /// Opens an existing workspace: validates the manifest, migrates the
    /// metadata schema forward, and runs the crash-recovery sweep.
    pub fn open(root: &Path, product_version: &str) -> Result<Workspace, WorkspaceError> {
        let layout = WorkspaceLayout::new(root);
        if !layout.manifest_path().exists() {
            return Err(WorkspaceError::NotAWorkspace(root.display().to_string()));
        }
        let mut manifest = Manifest::load(&layout.manifest_path())?;
        layout.ensure_dirs()?;
        let meta = MetaDb::open(&layout.db_path())?;
        let schema_version = meta.schema_version()?;

        let mut ws = Workspace {
            layout,
            manifest: manifest.clone(),
            meta,
            recovery: RecoveryReport::default(),
        };
        ws.recovery = ws.recover_interrupted_state()?;

        // Persist a bumped schema/product version in the manifest if the
        // migration moved it forward.
        if manifest.schema_version != schema_version || manifest.product_version != product_version
        {
            manifest.schema_version = schema_version;
            manifest.product_version = product_version.to_string();
            manifest.save(&ws.layout.manifest_path())?;
            ws.manifest = manifest;
        }
        Ok(ws)
    }

    /// Recovery sweep, run on open (no jobs can be running then):
    /// 1. non-terminal jobs -> failed (`job/interrupted`);
    /// 2. every staging/<job> directory is discarded (imports are
    ///    re-runnable; user source files are never touched);
    /// 3. dataset-directory files not referenced by any committed segment
    ///    row are removed (crash between file move and commit);
    /// 4. `staging`-status datasets without segments are deleted;
    /// 5. `running` report-artifact / bundle-export records are finished
    ///    as failed (`job/interrupted`) — the tombstone is completed,
    ///    never deleted, so an interrupted generation stays on record;
    /// 6. `pending`/`running` analysis runs are finished the same way
    ///    (`analysis_run:<id>` entries in the same recovery field).
    fn recover_interrupted_state(&self) -> Result<RecoveryReport, WorkspaceError> {
        let mut interrupted = self.meta.fail_interrupted_case_records()?;
        interrupted.extend(
            self.meta
                .fail_interrupted_analysis_runs()?
                .into_iter()
                .map(|id| format!("analysis_run:{id}")),
        );
        let mut report = RecoveryReport {
            interrupted_case_records: interrupted,
            ..RecoveryReport::default()
        };

        for job in self.meta.list_jobs()? {
            if matches!(job.status.as_str(), "running" | "pending" | "paused") {
                let err = serde_json::json!({
                    "code": "job/interrupted",
                    "message": "job was interrupted by application shutdown and its staged state was discarded",
                    "recovered": true,
                })
                .to_string();
                self.meta
                    .update_job_status(&job.job_id, "failed", Some(&err))?;
                report.interrupted_jobs.push(job.job_id);
            }
        }

        let staging = self.layout.staging_dir();
        if staging.exists() {
            for entry in std::fs::read_dir(&staging)
                .map_err(|e| WorkspaceError::io(staging.display().to_string(), e))?
            {
                let entry =
                    entry.map_err(|e| WorkspaceError::io(staging.display().to_string(), e))?;
                let path = entry.path();
                remove_path_all(&path)?;
                report
                    .discarded_staging_dirs
                    .push(entry.file_name().to_string_lossy().to_string());
            }
        }

        for dataset in self.meta.list_datasets()? {
            let dir = self.layout.dataset_dir(&dataset.dataset_id);
            let referenced: std::collections::HashSet<String> = self
                .meta
                .all_segment_file_names(&dataset.dataset_id)?
                .into_iter()
                .collect();
            if dir.exists() {
                for entry in std::fs::read_dir(&dir)
                    .map_err(|e| WorkspaceError::io(dir.display().to_string(), e))?
                {
                    let entry =
                        entry.map_err(|e| WorkspaceError::io(dir.display().to_string(), e))?;
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    if !referenced.contains(&file_name) {
                        remove_path_all(&entry.path())?;
                        tracing::warn!(
                            dataset = %dataset.dataset_id,
                            file = %file_name,
                            "removed orphan data file with no committed segment row"
                        );
                        report
                            .removed_orphan_files
                            .push(format!("{}/{}", dataset.dataset_id, file_name));
                    }
                }
            }
            if dataset.status == "staging" && referenced.is_empty() {
                self.meta.delete_dataset(&dataset.dataset_id)?;
                report.discarded_staging_datasets.push(dataset.dataset_id);
            }
        }

        Ok(report)
    }

    /// Begins a staged import: creates `staging/<job_id>/`.
    pub fn begin_staging(&self, job_id: &str) -> Result<PathBuf, WorkspaceError> {
        let dir = self.layout.staging_job_dir(job_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| WorkspaceError::io(dir.display().to_string(), e))?;
        Ok(dir)
    }

    /// Discards a staged import (cancellation/failure). Existing datasets
    /// are untouched; the staging-status dataset row is removed.
    pub fn discard_staging(
        &self,
        job_id: &str,
        dataset_id: Option<&str>,
    ) -> Result<(), WorkspaceError> {
        let dir = self.layout.staging_job_dir(job_id);
        if dir.exists() {
            remove_path_all(&dir)?;
        }
        if let Some(ds) = dataset_id {
            if let Some(row) = self.meta.get_dataset(ds)? {
                if row.status == "staging" {
                    self.meta.delete_dataset(ds)?;
                }
            }
        }
        Ok(())
    }

    /// Atomically publishes staged segment files:
    /// files are renamed into `data/<dataset>/` first, then one metadata
    /// transaction makes them visible. A crash in between leaves orphan
    /// files that the next open removes.
    pub fn publish_staged_import(
        &self,
        job_id: &str,
        dataset_id: &str,
        staged_files: &[(PathBuf, SegmentToPublish)],
        ledger: &[LedgerEntry],
        versions: &PublishVersions,
    ) -> Result<(), WorkspaceError> {
        let dataset_dir = self.layout.dataset_dir(dataset_id);
        std::fs::create_dir_all(&dataset_dir)
            .map_err(|e| WorkspaceError::io(dataset_dir.display().to_string(), e))?;

        for (staged_path, seg) in staged_files {
            let dest = dataset_dir.join(&seg.file_name);
            std::fs::rename(staged_path, &dest)
                .map_err(|e| WorkspaceError::io(dest.display().to_string(), e))?;
        }

        let segments: Vec<SegmentToPublish> = staged_files.iter().map(|(_, s)| s.clone()).collect();
        self.meta
            .publish_import(job_id, dataset_id, &segments, ledger, versions)?;

        // Best-effort cleanup of the now-empty staging dir.
        let staging = self.layout.staging_job_dir(job_id);
        if staging.exists() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        Ok(())
    }

    /// Records a newly available signal in the manifest inventory.
    pub fn note_signal_available(&mut self, signal: Signal) -> Result<(), WorkspaceError> {
        let s = signal.as_str().to_string();
        if !self.manifest.available_signals.contains(&s) {
            self.manifest.available_signals.push(s);
            self.manifest.save(&self.layout.manifest_path())?;
        }
        Ok(())
    }

    /// Absolute paths of every published segment file for a dataset.
    pub fn segment_paths(&self, dataset_id: &str) -> Result<Vec<PathBuf>, WorkspaceError> {
        let dir = self.layout.dataset_dir(dataset_id);
        Ok(self
            .meta
            .all_segment_file_names(dataset_id)?
            .into_iter()
            .map(|f| dir.join(f))
            .collect())
    }
}

fn remove_path_all(path: &Path) -> Result<(), WorkspaceError> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    result.map_err(|e| WorkspaceError::io(path.display().to_string(), e))
}
