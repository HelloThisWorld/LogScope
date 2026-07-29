//! Versioned public logical-source contracts (v0.0 drafts).
//!
//! v0.0 implements enough of `StaticFileSet` to prove the contract; archive
//! bundles, watched folders, and OTLP sessions are drafted here so v0.1 and
//! v0.7 extend rather than replace the shapes. Live watching is explicitly
//! out of scope until v0.7.

use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::IngestError;

pub const SOURCE_CONTRACT_VERSION: u32 = 1;

/// Compatibility tier of a source/profile pairing.
///
/// - `A`: structured source with full canonical fidelity (typed fields,
///   timestamps with offsets, correlation IDs).
/// - `B`: structured source requiring documented assumptions (e.g. timezone
///   or severity mapping).
/// - `C`: text source parsed via profile rules (framing/multiline heuristics
///   involved).
/// - `D`: preserved raw records only (no reliable field extraction yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompatibilityTier {
    A,
    B,
    C,
    D,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalClassification {
    Logs,
    Metrics,
    Spans,
    Mixed,
    Unknown,
}

/// Identity of one physical source file, captured at registration time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalSourceIdentity {
    pub path: String,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
    /// BLAKE3 hex of the complete raw file bytes (streamed).
    pub content_hash: String,
}

/// Role of a file inside a rotation family (draft; static snapshot only in
/// v0.0/v0.1 — no live rotation following before v0.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum FamilyRole {
    Current,
    Rotated { index: u32 },
    CompressedHistory { index: u32 },
}

/// Logical source kinds (public contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogicalSourceSpec {
    /// A fixed set of files selected by the user (v0.0: implemented).
    StaticFileSet { paths: Vec<String> },
    /// A zip/gzip bundle with safety limits (v0.1).
    ArchiveBundle { path: String },
    /// A folder watched for growth/rotation (v0.7; draft only).
    WatchedFolder { path: String, pattern: String },
    /// A live OTLP receiver session (v0.7; experimental spike in v0.0).
    OtlpSession { session_id: String },
}

/// Parser identity manifest (public contract draft).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParserManifest {
    pub parser_id: String,
    pub version: String,
    /// Content families this parser accepts, e.g. `csv`, `jsonl`.
    pub formats: Vec<String>,
}

/// Resumable position in a source file (ingestion/checkpoint ledger entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct IngestCheckpoint {
    /// Last fully processed record number (1-based).
    pub record_number: u64,
    /// Byte offset just past the last fully processed record, when the
    /// format supports safe seeking; compressed streams restart the entry
    /// with deterministic deduplication instead (documented behavior).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_offset: Option<u64>,
}

/// Streams a file to compute its `PhysicalSourceIdentity` without loading
/// it into memory.
pub fn fingerprint_file(path: &Path) -> Result<PhysicalSourceIdentity, IngestError> {
    let meta =
        std::fs::metadata(path).map_err(|e| IngestError::io(path.display().to_string(), e))?;
    let mut file =
        std::fs::File::open(path).map_err(|e| IngestError::io(path.display().to_string(), e))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| IngestError::io(path.display().to_string(), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let modified_at = meta
        .modified()
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
    Ok(PhysicalSourceIdentity {
        path: path.display().to_string(),
        size_bytes: meta.len(),
        modified_at,
        content_hash: hasher.finalize().to_hex().to_string(),
    })
}
