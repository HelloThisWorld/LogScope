# Changelog

## 0.2.1 — 2026-08-03

Patch release. One user-visible correctness fix in the bounded query path.

### Fixed
- **Cancellation raised before a query entered execution was silently
  dropped.** DuckDB clears any pending interrupt when a query begins, so a
  cancel landing in the window between the request and the engine starting
  work was discarded and the query then ran to completion. Both call sites
  cancel exactly once from a polling thread and never retry, so the
  cancellation was ignored for the whole remaining budget — up to 600 s for
  field statistics and 24 h for export. `run_bounded` now short-circuits
  when cancellation was already requested, and its watchdog re-asserts the
  interrupt while cancellation is pending. A query that finishes before the
  interrupt lands still returns its value; cancellation stays best-effort.

### Added
- Dispatch-gated Linux shared-core CI leg (`linux-core`), covering path
  canonicalization, encodings, timezones, line endings and file identity on
  a case-sensitive filesystem. Not yet executed: GitHub-hosted minutes are
  unavailable under the Actions billing limit.
- `docs/development/v1.0-implementation-plan.md` — the v1.0 preflight
  record, public contract census and 160-item acceptance assessment.

### Correction to the 0.2.0 entry below
The 0.2.0 "Unchanged / preserved" line claims packaging is "setup EXE +
portable ZIP from one payload". **There is no setup EXE.** Only the portable
ZIP path exists (`scripts/package-portable.ps1`); the graphical extractor is
unbuilt. The entry is left as written rather than rewritten, and corrected
here.

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
