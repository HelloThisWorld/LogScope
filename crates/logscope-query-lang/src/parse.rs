//! Recursive-descent parser: tokens → raw AST.
//!
//! Precedence: parentheses, then `NOT`, then `AND` (explicit or adjacency),
//! then `OR`. The first error aborts the parse with a spanned diagnostic.

use crate::ast::{CmpOp, RawExpr, RawField, RawValue, RawValueExpr};
use crate::diag::{Diagnostic, Span};
use crate::lex::{Token, TokenKind};
use crate::limits::LangLimits;

pub struct ParseOutcome {
    /// `None` for an empty query (matches everything) or on error.
    pub ast: Option<RawExpr>,
    pub diagnostics: Vec<Diagnostic>,
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    depth: usize,
    limits: &'a LangLimits,
    end_span: Span,
}

type PResult<T> = Result<T, Diagnostic>;

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }
    fn peek2(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos + 1)
    }
    fn bump(&mut self) -> Option<&'a Token> {
        let t = self.tokens.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn here(&self) -> Span {
        self.peek().map(|t| t.span).unwrap_or(self.end_span)
    }

    fn enter(&mut self, span: Span) -> PResult<()> {
        self.depth += 1;
        if self.depth > self.limits.max_depth {
            return Err(Diagnostic::error(
                "lang/too-deep",
                format!("query nesting exceeds {} levels", self.limits.max_depth),
                span,
            ));
        }
        Ok(())
    }
    fn leave(&mut self) {
        self.depth -= 1;
    }

    // query := or_expr EOF
    fn parse_query(&mut self) -> PResult<RawExpr> {
        let expr = self.parse_or()?;
        if let Some(t) = self.peek() {
            return Err(Diagnostic::error(
                "lang/unexpected-token",
                format!("unexpected {}", t.kind.describe()),
                t.span,
            )
            .with_expected("AND, OR, or end of query"));
        }
        Ok(expr)
    }

    fn parse_or(&mut self) -> PResult<RawExpr> {
        let first = self.parse_and()?;
        let mut items = vec![first];
        while matches!(self.peek().map(|t| &t.kind), Some(TokenKind::KwOr)) {
            self.bump();
            items.push(self.parse_and()?);
        }
        if items.len() == 1 {
            Ok(items.pop().expect("one item"))
        } else {
            let span = Span::merge(items[0].span(), items[items.len() - 1].span());
            Ok(RawExpr::Or(items, span))
        }
    }

    fn parse_and(&mut self) -> PResult<RawExpr> {
        let first = self.parse_unary()?;
        let mut items = vec![first];
        loop {
            match self.peek().map(|t| &t.kind) {
                Some(TokenKind::KwAnd) => {
                    self.bump();
                    items.push(self.parse_unary()?);
                }
                // Implicit AND: the next token can start a new unary.
                Some(
                    TokenKind::Word { .. }
                    | TokenKind::Quoted { .. }
                    | TokenKind::LParen
                    | TokenKind::KwNot,
                ) => {
                    items.push(self.parse_unary()?);
                }
                _ => break,
            }
        }
        if items.len() == 1 {
            Ok(items.pop().expect("one item"))
        } else {
            let span = Span::merge(items[0].span(), items[items.len() - 1].span());
            Ok(RawExpr::And(items, span))
        }
    }

    fn parse_unary(&mut self) -> PResult<RawExpr> {
        match self.peek().map(|t| (&t.kind, t.span)) {
            Some((TokenKind::KwNot, span)) => {
                self.bump();
                self.enter(span)?;
                let inner = self.parse_unary()?;
                self.leave();
                let full = Span::merge(span, inner.span());
                Ok(RawExpr::Not(Box::new(inner), full))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> PResult<RawExpr> {
        let Some(t) = self.peek() else {
            return Err(Diagnostic::error(
                "lang/unexpected-end",
                "expected an expression",
                self.end_span,
            )
            .with_expected("a term, phrase, field predicate, or `(`"));
        };
        match &t.kind {
            TokenKind::LParen => {
                let open = t.span;
                self.bump();
                self.enter(open)?;
                let inner = self.parse_or()?;
                self.leave();
                match self.peek().map(|t| &t.kind) {
                    Some(TokenKind::RParen) => {
                        self.bump();
                        Ok(inner)
                    }
                    _ => Err(Diagnostic::error(
                        "lang/unbalanced-paren",
                        "missing closing `)`",
                        self.here(),
                    )
                    .with_expected("`)`")),
                }
            }
            TokenKind::Quoted { text } => {
                let span = t.span;
                let text = text.clone();
                self.bump();
                Ok(RawExpr::Phrase { text, span })
            }
            TokenKind::Word { text, wildcard } => {
                // Field predicate when directly followed by a comparison op.
                if self.peek2().is_some_and(|n| n.kind.is_cmp_op()) {
                    return self.parse_predicate();
                }
                let (text, wildcard, span) = (text.clone(), *wildcard, t.span);
                self.bump();
                if text.is_empty() {
                    return Err(Diagnostic::error("lang/empty-term", "empty term", span));
                }
                Ok(RawExpr::Term {
                    text,
                    wildcard,
                    span,
                })
            }
            TokenKind::Regex { .. } => Err(Diagnostic::error(
                "lang/regex-needs-field",
                "a regex must be attached to a field, e.g. message:/timeout/",
                t.span,
            )
            .with_hint("write `message:/pattern/` to search the message text")),
            other => Err(Diagnostic::error(
                "lang/unexpected-token",
                format!("unexpected {}", other.describe()),
                t.span,
            )
            .with_expected("a term, phrase, field predicate, or `(`")),
        }
    }

    fn parse_predicate(&mut self) -> PResult<RawExpr> {
        let field_tok = self.bump().expect("word token");
        let TokenKind::Word { text, .. } = &field_tok.kind else {
            unreachable!("caller checked word");
        };
        let field = RawField {
            text: text.clone(),
            span: field_tok.span,
        };
        let op_tok = self.bump().expect("cmp op token");
        let op = match op_tok.kind {
            TokenKind::Colon | TokenKind::Eq => CmpOp::Eq,
            TokenKind::Ne => CmpOp::Ne,
            TokenKind::Gt => CmpOp::Gt,
            TokenKind::Ge => CmpOp::Ge,
            TokenKind::Lt => CmpOp::Lt,
            TokenKind::Le => CmpOp::Le,
            _ => unreachable!("caller checked cmp op"),
        };
        let colon = matches!(op_tok.kind, TokenKind::Colon);

        // Field group: `field:( … )` (only with `:` or `=`).
        if matches!(self.peek().map(|t| &t.kind), Some(TokenKind::LParen)) {
            if op != CmpOp::Eq {
                return Err(Diagnostic::error(
                    "lang/group-op",
                    format!("value groups are not supported with `{}`", op.describe()),
                    op_tok.span,
                )
                .with_hint("write NOT field:(…) instead"));
            }
            let open = self.bump().expect("lparen").span;
            self.enter(open)?;
            let expr = self.parse_value_or(&field)?;
            self.leave();
            match self.peek().map(|t| &t.kind) {
                Some(TokenKind::RParen) => {
                    let close = self.bump().expect("rparen").span;
                    let span = Span::merge(field.span, close);
                    return Ok(RawExpr::Group { field, expr, span });
                }
                _ => {
                    return Err(Diagnostic::error(
                        "lang/unbalanced-paren",
                        "missing closing `)` in field group",
                        self.here(),
                    )
                    .with_expected("`)`"))
                }
            }
        }

        let value = self.parse_value(&field, op, colon)?;
        let span = Span::merge(field.span, value.span());
        Ok(RawExpr::Cmp {
            field,
            op,
            value,
            span,
        })
    }

    fn parse_value(&mut self, field: &RawField, op: CmpOp, _colon: bool) -> PResult<RawValue> {
        let Some(t) = self.peek() else {
            return Err(Diagnostic::error(
                "lang/missing-value",
                format!("`{}` needs a value", field.text),
                self.end_span,
            )
            .with_expected("a value, quoted string, `*`, or /regex/"));
        };
        match &t.kind {
            TokenKind::Word { text, wildcard } => {
                let (text, wildcard, span) = (text.clone(), *wildcard, t.span);
                self.bump();
                if text == "*" {
                    if !matches!(op, CmpOp::Eq) {
                        return Err(Diagnostic::error(
                            "lang/exists-op",
                            format!(
                                "existence `*` only combines with `:`, not `{}`",
                                op.describe()
                            ),
                            span,
                        )
                        .with_hint("use `field:*` (exists) or `NOT field:*` (missing)"));
                    }
                    return Ok(RawValue::Star { span });
                }
                if text.is_empty() {
                    return Err(Diagnostic::error(
                        "lang/missing-value",
                        format!("`{}` needs a value", field.text),
                        span,
                    ));
                }
                Ok(RawValue::Word {
                    text,
                    wildcard,
                    span,
                })
            }
            TokenKind::Quoted { text } => {
                let (text, span) = (text.clone(), t.span);
                self.bump();
                Ok(RawValue::Quoted { text, span })
            }
            TokenKind::Regex {
                pattern,
                case_insensitive,
            } => {
                let (pattern, ci, span) = (pattern.clone(), *case_insensitive, t.span);
                self.bump();
                if !matches!(op, CmpOp::Eq) {
                    return Err(Diagnostic::error(
                        "lang/regex-op",
                        format!(
                            "regex only combines with `:` or `=`, not `{}`",
                            op.describe()
                        ),
                        span,
                    )
                    .with_hint("use NOT field:/pattern/ to negate a regex match"));
                }
                Ok(RawValue::Regex {
                    pattern,
                    case_insensitive: ci,
                    span,
                })
            }
            other => Err(Diagnostic::error(
                "lang/missing-value",
                format!("`{}` needs a value, found {}", field.text, other.describe()),
                t.span,
            )
            .with_expected("a value, quoted string, `*`, or /regex/")),
        }
    }

    // Value-group boolean grammar: vor := vand (OR vand)* …
    fn parse_value_or(&mut self, field: &RawField) -> PResult<RawValueExpr> {
        let first = self.parse_value_and(field)?;
        let mut items = vec![first];
        while matches!(self.peek().map(|t| &t.kind), Some(TokenKind::KwOr)) {
            self.bump();
            items.push(self.parse_value_and(field)?);
        }
        if items.len() == 1 {
            Ok(items.pop().expect("one item"))
        } else {
            let span = value_expr_list_span(&items);
            Ok(RawValueExpr::Or(items, span))
        }
    }

    fn parse_value_and(&mut self, field: &RawField) -> PResult<RawValueExpr> {
        let first = self.parse_value_unary(field)?;
        let mut items = vec![first];
        loop {
            match self.peek().map(|t| &t.kind) {
                Some(TokenKind::KwAnd) => {
                    self.bump();
                    items.push(self.parse_value_unary(field)?);
                }
                Some(
                    TokenKind::Word { .. }
                    | TokenKind::Quoted { .. }
                    | TokenKind::Regex { .. }
                    | TokenKind::LParen
                    | TokenKind::KwNot,
                ) => {
                    items.push(self.parse_value_unary(field)?);
                }
                _ => break,
            }
        }
        if items.len() == 1 {
            Ok(items.pop().expect("one item"))
        } else {
            let span = value_expr_list_span(&items);
            Ok(RawValueExpr::And(items, span))
        }
    }

    fn parse_value_unary(&mut self, field: &RawField) -> PResult<RawValueExpr> {
        match self.peek().map(|t| (&t.kind, t.span)) {
            Some((TokenKind::KwNot, span)) => {
                self.bump();
                self.enter(span)?;
                let inner = self.parse_value_unary(field)?;
                self.leave();
                let full = Span::merge(span, value_expr_span(&inner));
                Ok(RawValueExpr::Not(Box::new(inner), full))
            }
            Some((TokenKind::LParen, span)) => {
                self.bump();
                self.enter(span)?;
                let inner = self.parse_value_or(field)?;
                self.leave();
                match self.peek().map(|t| &t.kind) {
                    Some(TokenKind::RParen) => {
                        self.bump();
                        Ok(inner)
                    }
                    _ => Err(Diagnostic::error(
                        "lang/unbalanced-paren",
                        "missing closing `)` in field group",
                        self.here(),
                    )),
                }
            }
            _ => {
                let value = self.parse_value(field, CmpOp::Eq, true)?;
                Ok(RawValueExpr::Value(value))
            }
        }
    }
}

fn value_expr_span(e: &RawValueExpr) -> Span {
    match e {
        RawValueExpr::Value(v) => v.span(),
        RawValueExpr::Not(_, s) | RawValueExpr::And(_, s) | RawValueExpr::Or(_, s) => *s,
    }
}

fn value_expr_list_span(items: &[RawValueExpr]) -> Span {
    Span::merge(
        value_expr_span(&items[0]),
        value_expr_span(&items[items.len() - 1]),
    )
}

/// Parses a token stream. Lex errors must be handled by the caller first.
pub fn parse(tokens: &[Token], text: &str, limits: &LangLimits) -> ParseOutcome {
    if tokens.is_empty() {
        return ParseOutcome {
            ast: None,
            diagnostics: vec![],
        };
    }
    let end_byte = text.len() as u32;
    let end16 = text.encode_utf16().count() as u32;
    let end_span = Span {
        start: end_byte,
        end: end_byte,
        start_utf16: end16,
        end_utf16: end16,
    };
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
        limits,
        end_span,
    };
    match parser.parse_query() {
        Ok(ast) => ParseOutcome {
            ast: Some(ast),
            diagnostics: vec![],
        },
        Err(diag) => ParseOutcome {
            ast: None,
            diagnostics: vec![diag],
        },
    }
}
