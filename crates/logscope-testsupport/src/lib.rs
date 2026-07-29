//! Test support for LogScope: golden fixture access and deterministic
//! corpus generators (fixed-seed, never committed to Git).

pub mod gen_logs;
pub mod gen_metrics;
pub mod gen_spans;
pub mod mem;

pub use gen_logs::{write_logs_jsonl, LogCorpusShape};
pub use gen_metrics::{write_metrics_otlp_jsonl, MetricCorpusShape};
pub use gen_spans::{write_spans_otlp_jsonl, SpanCorpusShape};
pub use mem::peak_working_set_bytes;

/// Repository-relative fixtures root (usable from any crate's tests).
pub fn fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from("fixtures"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_generator_is_deterministic() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        let sa = write_logs_jsonl(&mut a, 500, 42).unwrap();
        let sb = write_logs_jsonl(&mut b, 500, 42).unwrap();
        assert_eq!(a, b, "same seed must produce identical bytes");
        assert_eq!(sa.bytes, sb.bytes);
        let mut c = Vec::new();
        write_logs_jsonl(&mut c, 500, 43).unwrap();
        assert_ne!(a, c, "different seed must differ");
    }

    #[test]
    fn metric_generator_is_deterministic() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        let sa = write_metrics_otlp_jsonl(&mut a, 5_000, 100, 7).unwrap();
        write_metrics_otlp_jsonl(&mut b, 5_000, 100, 7).unwrap();
        assert_eq!(a, b);
        assert_eq!(sa.points, 5_000);
    }

    #[test]
    fn span_generator_is_deterministic() {
        let mut a = Vec::new();
        let mut b = Vec::new();
        let sa = write_spans_otlp_jsonl(&mut a, 2_000, 11).unwrap();
        write_spans_otlp_jsonl(&mut b, 2_000, 11).unwrap();
        assert_eq!(a, b);
        assert!(sa.spans >= 2_000);
    }
}
