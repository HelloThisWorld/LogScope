//! Deterministic synthetic JSONL log corpus generator.
//!
//! Fixed seed -> byte-identical corpus. Entirely synthetic names; no real
//! company, customer, host, user, path, or token data.

use std::io::Write;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct LogCorpusShape {
    pub lines: u64,
    pub bytes: u64,
    pub seed: u64,
    /// Occurrences of the rare searchable token `cascade`.
    pub searchable_token_lines: u64,
    pub error_lines: u64,
    pub trace_lines: u64,
}

const SERVICES: &[&str] = &[
    "checkout-svc",
    "catalog-svc",
    "payment-svc",
    "shipping-svc",
    "auth-svc",
    "search-svc",
    "cart-svc",
    "notify-svc",
];
const REGIONS: &[&str] = &["alpha", "beta", "gamma"];
const OPS: &[&str] = &[
    "GET /api/items",
    "POST /api/orders",
    "PUT /api/cart",
    "GET /api/profile",
    "POST /api/checkout",
];
const INFO_MSG: &[&str] = &[
    "request completed",
    "cache refreshed",
    "session established",
    "job scheduled",
    "payload validated",
];
const WARN_MSG: &[&str] = &[
    "retrying upstream call",
    "slow response detected",
    "queue depth rising",
];
const ERROR_MSG: &[&str] = &[
    "upstream timeout",
    "connection refused",
    "deserialization failed",
    "constraint violation",
];

/// Writes `count` deterministic JSONL log records.
pub fn write_logs_jsonl(
    mut w: impl Write,
    count: u64,
    seed: u64,
) -> std::io::Result<LogCorpusShape> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut shape = LogCorpusShape {
        lines: 0,
        bytes: 0,
        seed,
        searchable_token_lines: 0,
        error_lines: 0,
        trace_lines: 0,
    };
    let base_nanos: i64 = 1_717_236_000_000_000_000; // 2024-06-01T10:00:00Z
    let mut line = String::with_capacity(512);

    for i in 0..count {
        line.clear();
        // Mostly monotonic time with ~1% out-of-order jitter.
        let jitter: i64 = if rng.random_bool(0.01) {
            -(rng.random_range(1_000_000..50_000_000))
        } else {
            rng.random_range(0..3_000_000)
        };
        let t = base_nanos + (i as i64) * 3_000_000 + jitter;
        let secs = t / 1_000_000_000;
        let millis = (t % 1_000_000_000) / 1_000_000;
        let dt = chrono::DateTime::from_timestamp(secs, (millis * 1_000_000) as u32)
            .expect("valid timestamp");
        let ts = dt.format("%Y-%m-%dT%H:%M:%S%.3fZ");

        let roll: f64 = rng.random_range(0.0..1.0);
        let (level, msg) = if roll < 0.07 {
            shape.error_lines += 1;
            ("ERROR", ERROR_MSG[rng.random_range(0..ERROR_MSG.len())])
        } else if roll < 0.19 {
            ("WARN", WARN_MSG[rng.random_range(0..WARN_MSG.len())])
        } else if roll < 0.20 {
            ("DEBUG", "state dump emitted")
        } else {
            ("INFO", INFO_MSG[rng.random_range(0..INFO_MSG.len())])
        };
        let rare = i % 50_021 == 0; // rare searchable token, deterministic
        if rare {
            shape.searchable_token_lines += 1;
        }

        let service = SERVICES[rng.random_range(0..SERVICES.len())];
        let region = REGIONS[rng.random_range(0..REGIONS.len())];
        let op = OPS[rng.random_range(0..OPS.len())];
        let status = if level == "ERROR" {
            [500u16, 502, 503, 504][rng.random_range(0..4)]
        } else {
            [200u16, 200, 200, 201, 204, 302][rng.random_range(0..6)]
        };
        let elapsed: f64 = rng.random_range(0.2..950.0);
        let pod = format!("{}-{:04x}", service, rng.random_range(0u32..16));

        use std::fmt::Write as _;
        write!(
            line,
            "{{\"@timestamp\":\"{ts}\",\"level\":\"{level}\",\"message\":\"{msg}{}\",\
             \"service\":\"{service}\",\"region\":\"{region}\",\"operation\":\"{op}\",\
             \"http\":{{\"status\":{status},\"elapsed_ms\":{elapsed:.2}}},\
             \"k8s\":{{\"pod\":\"{pod}\"}},\"seq\":{i}",
            if rare { " cascade" } else { "" },
        )
        .expect("write to string");
        if rng.random_bool(0.10) {
            shape.trace_lines += 1;
            let hi: u64 = rng.random();
            let lo: u64 = rng.random();
            let span: u64 = rng.random();
            write!(
                line,
                ",\"trace_id\":\"{hi:016x}{lo:016x}\",\"span_id\":\"{span:016x}\""
            )
            .expect("write to string");
        }
        line.push('}');
        line.push('\n');
        w.write_all(line.as_bytes())?;
        shape.lines += 1;
        shape.bytes += line.len() as u64;
    }
    Ok(shape)
}
