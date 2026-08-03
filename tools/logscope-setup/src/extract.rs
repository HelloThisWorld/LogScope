//! Staged extraction, manifest verification and rollback.
//!
//! The contract from ADR-0018 is that a failed or cancelled extraction must
//! leave **no launchable partial installation**. That is achieved by
//! extracting into a staging directory beside the destination, verifying
//! every file against `package-manifest.json`, and only then publishing the
//! staging directory into place. Any failure removes the staging directory
//! and leaves the destination exactly as it was found.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::payload::hex;

/// The subset of `package-manifest.json` the extractor needs. Unknown
/// fields are ignored so a newer payload manifest stays readable.
#[derive(Debug, Deserialize)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub files: Vec<ManifestEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug)]
pub enum ExtractError {
    /// A ZIP entry tried to escape the destination.
    UnsafePath(String),
    /// Two entries resolve to the same destination path.
    Collision(String),
    /// The payload has no `package-manifest.json`, so nothing can be verified.
    MissingManifest,
    ManifestParse(String),
    /// A file listed in the manifest was not extracted.
    MissingFile(String),
    /// A file was extracted that the manifest does not declare.
    UnexpectedFile(String),
    SizeMismatch {
        path: String,
        expected: u64,
        actual: u64,
    },
    DigestMismatch {
        path: String,
        expected: String,
        actual: String,
    },
    /// The user asked to stop. Not an error condition, but it aborts.
    Cancelled,
    /// The destination already holds files the extractor did not put there.
    DestinationNotEmpty(PathBuf),
    Zip(String),
    Io(std::io::Error),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::UnsafePath(p) => {
                write!(f, "refusing entry '{p}': it points outside the destination")
            }
            ExtractError::Collision(p) => write!(f, "two payload entries resolve to '{p}'"),
            ExtractError::MissingManifest => {
                write!(
                    f,
                    "the payload has no package-manifest.json to verify against"
                )
            }
            ExtractError::ManifestParse(e) => write!(f, "package-manifest.json is unreadable: {e}"),
            ExtractError::MissingFile(p) => write!(f, "'{p}' is declared but was not extracted"),
            ExtractError::UnexpectedFile(p) => {
                write!(f, "'{p}' was extracted but is not declared in the manifest")
            }
            ExtractError::SizeMismatch {
                path,
                expected,
                actual,
            } => write!(f, "'{path}': expected {expected} bytes, wrote {actual}"),
            ExtractError::DigestMismatch {
                path,
                expected,
                actual,
            } => write!(f, "'{path}': expected SHA-256 {expected}, wrote {actual}"),
            ExtractError::Cancelled => write!(f, "extraction was cancelled"),
            ExtractError::DestinationNotEmpty(p) => write!(
                f,
                "'{}' already contains files. Choose an empty folder, or confirm \
                 replacement explicitly.",
                p.display()
            ),
            ExtractError::Zip(e) => write!(f, "the payload archive is unreadable: {e}"),
            ExtractError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for ExtractError {
    fn from(e: std::io::Error) -> Self {
        ExtractError::Io(e)
    }
}

/// Progress reported while extracting. The UI renders these; the core has
/// no opinion about how.
pub enum Progress<'a> {
    Started {
        total: usize,
    },
    File {
        index: usize,
        total: usize,
        path: &'a str,
    },
    Verifying,
    Publishing,
    Done {
        root: &'a Path,
    },
}

/// Rejects any archive path that could escape the destination.
///
/// Absolute paths, drive letters, UNC prefixes, `..` traversal and
/// backslash separators are all refused rather than sanitised, because a
/// payload we build ourselves has no legitimate reason to contain them.
///
/// The rules are deliberately **platform-independent**. `Path::components`
/// is not: on Windows `C:` parses as a `Prefix` and is refused, while on
/// Unix it is an ordinary `Normal` component and would be accepted. A
/// payload is extracted on whichever platform the user runs, and the
/// shared core must classify a hostile path identically everywhere, so the
/// colon and separator rules below are applied by inspection rather than
/// delegated to the host's path parser.
fn safe_relative(raw: &str) -> Result<PathBuf, ExtractError> {
    if raw.is_empty() || raw.contains('\0') {
        return Err(ExtractError::UnsafePath(raw.to_string()));
    }
    // Normalise separators before inspection so `a\..\..\b` cannot slip past.
    let normalised = raw.replace('\\', "/");

    // A colon anywhere means a drive reference (`C:/x`), a device path
    // (`\\?\C:`) or an NTFS alternate data stream (`f.txt:hidden`). None is
    // legitimate in a payload, and only Windows' parser would catch them.
    if normalised.contains(':') {
        return Err(ExtractError::UnsafePath(raw.to_string()));
    }
    // Leading separator is absolute on every platform once normalised.
    if normalised.starts_with('/') {
        return Err(ExtractError::UnsafePath(raw.to_string()));
    }

    let candidate = Path::new(&normalised);
    let mut out = PathBuf::new();
    for c in candidate.components() {
        match c {
            Component::Normal(part) => {
                let s = part.to_string_lossy();
                // Windows reserves trailing dots/spaces and device names.
                if s.ends_with('.') || s.ends_with(' ') || is_reserved_device(&s) {
                    return Err(ExtractError::UnsafePath(raw.to_string()));
                }
                out.push(part);
            }
            // Everything else (RootDir, Prefix, ParentDir, CurDir) is refused.
            _ => return Err(ExtractError::UnsafePath(raw.to_string())),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(ExtractError::UnsafePath(raw.to_string()));
    }
    Ok(out)
}

fn is_reserved_device(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Strips the single shared top-level directory used by the portable ZIP
/// (`LogScope-<version>-windows-x64-portable/...`) so the setup and the ZIP
/// produce byte-equivalent payload trees.
fn strip_common_root(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut roots = paths.iter().filter_map(|p| p.components().next());
    let first = roots.next()?;
    let Component::Normal(root) = first else {
        return None;
    };
    // Every entry must live under the same root, and none may *be* the root.
    if paths.iter().any(|p| p.components().count() < 2) {
        return None;
    }
    if roots.all(|c| c == first) {
        Some(PathBuf::from(root))
    } else {
        None
    }
}

/// Extracts `payload` into `dest`, verifying against the embedded manifest.
///
/// `should_cancel` is polled between entries so a long extraction stays
/// responsive without the core knowing anything about the UI.
pub fn extract_verified(
    payload: &[u8],
    dest: &Path,
    mut on_progress: impl FnMut(Progress<'_>),
    should_cancel: &dyn Fn() -> bool,
) -> Result<PackageManifest, ExtractError> {
    let reader = std::io::Cursor::new(payload);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| ExtractError::Zip(e.to_string()))?;

    // Pass 1: validate every path before writing anything at all.
    let mut planned: Vec<(usize, PathBuf)> = Vec::new();
    for i in 0..zip.len() {
        let entry = zip
            .by_index(i)
            .map_err(|e| ExtractError::Zip(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        planned.push((i, safe_relative(entry.name())?));
    }
    let rels: Vec<PathBuf> = planned.iter().map(|(_, p)| p.clone()).collect();
    let common = strip_common_root(&rels);
    let planned: Vec<(usize, PathBuf)> = planned
        .into_iter()
        .map(|(i, p)| {
            let stripped = match &common {
                Some(root) => p.strip_prefix(root).unwrap_or(&p).to_path_buf(),
                None => p,
            };
            (i, stripped)
        })
        .collect();

    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    for (_, p) in &planned {
        let key = p.to_string_lossy().to_ascii_lowercase(); // Windows is case-insensitive
        if seen.insert(key, ()).is_some() {
            return Err(ExtractError::Collision(p.to_string_lossy().into_owned()));
        }
    }

    on_progress(Progress::Started {
        total: planned.len(),
    });

    // Stage beside the destination so publication is a rename, never a
    // partially-populated destination.
    let staging = staging_dir_for(dest);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;

    let result = (|| -> Result<PackageManifest, ExtractError> {
        let total = planned.len();
        for (n, (index, rel)) in planned.iter().enumerate() {
            if should_cancel() {
                return Err(ExtractError::Cancelled);
            }
            let shown = rel.to_string_lossy();
            on_progress(Progress::File {
                index: n + 1,
                total,
                path: &shown,
            });

            let mut entry = zip
                .by_index(*index)
                .map_err(|e| ExtractError::Zip(e.to_string()))?;
            let out = staging.join(rel);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut buf = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut buf)?;
            std::fs::write(&out, &buf)?;
        }

        on_progress(Progress::Verifying);
        let manifest = verify_against_manifest(&staging)?;

        on_progress(Progress::Publishing);
        publish(&staging, dest)?;
        Ok(manifest)
    })();

    match result {
        Ok(manifest) => {
            on_progress(Progress::Done { root: dest });
            Ok(manifest)
        }
        Err(e) => {
            // Rollback: the destination is left exactly as it was found.
            let _ = std::fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

fn staging_dir_for(dest: &Path) -> PathBuf {
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "logscope".into());
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{name}.logscope-staging"))
}

/// Reads the staged `package-manifest.json` and checks every declared file's
/// size and digest, and that nothing undeclared was written.
pub fn verify_against_manifest(root: &Path) -> Result<PackageManifest, ExtractError> {
    let manifest_path = root.join("package-manifest.json");
    if !manifest_path.exists() {
        return Err(ExtractError::MissingManifest);
    }
    let raw = std::fs::read_to_string(&manifest_path)?;
    let manifest: PackageManifest =
        serde_json::from_str(&raw).map_err(|e| ExtractError::ManifestParse(e.to_string()))?;

    for entry in &manifest.files {
        let rel = safe_relative(&entry.path)?;
        let path = root.join(&rel);
        if !path.is_file() {
            return Err(ExtractError::MissingFile(entry.path.clone()));
        }
        let bytes = std::fs::read(&path)?;
        if bytes.len() as u64 != entry.bytes {
            return Err(ExtractError::SizeMismatch {
                path: entry.path.clone(),
                expected: entry.bytes,
                actual: bytes.len() as u64,
            });
        }
        let actual = hex(&<[u8; 32]>::from(Sha256::digest(&bytes)));
        if actual != entry.sha256.to_ascii_lowercase() {
            return Err(ExtractError::DigestMismatch {
                path: entry.path.clone(),
                expected: entry.sha256.clone(),
                actual,
            });
        }
    }

    // Nothing undeclared may ship: an extra executable in the payload is a
    // release-blocking fault, not a curiosity.
    let declared: BTreeMap<String, ()> = manifest
        .files
        .iter()
        .map(|e| (e.path.replace('\\', "/").to_ascii_lowercase(), ()))
        .chain(std::iter::once(("package-manifest.json".into(), ())))
        .collect();
    for found in walk_files(root)? {
        let rel = found
            .strip_prefix(root)
            .unwrap_or(&found)
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        if !declared.contains_key(&rel) {
            return Err(ExtractError::UnexpectedFile(rel));
        }
    }

    Ok(manifest)
}

fn walk_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// Moves verified staging into place. The destination must not already hold
/// files the extractor did not put there.
fn publish(staging: &Path, dest: &Path) -> Result<(), ExtractError> {
    if dest.exists() {
        let occupied = std::fs::read_dir(dest)?.next().is_some();
        if occupied {
            return Err(ExtractError::DestinationNotEmpty(dest.to_path_buf()));
        }
        std::fs::remove_dir(dest)?;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(staging, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These must hold identically on every platform. `C:/abs.txt` was
    /// accepted on Unix and refused on Windows until the colon rule stopped
    /// delegating the decision to the host's path parser — caught by the
    /// macOS shared-core leg, not by local Windows runs.
    #[test]
    fn traversal_and_absolute_paths_are_refused() {
        for bad in [
            "../escape.txt",
            "a/../../escape.txt",
            "/abs.txt",
            "C:/abs.txt",
            "c:abs.txt",
            "\\\\server\\share\\f.txt",
            "\\\\?\\C:\\abs.txt",
            "a\\..\\..\\escape.txt",
            "ok/../../escape.txt",
            "payload.txt:hidden",
            "",
            "nul",
            "COM1.txt",
            "trailing.",
            "trailing ",
        ] {
            assert!(safe_relative(bad).is_err(), "should have refused {bad:?}");
        }
    }

    #[test]
    fn ordinary_relative_paths_are_accepted() {
        assert_eq!(
            safe_relative("logscope.exe").unwrap(),
            Path::new("logscope.exe")
        );
        assert_eq!(
            safe_relative("webview2/msedgewebview2.exe").unwrap(),
            Path::new("webview2/msedgewebview2.exe")
        );
    }

    #[test]
    fn common_root_is_stripped_only_when_shared() {
        let shared = vec![
            PathBuf::from("LogScope-1.0.0/logscope.exe"),
            PathBuf::from("LogScope-1.0.0/README.md"),
        ];
        assert_eq!(
            strip_common_root(&shared),
            Some(PathBuf::from("LogScope-1.0.0"))
        );

        let mixed = vec![PathBuf::from("a/x.txt"), PathBuf::from("b/y.txt")];
        assert_eq!(strip_common_root(&mixed), None);

        // A file at the root means there is no single root directory.
        let flat = vec![PathBuf::from("logscope.exe")];
        assert_eq!(strip_common_root(&flat), None);
    }
}
