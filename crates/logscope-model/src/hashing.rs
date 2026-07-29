//! Deterministic content hashing.
//!
//! Canonical IDs and record hashes are BLAKE3 digests over an explicit,
//! versioned byte encoding (tag byte + length-prefixed payloads). We do not
//! hash JSON text: the byte encoding is independent of serializer quirks and
//! is the stability contract for deterministic re-imports.

/// Incremental digest writer with an explicit, self-delimiting encoding.
///
/// Every variable-length payload is length-prefixed, and every composite
/// value writes a tag first, so distinct value trees can never produce the
/// same byte stream.
pub struct Digest {
    hasher: blake3::Hasher,
}

impl Digest {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            hasher: blake3::Hasher::new(),
        }
    }

    pub fn tag(&mut self, tag: u8) -> &mut Self {
        self.hasher.update(&[tag]);
        self
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.hasher.update(&(bytes.len() as u64).to_le_bytes());
        self.hasher.update(bytes);
        self
    }

    pub fn str(&mut self, s: &str) -> &mut Self {
        self.bytes(s.as_bytes())
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.hasher.update(&[v as u8]);
        self
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.hasher.update(&[v]);
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.hasher.update(&v.to_le_bytes());
        self
    }

    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.hasher.update(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.hasher.update(&v.to_le_bytes());
        self
    }

    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.hasher.update(&v.to_le_bytes());
        self
    }

    /// Hashes the exact bit pattern, so `NaN` payloads and `-0.0` are
    /// distinguished deterministically.
    pub fn f64_bits(&mut self, v: f64) -> &mut Self {
        self.hasher.update(&v.to_bits().to_le_bytes());
        self
    }

    /// Encodes `None` as tag 0 and `Some` as tag 1 + value, so an absent
    /// value can never collide with an empty one.
    pub fn opt_str(&mut self, v: Option<&str>) -> &mut Self {
        match v {
            None => self.tag(0),
            Some(s) => self.tag(1).str(s),
        }
    }

    pub fn opt_i64(&mut self, v: Option<i64>) -> &mut Self {
        match v {
            None => self.tag(0),
            Some(n) => self.tag(1).i64(n),
        }
    }

    pub fn opt_u64(&mut self, v: Option<u64>) -> &mut Self {
        match v {
            None => self.tag(0),
            Some(n) => self.tag(1).u64(n),
        }
    }

    pub fn opt_u32(&mut self, v: Option<u32>) -> &mut Self {
        match v {
            None => self.tag(0),
            Some(n) => self.tag(1).u32(n),
        }
    }

    pub fn opt_i32(&mut self, v: Option<i32>) -> &mut Self {
        match v {
            None => self.tag(0),
            Some(n) => self.tag(1).i32(n),
        }
    }

    /// Full 64-hex-char BLAKE3 digest.
    pub fn finish_hex(self) -> String {
        self.hasher.finalize().to_hex().to_string()
    }

    /// First 32 hex chars (128 bits) — the standard length for stable IDs.
    pub fn finish_hex32(self) -> String {
        let hex = self.hasher.finalize().to_hex();
        hex.as_str()[..32].to_string()
    }
}

/// Builds a stable ID of the form `{prefix}-{32 hex chars}`.
pub fn stable_id(prefix: &str, write: impl FnOnce(&mut Digest)) -> String {
    let mut d = Digest::new();
    write(&mut d);
    format!("{}-{}", prefix, d.finish_hex32())
}

/// BLAKE3 hex digest of raw bytes (used for raw-record and file hashes).
pub fn hash_bytes_hex(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_is_deterministic() {
        let a = stable_id("res", |d| {
            d.str("service.name").str("checkout");
        });
        let b = stable_id("res", |d| {
            d.str("service.name").str("checkout");
        });
        assert_eq!(a, b);
        assert!(a.starts_with("res-"));
        assert_eq!(a.len(), "res-".len() + 32);
    }

    #[test]
    fn none_and_empty_do_not_collide() {
        let none = stable_id("x", |d| {
            d.opt_str(None);
        });
        let empty = stable_id("x", |d| {
            d.opt_str(Some(""));
        });
        assert_ne!(none, empty);
    }

    #[test]
    fn adjacent_strings_do_not_merge() {
        let ab_c = stable_id("x", |d| {
            d.str("ab").str("c");
        });
        let a_bc = stable_id("x", |d| {
            d.str("a").str("bc");
        });
        assert_ne!(ab_c, a_bc);
    }
}
