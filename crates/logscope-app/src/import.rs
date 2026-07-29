//! Import service: streaming file ingestion into a staged, atomically
//! published dataset, with progress, cancellation, and structured failure.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use logscope_ingest::{
    fingerprint_file, normalize_log, CsvReader, FormatSpec, ImportProfile, JsonlReader,
    NormalizeContext, ReadItem, RecordReader, CSV_PARSER_ID, JSONL_PARSER_ID, PARSER_VERSION,
};
use logscope_jobs::{JobContext, JobError, JobProgress};
use logscope_model::{AttrMap, ResourceDescriptor, ScopeDescriptor, SourceProtocol, UnixNanos};
use logscope_query::{index_segment_into_fts, EngineConnection};
use logscope_store::{FtsIndex, LogSegmentWriter};
use logscope_workspace::{
    LedgerCounts, LedgerEntry, PublishVersions, SegmentToPublish, Signal, Workspace,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRequest {
    pub paths: Vec<PathBuf>,
    pub profile: ImportProfile,
    pub dataset_name: String,
    /// Records normalized per batch (bounded memory).
    pub batch_size: usize,
    /// Rotation threshold for one Parquet segment.
    pub segment_max_rows: u64,
}

impl ImportRequest {
    pub fn new(paths: Vec<PathBuf>, profile: ImportProfile, dataset_name: &str) -> Self {
        ImportRequest {
            paths,
            profile,
            dataset_name: dataset_name.to_string(),
            batch_size: 4096,
            segment_max_rows: 250_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportOutcome {
    pub job_id: String,
    pub dataset_id: String,
    pub accepted: u64,
    /// Reader-level parse failures (malformed records).
    pub unparsed: u64,
    /// Normalization rejects.
    pub rejected: u64,
    /// Records skipped under the duplicate policy (identical canonical
    /// content at identical intra-file position; first occurrence wins).
    pub duplicates: u64,
    pub segments: Vec<String>,
}

fn job_err(code: &str, e: impl std::fmt::Display) -> JobError {
    JobError::new(code, e.to_string())
}

/// Compact 16-byte dedup key from a `xxx-<32 hex>` record ID, keeping the
/// in-job duplicate set bounded (~48 bytes/record instead of ~100).
fn record_id_key(record_id: &str) -> [u8; 16] {
    let hex = record_id
        .split_once('-')
        .map(|(_, h)| h)
        .unwrap_or(record_id);
    let mut key = [0u8; 16];
    if hex.len() == 32 {
        let bytes = hex.as_bytes();
        let mut ok = true;
        for (i, chunk) in bytes.chunks_exact(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16);
            let lo = (chunk[1] as char).to_digit(16);
            match (hi, lo) {
                (Some(h), Some(l)) => key[i] = ((h << 4) | l) as u8,
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return key;
        }
    }
    let digest = blake3::hash(record_id.as_bytes());
    key.copy_from_slice(&digest.as_bytes()[..16]);
    key
}

fn open_reader(
    path: &Path,
    profile: &ImportProfile,
) -> Result<(Box<dyn RecordReader>, &'static str, String), JobError> {
    let file = std::fs::File::open(path).map_err(|e| job_err("import/io", e).retryable())?;
    let is_gz = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gz"));
    let stream: Box<dyn Read> = if is_gz {
        Box::new(flate2::read::MultiGzDecoder::new(file))
    } else {
        Box::new(file)
    };
    match &profile.format {
        FormatSpec::Jsonl => Ok((
            Box::new(JsonlReader::new(stream)),
            JSONL_PARSER_ID,
            "application/x-ndjson".to_string(),
        )),
        FormatSpec::Csv {
            delimiter,
            has_headers,
        } => {
            let reader = CsvReader::new(stream, *delimiter, *has_headers)
                .map_err(|e| job_err("import/unsupported-format", e))?;
            Ok((Box::new(reader), CSV_PARSER_ID, "text/csv".to_string()))
        }
    }
}

/// Runs a complete import as the body of a background job. On success the
/// dataset is atomically published and FTS-indexed; on cancellation or
/// failure all staged state is discarded and existing datasets are
/// untouched.
pub fn run_import(
    ws: &mut Workspace,
    engine: &EngineConnection,
    request: &ImportRequest,
    ctx: &JobContext,
) -> Result<ImportOutcome, JobError> {
    request
        .profile
        .validate()
        .map_err(|e| job_err("import/invalid-profile", e))?;
    if request.paths.is_empty() {
        return Err(job_err("import/invalid-argument", "no input files"));
    }
    let batch_size = request.batch_size.clamp(64, 65_536);
    let segment_max_rows = request.segment_max_rows.clamp(1_024, 4_000_000);

    let job_id = ctx.job_id.clone();
    let dataset_id = format!("ds-{}", uuid::Uuid::new_v4());
    let source_id = format!("src-{}", uuid::Uuid::new_v4());

    let meta_fail = |e: logscope_workspace::WorkspaceError| job_err(e.code(), e);

    ws.meta
        .insert_job(&job_id, "import", Some(&dataset_id))
        .map_err(meta_fail)?;
    ws.meta
        .create_dataset(&dataset_id, &request.dataset_name, Signal::Logs)
        .map_err(meta_fail)?;
    ws.meta
        .insert_source(
            &source_id,
            "static_file_set",
            &request.dataset_name,
            "referenced",
            "{}",
        )
        .map_err(meta_fail)?;
    ws.meta
        .link_dataset_source(&dataset_id, &source_id)
        .map_err(meta_fail)?;

    // Cleanup helper for every non-success exit.
    let abort = |ws: &Workspace, status: &str, error: Option<&JobError>| {
        let _ = ws.discard_staging(&job_id, Some(&dataset_id));
        let error_json = error.and_then(|e| serde_json::to_string(e).ok());
        let _ = ws
            .meta
            .update_job_status(&job_id, status, error_json.as_deref());
    };

    let result = run_import_inner(
        ws,
        engine,
        request,
        ctx,
        &job_id,
        &dataset_id,
        &source_id,
        batch_size,
        segment_max_rows,
    );
    match result {
        Ok(outcome) => Ok(outcome),
        Err(e) if e.code == "job/cancelled" => {
            abort(ws, "cancelled", Some(&e));
            Err(e)
        }
        Err(e) => {
            abort(ws, "failed", Some(&e));
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_import_inner(
    ws: &mut Workspace,
    engine: &EngineConnection,
    request: &ImportRequest,
    ctx: &JobContext,
    job_id: &str,
    dataset_id: &str,
    source_id: &str,
    batch_size: usize,
    segment_max_rows: u64,
) -> Result<ImportOutcome, JobError> {
    let meta_fail = |e: logscope_workspace::WorkspaceError| job_err(e.code(), e);
    let store_fail = |e: logscope_store::StoreError| job_err(e.code(), e);

    // Shared "unknown" resource/scope for plain file imports (no resource
    // information exists in generic files; profiles may add derivation
    // later).
    let resource = ResourceDescriptor::from_attributes(AttrMap::new(), None, 0);
    let scope = ScopeDescriptor::unknown();
    ws.meta.upsert_resource(&resource).map_err(meta_fail)?;
    ws.meta.upsert_scope(&scope).map_err(meta_fail)?;

    let staging = ws.begin_staging(job_id).map_err(meta_fail)?;
    let ingest_time = UnixNanos::now();

    let mut accepted = 0u64;
    let mut unparsed = 0u64;
    let mut rejected = 0u64;
    let mut duplicates = 0u64;
    let mut bytes_before_current = 0u64;
    let mut seen_record_ids: HashSet<[u8; 16]> = HashSet::new();
    let mut staged: Vec<(PathBuf, SegmentToPublish)> = Vec::new();
    let mut ledger: Vec<LedgerEntry> = Vec::new();

    let mut current: Option<(String, LogSegmentWriter, PathBuf)> = None;
    let new_segment = |staging: &Path| -> Result<(String, LogSegmentWriter, PathBuf), JobError> {
        let segment_id = format!("seg-{}", uuid::Uuid::new_v4());
        let file_name = format!("logs-{segment_id}.parquet");
        let path = staging.join(&file_name);
        let writer = LogSegmentWriter::create(&path).map_err(store_fail)?;
        Ok((segment_id, writer, path))
    };

    for path in &request.paths {
        ctx.control.checkpoint().map_err(JobError::from)?;

        let identity = fingerprint_file(path).map_err(|e| job_err(e.code(), e).retryable())?;
        let file_id = format!("file-{}", uuid::Uuid::new_v4());
        ws.meta
            .insert_source_file(
                &file_id,
                source_id,
                &identity.path,
                None,
                None,
                identity.size_bytes as i64,
                identity.modified_at.as_deref(),
                &identity.content_hash,
            )
            .map_err(meta_fail)?;

        let (mut reader, parser_id, content_type) = open_reader(path, &request.profile)?;
        let norm_ctx = NormalizeContext {
            dataset_id: dataset_id.to_string(),
            logical_source_id: source_id.to_string(),
            file_id: file_id.clone(),
            archive_entry: None,
            resource_id: resource.resource_id.clone(),
            scope_id: scope.scope_id.clone(),
            parser_id: parser_id.to_string(),
            parser_version: PARSER_VERSION.to_string(),
            protocol: SourceProtocol::FileImport,
            content_type: Some(content_type),
            ingest_time,
        };
        let mut file_counts = LedgerCounts::default();
        let mut last_record_number = 0u64;

        loop {
            ctx.control.checkpoint().map_err(JobError::from)?;
            let items = reader
                .next_batch(batch_size)
                .map_err(|e| job_err(e.code(), e))?;
            if items.is_empty() {
                break;
            }

            let mut batch_records = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    ReadItem::Parsed(parsed) => {
                        if let Some(n) = parsed.locator.record_number {
                            last_record_number = n;
                        }
                        match normalize_log(parsed, &request.profile, &norm_ctx) {
                            Ok(record) => {
                                if seen_record_ids.insert(record_id_key(&record.record_id)) {
                                    batch_records.push(record);
                                } else {
                                    duplicates += 1;
                                    file_counts.duplicate += 1;
                                }
                            }
                            Err(reject) => {
                                rejected += 1;
                                file_counts.rejected += 1;
                                ws.meta
                                    .insert_rejected(
                                        dataset_id,
                                        source_id,
                                        &file_id,
                                        &reject.locator,
                                        &reject.reason_code,
                                        &reject.message,
                                        Some(&reject.raw_excerpt),
                                        (parser_id, PARSER_VERSION),
                                        Some((
                                            request.profile.profile_id.as_str(),
                                            request.profile.version.as_str(),
                                        )),
                                        false,
                                    )
                                    .map_err(meta_fail)?;
                            }
                        }
                    }
                    ReadItem::Malformed(m) => {
                        if let Some(n) = m.locator.record_number {
                            last_record_number = n;
                        }
                        unparsed += 1;
                        file_counts.unparsed += 1;
                        ws.meta
                            .insert_rejected(
                                dataset_id,
                                source_id,
                                &file_id,
                                &m.locator,
                                m.reason_code,
                                &m.message,
                                Some(&m.raw_excerpt),
                                (parser_id, PARSER_VERSION),
                                Some((
                                    request.profile.profile_id.as_str(),
                                    request.profile.version.as_str(),
                                )),
                                m.truncated,
                            )
                            .map_err(meta_fail)?;
                    }
                }
            }

            if !batch_records.is_empty() {
                if current.is_none() {
                    current = Some(new_segment(&staging)?);
                }
                let (_, writer, _) = current.as_mut().expect("segment exists");
                writer.write_batch(&batch_records).map_err(store_fail)?;
                accepted += batch_records.len() as u64;
                file_counts.accepted += batch_records.len() as u64;

                if writer.rows_written() >= segment_max_rows {
                    let (segment_id, writer, path) = current.take().expect("segment exists");
                    let stats = writer.finish().map_err(store_fail)?;
                    staged.push((
                        path,
                        SegmentToPublish {
                            segment_id: segment_id.clone(),
                            signal: Signal::Logs,
                            file_name: format!("logs-{segment_id}.parquet"),
                            row_count: stats.rows as i64,
                            byte_size: stats.byte_size as i64,
                            min_event_time: stats.min_event_time,
                            max_event_time: stats.max_event_time,
                        },
                    ));
                }
            }

            ctx.report(JobProgress {
                stage: "importing".into(),
                current_item: Some(path.display().to_string()),
                records_accepted: accepted,
                records_rejected: rejected,
                records_unparsed: unparsed,
                records_duplicate: duplicates,
                bytes_processed: bytes_before_current + reader.bytes_read(),
                bytes_total: None,
            });
        }
        bytes_before_current += reader.bytes_read();

        ledger.push(LedgerEntry {
            source_id: source_id.to_string(),
            file_id,
            checkpoint_json: serde_json::to_string(&logscope_ingest::IngestCheckpoint {
                record_number: last_record_number,
                byte_offset: Some(reader.bytes_read()),
            })
            .unwrap_or_else(|_| "{}".into()),
            counts: file_counts,
        });
    }

    if let Some((segment_id, writer, path)) = current.take() {
        if writer.rows_written() > 0 {
            let stats = writer.finish().map_err(store_fail)?;
            staged.push((
                path,
                SegmentToPublish {
                    segment_id: segment_id.clone(),
                    signal: Signal::Logs,
                    file_name: format!("logs-{segment_id}.parquet"),
                    row_count: stats.rows as i64,
                    byte_size: stats.byte_size as i64,
                    min_event_time: stats.min_event_time,
                    max_event_time: stats.max_event_time,
                },
            ));
        } else {
            drop(writer);
            let _ = std::fs::remove_file(&path);
        }
    }

    ctx.control.checkpoint().map_err(JobError::from)?;
    ctx.report(JobProgress {
        stage: "publishing".into(),
        records_accepted: accepted,
        records_rejected: rejected,
        records_unparsed: unparsed,
        records_duplicate: duplicates,
        bytes_processed: bytes_before_current,
        ..Default::default()
    });

    let versions = PublishVersions {
        profile_id: Some(request.profile.profile_id.clone()),
        profile_version: Some(request.profile.version.clone()),
        parser_id: Some(match request.profile.format {
            FormatSpec::Jsonl => JSONL_PARSER_ID.to_string(),
            FormatSpec::Csv { .. } => CSV_PARSER_ID.to_string(),
        }),
        parser_version: Some(PARSER_VERSION.to_string()),
        normalizer_version: Some(logscope_normalize::NORMALIZER_VERSION.to_string()),
        model_version: Some(logscope_model::MODEL_VERSION.to_string()),
    };
    ws.publish_staged_import(job_id, dataset_id, &staged, &ledger, &versions)
        .map_err(meta_fail)?;
    ws.note_signal_available(Signal::Logs).map_err(meta_fail)?;

    // Post-publish FTS indexing (idempotent; re-runnable after a crash).
    ctx.report(JobProgress {
        stage: "indexing".into(),
        records_accepted: accepted,
        records_rejected: rejected,
        records_unparsed: unparsed,
        records_duplicate: duplicates,
        bytes_processed: bytes_before_current,
        ..Default::default()
    });
    let mut fts = FtsIndex::open(&ws.layout.fts_logs_path()).map_err(store_fail)?;
    let dataset_dir = ws.layout.dataset_dir(dataset_id);
    let mut segment_ids = Vec::with_capacity(staged.len());
    for (_, seg) in &staged {
        let published_path = dataset_dir.join(&seg.file_name);
        index_segment_into_fts(
            engine,
            &mut fts,
            dataset_id,
            &seg.segment_id,
            &published_path,
        )
        .map_err(|e| job_err(e.code(), e))?;
        ws.meta
            .mark_segment_fts_indexed(&seg.segment_id)
            .map_err(meta_fail)?;
        segment_ids.push(seg.segment_id.clone());
    }
    // Derived-index lifecycle: FTS rows just landed (current tokenizer if
    // the index file is current); the field catalog is built next.
    let fts_current = !fts.needs_rebuild().unwrap_or(true);
    drop(fts);
    if fts_current {
        crate::explorer::note_new_dataset_indexes(ws, dataset_id).map_err(meta_fail)?;
    } else {
        // Rows went into an outdated index file: text search for this
        // dataset stays on the exact fallback until the rebuild job runs.
        ws.meta
            .set_index_state("fts", dataset_id, 1, "pending", "{}")
            .map_err(meta_fail)?;
        ws.meta
            .set_index_state(
                "field_catalog",
                dataset_id,
                logscope_query::CATALOG_VERSION,
                "pending",
                "{}",
            )
            .map_err(meta_fail)?;
    }

    // Field catalog build. The dataset is already published and the
    // catalog is optional for integrity, so cancellation or failure here
    // never un-completes the import — the state stays pending/failed and a
    // later rebuild job picks it up.
    ctx.report(JobProgress {
        stage: "cataloguing".into(),
        records_accepted: accepted,
        records_rejected: rejected,
        records_unparsed: unparsed,
        records_duplicate: duplicates,
        bytes_processed: bytes_before_current,
        ..Default::default()
    });
    if let Err(e) = crate::explorer::build_field_catalog(ws, engine, dataset_id, ctx) {
        tracing::warn!(dataset = %dataset_id, error = %e, "field catalog build deferred");
    }

    Ok(ImportOutcome {
        job_id: job_id.to_string(),
        dataset_id: dataset_id.to_string(),
        accepted,
        unparsed,
        rejected,
        duplicates,
        segments: segment_ids,
    })
}
