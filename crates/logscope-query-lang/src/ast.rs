//! Raw (untyped) AST produced by the parser. Field and value meaning is
//! assigned later by `resolve` against the trusted field catalog.

use crate::diag::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum RawValue {
    /// Unquoted word; `wildcard` = contains an unescaped `*`/`?`.
    Word {
        text: String,
        wildcard: bool,
        span: Span,
    },
    Quoted {
        text: String,
        span: Span,
    },
    Regex {
        pattern: String,
        case_insensitive: bool,
        span: Span,
    },
    /// Bare `*`: existence test.
    Star {
        span: Span,
    },
}

impl RawValue {
    pub fn span(&self) -> Span {
        match self {
            RawValue::Word { span, .. }
            | RawValue::Quoted { span, .. }
            | RawValue::Regex { span, .. }
            | RawValue::Star { span } => *span,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

impl CmpOp {
    pub fn describe(self) -> &'static str {
        match self {
            CmpOp::Eq => "=",
            CmpOp::Ne => "!=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
        }
    }
}

/// Value expression inside a field group `f:(A OR B AND NOT C)`.
#[derive(Debug, Clone, PartialEq)]
pub enum RawValueExpr {
    Value(RawValue),
    Not(Box<RawValueExpr>, Span),
    And(Vec<RawValueExpr>, Span),
    Or(Vec<RawValueExpr>, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawField {
    /// Field path exactly as written (dotted).
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RawExpr {
    /// Bare word term (free text against the message).
    Term {
        text: String,
        wildcard: bool,
        span: Span,
    },
    /// Quoted phrase (free text against the message).
    Phrase {
        text: String,
        span: Span,
    },
    /// `field <op> value`.
    Cmp {
        field: RawField,
        op: CmpOp,
        value: RawValue,
        span: Span,
    },
    /// `field:(value expression)`.
    Group {
        field: RawField,
        expr: RawValueExpr,
        span: Span,
    },
    Not(Box<RawExpr>, Span),
    And(Vec<RawExpr>, Span),
    Or(Vec<RawExpr>, Span),
}

impl RawExpr {
    pub fn span(&self) -> Span {
        match self {
            RawExpr::Term { span, .. }
            | RawExpr::Phrase { span, .. }
            | RawExpr::Cmp { span, .. }
            | RawExpr::Group { span, .. }
            | RawExpr::Not(_, span)
            | RawExpr::And(_, span)
            | RawExpr::Or(_, span) => *span,
        }
    }
}
