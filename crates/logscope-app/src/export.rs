//! Bounded, streamed, atomic result export (ADR-0015).
//!
//! The export walks the exact same compiled filter, dataset selection,
//! resolved window, and keyset order as the Explorer table — page by page,
//! never materializing the result set. Output goes to a temp file next to
//! the destination and is renamed into place only on success; the
//! destination is never silently overwritten.

use std::io::Write;
use std::path::{Path, PathBuf};

use logscope_jobs::{JobContext, JobError, JobProgress};
use logscope_model::{attrs_from_canonical_json, UnixNanos};
use logscope_query::{
    stream_query, CompiledFilter, EngineConnection, LogRow, QueryCancelHandle, QueryError,
    ResolvedWindow,
};
use serde::{Deserialize, Serialize};

/// Hard ceilings; requests clamp into these.
pub const MAX_EXPORT_ROWS: u64 = 10_000_000;
pub const MAX_EXPORT_BYTES: u64 = 10 * 1024 * 1024 * 1024;
pub const DEFAULT_EXPORT_ROWS: u64 = 1_000_000;
pub const DEFAULT_EXPORT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Csv,
    Jsonl,
}

impl ExportFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Jsonl => "jsonl",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportSpec {
    pub format: ExportFormat,
    pub destination: PathBuf,
    pub row_limit: u64,
    pub byte_limit: u64,
    /// CSV column selection (canonical export columns and/or attribute
    /// display names). Empty = documented default set.
    pub csv_columns: Vec<String>,
    /// Guard against spreadsheet formula injection (CSV only, default on).
    pub csv_formula_guard: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportOutcome {
    pub rows_written: u64,
    pub bytes_written: u64,
    /// True when a configured bound stopped the export before the result
    /// set was exhausted. A truncated export is NEVER a complete export.
    pub truncated: bool,
    pub destination: String,
}

/// Default CSV columns (documented in the user guide).
pub const DEFAULT_CSV_COLUMNS: &[&str] = &[
    "timestamp",
    "severity",
    "severity.number",
    "message",
    "trace_id",
    "span_id",
    "dataset",
    "record_id",
    "attributes",
];

fn csv_guard(value: &str, guard: bool) -> String {
    if guard && value.starts_with(['=', '+', '-', '@']) {
        format!("'{value}")
    } else {
        value.to_string()
    }
}

fn severity_band_name(row: &LogRow) -> Option<String> {
    match row.severity_number {
        Some(n @ 1..=24) => Some(
            match (n - 1) / 4 {
                0 => "TRACE",
                1 => "DEBUG",
                2 => "INFO",
                3 => "WARN",
                4 => "ERROR",
                _ => "FATAL",
            }
            .to_string(),
        ),
        _ => row.severity_text.as_ref().map(|t| t.to_uppercase()),
    }
}

/// Extracts a CSV cell for one column identity from a full row. Attribute
/// columns receive the dotted display path; nested values resolve through
/// the same tagged representation the query path uses.
fn csv_cell(row: &LogRow, column: &str, guard: bool) -> String {
    let text = |v: Option<&str>| v.unwrap_or("").to_string();
    match column {
        "timestamp" => row
            .event_time
            .map(|t| UnixNanos(t).to_rfc3339())
            .unwrap_or_default(),
        "timestamp.nanos" => row.event_time.map(|t| t.to_string()).unwrap_or_default(),
        "severity" => severity_band_name(row).unwrap_or_default(),
        "severity.text" => csv_guard(&text(row.severity_text.as_deref()), guard),
        "severity.number" => row
            .severity_number
            .map(|n| n.to_string())
            .unwrap_or_default(),
        "message" => csv_guard(&row.display_message, guard),
        "trace_id" => text(row.trace_id.as_deref()),
        "span_id" => text(row.span_id.as_deref()),
        "dataset" => row.dataset_id.clone(),
        "source" => row.source_id.clone(),
        "record_id" => row.record_id.clone(),
        "record_number" => row.record_number.map(|n| n.to_string()).unwrap_or_default(),
        "line" => row.line_start.map(|n| n.to_string()).unwrap_or_default(),
        "attributes" => csv_guard(&row.attributes_json, guard),
        attr => {
            // Attribute display path → tagged lookup, canonical display
            // string. Lossless typed values live in the JSONL format.
            let Ok(attrs) = attrs_from_canonical_json(&row.attributes_json) else {
                return String::new();
            };
            let mut segments: Vec<&str> = vec![attr];
            let direct = attrs.get(attr);
            let value = if direct.is_some() {
                direct
            } else {
                segments = attr.split('.').collect();
                let mut cur = attrs.get(segments[0]);
                for seg in &segments[1..] {
                    cur = match cur {
                        Some(logscope_model::AnyValue::Map(m)) => m.get(*seg),
                        _ => None,
                    };
                }
                cur
            };
            match value {
                Some(v) => csv_guard(&v.display_string(), guard),
                None => String::new(),
            }
        }
    }
}

/// One JSONL record: deterministic field layout, typed attributes preserved
/// in canonical tagged form.
#[derive(Serialize)]
struct JsonlRecord<'a> {
    record_id: &'a str,
    timestamp: Option<String>,
    timestamp_nanos: Option<i64>,
    severity: Option<String>,
    severity_text: Option<&'a str>,
    severity_number: Option<i32>,
    message: &'a str,
    trace_id: Option<&'a str>,
    span_id: Option<&'a str>,
    dataset_id: &'a str,
    source_id: &'a str,
    record_number: Option<u64>,
    line_start: Option<u64>,
    attributes: serde_json::Value,
    provenance: serde_json::Value,
}

fn jsonl_line(row: &LogRow) -> Result<String, JobError> {
    let record = JsonlRecord {
        record_id: &row.record_id,
        timestamp: row.event_time.map(|t| UnixNanos(t).to_rfc3339()),
        timestamp_nanos: row.event_time,
        severity: severity_band_name(row),
        severity_text: row.severity_text.as_deref(),
        severity_number: row.severity_number,
        message: &row.display_message,
        trace_id: row.trace_id.as_deref(),
        span_id: row.span_id.as_deref(),
        dataset_id: &row.dataset_id,
        source_id: &row.source_id,
        record_number: row.record_number,
        line_start: row.line_start,
        attributes: serde_json::from_str(&row.attributes_json).unwrap_or(serde_json::Value::Null),
        provenance: serde_json::from_str(&row.provenance_json).unwrap_or(serde_json::Value::Null),
    };
    serde_json::to_string(&record).map_err(|e| JobError::new("export/serialize", e.to_string()))
}

fn csv_escape(cell: &str) -> String {
    if cell.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}

/// Runs a bounded streaming export as a job body. `filter`, `window`, and
/// `files` must come from the same compilation the Explorer used.
#[allow(clippy::too_many_arguments)]
pub fn run_export(
    engine: &EngineConnection,
    files: &[PathBuf],
    filter: &CompiledFilter,
    window: &ResolvedWindow,
    spec: &ExportSpec,
    ctx: &JobContext,
) -> Result<ExportOutcome, JobError> {
    let row_limit = spec.row_limit.clamp(1, MAX_EXPORT_ROWS);
    let byte_limit = spec.byte_limit.clamp(1024, MAX_EXPORT_BYTES);

    if spec.destination.exists() {
        return Err(JobError::new(
            "export/destination-exists",
            format!(
                "destination already exists: {} (choose a new file name)",
                spec.destination.display()
            ),
        ));
    }
    let dir = spec
        .destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            JobError::new(
                "export/invalid-destination",
                "destination has no parent directory",
            )
        })?;
    std::fs::create_dir_all(dir)
        .map_err(|e| JobError::new("export/io", format!("{}: {e}", dir.display())))?;
    let temp_path = dir.join(format!(
        ".{}.partial-{}",
        spec.destination
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "export".into()),
        uuid::Uuid::new_v4()
    ));

    let result = write_export(
        engine, files, filter, window, spec, ctx, &temp_path, row_limit, byte_limit,
    );
    match result {
        Ok(outcome) => {
            std::fs::rename(&temp_path, &spec.destination).map_err(|e| {
                let _ = std::fs::remove_file(&temp_path);
                JobError::new(
                    "export/publish",
                    format!("could not move export into place: {e}"),
                )
            })?;
            Ok(ExportOutcome {
                destination: spec.destination.display().to_string(),
                ..outcome
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(e)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_export(
    engine: &EngineConnection,
    files: &[PathBuf],
    filter: &CompiledFilter,
    window: &ResolvedWindow,
    spec: &ExportSpec,
    ctx: &JobContext,
    temp_path: &Path,
    row_limit: u64,
    byte_limit: u64,
) -> Result<ExportOutcome, JobError> {
    let file = std::fs::File::create(temp_path)
        .map_err(|e| JobError::new("export/io", format!("{}: {e}", temp_path.display())))?;
    let mut out = std::io::BufWriter::new(file);
    let mut rows_written = 0u64;
    let mut bytes_written = 0u64;
    let mut truncated = false;

    let columns: Vec<String> = if spec.csv_columns.is_empty() {
        DEFAULT_CSV_COLUMNS.iter().map(|s| s.to_string()).collect()
    } else {
        spec.csv_columns.clone()
    };

    let io_err = |e: std::io::Error| JobError::new("export/io", e.to_string()).retryable();

    if spec.format == ExportFormat::Csv {
        let header = columns
            .iter()
            .map(|c| csv_escape(c))
            .collect::<Vec<_>>()
            .join(",")
            + "\n";
        out.write_all(header.as_bytes()).map_err(io_err)?;
        bytes_written += header.len() as u64;
    }

    // One streaming ordered scan (identical ORDER/filter to the table);
    // fetching `row_limit + 1` makes hitting the row cap detectable.
    if ctx.control.is_cancel_requested() {
        return Err(JobError::new("job/cancelled", "the export was cancelled"));
    }
    let cancel = QueryCancelHandle::new(engine.interrupt_handle());
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = {
        let control = ctx.control.clone();
        let cancel = cancel.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if control.is_cancel_requested() {
                    cancel.cancel();
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        })
    };
    let mut write_error: Option<JobError> = None;
    let stream_result = stream_query(
        engine,
        files,
        filter,
        window,
        row_limit + 1,
        &cancel,
        std::time::Duration::from_secs(24 * 3600),
        |row| {
            if rows_written >= row_limit {
                truncated = true;
                return Ok(false);
            }
            let line = match spec.format {
                ExportFormat::Jsonl => match jsonl_line(&row) {
                    Ok(mut l) => {
                        l.push('\n');
                        l
                    }
                    Err(e) => {
                        write_error = Some(e);
                        return Ok(false);
                    }
                },
                ExportFormat::Csv => {
                    let mut l = columns
                        .iter()
                        .map(|c| csv_escape(&csv_cell(&row, c, spec.csv_formula_guard)))
                        .collect::<Vec<_>>()
                        .join(",");
                    l.push('\n');
                    l
                }
            };
            // A record is either written completely or not at all.
            if bytes_written + line.len() as u64 > byte_limit {
                truncated = true;
                return Ok(false);
            }
            if let Err(e) = out.write_all(line.as_bytes()) {
                write_error = Some(JobError::new("export/io", e.to_string()).retryable());
                return Ok(false);
            }
            bytes_written += line.len() as u64;
            rows_written += 1;
            if rows_written.is_multiple_of(10_000) {
                ctx.report(JobProgress {
                    stage: "exporting".into(),
                    records_accepted: rows_written,
                    bytes_processed: bytes_written,
                    ..Default::default()
                });
            }
            Ok(true)
        },
    );
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = watcher.join();
    if let Some(e) = write_error {
        return Err(e);
    }
    match stream_result {
        Ok(_) => {}
        Err(QueryError::Cancelled) => {
            return Err(JobError::new("job/cancelled", "the export was cancelled"))
        }
        Err(e) => return Err(JobError::new(e.code(), e.to_string())),
    }
    ctx.report(JobProgress {
        stage: "exporting".into(),
        records_accepted: rows_written,
        bytes_processed: bytes_written,
        ..Default::default()
    });

    out.flush().map_err(io_err)?;
    out.get_ref().sync_all().map_err(io_err)?;
    Ok(ExportOutcome {
        rows_written,
        bytes_written,
        truncated,
        destination: String::new(),
    })
}
