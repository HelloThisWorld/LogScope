# ADR-0011: Versioned query language v1 with one authoritative parser

Status: accepted (v0.2) · Date: 2026-07-29

## Context

v0.2 needs a small, documented log query language whose meaning cannot
drift between the editor, the table, aggregations, and exports.

## Decision

- `logscope-query-lang` owns lexing (UTF-8 + UTF-16 spans), parsing,
  the typed AST, catalog-based field resolution, limits, canonical
  serialization, and `qry-` fingerprints. `LANGUAGE_VERSION = 1` is stored
  with saved searches; newer versions are rejected, never guessed.
- Grammar: parens > `NOT` > `AND` > `OR`; adjacency = implicit `AND`;
  uppercase-only keywords (`and` is a term); `:` and `=` are exact aliases;
  `!=` compiles as `NOT(=)` (missing values match `!=`); value groups
  `f:(A OR B)` expand per value; `f:*` = present-and-not-empty.
- Severity is band-compared (TRACE 1–4 … FATAL 21–24, text fallback when
  the number is missing); `message` is token-matched text; other string
  fields are exact and reject ordering comparisons (`lang/type-mismatch`
  names field, type, and offending value).
- Wildcards/regex apply to string fields only; both validate on the
  linear-time `regex` crate with a 256 KiB compiled-size cap and execute
  on DuckDB RE2. Limits: 4096 bytes, 512 tokens, depth 32, 128 clauses,
  wildcard 128 chars, regex 512 chars.
- The editor receives tokens and diagnostics from the backend
  (`validate_query`); there is deliberately NO TypeScript grammar, so no
  cross-language conformance corpus is needed.

## Consequences

Every surface shares one meaning by construction. New syntax requires a
language-version bump plus documented migration for saved searches.
