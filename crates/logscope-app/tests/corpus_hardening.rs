//! W9 property corpora for the app-layer hardening surfaces:
//! - bundle entry-path validation (`check_entry_path`) against a hostile
//!   name corpus plus a generated equivalence check;
//! - marker timestamp parsing (`parse_marker_time`) — exact values for
//!   known instants, one stable error code for everything else;
//! - the disclosure projection compiler and applicator against hostile
//!   profiles and inputs — refusals are structured, denied values never
//!   leak, unparseable snapshots are projected as opaque text.
//!
//! Deterministic seeded PRNG; no fuzzing dependency.

use logscope_app::bundle::{self, MAX_ENTRY_DEPTH, MAX_ENTRY_NAME_CHARS};
use logscope_app::redact::{Projection, RedactionSummary, MAX_RULES};
use logscope_app::timeline::parse_marker_time;

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

// ---- bundle entry paths ------------------------------------------------------

#[test]
fn entry_path_hostile_corpus_is_refused_with_the_stable_code() {
    let too_long = "a/".repeat(MAX_ENTRY_NAME_CHARS); // > char bound
    let too_deep = "a/b/c/d/e/f/g/h/i"; // depth MAX_ENTRY_DEPTH + 1
    let hostile: Vec<String> = vec![
        String::new(),
        too_long,
        "a\\b".into(),
        "C:".into(),
        "C:/windows/system32".into(),
        "a:b".into(),
        "stream.txt:ads".into(),
        "/absolute".into(),
        "/".into(),
        "a\u{7}b".into(),
        "a\u{0}b".into(),
        "..".into(),
        ".".into(),
        "a/../b".into(),
        "./a".into(),
        "a/./b".into(),
        "../../../../etc/passwd".into(),
        "a//b".into(),
        "a/".into(),
        too_deep.into(),
        "con".into(),
        "CON".into(),
        "Con.txt".into(),
        "com1".into(),
        "Com1.parquet".into(),
        "lpt9.x.y".into(),
        "aux.tar.gz".into(),
        "nul".into(),
        "prn/report.md".into(),
        "case/nul.json".into(),
        "trailing./x".into(),
        "trailing /x".into(),
        "a.".into(),
        "a ".into(),
    ];
    for name in &hostile {
        let err = bundle::check_entry_path(name)
            .expect_err(&format!("hostile name must be refused: {name:?}"));
        assert_eq!(err.code, "bundle/invalid", "wrong code for {name:?}");
    }
}

/// Independent reimplementation of the safety invariants. The property
/// below asserts full equivalence: `check_entry_path` accepts a name if
/// and only if every invariant holds.
fn violates_invariants(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > MAX_ENTRY_NAME_CHARS {
        return true;
    }
    if name.contains('\\') || name.contains(':') || name.starts_with('/') {
        return true;
    }
    if name.chars().any(|c| c.is_control()) {
        return true;
    }
    let segments: Vec<&str> = name.split('/').collect();
    if segments.len() > MAX_ENTRY_DEPTH {
        return true;
    }
    for seg in &segments {
        if seg.is_empty() || *seg == "." || *seg == ".." {
            return true;
        }
        if seg.ends_with('.') || seg.ends_with(' ') {
            return true;
        }
        let stem = seg.split('.').next().unwrap_or(seg).to_ascii_lowercase();
        if ["con", "prn", "aux", "nul"].contains(&stem.as_str()) {
            return true;
        }
        if stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0'
        {
            return true;
        }
    }
    false
}

#[test]
fn entry_path_acceptance_is_equivalent_to_the_safety_invariants() {
    // Names that must be accepted (boundary cases included).
    let deepest = "a/b/c/d/e/f/g/h"; // depth exactly MAX_ENTRY_DEPTH
                                     // Exactly 512 chars across exactly 8 segments: 7×63 + 7 slashes + 64.
    let longest = format!("{}/{}", vec!["a".repeat(63); 7].join("/"), "b".repeat(64));
    assert_eq!(longest.chars().count(), MAX_ENTRY_NAME_CHARS);
    let good = [
        "manifest.json",
        "case/investigation.json",
        "imported-data/subset.parquet",
        "reports/r-1/report.md",
        "a.b.c",
        "console.txt", // stem "console" is not a reserved device
        "con2.txt",    // only com1–com9 / lpt1–lpt9 are reserved
        "com0.txt",
        "lpt0",
        "aux2/file",
        deepest,
        longest.as_str(),
        "UPPER/Case.TXT",
        "under_score-dash",
    ];
    for name in good {
        assert!(
            bundle::check_entry_path(name).is_ok(),
            "safe name must be accepted: {name:?}"
        );
        assert!(!violates_invariants(name), "oracle disagrees on {name:?}");
    }

    // Generated corpus: full equivalence with the independent oracle.
    const ALPHABET: &[char] = &[
        'a', 'b', 'c', 'o', 'n', 'm', 'l', 'p', 't', 'u', 'x', '1', '9', 'A', '/', '\\', ':', '.',
        ' ', '-', '_', 'é', '\u{1}',
    ];
    let mut rng = Rng(0x2026_0805_0003);
    for _ in 0..8000 {
        let len = rng.below(24);
        let name: String = (0..len)
            .map(|_| ALPHABET[rng.below(ALPHABET.len())])
            .collect();
        let accepted = bundle::check_entry_path(&name).is_ok();
        assert_eq!(
            accepted,
            !violates_invariants(&name),
            "validator and invariant oracle disagree on {name:?}"
        );
    }
}

// ---- marker timestamps -------------------------------------------------------

#[test]
fn marker_time_corpus_parses_known_instants_exactly() {
    // 2026-08-04T10:00:00Z in UTC nanoseconds.
    const T0: i64 = 1_785_837_600_000_000_000;
    let valid = [
        ("2026-08-04T10:00:00Z", T0, 0),
        ("2026-08-04T12:00:00+02:00", T0, 120),
        ("2026-08-04T04:30:00-05:30", T0, -330),
        ("2026-08-04T10:00:00.123456789Z", T0 + 123_456_789, 0),
        ("  2026-08-04T10:00:00Z  ", T0, 0), // surrounding whitespace is trimmed
        ("2026-08-04T10:00:00+00:00", T0, 0),
        // RFC 3339 permits a space in place of `T`; chrono accepts it and
        // the corpus documents that as part of the accepted grammar.
        ("2026-08-04 10:00:00Z", T0, 0),
    ];
    for (text, nanos, offset_min) in valid {
        let (got_nanos, got_offset) = parse_marker_time(text)
            .unwrap_or_else(|e| panic!("{text:?} must parse: {}", e.message));
        assert_eq!(got_nanos, nanos, "wrong instant for {text:?}");
        assert_eq!(
            got_offset, offset_min,
            "wrong preserved offset for {text:?}"
        );
    }
}

#[test]
fn marker_time_rejections_all_carry_the_one_stable_code() {
    let invalid = [
        "",
        "now",
        "yesterday 10am",
        "2026-08-04",           // date only
        "2026-08-04T10:00:00",  // missing offset
        "2026-13-01T00:00:00Z", // month 13
        "2026-02-30T00:00:00Z", // impossible day
        "2026-08-04T25:00:00Z", // hour 25
        "2026-08-04T10:61:00Z", // minute 61
        "T10:00:00Z",
        "1500-01-01T00:00:00Z", // parses, but outside the i64-nanos range
        "2500-01-01T00:00:00Z", // same, on the future side
        "1970-01-01T00:00:00+99:00",
    ];
    for text in invalid {
        let err = parse_marker_time(text).expect_err(&format!("{text:?} must be refused"));
        assert_eq!(
            err.code, "case/invalid-timestamp",
            "wrong code for {text:?}"
        );
    }

    // Generated garbage: parsing never panics and never invents a
    // second error code.
    const ALPHABET: &[char] = &[
        '0', '1', '2', '9', '-', ':', 'T', 'Z', '+', '.', ' ', 'a', 'x', 'é', '\u{0}',
    ];
    let mut rng = Rng(0x2026_0805_0004);
    for _ in 0..3000 {
        let len = rng.below(30);
        let text: String = (0..len)
            .map(|_| ALPHABET[rng.below(ALPHABET.len())])
            .collect();
        if let Err(e) = parse_marker_time(&text) {
            assert_eq!(e.code, "case/invalid-timestamp", "wrong code for {text:?}");
        }
    }
}

// ---- disclosure projection ---------------------------------------------------

#[test]
fn hostile_profiles_are_refused_as_structured_invalid_profile_errors() {
    let too_many: Vec<serde_json::Value> = (0..=MAX_RULES)
        .map(|i| serde_json::json!({"kind": "omit_field", "field": format!("f{i}")}))
        .collect();
    let too_many_json = serde_json::to_string(&too_many).unwrap();
    let long_pattern = serde_json::json!([{
        "kind": "replace_regex", "pattern": "a".repeat(513), "replace": "x"
    }])
    .to_string();
    // Compiles to a program far past the 1 MiB size bound.
    let size_bomb = serde_json::json!([{
        "kind": "replace_regex", "pattern": "(?:(?:(?:a{90}){90}){90})", "replace": "x"
    }])
    .to_string();

    let hostile: Vec<(&str, String)> = vec![
        ("not json", "not json{".into()),
        ("object not array", "{}".into()),
        ("missing field", r#"[{"kind":"omit_field"}]"#.into()),
        ("unknown kind", r#"[{"kind":"redact_everything"}]"#.into()),
        (
            "unknown extra key",
            r#"[{"kind":"omit_field","field":"a","surprise":1}]"#.into(),
        ),
        ("too many rules", too_many_json),
        ("pattern too long", long_pattern),
        (
            "regex does not compile",
            r#"[{"kind":"replace_regex","pattern":"(","replace":"x"}]"#.into(),
        ),
        ("regex size bomb", size_bomb),
        (
            "empty exact find",
            r#"[{"kind":"replace_exact","find":"","replace":"x"}]"#.into(),
        ),
    ];
    for (what, rules_json) in &hostile {
        let err = Projection::compile(rules_json, "")
            .err()
            .unwrap_or_else(|| panic!("{what}: hostile rules must refuse to compile"));
        assert_eq!(
            err.code, "redaction/invalid-profile",
            "wrong code for {what}"
        );
    }

    let hostile_posture = [
        ("posture not json", "]["),
        ("posture unknown key", r#"{"surprise":1}"#),
        ("posture wrong type", r#"{"max_text_chars":"big"}"#),
        ("posture is an array", "[]"),
    ];
    for (what, posture_json) in hostile_posture {
        let err = Projection::compile("[]", posture_json)
            .err()
            .unwrap_or_else(|| panic!("{what}: hostile posture must refuse to compile"));
        assert_eq!(
            err.code, "redaction/invalid-profile",
            "wrong code for {what}"
        );
    }
}

#[test]
fn allowlist_projection_never_leaks_generated_field_values() {
    let projection = Projection::compile("[]", r#"{"field_allow":["keep"]}"#).unwrap();
    let mut rng = Rng(0x2026_0805_0005);
    for _ in 0..300 {
        let n = 1 + rng.below(6);
        let fields: Vec<serde_json::Value> = (0..n)
            .map(|i| {
                serde_json::json!({
                    "name": format!("f{i}"),
                    "value": format!("secret-{:016x}", rng.next()),
                })
            })
            .collect();
        let snapshot = serde_json::json!({ "fields": fields }).to_string();
        let mut summary = RedactionSummary::default();
        let projected = projection.snapshot_json(&snapshot, &mut summary);
        assert!(
            !projected.contains("secret-"),
            "a denied value leaked: {projected}"
        );
        assert!(projected.contains("omitted_by_disclosure_profile"));
        assert_eq!(summary.fields_omitted, n as u64, "every field counted");
    }
}

#[test]
fn unparseable_snapshots_are_projected_as_opaque_text() {
    let projection = Projection::compile(
        r#"[{"kind":"replace_exact","find":"secret","replace":"[GONE]"}]"#,
        "",
    )
    .unwrap();
    let mut summary = RedactionSummary::default();
    let projected = projection.snapshot_json("secret{{{ not json secret", &mut summary);
    assert!(
        !projected.contains("secret"),
        "raw bytes bypassed the rules"
    );
    assert!(projected.contains("[GONE]"));
    assert_eq!(summary.text_replacements, 2);
}

#[test]
fn linear_time_regex_survives_pathological_input_and_bounds_output() {
    // A classic catastrophic-backtracking shape: harmless on the
    // linear-time engine. The input has no match and 200k chars.
    let projection = Projection::compile(
        r#"[{"kind":"replace_regex","pattern":"(?:a+)+b","replace":"X"}]"#,
        r#"{"max_text_chars":100}"#,
    )
    .unwrap();
    let input = "a".repeat(200_000);
    let mut summary = RedactionSummary::default();
    let out = projection.text(&input, &mut summary);
    assert_eq!(summary.text_replacements, 0);
    assert_eq!(summary.truncated_blocks, 1, "block bound applies");
    assert!(out.chars().count() < 200, "output stays bounded");
    assert!(out.contains("[truncated by disclosure profile]"));
}
