//! Locating the ZIP payload appended to this executable.
//!
//! Per ADR-0018 the graphical setup is a launcher stub with the canonical
//! payload appended, in the spirit of a self-contained Java archive. The
//! stub finds its own payload by reading a fixed-size trailer at the very
//! end of the file, so the stub never has to be rebuilt when the payload
//! changes and both public artifacts wrap byte-identical payload bytes.
//!
//! Layout:
//!
//! ```text
//! [ stub executable ][ payload ZIP ][ trailer ]
//! ```
//!
//! The trailer is the last [`TRAILER_LEN`] bytes of the file.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Identifies a LogScope setup payload. Version is part of the magic so a
/// future incompatible layout cannot be misread as this one.
pub const MAGIC: &[u8; 18] = b"LOGSCOPE-PAYLOAD-1";

/// `offset(8) + len(8) + sha256(32) + format(4) + magic(18)`.
pub const TRAILER_LEN: u64 = 8 + 8 + 32 + 4 + MAGIC.len() as u64;

/// Layout revision. Bumped only for an incompatible trailer change; the
/// magic changes with it so old stubs refuse rather than misparse.
pub const FORMAT_VERSION: u32 = 1;

/// A refusal to read a payload. Every variant is a safe, precise state:
/// none of them is recoverable by guessing.
#[derive(Debug)]
pub enum PayloadError {
    /// The file is smaller than a trailer, so it cannot carry a payload.
    TooSmall,
    /// No LogScope trailer at the end: this stub has no payload appended.
    NoPayload,
    /// Trailer found but its declared layout revision is not supported.
    UnsupportedFormat(u32),
    /// Trailer describes a region outside the file.
    OutOfRange {
        offset: u64,
        len: u64,
        file: u64,
    },
    /// Payload bytes do not match the digest recorded in the trailer.
    DigestMismatch {
        expected: String,
        actual: String,
    },
    Io(std::io::Error),
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PayloadError::TooSmall => write!(f, "file is too small to contain a payload"),
            PayloadError::NoPayload => write!(
                f,
                "no LogScope payload is attached to this executable; \
                 use the Portable ZIP instead"
            ),
            PayloadError::UnsupportedFormat(v) => write!(
                f,
                "payload layout revision {v} is not supported by this setup \
                 (supported: {FORMAT_VERSION})"
            ),
            PayloadError::OutOfRange { offset, len, file } => write!(
                f,
                "payload region {offset}..{} lies outside the {file}-byte file",
                offset.saturating_add(*len)
            ),
            PayloadError::DigestMismatch { expected, actual } => write!(
                f,
                "payload is corrupt: expected SHA-256 {expected}, found {actual}"
            ),
            PayloadError::Io(e) => write!(f, "cannot read the payload: {e}"),
        }
    }
}

impl From<std::io::Error> for PayloadError {
    fn from(e: std::io::Error) -> Self {
        PayloadError::Io(e)
    }
}

/// A verified payload read out of the executable.
pub struct Payload {
    pub bytes: Vec<u8>,
    pub sha256: String,
}

/// Reads and verifies the payload appended to `exe`.
///
/// The digest is checked before the bytes are handed back, so a truncated
/// or tampered download fails here rather than half-way through writing
/// files into the user's destination.
pub fn read_appended(exe: &Path) -> Result<Payload, PayloadError> {
    let mut f = File::open(exe)?;
    let file_len = f.seek(SeekFrom::End(0))?;
    if file_len < TRAILER_LEN {
        return Err(PayloadError::TooSmall);
    }

    f.seek(SeekFrom::End(-(TRAILER_LEN as i64)))?;
    let mut trailer = vec![0u8; TRAILER_LEN as usize];
    f.read_exact(&mut trailer)?;

    if &trailer[trailer.len() - MAGIC.len()..] != MAGIC {
        return Err(PayloadError::NoPayload);
    }

    let offset = u64::from_le_bytes(trailer[0..8].try_into().expect("8 bytes"));
    let len = u64::from_le_bytes(trailer[8..16].try_into().expect("8 bytes"));
    let expected: [u8; 32] = trailer[16..48].try_into().expect("32 bytes");
    let format = u32::from_le_bytes(trailer[48..52].try_into().expect("4 bytes"));

    if format != FORMAT_VERSION {
        return Err(PayloadError::UnsupportedFormat(format));
    }

    // The payload must end exactly where the trailer begins; anything else
    // means the trailer is describing a different file.
    let payload_end = offset.checked_add(len);
    if payload_end != Some(file_len - TRAILER_LEN) {
        return Err(PayloadError::OutOfRange {
            offset,
            len,
            file: file_len,
        });
    }

    f.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0u8; len as usize];
    f.read_exact(&mut bytes)?;

    let actual: [u8; 32] = Sha256::digest(&bytes).into();
    if actual != expected {
        return Err(PayloadError::DigestMismatch {
            expected: hex(&expected),
            actual: hex(&actual),
        });
    }

    Ok(Payload {
        sha256: hex(&actual),
        bytes,
    })
}

/// Builds the trailer for `payload`, to be appended after the payload bytes.
pub fn build_trailer(payload: &[u8], offset: u64) -> Vec<u8> {
    let digest: [u8; 32] = Sha256::digest(payload).into();
    let mut t = Vec::with_capacity(TRAILER_LEN as usize);
    t.extend_from_slice(&offset.to_le_bytes());
    t.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    t.extend_from_slice(&digest);
    t.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    t.extend_from_slice(MAGIC);
    debug_assert_eq!(t.len() as u64, TRAILER_LEN);
    t
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn stub_with(payload: &[u8], stub: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("setup.exe");
        let mut f = File::create(&path).unwrap();
        f.write_all(stub).unwrap();
        f.write_all(payload).unwrap();
        f.write_all(&build_trailer(payload, stub.len() as u64))
            .unwrap();
        drop(f);
        (dir, path)
    }

    #[test]
    fn round_trips_an_appended_payload() {
        let payload = b"PK\x03\x04 pretend archive".to_vec();
        let (_d, path) = stub_with(&payload, b"MZ stub bytes");
        let got = read_appended(&path).unwrap();
        assert_eq!(got.bytes, payload);
        assert_eq!(got.sha256, hex(&<[u8; 32]>::from(Sha256::digest(&payload))));
    }

    #[test]
    fn empty_payload_round_trips() {
        let (_d, path) = stub_with(b"", b"MZ stub bytes");
        assert!(read_appended(&path).unwrap().bytes.is_empty());
    }

    #[test]
    fn a_stub_without_a_payload_is_refused_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bare.exe");
        std::fs::write(&path, vec![0x4d; 4096]).unwrap();
        assert!(matches!(read_appended(&path), Err(PayloadError::NoPayload)));
    }

    #[test]
    fn a_file_shorter_than_the_trailer_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tiny.exe");
        std::fs::write(&path, b"MZ").unwrap();
        assert!(matches!(read_appended(&path), Err(PayloadError::TooSmall)));
    }

    #[test]
    fn a_truncated_payload_is_detected_before_extraction() {
        let payload = b"PK\x03\x04 pretend archive".to_vec();
        let (_d, path) = stub_with(&payload, b"MZ stub bytes");
        // Drop bytes from the middle of the payload, keeping the trailer.
        let all = std::fs::read(&path).unwrap();
        let cut = all.len() - TRAILER_LEN as usize - 4;
        let mut damaged = all[..cut].to_vec();
        damaged.extend_from_slice(&all[all.len() - TRAILER_LEN as usize..]);
        std::fs::write(&path, damaged).unwrap();
        assert!(matches!(
            read_appended(&path),
            Err(PayloadError::OutOfRange { .. })
        ));
    }

    #[test]
    fn a_tampered_payload_fails_the_digest() {
        let payload = b"PK\x03\x04 pretend archive".to_vec();
        let (_d, path) = stub_with(&payload, b"MZ stub bytes");
        let mut all = std::fs::read(&path).unwrap();
        let stub_len = b"MZ stub bytes".len();
        all[stub_len + 5] ^= 0xff; // flip a byte inside the payload
        std::fs::write(&path, all).unwrap();
        assert!(matches!(
            read_appended(&path),
            Err(PayloadError::DigestMismatch { .. })
        ));
    }

    #[test]
    fn a_future_layout_revision_is_refused_rather_than_misparsed() {
        let payload = b"payload".to_vec();
        let (_d, path) = stub_with(&payload, b"MZ");
        let mut all = std::fs::read(&path).unwrap();
        let fmt_at = all.len() - MAGIC.len() - 4;
        all[fmt_at..fmt_at + 4].copy_from_slice(&99u32.to_le_bytes());
        std::fs::write(&path, all).unwrap();
        assert!(matches!(
            read_appended(&path),
            Err(PayloadError::UnsupportedFormat(99))
        ));
    }
}
