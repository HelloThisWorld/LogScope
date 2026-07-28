//! Canonical time representation.
//!
//! All canonical timestamps are UTC nanoseconds since the Unix epoch.
//! The original timestamp text, its precision, and the timezone assumption
//! made during parsing are preserved alongside the canonical value.

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// UTC nanoseconds since the Unix epoch.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct UnixNanos(pub i64);

impl UnixNanos {
    pub fn from_datetime(dt: &DateTime<Utc>) -> Option<Self> {
        dt.timestamp_nanos_opt().map(UnixNanos)
    }

    pub fn to_datetime(self) -> DateTime<Utc> {
        Utc.timestamp_nanos(self.0)
    }

    pub fn to_rfc3339(self) -> String {
        self.to_datetime()
            .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
    }

    /// Wall-clock now. Never part of any deterministic hash.
    pub fn now() -> Self {
        UnixNanos(Utc::now().timestamp_nanos_opt().unwrap_or(0))
    }
}

/// Precision of the timestamp as written in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimestampPrecision {
    Seconds,
    Milliseconds,
    Microseconds,
    Nanoseconds,
}

/// How the timezone of a source timestamp was determined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "zone", rename_all = "snake_case")]
pub enum TimezoneAssumption {
    /// The source text carried an explicit offset or `Z`.
    OffsetInText,
    /// The source had no zone information; UTC was assumed.
    AssumedUtc,
    /// The Import Profile declared a zone (IANA name recorded).
    ProfileZone(String),
    /// Numeric epoch value: zone concept does not apply.
    EpochValue,
}
