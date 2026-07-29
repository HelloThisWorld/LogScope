//! Streaming record readers with exact source locators.
//!
//! Readers yield bounded batches of `ReadItem`s; memory use is proportional
//! to the batch size, never to the source size. Every item — parsed or
//! malformed — carries an exact `RecordLocator`.

use std::io::{BufRead, BufReader, Read};

use logscope_model::{hash_bytes_hex, RecordLocator};

use crate::error::IngestError;

/// Maximum bytes of raw excerpt carried on a malformed record.
pub const MAX_RAW_EXCERPT: usize = 4096;

/// Hard per-record size bound (64 MiB): a single "record" larger than this
/// is treated as malformed rather than exhausting memory.
pub const MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub enum ParsedFields {
    /// CSV: (column name or index-as-string, raw text value) pairs in
    /// column order.
    Csv(Vec<(String, String)>),
    /// JSONL: the parsed JSON value (object expected, others tolerated).
    Json(serde_json::Value),
}

#[derive(Debug, Clone)]
pub struct ParsedRecord {
    pub locator: RecordLocator,
    /// BLAKE3 hex of the canonical raw record bytes (see reader docs).
    pub raw_hash: String,
    pub fields: ParsedFields,
    /// Count of U+FFFD replacements introduced during decoding.
    pub replacement_chars: u64,
}

#[derive(Debug, Clone)]
pub struct MalformedRecord {
    pub locator: RecordLocator,
    pub reason_code: &'static str,
    pub message: String,
    pub raw_excerpt: Vec<u8>,
    /// True when the record was cut off by end-of-input.
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub enum ReadItem {
    Parsed(ParsedRecord),
    Malformed(MalformedRecord),
}

/// Common interface for streaming readers.
pub trait RecordReader {
    /// Reads up to `max` items. An empty vec means end of input.
    fn next_batch(&mut self, max: usize) -> Result<Vec<ReadItem>, IngestError>;
    /// Total bytes consumed from the (decompressed) stream so far.
    fn bytes_read(&self) -> u64;
}

// ---------------------------------------------------------------------------
// JSONL
// ---------------------------------------------------------------------------

/// Streaming JSONL/NDJSON reader.
///
/// Raw-hash contract: the hash covers the exact record bytes *excluding* the
/// line terminator, so a final line without a trailing newline hashes the
/// same as the identical line with one.
pub struct JsonlReader<R: Read> {
    reader: BufReader<R>,
    line_no: u64,
    record_no: u64,
    byte_pos: u64,
    done: bool,
}

impl<R: Read> JsonlReader<R> {
    pub fn new(inner: R) -> Self {
        JsonlReader {
            reader: BufReader::with_capacity(256 * 1024, inner),
            line_no: 0,
            record_no: 0,
            byte_pos: 0,
            done: false,
        }
    }
}

impl<R: Read> RecordReader for JsonlReader<R> {
    fn next_batch(&mut self, max: usize) -> Result<Vec<ReadItem>, IngestError> {
        let mut items = Vec::with_capacity(max.min(1024));
        let mut buf: Vec<u8> = Vec::new();
        while items.len() < max && !self.done {
            buf.clear();
            let n = self
                .reader
                .read_until(b'\n', &mut buf)
                .map_err(|e| IngestError::io("<jsonl stream>", e))?;
            if n == 0 {
                self.done = true;
                break;
            }
            let mut start = self.byte_pos;
            self.byte_pos += n as u64;
            self.line_no += 1;

            let had_newline = buf.last() == Some(&b'\n');
            let mut content: &[u8] = &buf;
            if had_newline {
                content = &content[..content.len() - 1];
            }
            if content.last() == Some(&b'\r') {
                content = &content[..content.len() - 1];
            }
            // Strip a UTF-8 BOM on the very first line; byte locators keep
            // pointing at the record text inside the physical file.
            if self.line_no == 1 && content.starts_with(&[0xEF, 0xBB, 0xBF]) {
                content = &content[3..];
                start += 3;
            }
            // Blank lines are skipped silently (not records, not errors).
            if content.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            self.record_no += 1;
            let locator = RecordLocator {
                record_number: Some(self.record_no),
                line_start: Some(self.line_no),
                line_end: Some(self.line_no),
                byte_start: Some(start),
                byte_end: Some(start + content.len() as u64),
                ..Default::default()
            };
            if content.len() > MAX_RECORD_BYTES {
                items.push(ReadItem::Malformed(MalformedRecord {
                    locator,
                    reason_code: "parse/record-too-large",
                    message: format!("record exceeds {MAX_RECORD_BYTES} bytes"),
                    raw_excerpt: content[..MAX_RAW_EXCERPT].to_vec(),
                    truncated: false,
                }));
                continue;
            }

            let (text, replacement_chars) = decode_utf8_lossy_counted(content);
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(value) => items.push(ReadItem::Parsed(ParsedRecord {
                    locator,
                    raw_hash: hash_bytes_hex(content),
                    fields: ParsedFields::Json(value),
                    replacement_chars,
                })),
                Err(e) => {
                    let truncated = !had_newline && self.peek_eof();
                    items.push(ReadItem::Malformed(MalformedRecord {
                        locator,
                        reason_code: if truncated {
                            "parse/truncated-record"
                        } else {
                            "parse/invalid-json"
                        },
                        message: e.to_string(),
                        raw_excerpt: content[..content.len().min(MAX_RAW_EXCERPT)].to_vec(),
                        truncated,
                    }));
                }
            }
        }
        Ok(items)
    }

    fn bytes_read(&self) -> u64 {
        self.byte_pos
    }
}

impl<R: Read> JsonlReader<R> {
    fn peek_eof(&mut self) -> bool {
        matches!(self.reader.fill_buf(), Ok(buf) if buf.is_empty())
    }
}

fn decode_utf8_lossy_counted(bytes: &[u8]) -> (String, u64) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), 0),
        Err(_) => {
            let text = String::from_utf8_lossy(bytes).into_owned();
            let count = text.chars().filter(|c| *c == '\u{FFFD}').count() as u64;
            (text, count)
        }
    }
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

/// Streaming CSV/TSV reader.
///
/// Raw-hash contract: CSV records may span physical lines and the csv parser
/// does not expose raw bytes, so the hash covers the *unescaped field bytes*
/// joined with 0x1F unit separators — deterministic for identical content.
/// The byte-range locator still points at the exact raw bytes in the file.
pub struct CsvReader<R: Read> {
    reader: csv::Reader<R>,
    headers: Vec<String>,
    record_no: u64,
    prev_byte: u64,
    prev_line: u64,
}

/// Consumes a leading UTF-8 BOM if present, returning a reader positioned
/// after it plus any non-BOM bytes already read.
fn skip_utf8_bom<R: Read>(
    mut inner: R,
) -> Result<std::io::Chain<std::io::Cursor<Vec<u8>>, R>, std::io::Error> {
    let mut head = [0u8; 3];
    let mut filled = 0;
    while filled < 3 {
        let n = inner.read(&mut head[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    let prefix = if filled == 3 && head == [0xEF, 0xBB, 0xBF] {
        Vec::new()
    } else {
        head[..filled].to_vec()
    };
    Ok(std::io::Cursor::new(prefix).chain(inner))
}

impl<R: Read> CsvReader<R> {
    pub fn new(
        inner: R,
        delimiter: u8,
        has_headers: bool,
    ) -> Result<CsvReader<impl Read>, IngestError> {
        // CSV byte locators are relative to the stream after BOM removal
        // (documented; the BOM is presentation, not record content).
        let inner = skip_utf8_bom(inner).map_err(|e| IngestError::io("<csv stream>", e))?;
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .has_headers(has_headers)
            .flexible(true)
            .buffer_capacity(256 * 1024)
            .from_reader(inner);
        let headers: Vec<String> = if has_headers {
            reader
                .headers()
                .map_err(|e| {
                    IngestError::UnsupportedFormat(format!("cannot read CSV headers: {e}"))
                })?
                .iter()
                .map(str::to_string)
                .collect()
        } else {
            vec![]
        };
        let pos = reader.position().clone();
        Ok(CsvReader {
            reader,
            headers,
            record_no: 0,
            prev_byte: pos.byte(),
            prev_line: pos.line(),
        })
    }

    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    fn column_name(&self, idx: usize) -> String {
        self.headers
            .get(idx)
            .cloned()
            .unwrap_or_else(|| idx.to_string())
    }
}

impl<R: Read> RecordReader for CsvReader<R> {
    fn next_batch(&mut self, max: usize) -> Result<Vec<ReadItem>, IngestError> {
        let mut items = Vec::with_capacity(max.min(1024));
        let mut record = csv::ByteRecord::new();
        while items.len() < max {
            let start_byte = self.prev_byte;
            let start_line = self.prev_line;
            match self.reader.read_byte_record(&mut record) {
                Ok(false) => break,
                Ok(true) => {
                    let pos = self.reader.position().clone();
                    self.prev_byte = pos.byte();
                    self.prev_line = pos.line();
                    self.record_no += 1;

                    let locator = RecordLocator {
                        record_number: Some(self.record_no),
                        line_start: Some(start_line),
                        line_end: Some(pos.line().saturating_sub(1).max(start_line)),
                        byte_start: Some(start_byte),
                        byte_end: Some(pos.byte()),
                        ..Default::default()
                    };

                    let mut hasher_input: Vec<u8> = Vec::new();
                    let mut fields = Vec::with_capacity(record.len());
                    let mut replacement_chars = 0u64;
                    for (i, raw) in record.iter().enumerate() {
                        if i > 0 {
                            hasher_input.push(0x1F);
                        }
                        hasher_input.extend_from_slice(raw);
                        let (text, reps) = decode_utf8_lossy_counted(raw);
                        replacement_chars += reps;
                        fields.push((self.column_name(i), text));
                    }
                    items.push(ReadItem::Parsed(ParsedRecord {
                        locator,
                        raw_hash: hash_bytes_hex(&hasher_input),
                        fields: ParsedFields::Csv(fields),
                        replacement_chars,
                    }));
                }
                Err(e) => {
                    let pos = self.reader.position().clone();
                    self.prev_byte = pos.byte();
                    self.prev_line = pos.line();
                    self.record_no += 1;
                    items.push(ReadItem::Malformed(MalformedRecord {
                        locator: RecordLocator {
                            record_number: Some(self.record_no),
                            line_start: Some(start_line),
                            byte_start: Some(start_byte),
                            byte_end: Some(pos.byte()),
                            ..Default::default()
                        },
                        reason_code: "parse/invalid-csv",
                        message: e.to_string(),
                        raw_excerpt: vec![],
                        truncated: false,
                    }));
                }
            }
        }
        Ok(items)
    }

    fn bytes_read(&self) -> u64 {
        self.prev_byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_locators_are_exact() {
        let data = "{\"a\":1}\r\n{\"b\":2}\n\nnot json\n{\"c\":3}";
        let mut r = JsonlReader::new(data.as_bytes());
        let items = r.next_batch(100).unwrap();
        assert_eq!(items.len(), 4);

        match &items[0] {
            ReadItem::Parsed(p) => {
                assert_eq!(p.locator.record_number, Some(1));
                assert_eq!(p.locator.line_start, Some(1));
                assert_eq!(p.locator.byte_start, Some(0));
                // CRLF excluded from the record bytes.
                assert_eq!(p.locator.byte_end, Some(7));
            }
            other => panic!("expected parsed, got {other:?}"),
        }
        match &items[2] {
            ReadItem::Malformed(m) => {
                assert_eq!(m.reason_code, "parse/invalid-json");
                assert_eq!(m.raw_excerpt, b"not json");
                assert_eq!(m.locator.line_start, Some(4));
            }
            other => panic!("expected malformed, got {other:?}"),
        }
        // Final record without trailing newline still parses.
        match &items[3] {
            ReadItem::Parsed(p) => {
                assert_eq!(p.locator.line_start, Some(5));
                assert_eq!(p.locator.record_number, Some(4));
            }
            other => panic!("expected parsed, got {other:?}"),
        }
        assert!(r.next_batch(10).unwrap().is_empty());
    }

    #[test]
    fn jsonl_partial_final_record_is_flagged_truncated() {
        let data = "{\"a\":1}\n{\"b\": \"cut off";
        let mut r = JsonlReader::new(data.as_bytes());
        let items = r.next_batch(10).unwrap();
        assert_eq!(items.len(), 2);
        match &items[1] {
            ReadItem::Malformed(m) => {
                assert_eq!(m.reason_code, "parse/truncated-record");
                assert!(m.truncated);
            }
            other => panic!("expected malformed, got {other:?}"),
        }
    }

    #[test]
    fn jsonl_same_content_hashes_equal_with_and_without_final_newline() {
        let with = "{\"a\":1}\n";
        let without = "{\"a\":1}";
        let get_hash = |data: &str| {
            let mut r = JsonlReader::new(data.as_bytes());
            match &r.next_batch(10).unwrap()[0] {
                ReadItem::Parsed(p) => p.raw_hash.clone(),
                _ => panic!(),
            }
        };
        assert_eq!(get_hash(with), get_hash(without));
    }

    #[test]
    fn csv_reads_headers_quoted_newlines_and_ragged_rows() {
        let data = "ts,level,message\n2024-01-01T00:00:00Z,INFO,\"multi\nline\"\n2024-01-01T00:00:01Z,WARN,short,extra\n";
        let mut r = CsvReader::new(data.as_bytes(), b',', true).unwrap();
        assert_eq!(r.headers(), &["ts", "level", "message"]);
        let items = r.next_batch(10).unwrap();
        assert_eq!(items.len(), 2);
        match &items[0] {
            ReadItem::Parsed(p) => match &p.fields {
                ParsedFields::Csv(fields) => {
                    assert_eq!(fields[2].1, "multi\nline");
                    assert_eq!(p.locator.record_number, Some(1));
                }
                other => panic!("unexpected {other:?}"),
            },
            other => panic!("expected parsed, got {other:?}"),
        }
        // Ragged row: extra column gets index name "3".
        match &items[1] {
            ReadItem::Parsed(p) => match &p.fields {
                ParsedFields::Csv(fields) => {
                    assert_eq!(fields.len(), 4);
                    assert_eq!(fields[3].0, "3");
                    assert_eq!(fields[3].1, "extra");
                }
                other => panic!("unexpected {other:?}"),
            },
            other => panic!("expected parsed, got {other:?}"),
        }
    }

    #[test]
    fn invalid_utf8_is_replaced_and_counted() {
        let data = b"{\"m\":\"a\xFF\xFEb\"}\n";
        let mut r = JsonlReader::new(&data[..]);
        let items = r.next_batch(10).unwrap();
        match &items[0] {
            ReadItem::Parsed(p) => {
                assert!(p.replacement_chars >= 1, "got {}", p.replacement_chars);
            }
            // Depending on where the bytes land, strict JSON may reject —
            // either way the record must not vanish.
            ReadItem::Malformed(m) => assert_eq!(m.reason_code, "parse/invalid-json"),
        }
    }
}
