# ADR-0003: Workspace layout, SQLite/Parquet ownership, migrations

Status: accepted (v0.0) · Date: 2026-07-29

## Context

A workspace must be a self-contained, durable, offline directory holding
control-plane metadata and bulk telemetry, safely evolvable across versions.

## Decision

Layout: `manifest.json`, `workspace.db`, `sources/`, `data/<dataset-id>/`,
`indexes/`, `attachments/`, `reports/`, `profiles/`, `staging/`.

Ownership:
- **SQLite (`workspace.db`)** owns control-plane metadata: workspace info,
  sources, source files, datasets, segments, resources, scopes, jobs,
  ingest ledger, rejected records. WAL mode, foreign keys, STRICT tables.
- **Parquet** owns normalized bulk records in immutable segments
  (`logs|metrics|spans-<segment-id>.parquet`), zstd-compressed, bounded row
  groups. A segment row in SQLite is the single visibility switch.
- **`manifest.json`** duplicates identity/version facts for cheap
  inspection and forward-compat checks; saved atomically (temp + rename).
- **`indexes/`** holds derived, rebuildable indexes (SQLite FTS5). Never
  the sole copy of anything.

Migrations: embedded, ordered, forward-only SQL applied one transaction per
migration and recorded in `schema_migrations`; opening a newer-schema
workspace fails with a stable error (`workspace/schema-too-new`).

## Consequences

Bulk data stays engine-agnostic (any Parquet reader); metadata gets ACID
semantics; downgrades are explicitly unsupported; index corruption is
recoverable by rebuild.
