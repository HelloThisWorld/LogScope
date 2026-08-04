//! Portable case bundles (v0.3 W7): `.logscope-case` export and
//! hostile-import.
//!
//! Export rules, in order of importance:
//!
//! - **Default-closed disclosure.** Every outbound string — case
//!   metadata, evidence envelopes (references included: they carry
//!   query text and source paths), marker labels — passes through the
//!   same [`Projection`] the reports use. When a profile is attached,
//!   the raw-data subset is **not** exported at all (the projection
//!   cannot yet guarantee parquet row content), and the manifest says
//!   so: scope `snapshot_only`, decision recorded. No unprojected raw
//!   data ever leaves under a profile.
//! - **Deterministic bytes.** Fixed ZIP timestamps, stable entry order,
//!   ordered data rows: the same workspace state exports byte-identical
//!   bundles.
//! - **The manifest is exhaustive.** Every entry is listed with type,
//!   size, and SHA-256; the ZIP may contain nothing the manifest does
//!   not name (hidden-payload defense, enforced at import).
//! - **Checksums are integrity aids, never authorship proof.**
//!
//! Import rules: validate EVERYTHING before extracting anything —
//! entry-path hardening (absolute, traversal, drive/ADS colons,
//! backslashes, reserved Windows names, control characters, trailing
//! dot/space, case-collisions, symlinks), count/depth/size limits,
//! manifest schema gate, exact entry-set equality — then stream-extract
//! into a staging directory with declared-size and checksum enforcement,
//! build the NEW isolated workspace, and rename it into place only when
//! everything held. Bundled reports are extracted as inert files and
//! never opened.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use logscope_jobs::JobError;
use logscope_query::EngineConnection;
use logscope_workspace::{
    BundleExportRow, NewEvidence, NewHypothesis, NewInvestigation, NewItem, NewMarker, Workspace,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::redact::{Projection, RedactionSummary};

pub const BUNDLE_SCHEMA_VERSION: u32 = 1;
/// Import limits — refusals, not truncations.
pub const MAX_ENTRIES: usize = 10_000;
pub const MAX_ENTRY_NAME_CHARS: usize = 512;
pub const MAX_ENTRY_DEPTH: usize = 8;
pub const MAX_ENTRY_BYTES: u64 = 1 << 30; // 1 GiB per entry
pub const MAX_TOTAL_BYTES: u64 = 4 << 30; // 4 GiB declared total
pub const MAX_MANIFEST_BYTES: u64 = 8 << 20; // 8 MiB
/// Export data-subset bounds (planner refuses beyond them).
pub const MAX_DATA_ROWS: usize = 200_000;

fn ws_err(e: logscope_workspace::WorkspaceError) -> JobError {
    JobError::new(e.code(), e.to_string())
}

fn io_err(context: &str, e: impl std::fmt::Display) -> JobError {
    JobError::new("bundle/io", format!("{context}: {e}"))
}

fn hostile(msg: impl std::fmt::Display) -> JobError {
    JobError::new("bundle/invalid", msg.to_string())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

// ---- manifest ----------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    /// `metadata | evidence | markers | queries | data | report | checksums`.
    pub entry_type: String,
    pub byte_size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub bundle_schema_version: u32,
    /// Oldest bundle schema an importer must understand to read this.
    pub min_compatible_version: u32,
    pub investigation_id: String,
    pub investigation_title: String,
    pub investigation_revision: i64,
    /// `full_within_declared_dataset_revision | included_subset_only | snapshot_only`.
    pub reproduction_scope: String,
    pub entries: Vec<ManifestEntry>,
    pub envelope_version: i64,
    pub app_version: String,
    /// Disclosure profile identity + honest application counts, when a
    /// profile shaped this bundle.
    pub disclosure: Option<DisclosureNote>,
    /// Explicit inclusion decisions, so an absence is always a decision
    /// on record and never an accident.
    pub inclusions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisclosureNote {
    pub profile_id: String,
    pub profile_name: String,
    pub profile_version: i64,
    pub summary: RedactionSummary,
}

// ---- export ------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct BundleOptions {
    pub redaction_profile_id: Option<String>,
    /// Include previously generated report artifacts that still exist on
    /// disk (explicit opt-in).
    pub include_reports: bool,
}

struct PlannedEntry {
    path: &'static str,
    entry_type: &'static str,
    bytes: Vec<u8>,
}

/// Projects a serde-serializable row through the disclosure projection
/// by walking its JSON form (strings, {name,value} fields, path keys).
fn project_row<T: Serialize>(
    row: &T,
    projection: Option<&Projection>,
    summary: &mut RedactionSummary,
) -> Result<serde_json::Value, JobError> {
    let json =
        serde_json::to_string(row).map_err(|e| JobError::new("bundle/serialize", e.to_string()))?;
    let projected = match projection {
        Some(p) => p.snapshot_json(&json, summary),
        None => json,
    };
    serde_json::from_str(&projected).map_err(|e| JobError::new("bundle/serialize", e.to_string()))
}

/// Exports one investigation as a `.logscope-case` bundle.
pub fn export_bundle(
    ws: &Workspace,
    engine: &EngineConnection,
    investigation_id: &str,
    destination: &Path,
    options: &BundleOptions,
) -> Result<BundleExportRow, JobError> {
    let investigation = ws
        .meta
        .get_investigation(investigation_id)
        .map_err(ws_err)?
        .ok_or_else(|| {
            JobError::new(
                "workspace/missing-entity",
                format!("investigation {investigation_id} does not exist"),
            )
        })?;

    if destination.exists() {
        return Err(JobError::new(
            "bundle/destination-exists",
            format!(
                "destination already exists: {} (choose a new file name)",
                destination.display()
            ),
        ));
    }
    let dir = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| hostile("destination has no parent directory"))?;
    std::fs::create_dir_all(dir).map_err(|e| io_err("create destination directory", e))?;

    // Load + compile the disclosure profile up front; a broken profile
    // refuses the whole export.
    let redaction = match options.redaction_profile_id.as_deref() {
        Some(pid) => {
            let profile = ws
                .meta
                .get_redaction_profile(pid)
                .map_err(ws_err)?
                .ok_or_else(|| {
                    JobError::new(
                        "workspace/missing-entity",
                        format!("redaction profile {pid} does not exist"),
                    )
                })?;
            let projection = Projection::compile(&profile.rules_json, &profile.posture_json)?;
            Some((profile, projection))
        }
        None => None,
    };
    let projection = redaction.as_ref().map(|(_, p)| p);
    let mut summary = RedactionSummary::default();
    let mut inclusions: BTreeMap<String, String> = BTreeMap::new();

    let bundle_id = format!("bun-{}", uuid::Uuid::new_v4());
    ws.meta
        .start_bundle_export(
            &bundle_id,
            investigation_id,
            &destination.display().to_string(),
        )
        .map_err(ws_err)?;

    let result = build_bundle(
        ws,
        engine,
        &investigation,
        destination,
        dir,
        options,
        projection,
        &mut summary,
        &mut inclusions,
        redaction.as_ref().map(|(p, _)| p),
    );

    match result {
        Ok((manifest_json, digest, size)) => ws
            .meta
            .finish_bundle_export(
                &bundle_id,
                "completed",
                Some(&manifest_json),
                Some(&digest),
                Some(size),
                None,
            )
            .map_err(ws_err),
        Err(e) => {
            let _ = ws.meta.finish_bundle_export(
                &bundle_id,
                "failed",
                None,
                None,
                None,
                serde_json::to_string(&e).ok().as_deref(),
            );
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_bundle(
    ws: &Workspace,
    engine: &EngineConnection,
    investigation: &logscope_workspace::InvestigationRow,
    destination: &Path,
    dir: &Path,
    options: &BundleOptions,
    projection: Option<&Projection>,
    summary: &mut RedactionSummary,
    inclusions: &mut BTreeMap<String, String>,
    profile: Option<&logscope_workspace::RedactionProfileRow>,
) -> Result<(String, String, i64), JobError> {
    let investigation_id = &investigation.investigation_id;
    let mut entries: Vec<PlannedEntry> = Vec::new();

    // Case metadata (each row projected; deterministic ordering comes
    // from the repositories' ORDER BY clauses).
    let inv_value = project_row(investigation, projection, summary)?;
    entries.push(PlannedEntry {
        path: "investigation/investigation.json",
        entry_type: "metadata",
        bytes: serde_json::to_vec_pretty(&inv_value).unwrap_or_default(),
    });

    let hypotheses = ws.meta.list_hypotheses(investigation_id).map_err(ws_err)?;
    let items = ws.meta.list_items(investigation_id, true).map_err(ws_err)?;
    let groups = ws
        .meta
        .list_evidence_groups(investigation_id)
        .map_err(ws_err)?;
    let hyp_values: Vec<_> = hypotheses
        .iter()
        .map(|h| project_row(h, projection, summary))
        .collect::<Result<_, _>>()?;
    let item_values: Vec<_> = items
        .iter()
        .map(|i| project_row(i, projection, summary))
        .collect::<Result<_, _>>()?;
    let group_values: Vec<_> = groups
        .iter()
        .map(|g| project_row(g, projection, summary))
        .collect::<Result<_, _>>()?;
    entries.push(PlannedEntry {
        path: "investigation/hypotheses.json",
        entry_type: "metadata",
        bytes: serde_json::to_vec_pretty(&hyp_values).unwrap_or_default(),
    });
    entries.push(PlannedEntry {
        path: "investigation/items.json",
        entry_type: "metadata",
        bytes: serde_json::to_vec_pretty(&item_values).unwrap_or_default(),
    });
    entries.push(PlannedEntry {
        path: "investigation/groups.json",
        entry_type: "metadata",
        bytes: serde_json::to_vec_pretty(&group_values).unwrap_or_default(),
    });

    // Evidence as JSONL, one projected row per line.
    let evidence = ws
        .meta
        .list_evidence(investigation_id, true)
        .map_err(ws_err)?;
    let mut evidence_jsonl = Vec::new();
    let mut referenced_ids: BTreeSet<String> = BTreeSet::new();
    for ev in &evidence {
        collect_record_ids(ev, &mut referenced_ids);
        let v = project_row(ev, projection, summary)?;
        serde_json::to_writer(&mut evidence_jsonl, &v)
            .map_err(|e| JobError::new("bundle/serialize", e.to_string()))?;
        evidence_jsonl.push(b'\n');
    }
    entries.push(PlannedEntry {
        path: "evidence/evidence.jsonl",
        entry_type: "evidence",
        bytes: evidence_jsonl,
    });

    let markers = ws
        .meta
        .list_markers(investigation_id, true)
        .map_err(ws_err)?;
    let marker_values: Vec<_> = markers
        .iter()
        .map(|m| project_row(m, projection, summary))
        .collect::<Result<_, _>>()?;
    entries.push(PlannedEntry {
        path: "timeline/markers.json",
        entry_type: "markers",
        bytes: serde_json::to_vec_pretty(&marker_values).unwrap_or_default(),
    });

    // Saved searches referenced by query pins, projected.
    let saved = referenced_saved_searches(ws, &evidence)?;
    let saved_values: Vec<_> = saved
        .iter()
        .map(|s| project_row(s, projection, summary))
        .collect::<Result<_, _>>()?;
    entries.push(PlannedEntry {
        path: "queries/queries.json",
        entry_type: "queries",
        bytes: serde_json::to_vec_pretty(&saved_values).unwrap_or_default(),
    });

    // Data subset. Under a disclosure profile no raw rows leave at all —
    // the projection cannot yet guarantee parquet content, and
    // default-closed beats convenient.
    let scope;
    if projection.is_some() {
        scope = "snapshot_only";
        inclusions.insert(
            "data".into(),
            "excluded: a disclosure profile is attached and raw rows cannot be projected yet"
                .into(),
        );
    } else if referenced_ids.is_empty() {
        scope = "snapshot_only";
        inclusions.insert(
            "data".into(),
            "excluded: no evidence references canonical records".into(),
        );
    } else if referenced_ids.len() > MAX_DATA_ROWS {
        return Err(JobError::new(
            "bundle/data-too-large",
            format!(
                "{} referenced records exceed the bundle bound of {MAX_DATA_ROWS}",
                referenced_ids.len()
            ),
        ));
    } else {
        let parquet = export_data_subset(ws, engine, &referenced_ids, dir)?;
        match parquet {
            Some(bytes) => {
                scope = "included_subset_only";
                inclusions.insert(
                    "data".into(),
                    format!("included: {} referenced records", referenced_ids.len()),
                );
                entries.push(PlannedEntry {
                    path: "data/logs.parquet",
                    entry_type: "data",
                    bytes,
                });
            }
            None => {
                scope = "snapshot_only";
                inclusions.insert(
                    "data".into(),
                    "excluded: no published segments cover the referenced records".into(),
                );
            }
        }
    }

    // Reports: explicit opt-in, copied only if the artifact file still
    // exists and its checksum still matches its record (an altered file
    // is refused, not silently shipped).
    let mut report_entries: Vec<(String, Vec<u8>)> = Vec::new();
    if options.include_reports {
        for art in ws
            .meta
            .list_report_artifacts(investigation_id)
            .map_err(ws_err)?
        {
            if art.status != "completed" {
                continue;
            }
            let Some(expected) = art.checksum_sha256.as_deref() else {
                continue;
            };
            let p = PathBuf::from(&art.destination_path);
            let Ok(bytes) = std::fs::read(&p) else {
                inclusions.insert(
                    format!("report:{}", art.artifact_id),
                    "excluded: artifact file no longer exists".into(),
                );
                continue;
            };
            if hex(&Sha256::digest(&bytes)) != expected {
                return Err(JobError::new(
                    "bundle/report-modified",
                    format!(
                        "report artifact {} no longer matches its recorded checksum; \
                         refusing to bundle an altered file",
                        art.artifact_id
                    ),
                ));
            }
            let ext = if art.format == "html" { "html" } else { "md" };
            report_entries.push((format!("reports/{}.{ext}", art.artifact_id), bytes));
            inclusions.insert(format!("report:{}", art.artifact_id), "included".into());
        }
        report_entries.sort_by(|a, b| a.0.cmp(&b.0));
    } else {
        inclusions.insert("reports".into(), "excluded: not requested".into());
    }

    // Manifest + checksums.
    let mut manifest_entries: Vec<ManifestEntry> = entries
        .iter()
        .map(|e| ManifestEntry {
            path: e.path.to_string(),
            entry_type: e.entry_type.to_string(),
            byte_size: e.bytes.len() as u64,
            sha256: hex(&Sha256::digest(&e.bytes)),
        })
        .collect();
    for (path, bytes) in &report_entries {
        manifest_entries.push(ManifestEntry {
            path: path.clone(),
            entry_type: "report".to_string(),
            byte_size: bytes.len() as u64,
            sha256: hex(&Sha256::digest(bytes)),
        });
    }

    let manifest = BundleManifest {
        bundle_schema_version: BUNDLE_SCHEMA_VERSION,
        min_compatible_version: BUNDLE_SCHEMA_VERSION,
        investigation_id: investigation_id.clone(),
        investigation_title: match projection {
            Some(p) => p.text(&investigation.title, summary),
            None => investigation.title.clone(),
        },
        investigation_revision: investigation.revision,
        reproduction_scope: scope.to_string(),
        entries: manifest_entries,
        envelope_version: logscope_case::EVIDENCE_ENVELOPE_VERSION,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        disclosure: profile.map(|p| DisclosureNote {
            profile_id: p.profile_id.clone(),
            profile_name: p.name.clone(),
            profile_version: p.profile_version,
            summary: summary.clone(),
        }),
        inclusions: inclusions.clone(),
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| JobError::new("bundle/serialize", e.to_string()))?;
    let manifest_json = String::from_utf8_lossy(&manifest_bytes).to_string();

    let mut checksums = String::new();
    checksums.push_str(&format!(
        "{}  manifest.json\n",
        hex(&Sha256::digest(&manifest_bytes))
    ));
    for e in &manifest.entries {
        checksums.push_str(&format!("{}  {}\n", e.sha256, e.path));
    }

    // Deterministic ZIP: fixed timestamps, stable order, staged write.
    let temp = dir.join(format!(
        ".{}.partial-{}",
        destination
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "bundle".into()),
        uuid::Uuid::new_v4()
    ));
    let write_result = (|| -> Result<(), JobError> {
        let file = std::fs::File::create(&temp).map_err(|e| io_err("create staging file", e))?;
        let mut zipw = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default());
        let mut write_entry = |name: &str, bytes: &[u8]| -> Result<(), JobError> {
            zipw.start_file(name, opts)
                .map_err(|e| io_err("zip entry", e))?;
            zipw.write_all(bytes).map_err(|e| io_err("zip write", e))
        };
        write_entry("manifest.json", &manifest_bytes)?;
        for e in &entries {
            write_entry(e.path, &e.bytes)?;
        }
        for (path, bytes) in &report_entries {
            write_entry(path, bytes)?;
        }
        write_entry("checksums.sha256", checksums.as_bytes())?;
        let mut file = zipw.finish().map_err(|e| io_err("zip finish", e))?;
        file.flush().map_err(|e| io_err("flush", e))?;
        file.sync_all().map_err(|e| io_err("sync", e))?;
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, destination) {
        let _ = std::fs::remove_file(&temp);
        return Err(io_err("publish bundle", e));
    }

    let published = std::fs::read(destination).map_err(|e| io_err("read published bundle", e))?;
    let digest = hex(&Sha256::digest(&published));
    Ok((manifest_json, digest, published.len() as i64))
}

/// Record ids referenced by an evidence reference (event, selection,
/// and bounded representatives), for the closure planner.
fn collect_record_ids(ev: &logscope_workspace::EvidenceRow, out: &mut BTreeSet<String>) {
    use logscope_case::envelope::{decode_reference, DecodeOutcome, EvidenceReference};
    if let DecodeOutcome::Decoded(reference) =
        decode_reference(ev.envelope_version, &ev.reference_json)
    {
        match reference {
            EvidenceReference::Event(e) => {
                out.insert(e.record_id);
            }
            EvidenceReference::Selection(s) => out.extend(s.record_ids),
            EvidenceReference::Query(q) => out.extend(q.representative_ids),
            EvidenceReference::ExplorerGroup(g) => out.extend(g.representative_ids),
            EvidenceReference::HistogramInterval(i) => out.extend(i.representative_ids),
            EvidenceReference::ItemRef(_) => {}
        }
    }
}

fn referenced_saved_searches(
    ws: &Workspace,
    evidence: &[logscope_workspace::EvidenceRow],
) -> Result<Vec<logscope_workspace::SavedSearchRow>, JobError> {
    use logscope_case::envelope::{decode_reference, DecodeOutcome, EvidenceReference};
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for ev in evidence {
        if let DecodeOutcome::Decoded(EvidenceReference::Query(q)) =
            decode_reference(ev.envelope_version, &ev.reference_json)
        {
            if let Some(id) = q.saved_search_id {
                wanted.insert(id);
            }
        }
    }
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let all = ws.meta.list_saved_searches().map_err(ws_err)?;
    Ok(all
        .into_iter()
        .filter(|s| wanted.contains(&s.saved_search_id))
        .collect())
}

/// Writes the referenced records to a parquet file via the engine and
/// returns its bytes. Rows are ordered by record id for determinism.
fn export_data_subset(
    ws: &Workspace,
    engine: &EngineConnection,
    record_ids: &BTreeSet<String>,
    staging_dir: &Path,
) -> Result<Option<Vec<u8>>, JobError> {
    let datasets = ws.meta.list_datasets().map_err(ws_err)?;
    let mut files: Vec<PathBuf> = Vec::new();
    for d in datasets.iter().filter(|d| d.status == "published") {
        for p in ws.segment_paths(&d.dataset_id).map_err(ws_err)? {
            if p.exists() {
                files.push(p);
            }
        }
    }
    if files.is_empty() {
        return Ok(None);
    }

    let out_path = staging_dir.join(format!(".bundle-data-{}.parquet", uuid::Uuid::new_v4()));
    let conn = engine.raw();
    conn.execute_batch("CREATE OR REPLACE TEMP TABLE bundle_ids(record_id VARCHAR); BEGIN")
        .map_err(|e| JobError::new("bundle/data", e.to_string()))?;
    {
        let mut stmt = conn
            .prepare("INSERT INTO bundle_ids VALUES (?)")
            .map_err(|e| JobError::new("bundle/data", e.to_string()))?;
        for id in record_ids {
            stmt.execute([id.as_str()])
                .map_err(|e| JobError::new("bundle/data", e.to_string()))?;
        }
    }
    conn.execute_batch("COMMIT")
        .map_err(|e| JobError::new("bundle/data", e.to_string()))?;
    let file_list = files
        .iter()
        .map(|f| {
            format!(
                "'{}'",
                f.display()
                    .to_string()
                    .replace('\'', "''")
                    .replace('\\', "/")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "COPY (SELECT p.* FROM read_parquet([{file_list}]) p \
         JOIN bundle_ids b ON p.record_id = b.record_id ORDER BY p.record_id) \
         TO '{}' (FORMAT PARQUET)",
        out_path
            .display()
            .to_string()
            .replace('\'', "''")
            .replace('\\', "/")
    );
    conn.execute_batch(&sql)
        .map_err(|e| JobError::new("bundle/data", e.to_string()))?;
    let bytes = std::fs::read(&out_path).map_err(|e| io_err("read data subset", e))?;
    let _ = std::fs::remove_file(&out_path);
    Ok(Some(bytes))
}

// ---- import: path hardening --------------------------------------------------

const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Validates one bundle entry path. Platform-independent by inspection:
/// every rule is enforced by string analysis, never by the host's path
/// parser (the same discipline as the setup extractor).
pub fn check_entry_path(raw: &str) -> Result<(), JobError> {
    if raw.is_empty() {
        return Err(hostile("empty entry name"));
    }
    if raw.chars().count() > MAX_ENTRY_NAME_CHARS {
        return Err(hostile(format!(
            "entry name exceeds {MAX_ENTRY_NAME_CHARS} chars"
        )));
    }
    if raw.contains('\\') {
        return Err(hostile(format!("backslash in entry name: {raw:?}")));
    }
    // A colon is a drive reference, device path, or NTFS alternate data
    // stream on Windows; none is legitimate in a bundle.
    if raw.contains(':') {
        return Err(hostile(format!("colon in entry name: {raw:?}")));
    }
    if raw.starts_with('/') {
        return Err(hostile(format!("absolute entry name: {raw:?}")));
    }
    if raw.chars().any(|c| c.is_control()) {
        return Err(hostile(format!("control character in entry name: {raw:?}")));
    }
    let segments: Vec<&str> = raw.split('/').collect();
    if segments.len() > MAX_ENTRY_DEPTH {
        return Err(hostile(format!(
            "entry deeper than {MAX_ENTRY_DEPTH}: {raw:?}"
        )));
    }
    for seg in &segments {
        if seg.is_empty() {
            return Err(hostile(format!("empty path segment in {raw:?}")));
        }
        if *seg == "." || *seg == ".." {
            return Err(hostile(format!("path traversal in entry name: {raw:?}")));
        }
        if seg.ends_with('.') || seg.ends_with(' ') {
            return Err(hostile(format!(
                "segment ends with dot/space (Windows hazard): {raw:?}"
            )));
        }
        let stem = seg.split('.').next().unwrap_or(seg).to_ascii_lowercase();
        if RESERVED_NAMES.contains(&stem.as_str()) {
            return Err(hostile(format!("reserved device name in entry: {raw:?}")));
        }
    }
    Ok(())
}

// ---- import ------------------------------------------------------------------

/// Everything the importer accepted, for the provenance record.
#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    pub investigation_id: String,
    pub evidence: usize,
    pub hypotheses: usize,
    pub items: usize,
    pub markers: usize,
    pub saved_searches: usize,
    pub reports: usize,
    pub data_included: bool,
    pub original_investigation_revision: i64,
}

/// Imports a bundle into a NEW isolated workspace at `new_root`. The
/// workspace is built in a staging directory and renamed into place only
/// after every validation and insert held; a failure leaves nothing.
pub fn import_bundle(
    bundle_path: &Path,
    new_root: &Path,
    workspace_name: &str,
    product_version: &str,
) -> Result<ImportSummary, JobError> {
    if new_root.exists() {
        return Err(hostile(format!(
            "destination already exists: {} (bundles import into a new workspace)",
            new_root.display()
        )));
    }
    let bundle_bytes = std::fs::read(bundle_path).map_err(|e| io_err("read bundle", e))?;
    let bundle_checksum = hex(&Sha256::digest(&bundle_bytes));

    let reader = std::io::Cursor::new(&bundle_bytes);
    let mut zipr =
        zip::ZipArchive::new(reader).map_err(|e| hostile(format!("not a ZIP archive: {e}")))?;

    // ---- validate the whole central directory before extracting anything.
    if zipr.len() > MAX_ENTRIES {
        return Err(hostile(format!(
            "{} entries exceed {MAX_ENTRIES}",
            zipr.len()
        )));
    }
    let mut declared_total: u64 = 0;
    let mut seen_folded: BTreeMap<String, String> = BTreeMap::new();
    let mut entry_names: Vec<String> = Vec::with_capacity(zipr.len());
    for i in 0..zipr.len() {
        let entry = zipr
            .by_index_raw(i)
            .map_err(|e| hostile(format!("unreadable entry {i}: {e}")))?;
        let name = entry.name().to_string();
        check_entry_path(&name)?;
        if entry.is_dir() {
            return Err(hostile(format!(
                "directory entries are not allowed: {name:?}"
            )));
        }
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                return Err(hostile(format!("symlink entry refused: {name:?}")));
            }
        }
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(hostile(format!(
                "entry {name:?} declares {} bytes (limit {MAX_ENTRY_BYTES})",
                entry.size()
            )));
        }
        declared_total = declared_total.saturating_add(entry.size());
        let folded = name.to_lowercase();
        if let Some(previous) = seen_folded.insert(folded, name.clone()) {
            return Err(hostile(format!(
                "case-colliding or duplicate entries: {previous:?} and {name:?}"
            )));
        }
        entry_names.push(name);
    }
    if declared_total > MAX_TOTAL_BYTES {
        return Err(hostile(format!(
            "declared total {declared_total} bytes exceeds {MAX_TOTAL_BYTES}"
        )));
    }

    // ---- manifest gate.
    let manifest_bytes = read_entry_exact(&mut zipr, "manifest.json", MAX_MANIFEST_BYTES)?;
    let manifest: BundleManifest = serde_json::from_str(
        std::str::from_utf8(&manifest_bytes).map_err(|_| hostile("manifest is not UTF-8"))?,
    )
    .map_err(|e| hostile(format!("manifest does not parse: {e}")))?;
    if manifest.min_compatible_version > BUNDLE_SCHEMA_VERSION {
        return Err(JobError::new(
            "bundle/unsupported-version",
            format!(
                "this bundle requires schema {} but this build supports {}; \
                 update LogScope to import it",
                manifest.min_compatible_version, BUNDLE_SCHEMA_VERSION
            ),
        ));
    }

    // The manifest must name exactly the ZIP's entries (minus itself and
    // the checksums file): nothing hidden, nothing missing.
    let mut expected: BTreeSet<String> = manifest.entries.iter().map(|e| e.path.clone()).collect();
    expected.insert("manifest.json".into());
    expected.insert("checksums.sha256".into());
    let actual: BTreeSet<String> = entry_names.iter().cloned().collect();
    if expected != actual {
        let hiddens: Vec<_> = actual.difference(&expected).collect();
        let missing: Vec<_> = expected.difference(&actual).collect();
        return Err(hostile(format!(
            "manifest/entry mismatch — not in manifest: {hiddens:?}; missing from archive: {missing:?}"
        )));
    }

    // ---- extract to staging with checksum + declared-size enforcement.
    let staging = new_root.with_file_name(format!(
        "{}.partial-{}",
        new_root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "import".into()),
        uuid::Uuid::new_v4()
    ));
    let outcome = (|| -> Result<ImportSummary, JobError> {
        std::fs::create_dir_all(&staging).map_err(|e| io_err("create staging", e))?;
        let mut staged: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        for e in &manifest.entries {
            let bytes = read_entry_exact(&mut zipr, &e.path, MAX_ENTRY_BYTES)?;
            if bytes.len() as u64 != e.byte_size {
                return Err(hostile(format!(
                    "entry {:?} is {} bytes but the manifest declares {}",
                    e.path,
                    bytes.len(),
                    e.byte_size
                )));
            }
            let digest = hex(&Sha256::digest(&bytes));
            if digest != e.sha256 {
                return Err(hostile(format!(
                    "entry {:?} fails its checksum (manifest {}, actual {digest})",
                    e.path, e.sha256
                )));
            }
            staged.insert(e.path.clone(), bytes);
        }

        // ---- build the new workspace inside the staging directory.
        let ws_dir = staging.join("ws");
        let ws = Workspace::create(&ws_dir, workspace_name, product_version).map_err(ws_err)?;
        let summary = insert_case(&ws, &manifest, &staged)?;

        // Reports and data are inert files, never opened.
        let mut report_count = 0usize;
        let mut data_included = false;
        for (path, bytes) in &staged {
            if let Some(name) = path.strip_prefix("reports/") {
                let dst = ws_dir.join("imported-reports").join(name);
                std::fs::create_dir_all(dst.parent().unwrap())
                    .map_err(|e| io_err("imported-reports", e))?;
                std::fs::write(&dst, bytes).map_err(|e| io_err("write report", e))?;
                report_count += 1;
            } else if path == "data/logs.parquet" {
                let dst = ws_dir.join("imported-data").join("logs.parquet");
                std::fs::create_dir_all(dst.parent().unwrap())
                    .map_err(|e| io_err("imported-data", e))?;
                std::fs::write(&dst, bytes).map_err(|e| io_err("write data", e))?;
                data_included = true;
            }
        }

        let summary = ImportSummary {
            reports: report_count,
            data_included,
            ..summary
        };
        ws.meta
            .record_bundle_import(
                &format!("imp-{}", uuid::Uuid::new_v4()),
                &bundle_path.display().to_string(),
                &bundle_checksum,
                std::str::from_utf8(&manifest_bytes).unwrap_or("{}"),
                &serde_json::to_string(&summary).unwrap_or_else(|_| "{}".into()),
            )
            .map_err(ws_err)?;
        drop(ws);

        // Move the finished workspace into place, then drop the staging shell.
        std::fs::rename(staging.join("ws"), new_root)
            .map_err(|e| io_err("publish workspace", e))?;
        let _ = std::fs::remove_dir_all(&staging);
        Ok(summary)
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    outcome
}

fn read_entry_exact(
    zipr: &mut zip::ZipArchive<std::io::Cursor<&Vec<u8>>>,
    name: &str,
    limit: u64,
) -> Result<Vec<u8>, JobError> {
    let mut entry = zipr
        .by_name(name)
        .map_err(|e| hostile(format!("missing entry {name:?}: {e}")))?;
    if entry.size() > limit {
        return Err(hostile(format!("entry {name:?} exceeds {limit} bytes")));
    }
    let declared = entry.size();
    // Read with a hard stop just past the declared size: a decompression
    // bomb that lies about its size is cut off, not trusted.
    let mut bytes = Vec::with_capacity(declared.min(1 << 20) as usize);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let n = entry
            .read(&mut chunk)
            .map_err(|e| hostile(format!("read {name:?}: {e}")))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() as u64 > declared {
            return Err(hostile(format!(
                "entry {name:?} inflates past its declared {declared} bytes"
            )));
        }
    }
    Ok(bytes)
}

/// Inserts the case metadata from staged entries. IDs are preserved
/// (provenance); duplicates within the bundle refuse the import.
fn insert_case(
    ws: &Workspace,
    manifest: &BundleManifest,
    staged: &BTreeMap<String, Vec<u8>>,
) -> Result<ImportSummary, JobError> {
    let get = |path: &str| -> Result<&Vec<u8>, JobError> {
        staged
            .get(path)
            .ok_or_else(|| hostile(format!("required entry missing: {path:?}")))
    };
    let parse = |bytes: &[u8], what: &str| -> Result<serde_json::Value, JobError> {
        serde_json::from_slice(bytes).map_err(|e| hostile(format!("{what} does not parse: {e}")))
    };

    let inv: serde_json::Value = parse(get("investigation/investigation.json")?, "investigation")?;
    let str_of = |v: &serde_json::Value, key: &str| -> String {
        v.get(key)
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let opt_str = |v: &serde_json::Value, key: &str| -> Option<String> {
        v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string())
    };
    let opt_i64 = |v: &serde_json::Value, key: &str| v.get(key).and_then(|x| x.as_i64());

    let investigation_id = str_of(&inv, "investigation_id");
    if investigation_id.is_empty() {
        return Err(hostile("investigation entry has no id"));
    }
    if investigation_id != manifest.investigation_id {
        return Err(hostile(format!(
            "manifest names investigation {:?} but the entry carries {investigation_id:?}",
            manifest.investigation_id
        )));
    }
    ws.meta
        .create_investigation(&NewInvestigation {
            investigation_id: investigation_id.clone(),
            title: str_of(&inv, "title"),
            description: opt_str(&inv, "description"),
            severity: opt_str(&inv, "severity"),
            owner_text: opt_str(&inv, "owner_text"),
            tags_json: inv
                .get("tags_json")
                .and_then(|x| x.as_str())
                .unwrap_or("[]")
                .to_string(),
            incident_started_at: opt_i64(&inv, "incident_started_at"),
            window_start: opt_i64(&inv, "window_start"),
            window_end: opt_i64(&inv, "window_end"),
        })
        .map_err(|e| hostile(format!("investigation insert failed: {e}")))?;

    let mut counts = (0usize, 0usize, 0usize, 0usize); // hyp, item, marker, evidence
    for h in parse(get("investigation/hypotheses.json")?, "hypotheses")?
        .as_array()
        .unwrap_or(&Vec::new())
    {
        ws.meta
            .create_hypothesis(&NewHypothesis {
                hypothesis_id: str_of(h, "hypothesis_id"),
                investigation_id: investigation_id.clone(),
                statement: str_of(h, "statement"),
                rationale: opt_str(h, "rationale"),
            })
            .map_err(|e| hostile(format!("hypothesis insert failed: {e}")))?;
        counts.0 += 1;
    }
    for it in parse(get("investigation/items.json")?, "items")?
        .as_array()
        .unwrap_or(&Vec::new())
    {
        ws.meta
            .create_item(&NewItem {
                item_id: str_of(it, "item_id"),
                investigation_id: investigation_id.clone(),
                kind: str_of(it, "kind"),
                content: str_of(it, "content"),
                task_status: opt_str(it, "task_status"),
                question_status: opt_str(it, "question_status"),
            })
            .map_err(|e| hostile(format!("item insert failed: {e}")))?;
        counts.1 += 1;
    }
    for m in parse(get("timeline/markers.json")?, "markers")?
        .as_array()
        .unwrap_or(&Vec::new())
    {
        ws.meta
            .create_marker(&NewMarker {
                marker_id: str_of(m, "marker_id"),
                investigation_id: investigation_id.clone(),
                kind: str_of(m, "kind"),
                label: str_of(m, "label"),
                description: opt_str(m, "description"),
                at_nanos: opt_i64(m, "at_nanos"),
                end_nanos: opt_i64(m, "end_nanos"),
                original_tz_offset_min: opt_i64(m, "original_tz_offset_min"),
                original_time_text: opt_str(m, "original_time_text"),
            })
            .map_err(|e| hostile(format!("marker insert failed: {e}")))?;
        counts.2 += 1;
    }

    let evidence_bytes = get("evidence/evidence.jsonl")?;
    for line in evidence_bytes.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let ev = parse(line, "evidence line")?;
        ws.meta
            .insert_evidence(&NewEvidence {
                evidence_id: str_of(&ev, "evidence_id"),
                investigation_id: investigation_id.clone(),
                envelope_version: opt_i64(&ev, "envelope_version").unwrap_or(0),
                kind: str_of(&ev, "kind"),
                signal: str_of(&ev, "signal"),
                title: str_of(&ev, "title"),
                annotation: opt_str(&ev, "annotation"),
                relevance: opt_str(&ev, "relevance"),
                captured_investigation_revision: opt_i64(&ev, "captured_investigation_revision")
                    .unwrap_or(1),
                group_id: None,
                supersedes_evidence_id: None,
                reference_json: str_of(&ev, "reference_json"),
                snapshot_json: str_of(&ev, "snapshot_json"),
            })
            .map_err(|e| hostile(format!("evidence insert failed: {e}")))?;
        counts.3 += 1;
    }

    let mut saved_count = 0usize;
    for s in parse(get("queries/queries.json")?, "queries")?
        .as_array()
        .unwrap_or(&Vec::new())
    {
        ws.meta
            .upsert_saved_search(
                &str_of(s, "saved_search_id"),
                &str_of(s, "name"),
                &str_of(s, "query_text"),
                opt_i64(s, "language_version").unwrap_or(1),
                &str_of(s, "fingerprint"),
                s.get("dataset_selection_json")
                    .and_then(|x| x.as_str())
                    .unwrap_or("{\"kind\":\"all\"}"),
                s.get("time_strategy_json")
                    .and_then(|x| x.as_str())
                    .unwrap_or("{\"kind\":\"all\"}"),
                opt_str(s, "description").as_deref(),
            )
            .map_err(|e| hostile(format!("saved search insert failed: {e}")))?;
        saved_count += 1;
    }

    Ok(ImportSummary {
        investigation_id,
        evidence: counts.3,
        hypotheses: counts.0,
        items: counts.1,
        markers: counts.2,
        saved_searches: saved_count,
        reports: 0,
        data_included: false,
        original_investigation_revision: manifest.investigation_revision,
    })
}
