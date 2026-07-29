//! Language-level resource limits (ADR-0011). Execution-time budgets
//! (deadline, scan bytes, rows) live in the query service, not here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangLimits {
    /// Maximum query text length in UTF-8 bytes.
    pub max_text_bytes: usize,
    pub max_tokens: usize,
    /// Maximum nesting depth of the AST (parens + NOT + groups).
    pub max_depth: usize,
    /// Maximum number of leaf predicates/terms in one query.
    pub max_clauses: usize,
    /// Maximum unquoted wildcard value length in characters.
    pub max_wildcard_chars: usize,
    /// Maximum regex pattern length in characters.
    pub max_regex_chars: usize,
    /// Compiled regex size limit in bytes (linear-time engine).
    pub max_regex_compiled_bytes: usize,
}

impl Default for LangLimits {
    fn default() -> Self {
        LangLimits {
            max_text_bytes: 4096,
            max_tokens: 512,
            max_depth: 32,
            max_clauses: 128,
            max_wildcard_chars: 128,
            max_regex_chars: 512,
            max_regex_compiled_bytes: 256 * 1024,
        }
    }
}
