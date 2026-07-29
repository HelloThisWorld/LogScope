//! Property/fuzz smoke tests: the pipeline never panics, limits always
//! bound resource use, and escaping round-trips.

use logscope_query_lang::*;
use proptest::prelude::*;

fn catalog() -> StaticCatalog {
    StaticCatalog::default()
        .with("service.name", &["service.name"], &[AttrType::Str])
        .with("retry.count", &["retry", "count"], &[AttrType::Int])
        .with("duration_ms", &["duration_ms"], &[AttrType::Double])
        .with("active", &["active"], &[AttrType::Bool])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Arbitrary text (including hostile operator soup) never panics and
    /// never produces an un-spanned diagnostic.
    #[test]
    fn analyze_never_panics(text in "\\PC{0,200}") {
        let a = analyze(&text, &catalog(), &LangLimits::default());
        for d in &a.diagnostics {
            prop_assert!(d.span.end >= d.span.start);
            prop_assert!((d.span.end as usize) <= text.len().max(1));
        }
    }

    /// Operator-dense strings exercise the parser paths.
    #[test]
    fn operator_soup_never_panics(text in "[a-z():=<>!\"*?/\\\\ ]{0,120}") {
        let _ = analyze(&text, &catalog(), &LangLimits::default());
    }

    /// Any value can be formatted into a predicate that re-parses to the
    /// same literal.
    #[test]
    fn format_value_round_trips(value in "\\PC{0,60}") {
        // Control characters other than the supported escapes cannot be
        // represented in query text; skip them.
        prop_assume!(!value.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r')));
        let text = format_predicate("service.name", &value);
        let a = analyze(&text, &catalog(), &LangLimits::default());
        let r = a.resolved.expect("formatted predicate parses");
        match r.expr {
            Some(ResolvedExpr::Attr { value: TypedScalar::Str(s), .. }) => {
                prop_assert_eq!(s, value);
            }
            Some(ResolvedExpr::AttrWildcard { .. }) => {
                prop_assert!(false, "formatting must escape wildcards: {}", text);
            }
            other => prop_assert!(false, "unexpected shape {:?} for {}", other, text),
        }
    }

    /// Wildcard translation always yields a compilable, anchored regex.
    #[test]
    fn wildcard_regex_always_compiles(glob in "[a-zA-Z0-9*?._\\-\\\\]{0,64}") {
        let regex = wildcard_to_regex(&glob);
        prop_assert!(regex.starts_with('^') && regex.ends_with('$'));
        let compiled = regex::RegexBuilder::new(&regex)
            .size_limit(256 * 1024)
            .build();
        prop_assert!(compiled.is_ok(), "wildcard {:?} → invalid regex {:?}", glob, regex);
    }

    /// Fingerprints are deterministic across repeated analysis.
    #[test]
    fn fingerprints_are_stable(text in "[a-z0-9 :\"()]{0,80}") {
        let a = analyze(&text, &catalog(), &LangLimits::default());
        let b = analyze(&text, &catalog(), &LangLimits::default());
        match (a.resolved, b.resolved) {
            (Some(x), Some(y)) => prop_assert_eq!(x.fingerprint, y.fingerprint),
            (None, None) => {}
            _ => prop_assert!(false, "non-deterministic outcome for {:?}", text),
        }
    }
}
