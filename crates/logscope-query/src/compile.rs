//! Compiler: typed `ResolvedExpr` → one parameterized DuckDB WHERE clause.
//!
//! Invariants (ADR-0012):
//! - every user value is a bound parameter — never string-built SQL;
//! - field identifiers come only from the compiled-in canonical column map
//!   or from JSON *path parameters* built out of catalog-resolved segments;
//! - every leaf predicate collapses SQL three-valued logic with
//!   `COALESCE(…, false)`, so `NOT` means "does not match", including
//!   records where the field is missing;
//! - `!=` is compiled as `NOT(=)` (missing values match `!=`);
//! - free-text terms/phrases have exactly one token semantics, executed
//!   either through the FTS index (candidate row-id join) or through an
//!   equivalent RE2 predicate — chosen per predicate, never mixed meaning.

use duckdb::types::Value;
use logscope_query_lang::{CanonicalField, CmpOp, ResolvedExpr, SeverityLevel, TypedScalar};
use logscope_store::FtsIndex;

use crate::error::QueryError;

/// FTS candidate bound: above this many hits for one text predicate the
/// compiler switches that predicate to the (exact, slower) regex scan.
pub const MAX_FTS_CANDIDATES: usize = 100_000;

/// How one free-text predicate was executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextExecMode {
    FtsIndex,
    RegexScan,
}

/// A compiled boolean filter plus everything needed to run it.
#[derive(Debug, Default)]
pub struct CompiledFilter {
    /// Boolean SQL expression (`true` when the query matches everything).
    pub where_sql: String,
    pub params: Vec<Value>,
    /// Temp tables of FTS candidate record ids: (table name, ids).
    pub temp_tables: Vec<(String, Vec<String>)>,
    pub text_modes: Vec<TextExecMode>,
}

impl CompiledFilter {
    pub fn used_fallback_scan(&self) -> bool {
        self.text_modes.contains(&TextExecMode::RegexScan)
    }
    pub fn used_fts(&self) -> bool {
        self.text_modes.contains(&TextExecMode::FtsIndex)
    }
}

/// FTS availability for the current dataset selection.
pub struct FtsContext<'a> {
    /// Present only when the index is ready (current version, all selected
    /// datasets indexed). `None` forces the regex path.
    pub index: Option<&'a FtsIndex>,
    pub dataset_ids: &'a [String],
}

/// Splits text into match tokens using the same definition as the FTS
/// tokenizer (`unicode61 remove_diacritics 0`): maximal runs of Unicode
/// alphanumeric characters.
pub fn text_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() {
            cur.push(c);
        } else if !cur.is_empty() {
            tokens.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

/// Escapes one token for literal use inside an RE2 pattern. Letters,
/// digits and non-ASCII pass through (never escape letters: `\d` etc. are
/// metacharacters); ASCII punctuation cannot appear in tokens.
fn regex_escape_token(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    for c in token.chars() {
        if c.is_ascii_alphanumeric() || !c.is_ascii() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

/// RE2 pattern implementing the documented token-match semantics: the
/// tokens appear consecutively, case-insensitively, separated by
/// non-alphanumeric runs, with token boundaries on both sides.
pub fn tokens_to_regex(tokens: &[String]) -> String {
    let sep = "[^\\p{L}\\p{N}]+";
    let boundary_start = "(^|[^\\p{L}\\p{N}])";
    let boundary_end = "([^\\p{L}\\p{N}]|$)";
    let body = tokens
        .iter()
        .map(|t| regex_escape_token(t))
        .collect::<Vec<_>>()
        .join(sep);
    format!("(?i){boundary_start}{body}{boundary_end}")
}

/// FTS5 MATCH expression for the same semantics: one quoted phrase (single
/// tokens are one-token phrases). Internal quotes cannot survive
/// tokenization, but double them defensively.
pub fn tokens_to_fts_expr(tokens: &[String]) -> String {
    let joined = tokens.join(" ").replace('"', "\"\"");
    format!("\"{joined}\"")
}

struct Compiler<'a> {
    sql: String,
    params: Vec<Value>,
    temp_tables: Vec<(String, Vec<String>)>,
    text_modes: Vec<TextExecMode>,
    fts: &'a FtsContext<'a>,
}

pub fn compile_filter(
    expr: Option<&ResolvedExpr>,
    fts: &FtsContext<'_>,
) -> Result<CompiledFilter, QueryError> {
    let mut c = Compiler {
        sql: String::with_capacity(256),
        params: Vec::new(),
        temp_tables: Vec::new(),
        text_modes: Vec::new(),
        fts,
    };
    match expr {
        None => c.sql.push_str("true"),
        Some(e) => c.emit(e)?,
    }
    Ok(CompiledFilter {
        where_sql: c.sql,
        params: c.params,
        temp_tables: c.temp_tables,
        text_modes: c.text_modes,
    })
}

/// Canonical column map — the only place canonical fields become SQL
/// identifiers.
fn canon_column(field: CanonicalField) -> &'static str {
    use CanonicalField::*;
    match field {
        Timestamp => "event_time",
        ObservedTimestamp => "observed_time",
        Severity => unreachable!("severity compiles through band logic"),
        SeverityText => "severity_text",
        SeverityNumber => "severity_number",
        Message => "display_message",
        EventName => "event_name",
        TraceId => "trace_id",
        SpanId => "span_id",
        Operation => "operation",
        Outcome => "outcome",
        EventType => "event_type",
        RequestId => "request_id",
        TransactionId => "transaction_id",
        MessageId => "message_id",
        EntityId => "entity_id",
        Dataset => "dataset_id",
        RecordId => "record_id",
    }
}

fn cmp_sql(op: CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "=",
        CmpOp::Ne => unreachable!("Ne compiles as NOT(Eq)"),
        CmpOp::Gt => ">",
        CmpOp::Ge => ">=",
        CmpOp::Lt => "<",
        CmpOp::Le => "<=",
    }
}

fn scalar_value(v: &TypedScalar) -> Value {
    match v {
        TypedScalar::Str(s) => Value::Text(s.clone()),
        TypedScalar::Int(i) => Value::BigInt(*i),
        TypedScalar::Double(d) => Value::Double(d.0),
        TypedScalar::Bool(b) => Value::Boolean(*b),
    }
}

/// JSON path to a nested attribute's tagged value (`$."k1".v."k2".v`).
/// The path travels as a bound parameter, never as SQL text.
pub fn attr_value_path(path: &[String]) -> String {
    attr_path_with_leaf(path, "v")
}

/// JSON path to a nested attribute's type tag (`…"k2".t`).
pub fn attr_tag_path(path: &[String]) -> String {
    attr_path_with_leaf(path, "t")
}

fn attr_path_with_leaf(path: &[String], leaf: &str) -> String {
    let mut out = String::from("$");
    for (i, seg) in path.iter().enumerate() {
        out.push_str(".\"");
        for c in seg.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                other => out.push(other),
            }
        }
        out.push('"');
        // Nested keys live inside the parent's map value; the leaf selects
        // the tagged value (`v`) or the type tag (`t`).
        let is_last = i + 1 == path.len();
        out.push('.');
        out.push_str(if is_last { leaf } else { "v" });
    }
    out
}

impl Compiler<'_> {
    fn emit(&mut self, e: &ResolvedExpr) -> Result<(), QueryError> {
        match e {
            ResolvedExpr::And { items } => {
                self.sql.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.sql.push_str(" AND ");
                    }
                    self.emit(item)?;
                }
                self.sql.push(')');
            }
            ResolvedExpr::Or { items } => {
                self.sql.push('(');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.sql.push_str(" OR ");
                    }
                    self.emit(item)?;
                }
                self.sql.push(')');
            }
            ResolvedExpr::Not { expr } => {
                self.sql.push_str("(NOT ");
                self.emit(expr)?;
                self.sql.push(')');
            }
            ResolvedExpr::Term { term } => self.emit_text(&text_tokens(term))?,
            ResolvedExpr::Phrase { phrase } => self.emit_text(&text_tokens(phrase))?,
            ResolvedExpr::Severity { op, level } => self.emit_severity(*op, *level),
            ResolvedExpr::Canon { field, op, value } => {
                if *op == CmpOp::Ne {
                    self.sql.push_str("(NOT ");
                    self.emit_canon_cmp(*field, CmpOp::Eq, value);
                    self.sql.push(')');
                } else {
                    self.emit_canon_cmp(*field, *op, value);
                }
            }
            ResolvedExpr::CanonExists { field } => {
                self.sql.push('(');
                self.sql.push_str(canon_column(*field));
                self.sql.push_str(" IS NOT NULL)");
            }
            ResolvedExpr::CanonWildcard { field, regex, .. } => {
                self.emit_regex_on(canon_column(*field), regex, false);
            }
            ResolvedExpr::CanonRegex {
                field,
                pattern,
                case_insensitive,
            } => {
                self.emit_regex_on(canon_column(*field), pattern, *case_insensitive);
            }
            ResolvedExpr::Attr {
                path,
                ty,
                op,
                value,
            } => {
                if *op == CmpOp::Ne {
                    self.sql.push_str("(NOT ");
                    self.emit_attr_cmp(path, *ty, CmpOp::Eq, value);
                    self.sql.push(')');
                } else {
                    self.emit_attr_cmp(path, *ty, *op, value);
                }
            }
            ResolvedExpr::AttrExists { path } => {
                // Present and not the OTLP empty value.
                self.sql.push_str(
                    "COALESCE(json_extract_string(attributes_json, ?) <> 'empty', false)",
                );
                self.params.push(Value::Text(attr_tag_path(path)));
            }
            ResolvedExpr::AttrWildcard { path, regex, .. } => {
                self.emit_attr_regex(path, regex, false);
            }
            ResolvedExpr::AttrRegex {
                path,
                pattern,
                case_insensitive,
            } => {
                self.emit_attr_regex(path, pattern, *case_insensitive);
            }
        }
        Ok(())
    }

    fn emit_text(&mut self, tokens: &[String]) -> Result<(), QueryError> {
        if tokens.is_empty() {
            // A term with no tokens (pure punctuation) matches nothing —
            // exactly what an FTS query for it would return.
            self.sql.push_str("false");
            return Ok(());
        }
        if let Some(index) = self.fts.index {
            let match_expr = tokens_to_fts_expr(tokens);
            let hits =
                index.count_logs_expr(self.fts.dataset_ids, &match_expr, MAX_FTS_CANDIDATES)?;
            if hits <= MAX_FTS_CANDIDATES {
                let ids: Vec<String> = index
                    .search_logs_expr(self.fts.dataset_ids, &match_expr, MAX_FTS_CANDIDATES + 1)?
                    .into_iter()
                    .map(|h| h.record_id)
                    .collect();
                let table = format!("qtext_{}", self.temp_tables.len());
                self.sql
                    .push_str(&format!("(record_id IN (SELECT record_id FROM {table}))"));
                self.temp_tables.push((table, ids));
                self.text_modes.push(TextExecMode::FtsIndex);
                return Ok(());
            }
        }
        // Exact fallback: RE2 token-boundary predicate.
        let pattern = tokens_to_regex(tokens);
        self.sql
            .push_str("COALESCE(regexp_matches(display_message, ?), false)");
        self.params.push(Value::Text(pattern));
        self.text_modes.push(TextExecMode::RegexScan);
        Ok(())
    }

    fn emit_severity(&mut self, op: CmpOp, level: SeverityLevel) {
        let (lo, hi) = level.band();
        match op {
            CmpOp::Eq | CmpOp::Ne => {
                let eq = "(COALESCE(severity_number BETWEEN ? AND ?, false) \
                     OR (severity_number IS NULL AND COALESCE(upper(severity_text) = ?, false)))"
                    .to_string();
                if op == CmpOp::Ne {
                    self.sql.push_str("(NOT ");
                    self.sql.push_str(&eq);
                    self.sql.push(')');
                } else {
                    self.sql.push_str(&eq);
                }
                self.params.push(Value::Int(lo));
                self.params.push(Value::Int(hi));
                self.params.push(Value::Text(level.name().to_string()));
            }
            CmpOp::Ge => {
                self.sql.push_str("COALESCE(severity_number >= ?, false)");
                self.params.push(Value::Int(lo));
            }
            CmpOp::Gt => {
                self.sql.push_str("COALESCE(severity_number > ?, false)");
                self.params.push(Value::Int(hi));
            }
            CmpOp::Le => {
                self.sql.push_str("COALESCE(severity_number <= ?, false)");
                self.params.push(Value::Int(hi));
            }
            CmpOp::Lt => {
                self.sql.push_str("COALESCE(severity_number < ?, false)");
                self.params.push(Value::Int(lo));
            }
        }
    }

    fn emit_canon_cmp(&mut self, field: CanonicalField, op: CmpOp, value: &TypedScalar) {
        let col = canon_column(field);
        let sql_op = cmp_sql(op);
        // Severity text equality is case-insensitive by language definition.
        if field == CanonicalField::SeverityText && op == CmpOp::Eq {
            self.sql
                .push_str("COALESCE(upper(severity_text) = upper(?), false)");
        } else {
            self.sql
                .push_str(&format!("COALESCE({col} {sql_op} ?, false)"));
        }
        self.params.push(scalar_value(value));
    }

    fn emit_attr_cmp(
        &mut self,
        path: &[String],
        ty: logscope_query_lang::AttrScalar,
        op: CmpOp,
        value: &TypedScalar,
    ) {
        use logscope_query_lang::AttrScalar;
        let sql_op = cmp_sql(op);
        let extract = "json_extract_string(attributes_json, ?)";
        let lhs = match ty {
            AttrScalar::Str => extract.to_string(),
            AttrScalar::Int => format!("TRY_CAST({extract} AS BIGINT)"),
            AttrScalar::Double => format!("TRY_CAST({extract} AS DOUBLE)"),
            AttrScalar::Bool => extract.to_string(),
        };
        self.sql
            .push_str(&format!("COALESCE({lhs} {sql_op} ?, false)"));
        self.params.push(Value::Text(attr_value_path(path)));
        match (ty, value) {
            (AttrScalar::Bool, TypedScalar::Bool(b)) => {
                // Tagged JSON stores booleans as JSON true/false; the string
                // extraction yields 'true'/'false'.
                self.params
                    .push(Value::Text(if *b { "true" } else { "false" }.into()));
            }
            _ => self.params.push(scalar_value(value)),
        }
    }

    fn emit_attr_regex(&mut self, path: &[String], pattern: &str, ci: bool) {
        self.sql.push_str(
            "COALESCE(regexp_matches(json_extract_string(attributes_json, ?), ?), false)",
        );
        self.params.push(Value::Text(attr_value_path(path)));
        self.params.push(Value::Text(apply_ci(pattern, ci)));
    }

    fn emit_regex_on(&mut self, col: &str, pattern: &str, ci: bool) {
        self.sql
            .push_str(&format!("COALESCE(regexp_matches({col}, ?), false)"));
        self.params.push(Value::Text(apply_ci(pattern, ci)));
    }
}

fn apply_ci(pattern: &str, ci: bool) -> String {
    if ci {
        format!("(?i){pattern}")
    } else {
        pattern.to_string()
    }
}

/// Creates and fills the compiled filter's temp tables on `conn`, returning
/// a guard that drops them (best effort) when the execution scope ends.
pub fn install_temp_tables<'a>(
    conn: &'a duckdb::Connection,
    filter: &CompiledFilter,
) -> Result<TempTableGuard<'a>, QueryError> {
    let mut installed = Vec::new();
    for (name, ids) in &filter.temp_tables {
        debug_assert!(name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP TABLE {name} (record_id VARCHAR)"
        ))?;
        let mut app = conn.appender(name)?;
        for id in ids {
            app.append_row(duckdb::params![id])?;
        }
        app.flush()?;
        installed.push(name.clone());
    }
    Ok(TempTableGuard { conn, installed })
}

pub struct TempTableGuard<'a> {
    conn: &'a duckdb::Connection,
    installed: Vec<String>,
}

impl Drop for TempTableGuard<'_> {
    fn drop(&mut self) {
        for name in &self.installed {
            let _ = self
                .conn
                .execute_batch(&format!("DROP TABLE IF EXISTS {name}"));
        }
    }
}
