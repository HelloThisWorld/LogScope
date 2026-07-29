//! Canonical typed attribute values.
//!
//! `AnyValue` mirrors the OTLP `AnyValue` shape (string, bool, int, double,
//! bytes, array, map, empty) with deterministic ordering (maps are
//! `BTreeMap`) and a lossless canonical JSON form. Doubles keep their exact
//! bit pattern (including NaN and infinities) across round trips.

use std::collections::BTreeMap;
use std::fmt;

use base64::Engine as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::hashing::Digest;

/// `f64` wrapper with deterministic (bit-pattern) equality and lossless JSON
/// round trips: finite values serialize as JSON numbers; non-finite values
/// serialize as the strings `"NaN"`, `"+Inf"`, `"-Inf"`.
#[derive(Debug, Clone, Copy)]
pub struct F64(pub f64);

impl PartialEq for F64 {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}
impl Eq for F64 {}

impl From<f64> for F64 {
    fn from(v: f64) -> Self {
        F64(v)
    }
}

impl Serialize for F64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.is_finite() {
            serializer.serialize_f64(self.0)
        } else if self.0.is_nan() {
            serializer.serialize_str("NaN")
        } else if self.0 > 0.0 {
            serializer.serialize_str("+Inf")
        } else {
            serializer.serialize_str("-Inf")
        }
    }
}

impl<'de> Deserialize<'de> for F64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = F64;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON number or one of \"NaN\", \"+Inf\", \"-Inf\"")
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<F64, E> {
                Ok(F64(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<F64, E> {
                Ok(F64(v as f64))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<F64, E> {
                Ok(F64(v as f64))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<F64, E> {
                match v {
                    "NaN" => Ok(F64(f64::NAN)),
                    "+Inf" => Ok(F64(f64::INFINITY)),
                    "-Inf" => Ok(F64(f64::NEG_INFINITY)),
                    other => Err(E::custom(format!("invalid double literal: {other:?}"))),
                }
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Byte payloads serialize as standard base64 strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByteValue(pub Vec<u8>);

impl Serialize for ByteValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for ByteValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map(ByteValue)
            .map_err(serde::de::Error::custom)
    }
}

/// Deterministically ordered attribute map.
pub type AttrMap = BTreeMap<String, AnyValue>;

/// Canonical typed value. The JSON form is adjacently tagged
/// (`{"t":"int","v":42}`) so types survive storage and re-parsing exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum AnyValue {
    /// OTLP "empty" value (present key, no value). Also used for JSON null.
    Empty,
    Str(String),
    Bool(bool),
    Int(i64),
    Double(F64),
    Bytes(ByteValue),
    Array(Vec<AnyValue>),
    Map(AttrMap),
}

impl AnyValue {
    pub fn str(v: impl Into<String>) -> Self {
        AnyValue::Str(v.into())
    }
    pub fn int(v: i64) -> Self {
        AnyValue::Int(v)
    }
    pub fn double(v: f64) -> Self {
        AnyValue::Double(F64(v))
    }
    pub fn bool(v: bool) -> Self {
        AnyValue::Bool(v)
    }
    pub fn bytes(v: Vec<u8>) -> Self {
        AnyValue::Bytes(ByteValue(v))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            AnyValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Converts plain (untagged) JSON into a typed value. Integers in the
    /// exact `i64` range become `Int`; other numbers become `Double`; null
    /// becomes `Empty`. Object keys are deterministically ordered.
    pub fn from_plain_json(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => AnyValue::Empty,
            serde_json::Value::Bool(b) => AnyValue::Bool(*b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    AnyValue::Int(i)
                } else if let Some(u) = n.as_u64() {
                    // u64 above i64::MAX: preserve digits as a string rather
                    // than silently losing precision in a double.
                    AnyValue::Str(u.to_string())
                } else {
                    AnyValue::Double(F64(n.as_f64().unwrap_or(f64::NAN)))
                }
            }
            serde_json::Value::String(s) => AnyValue::Str(s.clone()),
            serde_json::Value::Array(items) => {
                AnyValue::Array(items.iter().map(Self::from_plain_json).collect())
            }
            serde_json::Value::Object(map) => AnyValue::Map(
                map.iter()
                    .map(|(k, v)| (k.clone(), Self::from_plain_json(v)))
                    .collect(),
            ),
        }
    }

    /// Plain (untagged) JSON view for display and full-text indexing. Lossy
    /// for type tags (int vs double) and non-finite doubles; never used as a
    /// storage format.
    pub fn to_plain_json(&self) -> serde_json::Value {
        match self {
            AnyValue::Empty => serde_json::Value::Null,
            AnyValue::Str(s) => serde_json::Value::String(s.clone()),
            AnyValue::Bool(b) => serde_json::Value::Bool(*b),
            AnyValue::Int(i) => serde_json::Value::Number((*i).into()),
            AnyValue::Double(F64(d)) => serde_json::Number::from_f64(*d)
                .map(serde_json::Value::Number)
                .unwrap_or_else(|| serde_json::Value::String(format!("{d}"))),
            AnyValue::Bytes(b) => {
                serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(&b.0))
            }
            AnyValue::Array(items) => {
                serde_json::Value::Array(items.iter().map(|v| v.to_plain_json()).collect())
            }
            AnyValue::Map(map) => serde_json::Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), v.to_plain_json()))
                    .collect(),
            ),
        }
    }

    /// Human-readable rendering used for derived display messages.
    pub fn display_string(&self) -> String {
        match self {
            AnyValue::Str(s) => s.clone(),
            other => serde_json::to_string(&other.to_plain_json()).unwrap_or_default(),
        }
    }

    pub fn digest_into(&self, d: &mut Digest) {
        match self {
            AnyValue::Empty => {
                d.tag(0x00);
            }
            AnyValue::Str(s) => {
                d.tag(0x01).str(s);
            }
            AnyValue::Bool(b) => {
                d.tag(0x02).bool(*b);
            }
            AnyValue::Int(i) => {
                d.tag(0x03).i64(*i);
            }
            AnyValue::Double(F64(f)) => {
                d.tag(0x04).f64_bits(*f);
            }
            AnyValue::Bytes(b) => {
                d.tag(0x05).bytes(&b.0);
            }
            AnyValue::Array(items) => {
                d.tag(0x06).u64(items.len() as u64);
                for item in items {
                    item.digest_into(d);
                }
            }
            AnyValue::Map(map) => {
                d.tag(0x07).u64(map.len() as u64);
                for (k, v) in map {
                    d.str(k);
                    v.digest_into(d);
                }
            }
        }
    }
}

/// Digests a full attribute map (deterministic: BTreeMap iteration order).
pub fn digest_attrs(attrs: &AttrMap, d: &mut Digest) {
    d.u64(attrs.len() as u64);
    for (k, v) in attrs {
        d.str(k);
        v.digest_into(d);
    }
}

/// Canonical JSON text of an attribute map (compact, deterministic order).
pub fn attrs_canonical_json(attrs: &AttrMap) -> String {
    serde_json::to_string(attrs).expect("AttrMap serialization cannot fail")
}

/// Parses canonical JSON text back into an attribute map.
pub fn attrs_from_canonical_json(s: &str) -> Result<AttrMap, serde_json::Error> {
    serde_json::from_str(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_json_round_trip_preserves_types() {
        let mut map = AttrMap::new();
        map.insert("s".into(), AnyValue::str("text"));
        map.insert("i".into(), AnyValue::int(42));
        map.insert("d".into(), AnyValue::double(42.0));
        map.insert("b".into(), AnyValue::bool(true));
        map.insert("y".into(), AnyValue::bytes(vec![0, 1, 255]));
        map.insert(
            "a".into(),
            AnyValue::Array(vec![AnyValue::int(1), AnyValue::str("x")]),
        );
        map.insert("e".into(), AnyValue::Empty);

        let json = attrs_canonical_json(&map);
        let back = attrs_from_canonical_json(&json).unwrap();
        assert_eq!(map, back);
        // Int and Double with the same numeric value stay distinct.
        assert_ne!(back.get("i"), Some(&AnyValue::double(42.0)));
    }

    #[test]
    fn non_finite_doubles_round_trip() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0, 1.5e308] {
            let val = AnyValue::double(v);
            let json = serde_json::to_string(&val).unwrap();
            let back: AnyValue = serde_json::from_str(&json).unwrap();
            assert_eq!(val, back, "value {v} failed round trip via {json}");
        }
    }

    #[test]
    fn canonical_json_is_order_independent() {
        let mut a = AttrMap::new();
        a.insert("zeta".into(), AnyValue::int(1));
        a.insert("alpha".into(), AnyValue::int(2));
        let mut b = AttrMap::new();
        b.insert("alpha".into(), AnyValue::int(2));
        b.insert("zeta".into(), AnyValue::int(1));
        assert_eq!(attrs_canonical_json(&a), attrs_canonical_json(&b));
    }

    #[test]
    fn plain_json_conversion_keeps_integer_precision() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"big": 9007199254740993, "huge": 18446744073709551615}"#)
                .unwrap();
        let typed = AnyValue::from_plain_json(&v);
        match &typed {
            AnyValue::Map(m) => {
                assert_eq!(m.get("big"), Some(&AnyValue::int(9007199254740993)));
                // Above i64::MAX: preserved as digit string, not a lossy double.
                assert_eq!(m.get("huge"), Some(&AnyValue::str("18446744073709551615")));
            }
            other => panic!("expected map, got {other:?}"),
        }
    }
}
