# ADR-0014: Saved searches, column sets, recents, and time strategies

Status: accepted (v0.2) · Date: 2026-07-29

## Context

Investigations are re-entered days later; offline cases are historical,
so "last 15 minutes of wall clock" is usually empty and misleading.

## Decision

- SQLite (migration 0002) persists saved searches (name, original query
  text, language version, fingerprint, dataset selection as
  all-or-explicit, time strategy), column sets (ordered canonical field
  identities, optional default), and a bounded local recent list (50
  entries, deduplicated by fingerprint+selection+strategy with run
  counters; explicit delete/clear; never synchronized anywhere).
- The persisted time strategy is the *strategy*, not resolved timestamps:
  `all`, `absolute{start,end}`, or `relative_to_latest{duration}` anchored
  to the newest event timestamp in the selected datasets. Every result
  echoes the resolved interval (UTC, end-exclusive).
- Loading a saved search revalidates against the current catalog through
  the normal pipeline; missing fields or type changes surface as the same
  diagnostics a typed query would get. Query text is preserved verbatim —
  meaning is never rewritten silently.
- Sharing is copyable query text (language-versioned); datasets, saved
  views, and evidence stay local. Portable case bundles are a later
  milestone.

## Consequences

Saved state survives reopen with documented meaning; a fingerprint
mismatch after a language upgrade is detectable instead of silent.
