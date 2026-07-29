//! Semantic resolution: raw AST + trusted field catalog → typed AST.
//!
//! Field identifiers are only ever resolved through the catalog contract —
//! nothing in the raw query text can reach SQL as an identifier. Values are
//! typed here; the compiler binds them as parameters.

use logscope_model::{stable_id, F64};
use serde::{Deserialize, Serialize};

use crate::ast::{CmpOp, RawExpr, RawField, RawValue, RawValueExpr};
use crate::diag::{Diagnostic, Span};
use crate::lex::unescape_word;
use crate::limits::LangLimits;

/// Version of the query language grammar + semantics described here.
pub const LANGUAGE_VERSION: u32 = 1;

/// Canonical log fields addressable in queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalField {
    Timestamp,
    ObservedTimestamp,
    Severity,
    SeverityText,
    SeverityNumber,
    Message,
    EventName,
    TraceId,
    SpanId,
    Operation,
    Outcome,
    EventType,
    RequestId,
    TransactionId,
    MessageId,
    EntityId,
    Dataset,
    RecordId,
}

/// Logical type classes for canonical fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonType {
    Timestamp,
    /// Band-compared severity level.
    Severity,
    Int,
    /// Token-matched message text.
    Text,
    /// Exact string.
    Str,
}

impl CanonicalField {
    pub fn canon_type(self) -> CanonType {
        use CanonicalField::*;
        match self {
            Timestamp | ObservedTimestamp => CanonType::Timestamp,
            Severity => CanonType::Severity,
            SeverityNumber => CanonType::Int,
            Message => CanonType::Text,
            _ => CanonType::Str,
        }
    }
    /// Primary display name (also the serialization name).
    pub fn display(self) -> &'static str {
        use CanonicalField::*;
        match self {
            Timestamp => "timestamp",
            ObservedTimestamp => "observed_timestamp",
            Severity => "severity",
            SeverityText => "severity.text",
            SeverityNumber => "severity.number",
            Message => "message",
            EventName => "event.name",
            TraceId => "trace_id",
            SpanId => "span_id",
            Operation => "operation",
            Outcome => "outcome",
            EventType => "event_type",
            RequestId => "request_id",
            TransactionId => "transaction_id",
            MessageId => "message_id",
            EntityId => "entity_id",
            Dataset => "dataset",
            RecordId => "record_id",
        }
    }
}

/// Built-in canonical names and aliases (OTel/ECS-flavored). Returns the
/// canonical field for an exactly written name.
pub fn builtin_field(written: &str) -> Option<CanonicalField> {
    use CanonicalField::*;
    Some(match written {
        "timestamp" | "@timestamp" | "time" => Timestamp,
        "observed_timestamp" | "observed.timestamp" => ObservedTimestamp,
        "severity" | "level" | "log.level" => Severity,
        "severity.text" | "severity_text" => SeverityText,
        "severity.number" | "severity_number" => SeverityNumber,
        "message" | "msg" | "body" => Message,
        "event.name" | "event_name" => EventName,
        "trace_id" | "trace.id" => TraceId,
        "span_id" | "span.id" => SpanId,
        "operation" => Operation,
        "outcome" => Outcome,
        "event_type" | "event.type" => EventType,
        "request_id" | "request.id" => RequestId,
        "transaction_id" | "transaction.id" => TransactionId,
        "message_id" | "message.id" => MessageId,
        "entity_id" | "entity.id" => EntityId,
        "dataset" => Dataset,
        "record_id" => RecordId,
        _ => return None,
    })
}

/// All built-in names, for suggestions.
pub fn builtin_field_names() -> &'static [&'static str] {
    &[
        "timestamp",
        "observed_timestamp",
        "severity",
        "severity.text",
        "severity.number",
        "message",
        "event.name",
        "trace_id",
        "span_id",
        "operation",
        "outcome",
        "event_type",
        "request_id",
        "transaction_id",
        "message_id",
        "entity_id",
        "dataset",
        "record_id",
    ]
}

/// Observed value type of a dynamic attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrType {
    Str,
    Int,
    Double,
    Bool,
    Bytes,
    Array,
    Map,
    Empty,
}

impl AttrType {
    pub fn describe(self) -> &'static str {
        match self {
            AttrType::Str => "string",
            AttrType::Int => "integer",
            AttrType::Double => "double",
            AttrType::Bool => "boolean",
            AttrType::Bytes => "bytes",
            AttrType::Array => "array",
            AttrType::Map => "object",
            AttrType::Empty => "empty",
        }
    }
    pub fn is_scalar(self) -> bool {
        matches!(
            self,
            AttrType::Str | AttrType::Int | AttrType::Double | AttrType::Bool
        )
    }
}

/// Attribute field information supplied by the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttrFieldInfo {
    /// Path segments into the nested typed attribute maps. A flat key that
    /// contains dots is a single segment.
    pub path: Vec<String>,
    /// Dotted display form.
    pub display: String,
    /// Deduplicated observed types across the selected datasets.
    pub types: Vec<AttrType>,
}

/// Result of resolving one written field path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldResolution {
    Canonical(CanonicalField),
    Attr(AttrFieldInfo),
    Unknown { suggestions: Vec<String> },
    Ambiguous { candidates: Vec<String> },
}

/// The trusted field catalog contract. Implementations resolve only against
/// known dataset fields; they never echo unknown user text back as an
/// identifier.
pub trait CatalogView {
    /// Resolves a written name in the attribute namespace.
    fn resolve_attr(&self, written: &str) -> FieldResolution;
    /// True when the written name also names a real attribute (used to warn
    /// when a built-in alias shadows one).
    fn attr_exists(&self, written: &str) -> bool;
}

/// Severity levels with their OTLP number bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeverityLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl SeverityLevel {
    pub fn parse(text: &str) -> Option<SeverityLevel> {
        Some(match text.to_ascii_uppercase().as_str() {
            "TRACE" => SeverityLevel::Trace,
            "DEBUG" => SeverityLevel::Debug,
            "INFO" => SeverityLevel::Info,
            "WARN" | "WARNING" => SeverityLevel::Warn,
            "ERROR" | "ERR" => SeverityLevel::Error,
            "FATAL" | "CRITICAL" => SeverityLevel::Fatal,
            _ => return None,
        })
    }
    /// Inclusive OTLP severity-number band.
    pub fn band(self) -> (i32, i32) {
        match self {
            SeverityLevel::Trace => (1, 4),
            SeverityLevel::Debug => (5, 8),
            SeverityLevel::Info => (9, 12),
            SeverityLevel::Warn => (13, 16),
            SeverityLevel::Error => (17, 20),
            SeverityLevel::Fatal => (21, 24),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            SeverityLevel::Trace => "TRACE",
            SeverityLevel::Debug => "DEBUG",
            SeverityLevel::Info => "INFO",
            SeverityLevel::Warn => "WARN",
            SeverityLevel::Error => "ERROR",
            SeverityLevel::Fatal => "FATAL",
        }
    }
}

/// A typed literal ready for parameter binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum TypedScalar {
    Str(String),
    Int(i64),
    Double(F64),
    Bool(bool),
}

/// Scalar attribute comparison type (post type-checking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttrScalar {
    Str,
    Int,
    Double,
    Bool,
}

/// Typed, catalog-resolved query expression. This is the single canonical
/// form: the compiler, the fingerprint, and saved-search normalization all
/// consume exactly this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum ResolvedExpr {
    /// Free-text token match on the message.
    Term {
        term: String,
    },
    /// Free-text consecutive-token match on the message.
    Phrase {
        phrase: String,
    },
    /// Severity band comparison (Eq/Ne = band membership; ordering compares
    /// against the band boundary on `severity_number`).
    Severity {
        op: CmpOp,
        level: SeverityLevel,
    },
    /// Typed canonical comparison (timestamps carry Int nanos).
    Canon {
        field: CanonicalField,
        op: CmpOp,
        value: TypedScalar,
    },
    CanonExists {
        field: CanonicalField,
    },
    /// Wildcard on a string-typed canonical field; `regex` is the anchored
    /// translation produced by [`wildcard_to_regex`].
    CanonWildcard {
        field: CanonicalField,
        regex: String,
        original: String,
    },
    CanonRegex {
        field: CanonicalField,
        pattern: String,
        case_insensitive: bool,
    },
    Attr {
        path: Vec<String>,
        ty: AttrScalar,
        op: CmpOp,
        value: TypedScalar,
    },
    AttrExists {
        path: Vec<String>,
    },
    AttrWildcard {
        path: Vec<String>,
        regex: String,
        original: String,
    },
    AttrRegex {
        path: Vec<String>,
        pattern: String,
        case_insensitive: bool,
    },
    Not {
        expr: Box<ResolvedExpr>,
    },
    And {
        items: Vec<ResolvedExpr>,
    },
    Or {
        items: Vec<ResolvedExpr>,
    },
}

/// Outcome of successful resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedQuery {
    /// `None` = match all records.
    pub expr: Option<ResolvedExpr>,
    pub language_version: u32,
    /// Deterministic fingerprint of the resolved expression.
    pub fingerprint: String,
    pub warnings: Vec<Diagnostic>,
}

/// Canonical JSON of a resolved expression (deterministic: no maps, stable
/// variant field order).
pub fn canonical_json(expr: &Option<ResolvedExpr>) -> String {
    serde_json::to_string(expr).expect("resolved expression serialization cannot fail")
}

pub fn fingerprint(expr: &Option<ResolvedExpr>) -> String {
    let json = canonical_json(expr);
    stable_id("qry", |d| {
        d.str("query.v1");
        d.u32(LANGUAGE_VERSION);
        d.str(&json);
    })
}

/// Translates a wildcard value into an anchored regex (`*` → `.*`,
/// `?` → `.`, `\*`/`\?`/`\\` → literals, everything else escaped).
pub fn wildcard_to_regex(glob: &str) -> String {
    let mut out = String::with_capacity(glob.len() + 8);
    out.push('^');
    let mut chars = glob.chars();
    while let Some(c) = chars.next() {
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '\\' => {
                if let Some(next) = chars.next() {
                    push_escaped(&mut out, next);
                } else {
                    out.push_str("\\\\");
                }
            }
            other => push_escaped(&mut out, other),
        }
    }
    out.push('$');
    out
}

fn push_escaped(out: &mut String, c: char) {
    if c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii() {
        out.push(c);
    } else {
        out.push('\\');
        out.push(c);
    }
}

struct Resolver<'a> {
    catalog: &'a dyn CatalogView,
    limits: &'a LangLimits,
    errors: Vec<Diagnostic>,
    warnings: Vec<Diagnostic>,
    clauses: usize,
}

/// Resolves and validates a parsed query against the catalog. Returns every
/// semantic error at once (not just the first).
pub fn resolve(
    ast: Option<&RawExpr>,
    catalog: &dyn CatalogView,
    limits: &LangLimits,
) -> Result<ResolvedQuery, Vec<Diagnostic>> {
    let mut r = Resolver {
        catalog,
        limits,
        errors: vec![],
        warnings: vec![],
        clauses: 0,
    };
    let expr = ast.and_then(|a| r.expr(a));
    if r.clauses > limits.max_clauses {
        r.errors.push(Diagnostic::error(
            "lang/too-many-clauses",
            format!(
                "query has {} predicates; the maximum is {}",
                r.clauses, limits.max_clauses
            ),
            ast.map(|a| a.span()).unwrap_or_default(),
        ));
    }
    if !r.errors.is_empty() {
        return Err(r.errors);
    }
    Ok(ResolvedQuery {
        fingerprint: fingerprint(&expr),
        expr,
        language_version: LANGUAGE_VERSION,
        warnings: r.warnings,
    })
}

impl<'a> Resolver<'a> {
    fn err(&mut self, d: Diagnostic) -> Option<ResolvedExpr> {
        self.errors.push(d);
        None
    }

    fn expr(&mut self, raw: &RawExpr) -> Option<ResolvedExpr> {
        match raw {
            RawExpr::Term {
                text,
                wildcard,
                span,
            } => {
                self.clauses += 1;
                if *wildcard {
                    self.warnings.push(
                        Diagnostic::warning(
                            "lang/wildcard-in-text",
                            "wildcards only apply to field values; here `*`/`?` match literally",
                            *span,
                        )
                        .with_hint("use message:web-* for a wildcard match on the message"),
                    );
                }
                Some(ResolvedExpr::Term {
                    term: unescape_word(text),
                })
            }
            RawExpr::Phrase { text, span } => {
                self.clauses += 1;
                if text.trim().is_empty() {
                    return self.err(Diagnostic::error(
                        "lang/empty-phrase",
                        "empty phrase matches nothing",
                        *span,
                    ));
                }
                Some(ResolvedExpr::Phrase {
                    phrase: text.clone(),
                })
            }
            RawExpr::Cmp {
                field, op, value, ..
            } => {
                self.clauses += 1;
                self.predicate(field, *op, value)
            }
            RawExpr::Group { field, expr, .. } => self.group(field, expr),
            RawExpr::Not(inner, _) => {
                let inner = self.expr(inner)?;
                Some(ResolvedExpr::Not {
                    expr: Box::new(inner),
                })
            }
            RawExpr::And(items, _) => {
                let resolved: Vec<_> = items.iter().filter_map(|i| self.expr(i)).collect();
                if resolved.len() != items.len() {
                    return None;
                }
                Some(ResolvedExpr::And { items: resolved })
            }
            RawExpr::Or(items, _) => {
                let resolved: Vec<_> = items.iter().filter_map(|i| self.expr(i)).collect();
                if resolved.len() != items.len() {
                    return None;
                }
                Some(ResolvedExpr::Or { items: resolved })
            }
        }
    }

    fn group(&mut self, field: &RawField, expr: &RawValueExpr) -> Option<ResolvedExpr> {
        match expr {
            RawValueExpr::Value(v) => {
                self.clauses += 1;
                self.predicate(field, CmpOp::Eq, v)
            }
            RawValueExpr::Not(inner, _) => {
                let inner = self.group(field, inner)?;
                Some(ResolvedExpr::Not {
                    expr: Box::new(inner),
                })
            }
            RawValueExpr::And(items, _) => {
                let resolved: Vec<_> = items.iter().filter_map(|i| self.group(field, i)).collect();
                if resolved.len() != items.len() {
                    return None;
                }
                Some(ResolvedExpr::And { items: resolved })
            }
            RawValueExpr::Or(items, _) => {
                let resolved: Vec<_> = items.iter().filter_map(|i| self.group(field, i)).collect();
                if resolved.len() != items.len() {
                    return None;
                }
                Some(ResolvedExpr::Or { items: resolved })
            }
        }
    }

    fn resolve_field(&mut self, field: &RawField) -> Option<FieldResolution> {
        // `attr.` prefix always forces the attribute namespace.
        if let Some(rest) = field.text.strip_prefix("attr.") {
            let res = self.catalog.resolve_attr(rest);
            return self.check_attr_resolution(res, rest, field.span);
        }
        if let Some(canon) = builtin_field(&field.text) {
            if self.catalog.attr_exists(&field.text) {
                self.warnings.push(
                    Diagnostic::warning(
                        "lang/alias-shadows-attribute",
                        format!(
                            "`{}` is the built-in `{}` field; an attribute with the same name also exists",
                            field.text,
                            canon.display()
                        ),
                        field.span,
                    )
                    .with_hint(format!("use attr.{} to query the attribute", field.text)),
                );
            }
            return Some(FieldResolution::Canonical(canon));
        }
        let res = self.catalog.resolve_attr(&field.text);
        self.check_attr_resolution(res, &field.text, field.span)
    }

    fn check_attr_resolution(
        &mut self,
        res: FieldResolution,
        written: &str,
        span: Span,
    ) -> Option<FieldResolution> {
        match res {
            FieldResolution::Unknown { suggestions } => {
                let mut d = Diagnostic::error(
                    "lang/unknown-field",
                    format!("unknown field `{written}`"),
                    span,
                );
                if !suggestions.is_empty() {
                    d = d.with_hint(format!("did you mean: {}", suggestions.join(", ")));
                }
                self.errors.push(d);
                None
            }
            FieldResolution::Ambiguous { candidates } => {
                self.errors.push(
                    Diagnostic::error(
                        "lang/ambiguous-field",
                        format!(
                            "`{written}` is ambiguous between: {}",
                            candidates.join(", ")
                        ),
                        span,
                    )
                    .with_hint("write the full path of the field you mean"),
                );
                None
            }
            ok => Some(ok),
        }
    }

    fn predicate(&mut self, field: &RawField, op: CmpOp, value: &RawValue) -> Option<ResolvedExpr> {
        let resolution = self.resolve_field(field)?;

        // Existence tests are type-independent.
        if let RawValue::Star { span } = value {
            if !matches!(op, CmpOp::Eq) {
                return self.err(Diagnostic::error(
                    "lang/exists-op",
                    "existence `*` only combines with `:`",
                    *span,
                ));
            }
            return Some(match resolution {
                FieldResolution::Canonical(f) => {
                    if matches!(
                        f,
                        CanonicalField::Message
                            | CanonicalField::Dataset
                            | CanonicalField::RecordId
                    ) {
                        self.warnings.push(Diagnostic::warning(
                            "lang/always-present",
                            format!("`{}` is present on every record", f.display()),
                            *span,
                        ));
                    }
                    ResolvedExpr::CanonExists { field: f }
                }
                FieldResolution::Attr(info) => ResolvedExpr::AttrExists { path: info.path },
                _ => unreachable!("errors already reported"),
            });
        }

        match resolution {
            FieldResolution::Canonical(f) => self.canonical_predicate(f, field, op, value),
            FieldResolution::Attr(info) => self.attr_predicate(info, field, op, value),
            _ => unreachable!("errors already reported"),
        }
    }

    fn canonical_predicate(
        &mut self,
        f: CanonicalField,
        field: &RawField,
        op: CmpOp,
        value: &RawValue,
    ) -> Option<ResolvedExpr> {
        match value {
            RawValue::Regex {
                pattern,
                case_insensitive,
                span,
            } => {
                if !self.regex_ok(pattern, *case_insensitive, *span) {
                    return None;
                }
                match f.canon_type() {
                    CanonType::Text | CanonType::Str => Some(ResolvedExpr::CanonRegex {
                        field: f,
                        pattern: pattern.clone(),
                        case_insensitive: *case_insensitive,
                    }),
                    _ => self.err(type_mismatch(field, f, "a regex", *span)),
                }
            }
            RawValue::Word {
                text,
                wildcard: true,
                span,
            } => {
                if text.chars().count() > self.limits.max_wildcard_chars {
                    return self.err(Diagnostic::error(
                        "lang/wildcard-too-long",
                        format!(
                            "wildcard value exceeds {} characters",
                            self.limits.max_wildcard_chars
                        ),
                        *span,
                    ));
                }
                match f.canon_type() {
                    CanonType::Text | CanonType::Str => {
                        self.warn_leading_wildcard(text, *span);
                        let regex = wildcard_to_regex(text);
                        if !self.regex_ok(&regex, false, *span) {
                            return None;
                        }
                        Some(ResolvedExpr::CanonWildcard {
                            field: f,
                            regex,
                            original: text.clone(),
                        })
                    }
                    _ => self.err(type_mismatch(field, f, "a wildcard", *span)),
                }
            }
            RawValue::Word { text, span, .. } | RawValue::Quoted { text, span } => {
                let literal = match value {
                    RawValue::Word { .. } => unescape_word(text),
                    _ => text.clone(),
                };
                self.canonical_literal(
                    f,
                    field,
                    op,
                    &literal,
                    *span,
                    matches!(value, RawValue::Quoted { .. }),
                )
            }
            RawValue::Star { .. } => unreachable!("handled by caller"),
        }
    }

    fn canonical_literal(
        &mut self,
        f: CanonicalField,
        field: &RawField,
        op: CmpOp,
        literal: &str,
        span: Span,
        quoted: bool,
    ) -> Option<ResolvedExpr> {
        match f.canon_type() {
            CanonType::Timestamp => match parse_timestamp_nanos(literal) {
                Some(nanos) => Some(ResolvedExpr::Canon {
                    field: f,
                    op,
                    value: TypedScalar::Int(nanos),
                }),
                None => self.err(
                    Diagnostic::error(
                        "lang/invalid-timestamp",
                        format!("`{literal}` is not a valid timestamp for `{}`", field.text),
                        span,
                    )
                    .with_expected("RFC 3339, e.g. \"2026-07-29T00:00:00Z\", or a date YYYY-MM-DD"),
                ),
            },
            CanonType::Severity => {
                // Numeric values compare against severity_number directly.
                if let Ok(n) = literal.parse::<i64>() {
                    if !(1..=24).contains(&n) {
                        return self.err(Diagnostic::error(
                            "lang/invalid-severity",
                            format!("severity number {n} is outside 1..=24"),
                            span,
                        ));
                    }
                    return Some(ResolvedExpr::Canon {
                        field: CanonicalField::SeverityNumber,
                        op,
                        value: TypedScalar::Int(n),
                    });
                }
                match SeverityLevel::parse(literal) {
                    Some(level) => Some(ResolvedExpr::Severity { op, level }),
                    None => self.err(
                        Diagnostic::error(
                            "lang/invalid-severity",
                            format!("`{literal}` is not a severity level"),
                            span,
                        )
                        .with_expected("TRACE, DEBUG, INFO, WARN, ERROR, or FATAL"),
                    ),
                }
            }
            CanonType::Int => match literal.parse::<i64>() {
                Ok(n) => Some(ResolvedExpr::Canon {
                    field: f,
                    op,
                    value: TypedScalar::Int(n),
                }),
                Err(_) => self.err(
                    Diagnostic::error(
                        "lang/type-mismatch",
                        format!(
                            "`{}` is an integer field; `{literal}` is not an integer",
                            field.text
                        ),
                        span,
                    )
                    .with_expected("an integer value"),
                ),
            },
            CanonType::Text => match op {
                CmpOp::Eq | CmpOp::Ne => {
                    let is_multi = literal.split_whitespace().nth(1).is_some();
                    let matcher = if quoted || is_multi {
                        ResolvedExpr::Phrase {
                            phrase: literal.to_string(),
                        }
                    } else {
                        ResolvedExpr::Term {
                            term: literal.to_string(),
                        }
                    };
                    Some(if op == CmpOp::Ne {
                        ResolvedExpr::Not {
                            expr: Box::new(matcher),
                        }
                    } else {
                        matcher
                    })
                }
                _ => self.err(Diagnostic::error(
                    "lang/type-mismatch",
                    format!(
                        "`{}` is message text; ordering comparison `{}` is not defined for it",
                        field.text,
                        op.describe()
                    ),
                    span,
                )),
            },
            CanonType::Str => {
                if !matches!(op, CmpOp::Eq | CmpOp::Ne) {
                    return self.err(
                        Diagnostic::error(
                            "lang/type-mismatch",
                            format!(
                                "`{}` is a string field; ordering comparison `{}` with `{literal}` is not supported",
                                field.text,
                                op.describe()
                            ),
                            span,
                        )
                        .with_expected("a numeric, timestamp, or severity field for ordering"),
                    );
                }
                Some(ResolvedExpr::Canon {
                    field: f,
                    op,
                    value: TypedScalar::Str(literal.to_string()),
                })
            }
        }
    }

    fn attr_predicate(
        &mut self,
        info: AttrFieldInfo,
        field: &RawField,
        op: CmpOp,
        value: &RawValue,
    ) -> Option<ResolvedExpr> {
        let scalar_types: Vec<AttrType> = info
            .types
            .iter()
            .copied()
            .filter(|t| t.is_scalar())
            .collect();
        if scalar_types.is_empty() {
            let seen = info
                .types
                .iter()
                .map(|t| t.describe())
                .collect::<Vec<_>>()
                .join(", ");
            return self.err(
                Diagnostic::error(
                    "lang/unsupported-type",
                    format!(
                        "`{}` has non-comparable value type(s): {seen}",
                        info.display
                    ),
                    field.span,
                )
                .with_hint("only existence (`field:*`) is supported for this field"),
            );
        }
        // Type class: single scalar type, or int+double promoted to double.
        let ty = if scalar_types.len() == 1 {
            scalar_types[0]
        } else if scalar_types
            .iter()
            .all(|t| matches!(t, AttrType::Int | AttrType::Double))
        {
            AttrType::Double
        } else {
            let seen = scalar_types
                .iter()
                .map(|t| t.describe())
                .collect::<Vec<_>>()
                .join(", ");
            return self.err(
                Diagnostic::error(
                    "lang/type-conflict",
                    format!(
                        "`{}` has conflicting types across the selected datasets: {seen}",
                        info.display
                    ),
                    field.span,
                )
                .with_hint("narrow the dataset selection so the field has one type"),
            );
        };

        match value {
            RawValue::Regex {
                pattern,
                case_insensitive,
                span,
            } => {
                if ty != AttrType::Str {
                    return self.err(attr_type_mismatch(&info, ty, "a regex", *span));
                }
                if !self.regex_ok(pattern, *case_insensitive, *span) {
                    return None;
                }
                Some(ResolvedExpr::AttrRegex {
                    path: info.path,
                    pattern: pattern.clone(),
                    case_insensitive: *case_insensitive,
                })
            }
            RawValue::Word {
                text,
                wildcard: true,
                span,
            } => {
                if ty != AttrType::Str {
                    return self.err(attr_type_mismatch(&info, ty, "a wildcard", *span));
                }
                if text.chars().count() > self.limits.max_wildcard_chars {
                    return self.err(Diagnostic::error(
                        "lang/wildcard-too-long",
                        format!(
                            "wildcard value exceeds {} characters",
                            self.limits.max_wildcard_chars
                        ),
                        *span,
                    ));
                }
                self.warn_leading_wildcard(text, *span);
                let regex = wildcard_to_regex(text);
                if !self.regex_ok(&regex, false, *span) {
                    return None;
                }
                Some(ResolvedExpr::AttrWildcard {
                    path: info.path,
                    regex,
                    original: text.clone(),
                })
            }
            RawValue::Word { text, span, .. } | RawValue::Quoted { text, span } => {
                let literal = match value {
                    RawValue::Word { .. } => unescape_word(text),
                    _ => text.clone(),
                };
                if matches!(ty, AttrType::Bool | AttrType::Str)
                    && !matches!(op, CmpOp::Eq | CmpOp::Ne)
                {
                    let ty_name = if ty == AttrType::Bool {
                        "boolean"
                    } else {
                        "string"
                    };
                    return self.err(
                        Diagnostic::error(
                            "lang/type-mismatch",
                            format!(
                                "`{}` is a {ty_name} field; ordering comparison `{}` with `{literal}` is not supported",
                                info.display,
                                op.describe()
                            ),
                            *span,
                        )
                        .with_expected("a numeric or timestamp field for ordering"),
                    );
                }
                let (scalar, typed) = match ty {
                    AttrType::Str => (AttrScalar::Str, TypedScalar::Str(literal)),
                    AttrType::Int => match literal.parse::<i64>() {
                        Ok(n) => (AttrScalar::Int, TypedScalar::Int(n)),
                        Err(_) => match literal.parse::<f64>() {
                            Ok(d) if d.is_finite() => {
                                (AttrScalar::Double, TypedScalar::Double(F64(d)))
                            }
                            _ => {
                                return self.err(
                                    Diagnostic::error(
                                        "lang/type-mismatch",
                                        format!(
                                            "`{}` is an integer field; `{literal}` is not a number",
                                            info.display
                                        ),
                                        *span,
                                    )
                                    .with_expected("an integer or decimal value"),
                                )
                            }
                        },
                    },
                    AttrType::Double => match literal.parse::<f64>() {
                        Ok(d) if d.is_finite() => (AttrScalar::Double, TypedScalar::Double(F64(d))),
                        _ => {
                            return self.err(
                                Diagnostic::error(
                                    "lang/type-mismatch",
                                    format!(
                                        "`{}` is a numeric field; `{literal}` is not a number",
                                        info.display
                                    ),
                                    *span,
                                )
                                .with_expected("a numeric value"),
                            )
                        }
                    },
                    AttrType::Bool => match literal.to_ascii_lowercase().as_str() {
                        "true" => (AttrScalar::Bool, TypedScalar::Bool(true)),
                        "false" => (AttrScalar::Bool, TypedScalar::Bool(false)),
                        _ => {
                            return self.err(
                                Diagnostic::error(
                                    "lang/type-mismatch",
                                    format!(
                                        "`{}` is a boolean field; `{literal}` is not true/false",
                                        info.display
                                    ),
                                    *span,
                                )
                                .with_expected("true or false"),
                            )
                        }
                    },
                    _ => unreachable!("scalar types only"),
                };
                Some(ResolvedExpr::Attr {
                    path: info.path,
                    ty: scalar,
                    op,
                    value: typed,
                })
            }
            RawValue::Star { .. } => unreachable!("handled by caller"),
        }
    }

    fn warn_leading_wildcard(&mut self, text: &str, span: Span) {
        if text.starts_with(['*', '?']) {
            self.warnings.push(
                Diagnostic::warning(
                    "lang/leading-wildcard",
                    "leading wildcard must inspect every value and can be slow",
                    span,
                )
                .with_hint("prefer a prefix before the first `*` when possible"),
            );
        }
    }

    /// Validates a regex pattern with the linear-time engine and size limit.
    fn regex_ok(&mut self, pattern: &str, ci: bool, span: Span) -> bool {
        if pattern.is_empty() {
            self.errors.push(Diagnostic::error(
                "lang/empty-regex",
                "empty regex pattern",
                span,
            ));
            return false;
        }
        if pattern.chars().count() > self.limits.max_regex_chars {
            self.errors.push(Diagnostic::error(
                "lang/regex-too-long",
                format!("regex exceeds {} characters", self.limits.max_regex_chars),
                span,
            ));
            return false;
        }
        match regex::RegexBuilder::new(pattern)
            .case_insensitive(ci)
            .size_limit(self.limits.max_regex_compiled_bytes)
            .build()
        {
            Ok(_) => {
                if pattern.starts_with(".*") || pattern.starts_with("(?s)") {
                    self.warnings.push(Diagnostic::warning(
                        "lang/broad-regex",
                        "this pattern begins unanchored-broad and can be slow",
                        span,
                    ));
                }
                true
            }
            Err(e) => {
                let msg = match &e {
                    regex::Error::CompiledTooBig(_) => format!(
                        "regex is too complex (compiled size over {} bytes)",
                        self.limits.max_regex_compiled_bytes
                    ),
                    other => format!("unsupported regex: {other}"),
                };
                self.errors.push(
                    Diagnostic::error("lang/regex-unsupported", msg, span).with_hint(
                        "backreferences and lookaround are not supported; the engine is linear-time",
                    ),
                );
                false
            }
        }
    }
}

fn type_mismatch(field: &RawField, f: CanonicalField, what: &str, span: Span) -> Diagnostic {
    let ty = match f.canon_type() {
        CanonType::Timestamp => "a timestamp",
        CanonType::Severity => "a severity level",
        CanonType::Int => "an integer",
        CanonType::Text => "message text",
        CanonType::Str => "a string",
    };
    Diagnostic::error(
        "lang/type-mismatch",
        format!("`{}` is {ty} field; {what} does not apply", field.text),
        span,
    )
}

fn attr_type_mismatch(info: &AttrFieldInfo, ty: AttrType, what: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        "lang/type-mismatch",
        format!(
            "`{}` is {} typed; {what} only applies to string fields",
            info.display,
            ty.describe()
        ),
        span,
    )
}

/// Parses an RFC 3339 timestamp or plain date into UTC nanos.
pub fn parse_timestamp_nanos(text: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(text) {
        return dt.timestamp_nanos_opt();
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        let dt = date.and_hms_opt(0, 0, 0)?.and_utc();
        return dt.timestamp_nanos_opt();
    }
    None
}

/// Simple suggestion helper shared by catalog implementations: candidate
/// names that contain or are contained by the written name
/// (case-insensitive), best matches first, bounded.
pub fn suggest_fields<'a>(
    written: &str,
    candidates: impl Iterator<Item = &'a str>,
    max: usize,
) -> Vec<String> {
    let lower = written.to_lowercase();
    let mut scored: Vec<(usize, String)> = candidates
        .filter_map(|c| {
            let cl = c.to_lowercase();
            let common_prefix = cl
                .chars()
                .zip(lower.chars())
                .take_while(|(a, b)| a == b)
                .count();
            if cl == lower {
                Some((0, c.to_string()))
            } else if cl.starts_with(&lower) || lower.starts_with(&cl) {
                Some((1, c.to_string()))
            } else if cl.contains(&lower) || lower.contains(&cl) {
                Some((2, c.to_string()))
            } else if common_prefix >= 3 {
                Some((3, c.to_string()))
            } else {
                None
            }
        })
        .collect();
    scored.sort();
    scored.dedup();
    scored.truncate(max);
    scored.into_iter().map(|(_, s)| s).collect()
}
