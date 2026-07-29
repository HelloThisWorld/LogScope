//! Lexer: query text → spanned tokens.
//!
//! Bare words may contain backslash escapes (`\*`, `\?`, `\\`, `\:`, …);
//! an unescaped `*`/`?` inside a word marks it as a wildcard candidate.
//! `AND`/`OR`/`NOT` are keywords only as exact uppercase unescaped words.
//! A `/` directly after `:` or `=` starts a regex literal `/pattern/i?`.

use crate::diag::{Diagnostic, Span};
use crate::limits::LangLimits;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Word {
        text: String,
        wildcard: bool,
    },
    Quoted {
        text: String,
    },
    Regex {
        pattern: String,
        case_insensitive: bool,
    },
    LParen,
    RParen,
    Colon,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    KwAnd,
    KwOr,
    KwNot,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl TokenKind {
    pub fn is_cmp_op(&self) -> bool {
        matches!(
            self,
            TokenKind::Colon
                | TokenKind::Eq
                | TokenKind::Ne
                | TokenKind::Gt
                | TokenKind::Ge
                | TokenKind::Lt
                | TokenKind::Le
        )
    }
    /// Human name used in "expected …" diagnostics.
    pub fn describe(&self) -> String {
        match self {
            TokenKind::Word { text, .. } => format!("word `{text}`"),
            TokenKind::Quoted { .. } => "quoted string".into(),
            TokenKind::Regex { .. } => "regex".into(),
            TokenKind::LParen => "`(`".into(),
            TokenKind::RParen => "`)`".into(),
            TokenKind::Colon => "`:`".into(),
            TokenKind::Eq => "`=`".into(),
            TokenKind::Ne => "`!=`".into(),
            TokenKind::Gt => "`>`".into(),
            TokenKind::Ge => "`>=`".into(),
            TokenKind::Lt => "`<`".into(),
            TokenKind::Le => "`<=`".into(),
            TokenKind::KwAnd => "`AND`".into(),
            TokenKind::KwOr => "`OR`".into(),
            TokenKind::KwNot => "`NOT`".into(),
        }
    }
}

struct Cursor<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    byte: u32,
    utf16: u32,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Cursor {
            chars: text.chars().peekable(),
            byte: 0,
            utf16: 0,
        }
    }
    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.chars.next()?;
        self.byte += c.len_utf8() as u32;
        self.utf16 += c.len_utf16() as u32;
        Some(c)
    }
    fn pos(&self) -> (u32, u32) {
        (self.byte, self.utf16)
    }
    fn span_from(&self, start: (u32, u32)) -> Span {
        Span {
            start: start.0,
            end: self.byte,
            start_utf16: start.1,
            end_utf16: self.utf16,
        }
    }
}

fn is_word_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '(' | ')' | '"' | ':' | '=' | '<' | '>' | '!')
}

/// Lexes `text`. Always returns the tokens it could form; hard problems are
/// reported as error diagnostics (the parser refuses to run on lex errors).
pub fn lex(text: &str, limits: &LangLimits) -> (Vec<Token>, Vec<Diagnostic>) {
    let mut tokens = Vec::new();
    let mut diags = Vec::new();

    if text.len() > limits.max_text_bytes {
        let end = Span {
            start: 0,
            end: text.len() as u32,
            start_utf16: 0,
            end_utf16: text.encode_utf16().count() as u32,
        };
        diags.push(
            Diagnostic::error(
                "lang/query-too-long",
                format!(
                    "query is {} bytes; the maximum is {} bytes",
                    text.len(),
                    limits.max_text_bytes
                ),
                end,
            )
            .with_hint("shorten the query or split it into saved searches"),
        );
        return (tokens, diags);
    }

    let mut cur = Cursor::new(text);
    // Whether the previous token allows a regex literal to start here.
    let mut regex_position = false;

    while let Some(c) = cur.peek() {
        if c.is_whitespace() {
            cur.bump();
            continue;
        }
        let start = cur.pos();
        let prev_regex_position = regex_position;
        regex_position = false;

        match c {
            '(' => {
                cur.bump();
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    span: cur.span_from(start),
                });
            }
            ')' => {
                cur.bump();
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    span: cur.span_from(start),
                });
            }
            ':' => {
                cur.bump();
                regex_position = true;
                tokens.push(Token {
                    kind: TokenKind::Colon,
                    span: cur.span_from(start),
                });
            }
            '=' => {
                cur.bump();
                regex_position = true;
                tokens.push(Token {
                    kind: TokenKind::Eq,
                    span: cur.span_from(start),
                });
            }
            '!' => {
                cur.bump();
                if cur.peek() == Some('=') {
                    cur.bump();
                    tokens.push(Token {
                        kind: TokenKind::Ne,
                        span: cur.span_from(start),
                    });
                } else {
                    diags.push(
                        Diagnostic::error(
                            "lang/unexpected-char",
                            "`!` is only valid as part of `!=`",
                            cur.span_from(start),
                        )
                        .with_hint("use `NOT` to negate an expression"),
                    );
                }
            }
            '>' => {
                cur.bump();
                let kind = if cur.peek() == Some('=') {
                    cur.bump();
                    TokenKind::Ge
                } else {
                    TokenKind::Gt
                };
                tokens.push(Token {
                    kind,
                    span: cur.span_from(start),
                });
            }
            '<' => {
                cur.bump();
                let kind = if cur.peek() == Some('=') {
                    cur.bump();
                    TokenKind::Le
                } else {
                    TokenKind::Lt
                };
                tokens.push(Token {
                    kind,
                    span: cur.span_from(start),
                });
            }
            '"' => {
                cur.bump();
                let mut out = String::new();
                let mut terminated = false;
                while let Some(ch) = cur.bump() {
                    match ch {
                        '"' => {
                            terminated = true;
                            break;
                        }
                        '\\' => match cur.bump() {
                            Some('"') => out.push('"'),
                            Some('\\') => out.push('\\'),
                            Some('n') => out.push('\n'),
                            Some('t') => out.push('\t'),
                            Some('r') => out.push('\r'),
                            Some(other) => {
                                diags.push(
                                    Diagnostic::error(
                                        "lang/invalid-escape",
                                        format!("unsupported escape `\\{other}` in string"),
                                        cur.span_from(start),
                                    )
                                    .with_expected("one of \\\" \\\\ \\n \\t \\r"),
                                );
                                out.push(other);
                            }
                            None => break,
                        },
                        other => out.push(other),
                    }
                }
                if !terminated {
                    diags.push(
                        Diagnostic::error(
                            "lang/unterminated-string",
                            "string is missing its closing `\"`",
                            cur.span_from(start),
                        )
                        .with_hint("add a closing double quote"),
                    );
                }
                tokens.push(Token {
                    kind: TokenKind::Quoted { text: out },
                    span: cur.span_from(start),
                });
            }
            '/' if prev_regex_position => {
                cur.bump();
                let mut pattern = String::new();
                let mut terminated = false;
                while let Some(ch) = cur.bump() {
                    match ch {
                        '/' => {
                            terminated = true;
                            break;
                        }
                        '\\' => {
                            if cur.peek() == Some('/') {
                                cur.bump();
                                pattern.push('/');
                            } else {
                                pattern.push('\\');
                            }
                        }
                        other => pattern.push(other),
                    }
                }
                let mut ci = false;
                if terminated {
                    while let Some(flag) = cur.peek() {
                        if !flag.is_ascii_alphabetic() {
                            break;
                        }
                        cur.bump();
                        if flag == 'i' {
                            ci = true;
                        } else {
                            diags.push(
                                Diagnostic::error(
                                    "lang/unsupported-regex-flag",
                                    format!("unsupported regex flag `{flag}`"),
                                    cur.span_from(start),
                                )
                                .with_expected("only the `i` (case-insensitive) flag"),
                            );
                        }
                    }
                } else {
                    diags.push(
                        Diagnostic::error(
                            "lang/unterminated-regex",
                            "regex is missing its closing `/`",
                            cur.span_from(start),
                        )
                        .with_hint("close the pattern with `/`, e.g. message:/timeout/"),
                    );
                }
                if pattern.chars().count() > limits.max_regex_chars {
                    diags.push(Diagnostic::error(
                        "lang/regex-too-long",
                        format!(
                            "regex pattern exceeds {} characters",
                            limits.max_regex_chars
                        ),
                        cur.span_from(start),
                    ));
                }
                tokens.push(Token {
                    kind: TokenKind::Regex {
                        pattern,
                        case_insensitive: ci,
                    },
                    span: cur.span_from(start),
                });
            }
            _ => {
                // Bare word (may contain escapes and wildcard characters).
                let mut out = String::new();
                let mut wildcard = false;
                let mut had_escape = false;
                while let Some(ch) = cur.peek() {
                    if !is_word_char(ch) {
                        break;
                    }
                    cur.bump();
                    if ch == '\\' {
                        had_escape = true;
                        match cur.bump() {
                            // Wildcard-relevant escapes keep their backslash
                            // inside the token text so `\*` (literal star)
                            // stays distinguishable from `*` (wildcard);
                            // `unescape_word` removes them for exact values.
                            Some(esc @ ('*' | '?' | '\\')) => {
                                out.push('\\');
                                out.push(esc);
                            }
                            Some(esc) => out.push(esc),
                            None => {
                                diags.push(Diagnostic::error(
                                    "lang/trailing-backslash",
                                    "query ends with a lone `\\`",
                                    cur.span_from(start),
                                ));
                            }
                        }
                    } else {
                        if ch == '*' || ch == '?' {
                            wildcard = true;
                        }
                        out.push(ch);
                    }
                }
                let kind = match out.as_str() {
                    "AND" if !had_escape => TokenKind::KwAnd,
                    "OR" if !had_escape => TokenKind::KwOr,
                    "NOT" if !had_escape => TokenKind::KwNot,
                    _ => TokenKind::Word {
                        text: out,
                        wildcard,
                    },
                };
                tokens.push(Token {
                    kind,
                    span: cur.span_from(start),
                });
            }
        }
    }

    if tokens.len() > limits.max_tokens {
        let span = tokens.last().map(|t| t.span).unwrap_or_default();
        diags.push(Diagnostic::error(
            "lang/too-many-tokens",
            format!(
                "query has {} tokens; the maximum is {}",
                tokens.len(),
                limits.max_tokens
            ),
            span,
        ));
    }

    (tokens, diags)
}

/// Removes wildcard-preserving escapes from a word token's text, producing
/// the exact literal value (`\*` → `*`, `\?` → `?`, `\\` → `\`).
pub fn unescape_word(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Highlight classes for the editor (purely lexical; never semantic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HighlightKind {
    Field,
    Term,
    Keyword,
    String,
    Regex,
    Operator,
    Paren,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HighlightSpan {
    pub kind: HighlightKind,
    pub span: Span,
}

/// Computes editor highlight spans from the token stream: a word directly
/// followed by a comparison operator is a field, everything else keeps its
/// lexical class.
pub fn highlight(tokens: &[Token]) -> Vec<HighlightSpan> {
    let mut out = Vec::with_capacity(tokens.len());
    for (i, t) in tokens.iter().enumerate() {
        let kind = match &t.kind {
            TokenKind::Word { .. } => {
                if tokens.get(i + 1).is_some_and(|n| n.kind.is_cmp_op()) {
                    HighlightKind::Field
                } else {
                    HighlightKind::Term
                }
            }
            TokenKind::Quoted { .. } => HighlightKind::String,
            TokenKind::Regex { .. } => HighlightKind::Regex,
            TokenKind::KwAnd | TokenKind::KwOr | TokenKind::KwNot => HighlightKind::Keyword,
            TokenKind::LParen | TokenKind::RParen => HighlightKind::Paren,
            _ => HighlightKind::Operator,
        };
        out.push(HighlightSpan { kind, span: t.span });
    }
    out
}
