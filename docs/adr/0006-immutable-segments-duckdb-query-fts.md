# ADR-0006: Immutable segments, DuckDB query path, full-text indexing

Status: accepted (v0.0) · Date: 2026-07-29

## Context

Queries must work fully offline over growing datasets with bounded results
and cancellation. DuckDB's own `fts` extension is not statically linked in
the bundled build, and `INSTALL fts` implies a download — unacceptable at
runtime.

## Decision

- Query path: DuckDB `read_parquet([...], union_by_name)` over the
  **published segment files only** (a segment is visible iff its metadata
  row committed). Every connection is hardened at open:
  `autoinstall_known_extensions=false`, `autoload_known_extensions=false`;
  `parquet`+`json` are statically linked; `httpfs` does not exist in the
  binary. Executable probes (`logscope-query/tests/offline_probe.rs`)
  assert all of this, that `LOAD fts` fails, and that interruption works.
- Bounded queries: parameterized SQL, page limit clamped to 1000 with
  has-more detection, FTS candidate cap, execution budget with watchdog
  interrupt (`Timeout`), external cancellation (`Cancelled`), connection
  reusable afterwards.
- Full-text search runs on **SQLite FTS5** (`indexes/fts-logs.db`,
  unicode61), which is compiled into the bundled SQLite. Because segments
  are immutable, each is indexed exactly once after publication
  (idempotent, crash-safe: `fts_indexed` flags allow re-indexing). User
  search text is escaped into quoted phrases so FTS5 operators cannot be
  injected. The index is derived and rebuildable from segments at any
  time.
- v0.0 indexes display messages; widening to attribute text is a later,
  compatible change.

## Consequences

No FTS-update anomalies by construction (immutability); a second engine
file (SQLite) for search is accepted in exchange for a guaranteed-offline,
guaranteed-updatable index; FTS results join back through record IDs.
