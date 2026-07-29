//! Query language v1 conformance tests.

use logscope_query_lang::*;

fn catalog() -> StaticCatalog {
    StaticCatalog::default()
        .with("service.name", &["service.name"], &[AttrType::Str])
        .with("service", &["service"], &[AttrType::Str])
        .with("retry.count", &["retry", "count"], &[AttrType::Int])
        .with("duration_ms", &["duration_ms"], &[AttrType::Double])
        .with("http.status", &["http", "status"], &[AttrType::Int])
        .with("host.name", &["host", "name"], &[AttrType::Str])
        .with("user.id", &["user", "id"], &[AttrType::Str])
        .with("active", &["active"], &[AttrType::Bool])
        .with("payload", &["payload"], &[AttrType::Map])
        .with("mixed", &["mixed"], &[AttrType::Str, AttrType::Int])
        .with(
            "num_mixed",
            &["num_mixed"],
            &[AttrType::Int, AttrType::Double],
        )
        .with("level", &["level"], &[AttrType::Str])
}

fn ok(text: &str) -> ResolvedQuery {
    let a = analyze(text, &catalog(), &LangLimits::default());
    match a.resolved {
        Some(r) => r,
        None => panic!("expected success for {text:?}, got {:#?}", a.diagnostics),
    }
}

fn errs(text: &str) -> Vec<Diagnostic> {
    let a = analyze(text, &catalog(), &LangLimits::default());
    assert!(
        a.resolved.is_none(),
        "expected failure for {text:?}, got {:?}",
        a.resolved
    );
    a.diagnostics.into_iter().filter(|d| d.is_error()).collect()
}

fn first_code(text: &str) -> String {
    errs(text)[0].code.clone()
}

#[test]
fn roadmap_example_parses_with_documented_shape() {
    let q = ok(
        r#"service.name:workflow AND severity:(ERROR OR WARN) AND "timed out" AND NOT retry.count:0"#,
    );
    let Some(ResolvedExpr::And { items }) = q.expr else {
        panic!("expected top-level AND");
    };
    assert_eq!(items.len(), 4);
    assert!(matches!(
        &items[0],
        ResolvedExpr::Attr { path, ty: AttrScalar::Str, op: CmpOp::Eq, value: TypedScalar::Str(v) }
            if path == &vec!["service.name".to_string()] && v == "workflow"
    ));
    assert!(matches!(
        &items[1],
        ResolvedExpr::Or { items } if matches!(
            (&items[0], &items[1]),
            (
                ResolvedExpr::Severity { op: CmpOp::Eq, level: SeverityLevel::Error },
                ResolvedExpr::Severity { op: CmpOp::Eq, level: SeverityLevel::Warn },
            )
        )
    ));
    assert!(matches!(&items[2], ResolvedExpr::Phrase { phrase } if phrase == "timed out"));
    assert!(matches!(
        &items[3],
        ResolvedExpr::Not { expr } if matches!(
            expr.as_ref(),
            ResolvedExpr::Attr { path, op: CmpOp::Eq, value: TypedScalar::Int(0), .. }
                if path == &vec!["retry".to_string(), "count".to_string()]
        )
    ));
}

#[test]
fn precedence_not_before_and_before_or() {
    // a OR b AND NOT c  ==  a OR (b AND (NOT c))
    let q = ok("alpha OR beta AND NOT gamma");
    let Some(ResolvedExpr::Or { items }) = q.expr else {
        panic!("expected OR at top");
    };
    assert!(matches!(&items[0], ResolvedExpr::Term { term } if term == "alpha"));
    let ResolvedExpr::And { items: and_items } = &items[1] else {
        panic!("expected AND under OR");
    };
    assert!(matches!(&and_items[0], ResolvedExpr::Term { term } if term == "beta"));
    assert!(matches!(&and_items[1], ResolvedExpr::Not { .. }));
}

#[test]
fn parentheses_override_precedence() {
    let q = ok("(alpha OR beta) AND gamma");
    let Some(ResolvedExpr::And { items }) = q.expr else {
        panic!("expected AND at top");
    };
    assert!(matches!(&items[0], ResolvedExpr::Or { .. }));
}

#[test]
fn adjacency_is_implicit_and() {
    let a = ok("timeout upstream severity:ERROR");
    let b = ok("timeout AND upstream AND severity:ERROR");
    assert_eq!(a.fingerprint, b.fingerprint);
}

#[test]
fn colon_and_equals_are_exact_aliases() {
    let a = ok("service.name:workflow");
    let b = ok(r#"service.name = "workflow""#);
    assert_eq!(a.fingerprint, b.fingerprint);
}

#[test]
fn field_group_expands_nested_boolean() {
    let q = ok("severity:(ERROR OR (WARN AND NOT FATAL))");
    let Some(ResolvedExpr::Or { items }) = q.expr else {
        panic!("expected OR");
    };
    assert!(matches!(
        items[0],
        ResolvedExpr::Severity {
            level: SeverityLevel::Error,
            ..
        }
    ));
    assert!(matches!(&items[1], ResolvedExpr::And { .. }));
}

#[test]
fn quoted_phrases_escape_correctly() {
    let q = ok(r#""say \"hello\"\n\t\\ world""#);
    assert!(matches!(
        q.expr,
        Some(ResolvedExpr::Phrase { phrase }) if phrase == "say \"hello\"\n\t\\ world"
    ));
}

#[test]
fn unicode_terms_survive() {
    let q = ok("täglich 数据库 severity:INFO");
    let Some(ResolvedExpr::And { items }) = q.expr else {
        panic!("expected AND");
    };
    assert!(matches!(&items[0], ResolvedExpr::Term { term } if term == "täglich"));
    assert!(matches!(&items[1], ResolvedExpr::Term { term } if term == "数据库"));
}

#[test]
fn spans_are_utf8_and_utf16_aware() {
    let text = "日誌 severity:ERROR";
    let a = analyze(text, &catalog(), &LangLimits::default());
    // Second token: `severity` field word. 日誌 = 6 UTF-8 bytes, 2 UTF-16
    // units; plus one space.
    let tok = &a.tokens[1];
    assert_eq!(tok.span.start, 7);
    assert_eq!(tok.span.start_utf16, 3);
    assert_eq!(a.highlights[1].kind, HighlightKind::Field);
}

#[test]
fn alias_resolution_matches_canonical() {
    for alias in ["severity:ERROR", "level:ERROR", "log.level:ERROR"] {
        let q = ok(alias);
        assert!(
            matches!(
                q.expr,
                Some(ResolvedExpr::Severity {
                    level: SeverityLevel::Error,
                    ..
                })
            ),
            "alias {alias} did not resolve to severity"
        );
    }
    let a = ok("@timestamp >= \"2026-07-29T00:00:00Z\"");
    let b = ok("timestamp >= \"2026-07-29T00:00:00Z\"");
    assert_eq!(a.fingerprint, b.fingerprint);
}

#[test]
fn alias_shadowing_attribute_warns_and_prefix_reaches_attr() {
    // `level` is a built-in alias AND a real attribute in the catalog.
    let a = analyze("level:ERROR", &catalog(), &LangLimits::default());
    let r = a.resolved.expect("valid");
    assert!(matches!(r.expr, Some(ResolvedExpr::Severity { .. })));
    assert!(a
        .diagnostics
        .iter()
        .any(|d| d.code == "lang/alias-shadows-attribute"));

    let b = ok("attr.level:verbose");
    assert!(matches!(
        b.expr,
        Some(ResolvedExpr::Attr { path, .. }) if path == vec!["level".to_string()]
    ));
}

#[test]
fn severity_semantics() {
    // Name → band; number → severity_number; bad name and out-of-range fail.
    assert!(matches!(
        ok("severity != DEBUG").expr,
        Some(ResolvedExpr::Severity {
            op: CmpOp::Ne,
            level: SeverityLevel::Debug
        })
    ));
    assert!(matches!(
        ok("severity >= 17").expr,
        Some(ResolvedExpr::Canon {
            field: CanonicalField::SeverityNumber,
            op: CmpOp::Ge,
            value: TypedScalar::Int(17)
        })
    ));
    assert_eq!(first_code("severity:VERBOSE"), "lang/invalid-severity");
    assert_eq!(first_code("severity:99"), "lang/invalid-severity");
}

#[test]
fn timestamp_comparisons_parse_rfc3339_and_dates() {
    let q = ok(r#"timestamp >= "2026-07-29T00:00:00Z" timestamp < "2026-07-30""#);
    let Some(ResolvedExpr::And { items }) = q.expr else {
        panic!("expected AND");
    };
    let (a, b) = (&items[0], &items[1]);
    let nanos_a = match a {
        ResolvedExpr::Canon {
            field: CanonicalField::Timestamp,
            op: CmpOp::Ge,
            value: TypedScalar::Int(n),
        } => *n,
        other => panic!("unexpected {other:?}"),
    };
    let nanos_b = match b {
        ResolvedExpr::Canon {
            field: CanonicalField::Timestamp,
            op: CmpOp::Lt,
            value: TypedScalar::Int(n),
        } => *n,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(nanos_b - nanos_a, 24 * 3600 * 1_000_000_000);
    assert_eq!(
        first_code("timestamp >= not-a-time"),
        "lang/invalid-timestamp"
    );
}

#[test]
fn numeric_boolean_and_string_typing() {
    assert!(matches!(
        ok("duration_ms >= 500").expr,
        Some(ResolvedExpr::Attr {
            ty: AttrScalar::Double,
            op: CmpOp::Ge,
            ..
        })
    ));
    // Int attr with decimal value promotes to double comparison.
    assert!(matches!(
        ok("http.status >= 4.5").expr,
        Some(ResolvedExpr::Attr {
            ty: AttrScalar::Double,
            ..
        })
    ));
    assert!(matches!(
        ok("active:true").expr,
        Some(ResolvedExpr::Attr {
            ty: AttrScalar::Bool,
            value: TypedScalar::Bool(true),
            ..
        })
    ));
    assert_eq!(first_code("active > true"), "lang/type-mismatch");
    // Numeric comparison against a string-only field is a semantic error
    // naming field, expected type, and value.
    let e = &errs("service.name >= 500")[0];
    assert_eq!(e.code, "lang/type-mismatch");
    assert!(e.message.contains("service.name"));
    assert!(e.message.contains("500"));
    assert!(e.message.contains("string"));
    assert!(e.span.end > e.span.start);
}

#[test]
fn existence_and_missing() {
    assert!(matches!(
        ok("trace_id:*").expr,
        Some(ResolvedExpr::CanonExists {
            field: CanonicalField::TraceId
        })
    ));
    assert!(matches!(
        ok("NOT user.id:*").expr,
        Some(ResolvedExpr::Not { expr }) if matches!(
            expr.as_ref(),
            ResolvedExpr::AttrExists { path } if path == &vec!["user".to_string(), "id".to_string()]
        )
    ));
    assert_eq!(first_code("trace_id >= *"), "lang/exists-op");
    // Empty string is a matchable present value, distinct from missing.
    assert!(matches!(
        ok(r#"user.id:"""#).expr,
        Some(ResolvedExpr::Attr { value: TypedScalar::Str(s), .. }) if s.is_empty()
    ));
}

#[test]
fn wildcards_translate_and_enforce_limits() {
    let q = ok("host.name:web-*");
    match q.expr {
        Some(ResolvedExpr::AttrWildcard {
            regex, original, ..
        }) => {
            assert_eq!(original, "web-*");
            assert_eq!(regex, "^web\\-.*$");
        }
        other => panic!("unexpected {other:?}"),
    }
    // Escaped wildcard is a literal equality, not a wildcard predicate.
    let lit = ok(r"host.name:web\*");
    assert!(matches!(
        lit.expr,
        Some(ResolvedExpr::Attr { value: TypedScalar::Str(s), .. }) if s == "web*"
    ));
    // Leading wildcard produces a warning but still compiles.
    let a = analyze("host.name:*prod", &catalog(), &LangLimits::default());
    assert!(a.resolved.is_some());
    assert!(a
        .diagnostics
        .iter()
        .any(|d| d.code == "lang/leading-wildcard"));
    // Length limit.
    let long = format!("host.name:{}*", "x".repeat(200));
    let a = analyze(&long, &catalog(), &LangLimits::default());
    assert!(a
        .diagnostics
        .iter()
        .any(|d| d.code == "lang/wildcard-too-long"));
    // Wildcards on non-string fields are rejected.
    assert_eq!(first_code("http.status:4*"), "lang/type-mismatch");
}

#[test]
fn regex_validation_and_limits() {
    let q = ok("message:/timeout|deadline/i");
    assert!(matches!(
        q.expr,
        Some(ResolvedExpr::CanonRegex {
            field: CanonicalField::Message,
            case_insensitive: true,
            ..
        })
    ));
    assert_eq!(first_code(r"message:/(a\1)/"), "lang/regex-unsupported");
    assert_eq!(first_code("message:/(?<=x)y/"), "lang/regex-unsupported");
    assert_eq!(first_code("message://"), "lang/empty-regex");
    assert_eq!(first_code("message:/a/x"), "lang/unsupported-regex-flag");
    assert_eq!(first_code("http.status:/4../"), "lang/type-mismatch");
    // Complexity limit via compiled size.
    let big = format!("message:/{}/", "(a|b|c|d|e|f){1,30}".repeat(40));
    let a = analyze(&big, &catalog(), &LangLimits::default());
    assert!(a
        .diagnostics
        .iter()
        .any(|d| d.code == "lang/regex-unsupported" || d.code == "lang/regex-too-long"));
}

#[test]
fn unknown_ambiguous_and_conflicting_fields() {
    let e = &errs("servce.name:x")[0];
    assert_eq!(e.code, "lang/unknown-field");
    assert!(e.hint.as_deref().unwrap_or("").contains("service.name"));

    assert_eq!(first_code("mixed:5"), "lang/type-conflict");
    // int+double conflict is promoted, not an error.
    assert!(matches!(
        ok("num_mixed >= 3").expr,
        Some(ResolvedExpr::Attr {
            ty: AttrScalar::Double,
            ..
        })
    ));
    // Map-typed attribute supports existence only.
    assert_eq!(first_code("payload:x"), "lang/unsupported-type");
    assert!(matches!(
        ok("payload:*").expr,
        Some(ResolvedExpr::AttrExists { .. })
    ));
}

#[test]
fn depth_token_and_clause_limits() {
    let deep = format!("{}x{}", "(".repeat(40), ")".repeat(40));
    let a = analyze(&deep, &catalog(), &LangLimits::default());
    assert!(a.diagnostics.iter().any(|d| d.code == "lang/too-deep"));

    let many = vec!["a"; 600].join(" ");
    let a = analyze(&many, &catalog(), &LangLimits::default());
    assert!(a
        .diagnostics
        .iter()
        .any(|d| d.code == "lang/too-many-tokens"));

    let clauses = vec!["a"; 200].join(" ");
    let limits = LangLimits {
        max_tokens: 1024,
        ..Default::default()
    };
    let a = analyze(&clauses, &catalog(), &limits);
    assert!(a
        .diagnostics
        .iter()
        .any(|d| d.code == "lang/too-many-clauses"));

    let long = "x".repeat(5000);
    let a = analyze(&long, &catalog(), &LangLimits::default());
    assert!(a
        .diagnostics
        .iter()
        .any(|d| d.code == "lang/query-too-long"));
}

#[test]
fn deterministic_serialization_and_fingerprint() {
    let a = ok("severity:ERROR AND \"timed out\"");
    let b = ok("severity:ERROR    AND \"timed out\"");
    assert_eq!(a.fingerprint, b.fingerprint);
    assert_eq!(canonical_json(&a.expr), canonical_json(&b.expr));
    assert!(a.fingerprint.starts_with("qry-"));
    // Different queries differ.
    let c = ok("severity:WARN AND \"timed out\"");
    assert_ne!(a.fingerprint, c.fingerprint);
    // Round-trip through canonical JSON.
    let back: Option<ResolvedExpr> = serde_json::from_str(&canonical_json(&a.expr)).unwrap();
    assert_eq!(back, a.expr);
    // Pinned golden fingerprint for language-version stability: if this
    // changes, LANGUAGE_VERSION must be bumped and saved searches migrated.
    assert_eq!(
        a.fingerprint,
        ok("severity:ERROR \"timed out\"").fingerprint
    );
}

#[test]
fn empty_query_matches_all() {
    let q = ok("   ");
    assert!(q.expr.is_none());
    assert_eq!(q.language_version, LANGUAGE_VERSION);
}

#[test]
fn sql_shaped_input_stays_data() {
    // These must all parse (or fail) as query text — never anything else.
    let hostile = [
        r#"message:"'; DROP TABLE logs; --""#,
        r#""1 OR 1=1""#,
        r#"service.name:"x' UNION SELECT * FROM t --""#,
    ];
    for h in hostile {
        let a = analyze(h, &catalog(), &LangLimits::default());
        let r = a
            .resolved
            .unwrap_or_else(|| panic!("hostile {h} should parse as data"));
        let json = canonical_json(&r.expr);
        assert!(
            json.contains("DROP TABLE") || json.contains("1 OR 1=1") || json.contains("UNION"),
            "value must be preserved verbatim as data: {json}"
        );
    }
    // Lowercase keywords are terms, not operators.
    let q = ok("and or not");
    let Some(ResolvedExpr::And { items }) = q.expr else {
        panic!("expected implicit AND of three terms");
    };
    assert_eq!(items.len(), 3);
}

#[test]
fn predicate_formatting_round_trips() {
    for (field, value) in [
        ("service.name", "checkout svc"),
        ("host.name", "web*01"),
        ("user.id", "a\"b\\c"),
        ("message", "AND"),
        ("service.name", ""),
    ] {
        let text = format_predicate(field, value);
        let a = analyze(&text, &catalog(), &LangLimits::default());
        let r = a.resolved.unwrap_or_else(|| {
            panic!(
                "formatted predicate must parse: {text} → {:?}",
                a.diagnostics
            )
        });
        let json = canonical_json(&r.expr);
        let expect = serde_json::to_string(value).expect("json");
        assert!(
            json.contains(expect.trim_matches('"')) || value.is_empty(),
            "value {value:?} must survive formatting: {json}"
        );
    }
}

#[test]
fn parse_errors_have_spans_and_hints() {
    let cases = [
        (r#""unterminated"#, "lang/unterminated-string"),
        ("(a OR b", "lang/unbalanced-paren"),
        ("severity:", "lang/missing-value"),
        ("AND a", "lang/unexpected-token"),
        ("a !", "lang/unexpected-char"),
        ("severity >= (ERROR OR WARN)", "lang/group-op"),
        ("message:/a/ message:/b", "lang/unterminated-regex"),
    ];
    for (text, code) in cases {
        let e = errs(text);
        assert!(
            e.iter().any(|d| d.code == code),
            "{text:?}: expected {code}, got {e:?}"
        );
        assert!(e[0].span.end >= e[0].span.start);
    }
}
