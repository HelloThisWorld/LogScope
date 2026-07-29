//! W3C/OTLP trace and span identifiers, stored as lowercase hex strings.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IdError {
    #[error("expected {expected} hex chars, got {got}")]
    WrongLength { expected: usize, got: usize },
    #[error("non-hex character in identifier")]
    NotHex,
    #[error("all-zero identifier is invalid")]
    AllZero,
}

fn validate_hex(s: &str, expected_len: usize) -> Result<String, IdError> {
    if s.len() != expected_len {
        return Err(IdError::WrongLength {
            expected: expected_len,
            got: s.len(),
        });
    }
    if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(IdError::NotHex);
    }
    let lower = s.to_ascii_lowercase();
    if lower.bytes().all(|b| b == b'0') {
        return Err(IdError::AllZero);
    }
    Ok(lower)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 128-bit trace ID as 32 lowercase hex chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct TraceId(String);

impl TraceId {
    pub fn from_hex(s: &str) -> Result<Self, IdError> {
        validate_hex(s, 32).map(TraceId)
    }

    /// From raw OTLP bytes. Returns `None` for empty or all-zero IDs, which
    /// the OTLP spec defines as "absent". Non-16-byte payloads are an error.
    pub fn from_bytes(bytes: &[u8]) -> Result<Option<Self>, IdError> {
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() != 16 {
            return Err(IdError::WrongLength {
                expected: 16,
                got: bytes.len(),
            });
        }
        if bytes.iter().all(|b| *b == 0) {
            return Ok(None);
        }
        Ok(Some(TraceId(bytes_to_hex(bytes))))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 64-bit span ID as 16 lowercase hex chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct SpanId(String);

impl SpanId {
    pub fn from_hex(s: &str) -> Result<Self, IdError> {
        validate_hex(s, 16).map(SpanId)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Option<Self>, IdError> {
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() != 8 {
            return Err(IdError::WrongLength {
                expected: 8,
                got: bytes.len(),
            });
        }
        if bytes.iter().all(|b| *b == 0) {
            return Ok(None);
        }
        Ok(Some(SpanId(bytes_to_hex(bytes))))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_id_validation() {
        assert!(TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").is_ok());
        assert_eq!(
            TraceId::from_hex("0AF7651916CD43DD8448EB211C80319C")
                .unwrap()
                .as_str(),
            "0af7651916cd43dd8448eb211c80319c"
        );
        assert_eq!(
            TraceId::from_hex("00000000000000000000000000000000"),
            Err(IdError::AllZero)
        );
        assert!(matches!(
            TraceId::from_hex("abc"),
            Err(IdError::WrongLength { .. })
        ));
        assert_eq!(
            TraceId::from_hex("zzf7651916cd43dd8448eb211c80319c"),
            Err(IdError::NotHex)
        );
    }

    #[test]
    fn zero_bytes_mean_absent() {
        assert_eq!(TraceId::from_bytes(&[0u8; 16]).unwrap(), None);
        assert_eq!(SpanId::from_bytes(&[]).unwrap(), None);
        let id = SpanId::from_bytes(&[1, 2, 3, 4, 5, 6, 7, 8])
            .unwrap()
            .unwrap();
        assert_eq!(id.as_str(), "0102030405060708");
    }
}
