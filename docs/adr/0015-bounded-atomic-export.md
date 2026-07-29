# ADR-0015: Bounded, streamed, atomic result export

Status: accepted (v0.2) · Date: 2026-07-29

## Context

Exports must reflect exactly what the Explorer shows, survive
cancellation cleanly, and never masquerade a partial file as complete.

## Decision

- The export job runs ONE streaming ordered scan (`stream_query`) with
  the Explorer's compiled filter, window, and total order — no result
  materialization on the application side (measured 34 k rows/s at 1M,
  9× the paged approach it replaced).
- Bounds: row cap (default 1 M, hard 10 M) and byte cap (default 1 GiB,
  hard 10 GiB); a record is written completely or not at all; hitting a
  bound sets a `truncated` flag surfaced in the UI and stored in the
  workspace's `export_jobs` record together with query text, fingerprint,
  selection, strategy, and resolved window.
- Publication: write to `.name.partial-<uuid>` in the destination
  directory, fsync, then rename. Existing destinations are refused
  (`export/destination-exists`), cancellation and failure delete the
  temp file.
- CSV: UTF-8, LF, comma, RFC 4180 quoting, empty cell for null, RFC 3339
  UTC timestamps, JSON text for structured values; cells starting with
  `= + - @` get a leading `'` (spreadsheet formula-injection guard, on by
  default). CSV renders attribute values as display strings — the
  lossless typed path is JSONL, which emits one complete JSON object per
  line with a deterministic layout and canonical tagged attributes.

## Consequences

An export is either the whole truth, a clearly marked truncation, or
absent. Filter equivalence with the table is a test, not a promise.
