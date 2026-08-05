//! W9 property corpus: envelope decoding must never panic. Whatever
//! bytes are stored — mutated payloads, arbitrary JSON shapes, hostile
//! scalars, absurd versions — `decode_reference`/`decode_snapshot`
//! return a [`DecodeOutcome`] and nothing else: newer versions are
//! refused as `UnsupportedVersion`, garbage is reported as
//! `Undecodable`, and a decoded value survives `validate_reference`
//! without panicking. No fuzzing dependency: a seeded xorshift PRNG
//! makes every run identical.

use logscope_case::envelope::{
    self, bound_field, CountState, DatasetRevRef, DecodeOutcome, EventRef, EventSnapshot,
    EvidenceReference, EvidenceSnapshot, QueryContext, QueryRef, SelectionRef, SnapshotRow,
    MAX_SNAPSHOT_FIELD_BYTES,
};
use logscope_case::EVIDENCE_ENVELOPE_VERSION;

/// Deterministic xorshift64 PRNG — reproducible corpus, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn ctx() -> QueryContext {
    QueryContext {
        query_text: "severity:ERROR AND service:checkout".into(),
        language_version: 1,
        fingerprint: Some("qry-abc123".into()),
        dataset_ids: vec!["ds-1".into()],
        time_strategy_json: "{\"kind\":\"all\"}".into(),
        resolved_start: Some(1_700_000_000_000_000_000),
        resolved_end: Some(1_700_000_100_000_000_000),
        omitted_untimestamped: Some(2),
    }
}

/// Valid encoded payloads used as mutation bases.
fn base_references() -> Vec<String> {
    let event = EvidenceReference::Event(EventRef {
        record_id: "log-0123456789abcdef0123456789abcdef".into(),
        dataset_id: "ds-1".into(),
        dataset_revision: "dsrev-aa".into(),
        segment_id: Some("seg-1".into()),
        source_file_id: Some("file-1".into()),
        source_content_hash: Some("blake3-xyz".into()),
        source_locator_json: Some("{\"line_start\":10}".into()),
        profile_id: Some("jsonl-generic".into()),
        profile_version: Some("1".into()),
        parser_id: "jsonl".into(),
        parser_version: "1".into(),
        event_time: Some(1_700_000_000_000_000_000),
        timestamp_quality: vec!["explicit".into()],
    });
    let selection = EvidenceReference::Selection(SelectionRef {
        record_ids: (0..5).map(|i| format!("log-{i:032x}")).collect(),
        datasets: vec![DatasetRevRef {
            dataset_id: "ds-1".into(),
            dataset_revision: "dsrev-aa".into(),
        }],
        context: ctx(),
        selected_count: 5,
        max_allowed: 500,
        truncated: false,
    });
    let query = EvidenceReference::Query(QueryRef {
        context: ctx(),
        datasets: vec![DatasetRevRef {
            dataset_id: "ds-1".into(),
            dataset_revision: "dsrev-aa".into(),
        }],
        saved_search_id: Some("ss-1".into()),
        saved_search_fingerprint: Some("qry-def".into()),
        sort: "event_time DESC NULLS LAST, record_id DESC, dataset_id DESC".into(),
        count: CountState::Bounded { at_least: 100 },
        representative_ids: vec!["log-a".into()],
    });
    [event, selection, query]
        .iter()
        .map(|r| envelope::encode_reference(r).unwrap())
        .collect()
}

fn base_snapshot() -> String {
    let snap = EvidenceSnapshot::Event(EventSnapshot {
        row: SnapshotRow {
            record_id: "log-0123456789abcdef0123456789abcdef".into(),
            event_time: Some(1_700_000_000_000_000_000),
            severity_text: Some("ERROR".into()),
            severity_number: Some(17),
            display_message: "handler 12 finished with timeout".into(),
            display_message_truncated: false,
            fields: vec![],
        },
        raw_excerpt: Some("{\"level\":\"ERROR\"}".into()),
        raw_excerpt_truncated: false,
    });
    envelope::encode_snapshot(&snap).unwrap()
}

/// Char pool for mutations: JSON structure characters, escapes, unicode,
/// and a control character.
const POOL: &[char] = &[
    '"', '{', '}', '[', ']', '\\', ':', ',', 'a', 'e', '0', '9', '-', '.', ' ', 'n', 'u', 'l', 't',
    'é', '🦀', '\u{0}', '\u{202e}',
];

fn mutate(rng: &mut Rng, base: &str) -> String {
    let mut chars: Vec<char> = base.chars().collect();
    for _ in 0..=rng.below(4) {
        match rng.below(4) {
            0 if !chars.is_empty() => {
                let i = rng.below(chars.len());
                chars.remove(i);
            }
            1 => {
                let i = rng.below(chars.len() + 1);
                chars.insert(i, POOL[rng.below(POOL.len())]);
            }
            2 if !chars.is_empty() => {
                let i = rng.below(chars.len());
                chars[i] = POOL[rng.below(POOL.len())];
            }
            _ => {
                let cut = rng.below(chars.len() + 1);
                chars.truncate(cut);
            }
        }
    }
    chars.into_iter().collect()
}

const VERSIONS: &[i64] = &[i64::MIN, -1, 0, 1, 2, 42, i64::MAX];

/// The single property every stored payload must satisfy: decoding
/// yields a `DecodeOutcome` — version gating first, then decode-or-
/// refuse — and a decoded reference survives validation without panic.
fn check_reference_outcome(version: i64, payload: &str) {
    match envelope::decode_reference(version, payload) {
        DecodeOutcome::UnsupportedVersion { stored, supported } => {
            assert!(version > EVIDENCE_ENVELOPE_VERSION);
            assert_eq!(stored, version);
            assert_eq!(supported, EVIDENCE_ENVELOPE_VERSION);
        }
        DecodeOutcome::Decoded(r) => {
            assert!(version <= EVIDENCE_ENVELOPE_VERSION);
            let _ = envelope::validate_reference(&r);
        }
        DecodeOutcome::Undecodable { error } => {
            assert!(version <= EVIDENCE_ENVELOPE_VERSION);
            assert!(!error.is_empty(), "an undecodable payload names its error");
        }
    }
}

fn check_snapshot_outcome(version: i64, payload: &str) {
    match envelope::decode_snapshot(version, payload) {
        DecodeOutcome::UnsupportedVersion { stored, supported } => {
            assert!(version > EVIDENCE_ENVELOPE_VERSION);
            assert_eq!(stored, version);
            assert_eq!(supported, EVIDENCE_ENVELOPE_VERSION);
        }
        DecodeOutcome::Decoded(_) => assert!(version <= EVIDENCE_ENVELOPE_VERSION),
        DecodeOutcome::Undecodable { error } => {
            assert!(version <= EVIDENCE_ENVELOPE_VERSION);
            assert!(!error.is_empty());
        }
    }
}

#[test]
fn mutated_valid_payloads_never_panic() {
    let mut rng = Rng(0x2026_0805_0001);
    let bases = base_references();
    let snap_base = base_snapshot();
    for i in 0..4000 {
        let base = &bases[i % bases.len()];
        let payload = mutate(&mut rng, base);
        let version = VERSIONS[i % VERSIONS.len()];
        check_reference_outcome(version, &payload);

        let snap_payload = mutate(&mut rng, &snap_base);
        check_snapshot_outcome(version, &snap_payload);
    }
}

/// Random JSON value with realistic tag/key names so the corpus reaches
/// deep into the tagged-enum deserializer, not just the top-level error.
fn gen_json(rng: &mut Rng, depth: usize) -> serde_json::Value {
    const KEYS: &[&str] = &[
        "kind",
        "state",
        "record_id",
        "dataset_id",
        "dataset_revision",
        "count",
        "context",
        "rows",
        "value",
        "name",
        "record_ids",
        "datasets",
        "start",
        "end",
        "item_id",
        "field",
    ];
    const STRINGS: &[&str] = &[
        "event",
        "selection",
        "query",
        "explorer_group",
        "histogram_interval",
        "item_ref",
        "exact",
        "unknown",
        "telepathy",
        "",
        "log-1",
        "\u{202e}reversed",
    ];
    match if depth == 0 {
        rng.below(4)
    } else {
        rng.below(6)
    } {
        0 => serde_json::Value::Null,
        1 => serde_json::Value::Bool(rng.below(2) == 0),
        2 => serde_json::json!(rng.next() as i64),
        3 => serde_json::Value::String(STRINGS[rng.below(STRINGS.len())].to_string()),
        4 => {
            let n = rng.below(4);
            serde_json::Value::Array((0..n).map(|_| gen_json(rng, depth - 1)).collect())
        }
        _ => {
            let n = rng.below(5);
            let mut map = serde_json::Map::new();
            for _ in 0..n {
                map.insert(
                    KEYS[rng.below(KEYS.len())].to_string(),
                    gen_json(rng, depth - 1),
                );
            }
            serde_json::Value::Object(map)
        }
    }
}

#[test]
fn arbitrary_json_shapes_decode_or_refuse_without_panic() {
    let mut rng = Rng(0x2026_0805_0002);
    for i in 0..3000 {
        let payload = gen_json(&mut rng, 5).to_string();
        let version = VERSIONS[i % VERSIONS.len()];
        check_reference_outcome(version, &payload);
        check_snapshot_outcome(version, &payload);
    }
}

#[test]
fn deep_nesting_is_refused_gracefully_not_by_stack_overflow() {
    // serde_json's recursion limit turns pathological nesting into an
    // Undecodable outcome; the process must never crash.
    let deep_array = format!("{}{}", "[".repeat(300), "]".repeat(300));
    let deep_object = format!("{}\"kind\"{}", "{\"context\":".repeat(300), "}".repeat(300));
    for payload in [deep_array, deep_object] {
        match envelope::decode_reference(1, &payload) {
            DecodeOutcome::Undecodable { error } => assert!(!error.is_empty()),
            other => panic!("expected undecodable, got {other:?}"),
        }
        match envelope::decode_snapshot(1, &payload) {
            DecodeOutcome::Undecodable { .. } => {}
            other => panic!("expected undecodable, got {other:?}"),
        }
    }
}

#[test]
fn hostile_scalar_payloads_never_panic() {
    let huge = "x".repeat(2_000_000);
    let cases = [
        "9223372036854775808",  // i64::MAX + 1
        "-9223372036854775809", // i64::MIN - 1
        "1e309",                // f64 overflow
        "-0",
        "\"\\u0000\"",
        "[1,2",                 // truncated
        "nul",                  // truncated keyword
        "{\"kind\":\"event\"}", // right tag, missing fields
        "{\"kind\":42}",        // tag with wrong type
        "\u{feff}{}",           // BOM prefix
        "{}",
        "[]",
        "null",
        "true",
        huge.as_str(),
    ];
    for payload in cases {
        check_reference_outcome(1, payload);
        check_snapshot_outcome(1, payload);
        // A version that is too new must refuse before parsing anything.
        match envelope::decode_reference(EVIDENCE_ENVELOPE_VERSION + 1, payload) {
            DecodeOutcome::UnsupportedVersion { .. } => {}
            other => panic!("newer version must be refused, got {other:?}"),
        }
    }
}

#[test]
fn bound_field_cuts_at_char_boundaries_for_multibyte_input() {
    // 'a' + crabs: the byte bound lands mid-crab, so the cut must back
    // off to a char boundary instead of panicking or splitting UTF-8.
    let base = format!("a{}", "🦀".repeat(2000));
    let (out, truncated) = bound_field(&base);
    assert!(truncated);
    assert!(out.len() <= MAX_SNAPSHOT_FIELD_BYTES);
    assert!(
        base.starts_with(&out),
        "truncation is a prefix, never a rewrite"
    );

    // Exactly at the bound: no truncation.
    let exact = "y".repeat(MAX_SNAPSHOT_FIELD_BYTES);
    let (out, truncated) = bound_field(&exact);
    assert!(!truncated);
    assert_eq!(out, exact);
}
