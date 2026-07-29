//! `manifest.json`: workspace identity and version inventory.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::WorkspaceError;

/// Highest manifest layout version this build understands.
pub const SUPPORTED_MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub workspace_id: String,
    pub name: String,
    /// Product version that last wrote this workspace.
    pub product_version: String,
    /// Metadata schema version (max applied migration).
    pub schema_version: i64,
    pub created_at: String,
    /// Signals with at least one published dataset.
    #[serde(default)]
    pub available_signals: Vec<String>,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self, WorkspaceError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| WorkspaceError::io(path.display().to_string(), e))?;
        let manifest: Manifest = serde_json::from_str(&text)?;
        if manifest.manifest_version > SUPPORTED_MANIFEST_VERSION {
            return Err(WorkspaceError::ManifestTooNew {
                found: manifest.manifest_version,
                supported: SUPPORTED_MANIFEST_VERSION,
            });
        }
        Ok(manifest)
    }

    /// Atomic save: write to a temp file, flush to disk, then rename over
    /// the destination (replace-existing rename is atomic for files on the
    /// same NTFS volume).
    pub fn save(&self, path: &Path) -> Result<(), WorkspaceError> {
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self)?;
        {
            use std::io::Write as _;
            let mut f = std::fs::File::create(&tmp)
                .map_err(|e| WorkspaceError::io(tmp.display().to_string(), e))?;
            f.write_all(text.as_bytes())
                .map_err(|e| WorkspaceError::io(tmp.display().to_string(), e))?;
            f.sync_all()
                .map_err(|e| WorkspaceError::io(tmp.display().to_string(), e))?;
        }
        std::fs::rename(&tmp, path)
            .map_err(|e| WorkspaceError::io(path.display().to_string(), e))?;
        Ok(())
    }
}
