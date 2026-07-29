//! Turns parsed source records into canonical `LogRecord`s per profile.

use logscope_model::{
    AnyValue, AttrMap, IngestProvenance, LogRecord, PhysicalOrigin, QualityFlag, SourceProtocol,
    SpanId, TraceId, UnixNanos,
};
use logscope_normalize::{derive_display_message, map_severity_text, parse_timestamp};

use crate::profile::{FieldRef, ImportProfile};
use crate::reader::{MalformedRecord, ParsedFields, ParsedRecord};

/// Static identity of the normalization run (same for every record of a job).
#[derive(Debug, Clone)]
pub struct NormalizeContext {
    pub dataset_id: String,
    pub logical_source_id: String,
    pub file_id: String,
    pub archive_entry: Option<String>,
    pub resource_id: String,
    pub scope_id: String,
    pub parser_id: String,
    pub parser_version: String,
    pub protocol: SourceProtocol,
    pub content_type: Option<String>,
    /// One observation time for the whole import job (wall clock; excluded
    /// from hashes).
    pub ingest_time: UnixNanos,
}

/// A record that failed normalization (distinct from reader-level parse
/// failures, but reported through the same rejected-record channel).
#[derive(Debug, Clone)]
pub struct NormalizeReject {
    pub locator: logscope_model::RecordLocator,
    pub reason_code: String,
    pub message: String,
    pub raw_excerpt: Vec<u8>,
}

fn take_field(fields: &mut Vec<(String, String)>, candidates: &[FieldRef]) -> Option<String> {
    for c in candidates {
        let idx = match c {
            FieldRef::Name(name) => fields.iter().position(|(k, _)| k == name),
            FieldRef::Index(i) => fields.iter().position(|(k, _)| k == &i.to_string()),
        };
        if let Some(i) = idx {
            return Some(fields.remove(i).1);
        }
    }
    None
}

fn take_json_field(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    candidates: &[FieldRef],
) -> Option<serde_json::Value> {
    for c in candidates {
        if let FieldRef::Name(name) = c {
            if obj.contains_key(name) {
                return obj.remove(name);
            }
        }
    }
    None
}

fn json_value_to_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Normalizes one parsed record. On success the record is sealed (its
/// deterministic `record_id` is computed).
// Rejects are rare relative to accepted records; boxing them would cost
// more in ergonomics than the large-variant return costs in moves.
#[allow(clippy::result_large_err)]
pub fn normalize_log(
    parsed: ParsedRecord,
    profile: &ImportProfile,
    ctx: &NormalizeContext,
) -> Result<LogRecord, NormalizeReject> {
    let mut flags: Vec<QualityFlag> = Vec::new();
    if parsed.replacement_chars > 0 {
        flags.push(QualityFlag::EncodingReplacementChars {
            count: parsed.replacement_chars,
        });
    }

    // Extract mapped fields, leaving the rest for attributes.
    type Extracted = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<(String, String)>,
        AttrMap,
        Option<AnyValue>,
    );
    let (ts_text, severity_text, trace_text, span_text, generic, attributes, body): Extracted;

    match parsed.fields {
        ParsedFields::Csv(mut fields) => {
            ts_text = profile
                .timestamp
                .as_ref()
                .and_then(|t| take_field(&mut fields, &t.candidates));
            severity_text = take_field(&mut fields, &profile.severity);
            let msg_text = take_field(&mut fields, &profile.message);
            trace_text = take_field(&mut fields, &profile.trace_id);
            span_text = take_field(&mut fields, &profile.span_id);
            let mut generic_pairs = Vec::new();
            for (canon, candidates) in &profile.generic_fields {
                if let Some(v) = take_field(&mut fields, candidates) {
                    generic_pairs.push((canon.clone(), v));
                }
            }
            generic = generic_pairs;
            // Remaining CSV columns become string-typed attributes (CSV has
            // no type system; v0.1 profiles may declare column types).
            let mut attrs = AttrMap::new();
            for (k, v) in fields {
                if attrs.insert(k.clone(), AnyValue::Str(v)).is_some() {
                    flags.push(QualityFlag::DuplicateAttributeKey { key: k });
                }
            }
            attributes = attrs;
            body = msg_text.map(AnyValue::Str);
        }
        ParsedFields::Json(value) => {
            let mut obj = match value {
                serde_json::Value::Object(map) => map,
                // Non-object JSONL records: the whole value becomes the body.
                other => {
                    let body_val = AnyValue::from_plain_json(&other);
                    let display = derive_display_message(Some(&body_val));
                    let provenance =
                        build_provenance(ctx, &parsed.locator, &parsed.raw_hash, None, flags);
                    return Ok(LogRecord {
                        record_id: String::new(),
                        event_time: None,
                        observed_time: ctx.ingest_time,
                        original_timestamp_text: None,
                        timezone_assumption: None,
                        severity_text: None,
                        severity_number: None,
                        body: Some(body_val),
                        display_message: display,
                        event_name: None,
                        trace_id: None,
                        span_id: None,
                        trace_flags: None,
                        resource_id: ctx.resource_id.clone(),
                        scope_id: ctx.scope_id.clone(),
                        operation: None,
                        outcome: None,
                        event_type: None,
                        request_id: None,
                        transaction_id: None,
                        message_id: None,
                        entity_id: None,
                        attributes: AttrMap::new(),
                        dropped_attributes_count: 0,
                        provenance,
                    }
                    .seal());
                }
            };

            ts_text = profile
                .timestamp
                .as_ref()
                .and_then(|t| take_json_field(&mut obj, &t.candidates))
                .map(|v| json_value_to_text(&v));
            severity_text =
                take_json_field(&mut obj, &profile.severity).map(|v| json_value_to_text(&v));
            let msg_val = take_json_field(&mut obj, &profile.message);
            trace_text =
                take_json_field(&mut obj, &profile.trace_id).map(|v| json_value_to_text(&v));
            span_text = take_json_field(&mut obj, &profile.span_id).map(|v| json_value_to_text(&v));
            let mut generic_pairs = Vec::new();
            for (canon, candidates) in &profile.generic_fields {
                if let Some(v) = take_json_field(&mut obj, candidates) {
                    generic_pairs.push((canon.clone(), json_value_to_text(&v)));
                }
            }
            generic = generic_pairs;
            // Every unmapped member survives as a typed attribute.
            let mut attrs = AttrMap::new();
            for (k, v) in &obj {
                attrs.insert(k.clone(), AnyValue::from_plain_json(v));
            }
            attributes = attrs;
            body = msg_val.map(|v| AnyValue::from_plain_json(&v));
        }
    }

    // Timestamp.
    let mut event_time = None;
    let mut original_timestamp_text = None;
    let mut timezone_assumption = None;
    let mut precision = None;
    if let Some(rule) = &profile.timestamp {
        match &ts_text {
            None => flags.push(QualityFlag::TimestampMissing),
            Some(text) => match parse_timestamp(text, &rule.format, &rule.timezone) {
                Ok(p) => {
                    event_time = Some(p.nanos);
                    precision = Some(p.precision);
                    if matches!(
                        p.assumption,
                        logscope_model::TimezoneAssumption::AssumedUtc
                            | logscope_model::TimezoneAssumption::ProfileZone(_)
                    ) {
                        flags.push(QualityFlag::TimezoneAssumed);
                    }
                    timezone_assumption = Some(p.assumption);
                    original_timestamp_text = Some(text.clone());
                }
                Err(e) => {
                    // Unparsable timestamp does not reject the record: the
                    // record survives with the original text and a flag.
                    flags.push(QualityFlag::TimestampUnparsed);
                    original_timestamp_text = Some(text.clone());
                    tracing::debug!(error = %e, "timestamp unparsed");
                }
            },
        }
    }

    // Severity.
    let severity_number = severity_text.as_deref().and_then(map_severity_text);
    if severity_text.is_some() && severity_number.is_none() {
        flags.push(QualityFlag::SeverityUnmapped);
    }

    // Correlation IDs (invalid values stay visible in attributes).
    let mut attributes = attributes;
    let trace_id = match trace_text {
        None => None,
        Some(t) => match TraceId::from_hex(t.trim()) {
            Ok(id) => Some(id),
            Err(_) => {
                flags.push(QualityFlag::TraceIdInvalid);
                attributes.insert("trace_id.raw".to_string(), AnyValue::Str(t));
                None
            }
        },
    };
    let span_id = match span_text {
        None => None,
        Some(s) => match SpanId::from_hex(s.trim()) {
            Ok(id) => Some(id),
            Err(_) => {
                flags.push(QualityFlag::SpanIdInvalid);
                attributes.insert("span_id.raw".to_string(), AnyValue::Str(s));
                None
            }
        },
    };

    let display_message = derive_display_message(body.as_ref());

    let generic_get = |name: &str| -> Option<String> {
        generic
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let operation = generic_get("operation");
    let outcome = generic_get("outcome");
    let event_type = generic_get("event_type");
    let request_id = generic_get("request_id");
    let transaction_id = generic_get("transaction_id");
    let message_id = generic_get("message_id");
    let entity_id = generic_get("entity_id");

    let mut provenance =
        build_provenance(ctx, &parsed.locator, &parsed.raw_hash, Some(profile), flags);
    provenance.original_timestamp_precision = precision;

    Ok(LogRecord {
        record_id: String::new(),
        event_time,
        observed_time: ctx.ingest_time,
        original_timestamp_text,
        timezone_assumption,
        severity_text,
        severity_number,
        body,
        display_message,
        event_name: None,
        trace_id,
        span_id,
        trace_flags: None,
        resource_id: ctx.resource_id.clone(),
        scope_id: ctx.scope_id.clone(),
        operation,
        outcome,
        event_type,
        request_id,
        transaction_id,
        message_id,
        entity_id,
        attributes,
        dropped_attributes_count: 0,
        provenance,
    }
    .seal())
}

fn build_provenance(
    ctx: &NormalizeContext,
    locator: &logscope_model::RecordLocator,
    raw_hash: &str,
    profile: Option<&ImportProfile>,
    flags: Vec<QualityFlag>,
) -> IngestProvenance {
    IngestProvenance {
        dataset_id: ctx.dataset_id.clone(),
        logical_source_id: ctx.logical_source_id.clone(),
        origin: PhysicalOrigin::File {
            file_id: ctx.file_id.clone(),
            archive_entry: ctx.archive_entry.clone(),
        },
        locator: locator.clone(),
        parser_id: ctx.parser_id.clone(),
        parser_version: ctx.parser_version.clone(),
        profile_id: profile.map(|p| p.profile_id.clone()),
        profile_version: profile.map(|p| p.version.clone()),
        normalizer_version: logscope_normalize::NORMALIZER_VERSION.to_string(),
        protocol: ctx.protocol,
        content_type: ctx.content_type.clone(),
        ingest_time: ctx.ingest_time,
        raw_hash: raw_hash.to_string(),
        original_timestamp_precision: None,
        flags,
    }
}

/// Converts a reader-level malformed record into the rejected-record shape.
pub fn reject_from_malformed(m: &MalformedRecord) -> NormalizeReject {
    NormalizeReject {
        locator: m.locator.clone(),
        reason_code: m.reason_code.to_string(),
        message: m.message.clone(),
        raw_excerpt: m.raw_excerpt.clone(),
    }
}
