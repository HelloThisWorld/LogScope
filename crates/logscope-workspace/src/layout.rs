//! On-disk workspace layout.
//!
//! ```text
//! workspace/
//!   manifest.json
//!   workspace.db
//!   sources/          managed copies of source files (managed-copy mode)
//!   data/<dataset-id>/{logs,metrics,spans}-*.parquet
//!   indexes/          derived indexes (SQLite FTS5)
//!   attachments/      user attachments (future)
//!   reports/          exported reports (future)
//!   profiles/         workspace-local Import Profiles
//!   staging/          transient import state; recoverable/removable on launch
//! ```

use std::path::{Path, PathBuf};

use crate::error::WorkspaceError;

#[derive(Debug, Clone)]
pub struct WorkspaceLayout {
    root: PathBuf,
}

impl WorkspaceLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        WorkspaceLayout { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }
    pub fn db_path(&self) -> PathBuf {
        self.root.join("workspace.db")
    }
    pub fn sources_dir(&self) -> PathBuf {
        self.root.join("sources")
    }
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }
    pub fn dataset_dir(&self, dataset_id: &str) -> PathBuf {
        self.data_dir().join(dataset_id)
    }
    pub fn indexes_dir(&self) -> PathBuf {
        self.root.join("indexes")
    }
    pub fn fts_logs_path(&self) -> PathBuf {
        self.indexes_dir().join("fts-logs.db")
    }
    pub fn attachments_dir(&self) -> PathBuf {
        self.root.join("attachments")
    }
    pub fn reports_dir(&self) -> PathBuf {
        self.root.join("reports")
    }
    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }
    pub fn staging_job_dir(&self, job_id: &str) -> PathBuf {
        self.staging_dir().join(job_id)
    }
    /// Rebuildable derived analysis data, one directory per run
    /// (v0.4; disposable without losing canonical data).
    pub fn derived_analysis_dir(&self, run_id: &str) -> PathBuf {
        self.root.join("derived").join("analysis").join(run_id)
    }

    /// Creates every directory of the layout.
    pub fn ensure_dirs(&self) -> Result<(), WorkspaceError> {
        for dir in [
            self.root.clone(),
            self.sources_dir(),
            self.data_dir(),
            self.indexes_dir(),
            self.attachments_dir(),
            self.reports_dir(),
            self.profiles_dir(),
            self.staging_dir(),
        ] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| WorkspaceError::io(dir.display().to_string(), e))?;
        }
        Ok(())
    }
}
