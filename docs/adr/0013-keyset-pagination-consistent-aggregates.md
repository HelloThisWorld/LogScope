# ADR-0013: Keyset pagination and one compiled filter for every surface

Status: accepted (v0.2) · Date: 2026-07-29

## Context

Offset pagination destabilizes under large offsets, and separate filter
implementations for table/histogram/facets/export inevitably diverge.

## Decision

- Total order: `event_time DESC NULLS LAST, record_id DESC, dataset_id
  DESC` (unique triple; untimestamped records form the tail block).
  Cursors are opaque base64 positions (version-tagged, length-capped,
  values only ever bound as parameters); forward and backward pages fetch
  `limit+1` for `has_more`. Proven duplicate-free/gap-free across equal
  and missing timestamps.
- `query_page`, `query_counts`, `query_histogram`, `query_facets`,
  `query_field_summary`, and the export stream all take the same
  `CompiledFilter` + `ResolvedWindow`; none re-interprets query text.
- Time strategies: all-data, absolute `[start, end)`, relative-to-latest
  (anchored to the newest event in the selection, `[latest−d, latest]`).
  Bounded windows exclude untimestamped records and report the omitted
  count; nothing ever invents a timestamp.
- Histogram bins align to multiples of a nice width (1 ms … 30 d picked
  for ≤ the requested bin cap), are zero-filled, and display in UTC.
- Facets return bounded top-K per field (≤ 8 fields, K ≤ 50) with exact
  missing counts and truncation flags; summaries switch to
  `approx_count_distinct` above 10 000 distinct values and say so.
- The bundled DuckDB 1.10505.0 hits an internal compressed-materialization
  assertion on NULL-bearing integer sort keys; that optimizer is disabled
  at connection hardening until the next engine upgrade.

## Consequences

"Results, histogram, facets, counts, and exports agree" is a tested
property, not a convention. Deep scrolling costs one ordered scan per
page (measured ~0.25 s/page at 1M), which stays inside the interactive
budget.
