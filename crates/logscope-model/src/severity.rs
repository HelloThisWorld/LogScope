//! OTLP-aligned severity numbers.
//!
//! The canonical severity is the OTLP `SeverityNumber` (1..=24). The original
//! severity text from the source is always preserved separately; mapping from
//! arbitrary source text to a number lives in the normalizer.

/// OTLP severity number range constants.
pub mod levels {
    pub const TRACE: i32 = 1;
    pub const TRACE2: i32 = 2;
    pub const TRACE3: i32 = 3;
    pub const TRACE4: i32 = 4;
    pub const DEBUG: i32 = 5;
    pub const DEBUG2: i32 = 6;
    pub const DEBUG3: i32 = 7;
    pub const DEBUG4: i32 = 8;
    pub const INFO: i32 = 9;
    pub const INFO2: i32 = 10;
    pub const INFO3: i32 = 11;
    pub const INFO4: i32 = 12;
    pub const WARN: i32 = 13;
    pub const WARN2: i32 = 14;
    pub const WARN3: i32 = 15;
    pub const WARN4: i32 = 16;
    pub const ERROR: i32 = 17;
    pub const ERROR2: i32 = 18;
    pub const ERROR3: i32 = 19;
    pub const ERROR4: i32 = 20;
    pub const FATAL: i32 = 21;
    pub const FATAL2: i32 = 22;
    pub const FATAL3: i32 = 23;
    pub const FATAL4: i32 = 24;
}

/// Canonical display name for an OTLP severity number, if in range.
pub fn severity_number_name(n: i32) -> Option<&'static str> {
    Some(match n {
        1 => "TRACE",
        2 => "TRACE2",
        3 => "TRACE3",
        4 => "TRACE4",
        5 => "DEBUG",
        6 => "DEBUG2",
        7 => "DEBUG3",
        8 => "DEBUG4",
        9 => "INFO",
        10 => "INFO2",
        11 => "INFO3",
        12 => "INFO4",
        13 => "WARN",
        14 => "WARN2",
        15 => "WARN3",
        16 => "WARN4",
        17 => "ERROR",
        18 => "ERROR2",
        19 => "ERROR3",
        20 => "ERROR4",
        21 => "FATAL",
        22 => "FATAL2",
        23 => "FATAL3",
        24 => "FATAL4",
        _ => return None,
    })
}
