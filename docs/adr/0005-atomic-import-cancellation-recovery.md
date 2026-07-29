# ADR-0005: Atomic import, cancellation, pause/resume, recovery

Status: accepted (v0.0) · Date: 2026-07-29

## Context

A failed, cancelled, or crashed import must never expose a partially
committed dataset or corrupt existing data; user source files are never
modified.

## Decision

- Imports run as background jobs (`logscope-jobs`): worker threads with
  progress events, cooperative `checkpoint()` pause/cancel between bounded
  batches, and panic isolation into structured `job/panic` failures.
- Staging protocol: segments are written under `staging/<job-id>/`;
  publication renames files into `data/<dataset>/` and then commits one
  SQLite transaction inserting segment rows, ledger entries, dataset
  status, and job completion. Segment visibility == committed row.
- Cancellation/failure discards the staging directory, removes the
  staging-status dataset row, and records the job outcome; measured cancel
  latency is milliseconds (see benchmark report).
- Recovery on open: non-terminal jobs are marked failed
  (`job/interrupted`), all staging directories are discarded, data files
  without committed segment rows are deleted (derived data; imports are
  re-runnable), and empty staging datasets are removed. The recovery report
  is surfaced to the caller/UI.
- Resume: the ingest ledger stores per-file checkpoints (record number,
  byte offset). v0.0 recovery discards and re-runs; seekable-format resume
  lands with v0.1+, and non-seekable (gzip/zip) streams restart the entry
  relying on deterministic record hashes for deduplication (documented,
  visible behavior).

## Consequences

The only crash windows leave either untracked staging files or unreferenced
data files, both cleaned deterministically at next open; published datasets
are immutable and never touched by later imports.
