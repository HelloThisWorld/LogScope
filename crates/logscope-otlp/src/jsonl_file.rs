//! OTLP JSONL file import: one OTLP/JSON export envelope per line.
//!
//! Uses the shared streaming JSONL reader (exact locators, bounded batches)
//! and the same tolerant OTLP/JSON decoding as the HTTP receiver, so file
//! import and live receipt share one semantic path. Unknown JSON fields are
//! tolerated; the source file itself is retained by the workspace (referenced
//! source), so future reprocessing has the complete raw envelopes.

use std::io::Read;

use logscope_ingest::{JsonlReader, ReadItem, RecordReader};
use logscope_model::{RecordLocator, SourceProtocol, UnixNanos};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;

use crate::error::OtlpError;
use crate::receiver::{EnvelopeMeta, EnvelopePayload};

#[derive(Debug)]
pub struct JsonlEnvelope {
    pub payload: EnvelopePayload,
    pub meta: EnvelopeMeta,
    pub locator: RecordLocator,
}

#[derive(Debug)]
pub struct JsonlReject {
    pub locator: RecordLocator,
    pub reason_code: String,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct JsonlImportResult {
    pub envelopes: Vec<JsonlEnvelope>,
    pub rejects: Vec<JsonlReject>,
}

fn detect_and_parse(value: serde_json::Value) -> Result<EnvelopePayload, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "envelope is not a JSON object".to_string())?;
    let has = |a: &str, b: &str| obj.contains_key(a) || obj.contains_key(b);
    if has("resourceLogs", "resource_logs") {
        serde_json::from_value::<ExportLogsServiceRequest>(value.clone())
            .map(EnvelopePayload::Logs)
            .map_err(|e| e.to_string())
    } else if has("resourceMetrics", "resource_metrics") {
        serde_json::from_value::<ExportMetricsServiceRequest>(value.clone())
            .map(EnvelopePayload::Metrics)
            .map_err(|e| e.to_string())
    } else if has("resourceSpans", "resource_spans") {
        serde_json::from_value::<ExportTraceServiceRequest>(value.clone())
            .map(EnvelopePayload::Traces)
            .map_err(|e| e.to_string())
    } else {
        Err("no resourceLogs/resourceMetrics/resourceSpans member".to_string())
    }
}

/// Streams an OTLP JSONL input envelope by envelope: memory stays bounded
/// by the largest single envelope, never the file. The sink returns `false`
/// to stop early.
pub fn stream_otlp_jsonl(
    input: impl Read,
    mut sink: impl FnMut(JsonlEnvelope) -> bool,
    mut on_reject: impl FnMut(JsonlReject),
) -> Result<u64, OtlpError> {
    let mut reader = JsonlReader::new(input);
    let mut delivered = 0u64;
    'outer: loop {
        let items = reader
            .next_batch(64)
            .map_err(|e| OtlpError::Envelope(e.to_string()))?;
        if items.is_empty() {
            break;
        }
        for item in items {
            match item {
                ReadItem::Parsed(parsed) => {
                    let value = match parsed.fields {
                        logscope_ingest::ParsedFields::Json(v) => v,
                        other => unreachable!("jsonl reader yields json fields, got {other:?}"),
                    };
                    match detect_and_parse(value) {
                        Ok(payload) => {
                            delivered += 1;
                            let envelope = JsonlEnvelope {
                                payload,
                                meta: EnvelopeMeta {
                                    protocol: SourceProtocol::OtlpJsonlFile,
                                    content_type: "application/x-ndjson".to_string(),
                                    raw_hash: parsed.raw_hash,
                                    received_at: UnixNanos::now(),
                                },
                                locator: parsed.locator,
                            };
                            if !sink(envelope) {
                                break 'outer;
                            }
                        }
                        Err(message) => on_reject(JsonlReject {
                            locator: parsed.locator,
                            reason_code: "otlp/invalid-envelope".to_string(),
                            message,
                        }),
                    }
                }
                ReadItem::Malformed(m) => on_reject(JsonlReject {
                    locator: m.locator,
                    reason_code: m.reason_code.to_string(),
                    message: m.message,
                }),
            }
        }
    }
    Ok(delivered)
}

/// Reads a complete OTLP JSONL stream (bounded per-batch internally).
pub fn read_otlp_jsonl(input: impl Read) -> Result<JsonlImportResult, OtlpError> {
    let mut reader = JsonlReader::new(input);
    let mut result = JsonlImportResult::default();
    loop {
        let items = reader
            .next_batch(1024)
            .map_err(|e| OtlpError::Envelope(e.to_string()))?;
        if items.is_empty() {
            break;
        }
        for item in items {
            match item {
                ReadItem::Parsed(parsed) => {
                    let value = match parsed.fields {
                        logscope_ingest::ParsedFields::Json(v) => v,
                        other => {
                            unreachable!("jsonl reader yields json fields, got {other:?}")
                        }
                    };
                    match detect_and_parse(value) {
                        Ok(payload) => result.envelopes.push(JsonlEnvelope {
                            payload,
                            meta: EnvelopeMeta {
                                protocol: SourceProtocol::OtlpJsonlFile,
                                content_type: "application/x-ndjson".to_string(),
                                raw_hash: parsed.raw_hash,
                                received_at: UnixNanos::now(),
                            },
                            locator: parsed.locator,
                        }),
                        Err(message) => result.rejects.push(JsonlReject {
                            locator: parsed.locator,
                            reason_code: "otlp/invalid-envelope".to_string(),
                            message,
                        }),
                    }
                }
                ReadItem::Malformed(m) => result.rejects.push(JsonlReject {
                    locator: m.locator,
                    reason_code: m.reason_code.to_string(),
                    message: m.message,
                }),
            }
        }
    }
    Ok(result)
}
