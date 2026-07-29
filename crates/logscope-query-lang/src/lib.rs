//! LogScope query language v1 (ADR-0011).
//!
//! One authoritative pipeline: lex → parse → resolve against the trusted
//! field catalog. The editor consumes tokens and diagnostics from here; the
//! compiler in `logscope-query` consumes the typed [`ResolvedExpr`]. There
//! is deliberately no second grammar anywhere.

pub mod ast;
pub mod diag;
pub mod lex;
pub mod limits;
pub mod parse;
pub mod resolve;

pub use ast::{CmpOp, RawExpr};
pub use diag::{DiagSeverity, Diagnostic, Span};
pub use lex::{highlight, lex, HighlightKind, HighlightSpan, Token, TokenKind};
pub use limits::LangLimits;
pub use parse::{parse, ParseOutcome};
pub use resolve::{
    builtin_field, builtin_field_names, canonical_json, fingerprint, parse_timestamp_nanos,
    resolve, suggest_fields, wildcard_to_regex, AttrFieldInfo, AttrScalar, AttrType, CanonType,
    CanonicalField, CatalogView, FieldResolution, ResolvedExpr, ResolvedQuery, SeverityLevel,
    TypedScalar, LANGUAGE_VERSION,
};

/// Complete analysis of one query text: tokens for highlighting, all
/// diagnostics (lex + parse + semantic, errors and warnings), and the typed
/// query when everything succeeded.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub tokens: Vec<Token>,
    pub highlights: Vec<HighlightSpan>,
    pub diagnostics: Vec<Diagnostic>,
    /// Present iff there were no errors. `resolved.expr == None` = match all.
    pub resolved: Option<ResolvedQuery>,
}

/// Runs the full authoritative pipeline on `text`.
pub fn analyze(text: &str, catalog: &dyn CatalogView, limits: &LangLimits) -> Analysis {
    let (tokens, mut diagnostics) = lex::lex(text, limits);
    let highlights = lex::highlight(&tokens);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Analysis {
            tokens,
            highlights,
            diagnostics,
            resolved: None,
        };
    }
    let parsed = parse::parse(&tokens, text, limits);
    diagnostics.extend(parsed.diagnostics);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Analysis {
            tokens,
            highlights,
            diagnostics,
            resolved: None,
        };
    }
    match resolve::resolve(parsed.ast.as_ref(), catalog, limits) {
        Ok(resolved) => {
            diagnostics.extend(resolved.warnings.iter().cloned());
            Analysis {
                tokens,
                highlights,
                diagnostics,
                resolved: Some(resolved),
            }
        }
        Err(errors) => {
            diagnostics.extend(errors);
            Analysis {
                tokens,
                highlights,
                diagnostics,
                resolved: None,
            }
        }
    }
}

/// Serializes a field predicate for programmatic query building (facet
/// clicks, detail-panel "add to query"). This is the single authoritative
/// escaper — UI code must never assemble query fragments by hand.
pub fn format_predicate(field: &str, value: &str) -> String {
    format!("{}:{}", format_field(field), format_value(value))
}

/// Quotes/escapes a field name for query text (fields are bare words).
pub fn format_field(field: &str) -> String {
    field.to_string()
}

/// Quotes a value when it cannot stand as a bare word.
pub fn format_value(value: &str) -> String {
    let bare_safe = !value.is_empty()
        && value.chars().all(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '(' | ')' | '"' | ':' | '=' | '<' | '>' | '!' | '*' | '?' | '\\' | '/'
                )
        })
        && !matches!(value, "AND" | "OR" | "NOT");
    if bare_safe {
        value.to_string()
    } else {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                other => out.push(other),
            }
        }
        out.push('"');
        out
    }
}

/// Test/reference catalog over a fixed attribute table. Real catalogs live
/// in `logscope-query` and are dataset-aware.
#[derive(Debug, Default, Clone)]
pub struct StaticCatalog {
    pub attrs: std::collections::BTreeMap<String, AttrFieldInfo>,
}

impl StaticCatalog {
    pub fn with(mut self, display: &str, path: &[&str], types: &[AttrType]) -> Self {
        self.attrs.insert(
            display.to_string(),
            AttrFieldInfo {
                path: path.iter().map(|s| s.to_string()).collect(),
                display: display.to_string(),
                types: types.to_vec(),
            },
        );
        self
    }
}

impl CatalogView for StaticCatalog {
    fn resolve_attr(&self, written: &str) -> FieldResolution {
        match self.attrs.get(written) {
            Some(info) => FieldResolution::Attr(info.clone()),
            None => FieldResolution::Unknown {
                suggestions: suggest_fields(
                    written,
                    self.attrs
                        .keys()
                        .map(|s| s.as_str())
                        .chain(builtin_field_names().iter().copied()),
                    3,
                ),
            },
        }
    }
    fn attr_exists(&self, written: &str) -> bool {
        self.attrs.contains_key(written)
    }
}
