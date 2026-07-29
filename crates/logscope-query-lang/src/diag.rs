//! Source spans and structured diagnostics.

use serde::{Deserialize, Serialize};

/// Half-open source range in the original query text, in UTF-8 bytes and
/// UTF-16 code units (the latter for the JavaScript editor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub start_utf16: u32,
    pub end_utf16: u32,
}

impl Span {
    pub fn merge(a: Span, b: Span) -> Span {
        Span {
            start: a.start.min(b.start),
            end: a.end.max(b.end),
            start_utf16: a.start_utf16.min(b.start_utf16),
            end_utf16: a.end_utf16.max(b.end_utf16),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagSeverity {
    Error,
    Warning,
}

/// One structured diagnostic with a stable code, span, and safe hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Stable machine code, e.g. `lang/unknown-field`.
    pub code: String,
    pub severity: DiagSeverity,
    pub message: String,
    pub span: Span,
    /// What was expected here, when that is well-defined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// Safe remediation hint (never echoes compiled SQL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Diagnostic {
    pub fn error(code: &str, message: impl Into<String>, span: Span) -> Self {
        Diagnostic {
            code: code.to_string(),
            severity: DiagSeverity::Error,
            message: message.into(),
            span,
            expected: None,
            hint: None,
        }
    }
    pub fn warning(code: &str, message: impl Into<String>, span: Span) -> Self {
        Diagnostic {
            severity: DiagSeverity::Warning,
            ..Diagnostic::error(code, message, span)
        }
    }
    pub fn with_expected(mut self, expected: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self
    }
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
    pub fn is_error(&self) -> bool {
        self.severity == DiagSeverity::Error
    }
}
