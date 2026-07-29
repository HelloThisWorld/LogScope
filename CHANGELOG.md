# Changelog

## 0.2.0 — 2026-07-29

First usable Log Explorer: import a log export, query it interactively,
entirely offline.

### Added
- **Query language v1** (`logscope-query-lang`): free text and phrases,
  typed field comparisons, boolean logic with parentheses and implicit
  AND, existence/missing tests, severity bands, timestamp bounds,
  bounded wildcards and linear-time regexes, aliases (OTel/ECS flavored),
  spanned diagnostics with stable codes and hints, deterministic
  fingerprints, versioned grammar.
- **Log Explorer**: highlighted editor with debounced authoritative
  validation, UTC histogram with brush zoom, virtualized keyset-paged
  event table, facets and field summaries, record detail (canonical
  fields, typed attributes, Resource/Scope, timestamp quality,
  provenance), source-order context with raw-byte excerpts and honest
  changed/missing-file reporting.
- **Field catalog**: derived, rebuildable per-dataset attribute statistics
  (types, counts, bounded distinct, safe examples) powering trusted field
  resolution, suggestions, and type-conflict detection.
- **Saved state**: saved searches (query text + dataset selection + time
  strategy incl. relative-to-newest-event), column sets, bounded local
  recent searches.
- **Export**: streamed, bounded, atomic CSV/JSONL of the exact current
  filter and order, with truncation marking, overwrite protection,
  cancellation cleanup, and a spreadsheet formula-injection guard.
- Built-in generic **Elasticsearch export** import profile (ECS names).
- Workspace **migration 0002** (transactional, forward-only) with derived
  index lifecycle; pre-0.2 workspaces stay searchable without re-import
  (exact-scan text search until the one-time FTS rebuild).
- Benchmark harness `bench_query` with recorded 1M/5M measurements.

### Changed
- FTS index v2 tokenizer (`unicode61 remove_diacritics 0`) so indexed and
  fallback text search share one documented token semantics; v1 index
  files are detected and rebuilt as a cancellable, resumable job.
- Queries run on a pooled set of hardened DuckDB connections with
  per-request cancellation; DuckDB's `compressed_materialization`
  optimizer is disabled (bundled-build assertion with NULL integer sort
  keys).

### Unchanged / preserved
- Parquet storage schema v1 (no data rewrite), portable-first packaging
  (setup EXE + portable ZIP from one payload), zero network access at
  runtime, platform-neutral shared core.
