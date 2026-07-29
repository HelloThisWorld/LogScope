# Log Explorer guide

From an exported log file to a useful first query in under five minutes:

1. **Create or open a workspace** (any folder you choose; everything
   stays inside it, fully offline).
2. **Import**: pick your files, choose the profile — generic JSON lines,
   CSV with headers, or *Elasticsearch export (JSONL, ECS)* — and start.
   Progress, rejects, and duplicates are reported; imports are
   cancellable and never leave half-published data.
3. **Explore logs** opens once a log dataset is published.
4. Type a query (see the [query language reference](query-language.md)),
   press **Ctrl+Enter** (or Run). **Escape** cancels.

## The screen

- **Header**: dataset selection (defaults to every published log
  dataset), index status, Export.
- **Query row**: the editor (live syntax colors + diagnostics with exact
  positions), Run/Cancel/clear/copy, and the time-range strategy:
  *All data*, *Relative to newest event* (anchored to your data, not the
  wall clock — offline cases are historical), or *Absolute UTC range*.
- **Histogram**: UTC event counts under the exact current filter. Drag
  to zoom into a range (this rewrites the time range and reruns
  everything); *reset range* returns to the previous strategy. Records
  without timestamps are counted next to the chart, never drawn as fake
  bars.
- **Left panel**: facets (add up to 8 fields; clicking a value appends a
  real query predicate — the query text always shows the whole truth),
  the field list with types/counts/examples, saved searches, recent
  searches (local only, deletable), and column sets.
- **Table**: virtualized, keyboard-navigable (↑/↓), ordered newest
  first; equal timestamps and missing timestamps have a stable
  documented order (untimestamped rows sort last). Scrolling loads more
  pages up to 10 000 loaded rows — beyond that, narrow the query.
- **Detail panel**: canonical fields, typed attributes (with add-to-query
  and copy per field), Resource and Instrumentation Scope, timestamp
  quality flags, parser/profile provenance, and **context** — the
  neighboring records in original source order (not time order), with
  the raw bytes of the record when the referenced file is still present
  and unchanged. A changed or missing source file is reported honestly;
  the canonical copies still display.

## Status line honesty

Every run reports: match count, execution time, whether text search used
the index or the exact scan, the resolved UTC window, omitted
untimestamped records for bounded windows, and cancel/timeout/failure
states. Approximate numbers (high-cardinality distinct counts) are always
labelled approximate.

## Saved state

Saved searches keep your query text, dataset selection, and time
*strategy* (a relative strategy re-anchors on load and shows the resolved
interval). Column sets keep visible columns. Recent searches are bounded,
local, and deduplicated by meaning. All of it survives close/reopen.

## Export

Export writes exactly the current filter/order, streaming to a new file
(existing files are never overwritten): **JSONL** is lossless (typed
attributes, one JSON object per line); **CSV** uses your visible columns
(UTF-8, comma, RFC 4180; values that could be spreadsheet formulas are
prefixed with `'`). Defaults: 1 000 000 rows / 1 GiB — reaching a limit
marks the export **TRUNCATED**, and cancelling cleans up completely.

## Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl+Enter / Cmd+Enter | run query |
| Escape | cancel running query |
| ↑ / ↓ (table focused) | move selection |
| Tab / Shift+Tab | move between controls |

## Older workspaces

Opening a pre-0.2 workspace migrates its metadata in one transaction —
imported data is NOT re-imported. Text search runs in exact-scan mode
until the one-time index rebuild finishes ("rebuild now" in the header,
cancellable and resumable).
