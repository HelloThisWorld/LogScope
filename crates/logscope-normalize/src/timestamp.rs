//! Timestamp parsing with explicit timezone assumptions.
//!
//! Every parse records how the zone was determined and what precision the
//! source text carried. Local times in a profile-declared zone handle DST
//! transitions deterministically: ambiguous times resolve to the earlier
//! instant (recorded via `ambiguous`), nonexistent times are errors.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use logscope_model::{TimestampPrecision, TimezoneAssumption, UnixNanos};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// How to interpret the timestamp field/column text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimestampFormat {
    /// RFC 3339 / ISO 8601 with offset (`2024-05-01T12:00:00.123Z`).
    Rfc3339,
    /// Integer seconds since epoch; fractional part allowed.
    EpochSeconds,
    EpochMillis,
    EpochMicros,
    EpochNanos,
    /// chrono strftime format string, e.g. `%Y-%m-%d %H:%M:%S%.3f`.
    Chrono {
        format: String,
    },
}

/// Zone to apply when the text itself has no offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TimezonePolicy {
    /// Assume UTC (the documented default for offset-less sources).
    AssumeUtc,
    /// IANA zone name declared by the Import Profile.
    Zone { name: String },
    /// Fixed offset in seconds east of UTC declared by the profile.
    FixedOffsetSeconds { seconds: i32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTimestamp {
    pub nanos: UnixNanos,
    pub precision: TimestampPrecision,
    pub assumption: TimezoneAssumption,
    /// True when a profile-zone local time fell in a DST overlap and the
    /// earlier instant was chosen.
    pub ambiguous: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TimestampError {
    #[error("timestamp text {text:?} does not match format: {detail}")]
    Unparsable { text: String, detail: String },
    #[error("timestamp out of representable range (1677..2262)")]
    OutOfRange,
    #[error("unknown IANA timezone {0:?}")]
    UnknownZone(String),
    #[error("local time {0:?} does not exist in the target zone (DST gap)")]
    NonexistentLocalTime(String),
    #[error("invalid fixed offset seconds {0}")]
    InvalidOffset(i32),
}

/// Counts fractional-second digits in the text to classify precision.
fn text_precision(text: &str) -> TimestampPrecision {
    let frac_digits = text
        .rfind('.')
        .map(|dot| {
            text[dot + 1..]
                .bytes()
                .take_while(|b| b.is_ascii_digit())
                .count()
        })
        .unwrap_or(0);
    match frac_digits {
        0 => TimestampPrecision::Seconds,
        1..=3 => TimestampPrecision::Milliseconds,
        4..=6 => TimestampPrecision::Microseconds,
        _ => TimestampPrecision::Nanoseconds,
    }
}

fn to_nanos(dt: DateTime<Utc>) -> Result<UnixNanos, TimestampError> {
    UnixNanos::from_datetime(&dt).ok_or(TimestampError::OutOfRange)
}

fn resolve_local(
    naive: NaiveDateTime,
    policy: &TimezonePolicy,
    text: &str,
) -> Result<(DateTime<Utc>, TimezoneAssumption, bool), TimestampError> {
    match policy {
        TimezonePolicy::AssumeUtc => Ok((
            Utc.from_utc_datetime(&naive),
            TimezoneAssumption::AssumedUtc,
            false,
        )),
        TimezonePolicy::FixedOffsetSeconds { seconds } => {
            let offset = chrono::FixedOffset::east_opt(*seconds)
                .ok_or(TimestampError::InvalidOffset(*seconds))?;
            let dt = offset
                .from_local_datetime(&naive)
                .single()
                .ok_or_else(|| TimestampError::NonexistentLocalTime(text.to_string()))?;
            Ok((
                dt.with_timezone(&Utc),
                TimezoneAssumption::ProfileZone(format!("UTC{}", offset)),
                false,
            ))
        }
        TimezonePolicy::Zone { name } => {
            let tz: chrono_tz::Tz = name
                .parse()
                .map_err(|_| TimestampError::UnknownZone(name.clone()))?;
            match tz.from_local_datetime(&naive) {
                chrono::LocalResult::Single(dt) => Ok((
                    dt.with_timezone(&Utc),
                    TimezoneAssumption::ProfileZone(name.clone()),
                    false,
                )),
                // DST overlap: deterministically choose the earlier instant.
                chrono::LocalResult::Ambiguous(earlier, _later) => Ok((
                    earlier.with_timezone(&Utc),
                    TimezoneAssumption::ProfileZone(name.clone()),
                    true,
                )),
                chrono::LocalResult::None => {
                    Err(TimestampError::NonexistentLocalTime(text.to_string()))
                }
            }
        }
    }
}

/// True when the strftime format consumes an offset or zone from the text.
fn chrono_format_has_zone(format: &str) -> bool {
    let bytes = format.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'%' {
            match bytes[i + 1] {
                b'z' | b'Z' => return true,
                b':' | b'#' if i + 2 < bytes.len() && bytes[i + 2] == b'z' => return true,
                _ => {}
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    false
}

fn parse_epoch(
    text: &str,
    unit_nanos: i128,
    precision: TimestampPrecision,
) -> Result<ParsedTimestamp, TimestampError> {
    let trimmed = text.trim();
    let nanos: i128 = if let Ok(int) = trimmed.parse::<i128>() {
        int * unit_nanos
    } else if let Ok(float) = trimmed.parse::<f64>() {
        if !float.is_finite() {
            return Err(TimestampError::Unparsable {
                text: text.to_string(),
                detail: "non-finite epoch value".into(),
            });
        }
        (float * unit_nanos as f64) as i128
    } else {
        return Err(TimestampError::Unparsable {
            text: text.to_string(),
            detail: "not a number".into(),
        });
    };
    let nanos = i64::try_from(nanos).map_err(|_| TimestampError::OutOfRange)?;
    // Fractional input refines the effective precision.
    let effective = if trimmed.contains('.') {
        text_precision(trimmed)
    } else {
        precision
    };
    Ok(ParsedTimestamp {
        nanos: UnixNanos(nanos),
        precision: effective,
        assumption: TimezoneAssumption::EpochValue,
        ambiguous: false,
    })
}

/// Parses timestamp text according to the profile's format and zone policy.
pub fn parse_timestamp(
    text: &str,
    format: &TimestampFormat,
    policy: &TimezonePolicy,
) -> Result<ParsedTimestamp, TimestampError> {
    match format {
        TimestampFormat::Rfc3339 => {
            let dt = DateTime::parse_from_rfc3339(text.trim()).map_err(|e| {
                TimestampError::Unparsable {
                    text: text.to_string(),
                    detail: e.to_string(),
                }
            })?;
            Ok(ParsedTimestamp {
                nanos: to_nanos(dt.with_timezone(&Utc))?,
                precision: text_precision(text),
                assumption: TimezoneAssumption::OffsetInText,
                ambiguous: false,
            })
        }
        TimestampFormat::EpochSeconds => {
            parse_epoch(text, 1_000_000_000, TimestampPrecision::Seconds)
        }
        TimestampFormat::EpochMillis => {
            parse_epoch(text, 1_000_000, TimestampPrecision::Milliseconds)
        }
        TimestampFormat::EpochMicros => parse_epoch(text, 1_000, TimestampPrecision::Microseconds),
        TimestampFormat::EpochNanos => parse_epoch(text, 1, TimestampPrecision::Nanoseconds),
        TimestampFormat::Chrono { format } => {
            let trimmed = text.trim();
            if chrono_format_has_zone(format) {
                let dt = DateTime::parse_from_str(trimmed, format).map_err(|e| {
                    TimestampError::Unparsable {
                        text: text.to_string(),
                        detail: e.to_string(),
                    }
                })?;
                Ok(ParsedTimestamp {
                    nanos: to_nanos(dt.with_timezone(&Utc))?,
                    precision: text_precision(trimmed),
                    assumption: TimezoneAssumption::OffsetInText,
                    ambiguous: false,
                })
            } else {
                let naive = NaiveDateTime::parse_from_str(trimmed, format).map_err(|e| {
                    TimestampError::Unparsable {
                        text: text.to_string(),
                        detail: e.to_string(),
                    }
                })?;
                let (dt, assumption, ambiguous) = resolve_local(naive, policy, trimmed)?;
                Ok(ParsedTimestamp {
                    nanos: to_nanos(dt)?,
                    precision: text_precision(trimmed),
                    assumption,
                    ambiguous,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_with_offset() {
        let p = parse_timestamp(
            "2024-05-01T12:30:00.123456+02:00",
            &TimestampFormat::Rfc3339,
            &TimezonePolicy::AssumeUtc,
        )
        .unwrap();
        assert_eq!(p.assumption, TimezoneAssumption::OffsetInText);
        assert_eq!(p.precision, TimestampPrecision::Microseconds);
        assert_eq!(p.nanos.to_rfc3339(), "2024-05-01T10:30:00.123456Z");
    }

    #[test]
    fn epoch_variants() {
        let s = parse_timestamp(
            "1700000000",
            &TimestampFormat::EpochSeconds,
            &TimezonePolicy::AssumeUtc,
        )
        .unwrap();
        assert_eq!(s.nanos.0, 1_700_000_000_000_000_000);
        assert_eq!(s.precision, TimestampPrecision::Seconds);
        assert_eq!(s.assumption, TimezoneAssumption::EpochValue);

        let ms = parse_timestamp(
            "1700000000123",
            &TimestampFormat::EpochMillis,
            &TimezonePolicy::AssumeUtc,
        )
        .unwrap();
        assert_eq!(ms.nanos.0, 1_700_000_000_123_000_000);

        let frac = parse_timestamp(
            "1700000000.5",
            &TimestampFormat::EpochSeconds,
            &TimezonePolicy::AssumeUtc,
        )
        .unwrap();
        assert_eq!(frac.nanos.0, 1_700_000_000_500_000_000);
    }

    #[test]
    fn naive_format_assumes_utc_when_told_to() {
        let p = parse_timestamp(
            "2024-05-01 12:30:00.123",
            &TimestampFormat::Chrono {
                format: "%Y-%m-%d %H:%M:%S%.3f".into(),
            },
            &TimezonePolicy::AssumeUtc,
        )
        .unwrap();
        assert_eq!(p.assumption, TimezoneAssumption::AssumedUtc);
        assert_eq!(p.precision, TimestampPrecision::Milliseconds);
        assert_eq!(p.nanos.to_rfc3339(), "2024-05-01T12:30:00.123Z");
    }

    #[test]
    fn profile_zone_regular_time() {
        let p = parse_timestamp(
            "2024-01-15 12:00:00",
            &TimestampFormat::Chrono {
                format: "%Y-%m-%d %H:%M:%S".into(),
            },
            &TimezonePolicy::Zone {
                name: "Europe/Berlin".into(),
            },
        )
        .unwrap();
        // Berlin is UTC+1 in January.
        assert_eq!(p.nanos.to_rfc3339(), "2024-01-15T11:00:00Z");
        assert!(!p.ambiguous);
        assert_eq!(
            p.assumption,
            TimezoneAssumption::ProfileZone("Europe/Berlin".into())
        );
    }

    #[test]
    fn dst_overlap_resolves_to_earlier_instant() {
        // 2024-10-27 02:30 happens twice in Europe/Berlin (clocks fall back
        // 03:00 -> 02:00). The earlier instant is 00:30 UTC (still CEST).
        let p = parse_timestamp(
            "2024-10-27 02:30:00",
            &TimestampFormat::Chrono {
                format: "%Y-%m-%d %H:%M:%S".into(),
            },
            &TimezonePolicy::Zone {
                name: "Europe/Berlin".into(),
            },
        )
        .unwrap();
        assert!(p.ambiguous);
        assert_eq!(p.nanos.to_rfc3339(), "2024-10-27T00:30:00Z");
    }

    #[test]
    fn dst_gap_is_an_error() {
        // 2024-03-31 02:30 does not exist in Europe/Berlin (02:00 -> 03:00).
        let err = parse_timestamp(
            "2024-03-31 02:30:00",
            &TimestampFormat::Chrono {
                format: "%Y-%m-%d %H:%M:%S".into(),
            },
            &TimezonePolicy::Zone {
                name: "Europe/Berlin".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, TimestampError::NonexistentLocalTime(_)));
    }

    #[test]
    fn format_with_offset_ignores_policy() {
        let p = parse_timestamp(
            "2024-05-01 12:30:00 +0530",
            &TimestampFormat::Chrono {
                format: "%Y-%m-%d %H:%M:%S %z".into(),
            },
            &TimezonePolicy::Zone {
                name: "America/New_York".into(),
            },
        )
        .unwrap();
        assert_eq!(p.assumption, TimezoneAssumption::OffsetInText);
        assert_eq!(p.nanos.to_rfc3339(), "2024-05-01T07:00:00Z");
    }

    #[test]
    fn unknown_zone_is_an_error() {
        let err = parse_timestamp(
            "2024-05-01 12:30:00",
            &TimestampFormat::Chrono {
                format: "%Y-%m-%d %H:%M:%S".into(),
            },
            &TimezonePolicy::Zone {
                name: "Mars/Olympus".into(),
            },
        )
        .unwrap_err();
        assert_eq!(err, TimestampError::UnknownZone("Mars/Olympus".into()));
    }

    #[test]
    fn garbage_is_unparsable() {
        let err = parse_timestamp(
            "not-a-time",
            &TimestampFormat::Rfc3339,
            &TimezonePolicy::AssumeUtc,
        )
        .unwrap_err();
        assert!(matches!(err, TimestampError::Unparsable { .. }));
    }
}
