# ADR-0001: Repository and module boundaries

Status: accepted (v0.0) · Date: 2026-07-29

## Context

LogScope needs strict layering (UI / commands / services / model / storage /
query) that survives growth to CLI, Agent API, dashboards, and reports, and
must never collapse into one monolithic crate.

## Decision

One Cargo workspace with focused crates:

| Crate | Responsibility |
|-------|----------------|
| `logscope-model` | canonical versioned types, deterministic IDs/hashes |
| `logscope-normalize` | timestamps, severities, attribute typing, derivations |
| `logscope-workspace` | layout, manifest, SQLite metadata, migrations, staging/recovery |
| `logscope-jobs` | thread-based jobs: progress, pause, cancel, panic isolation |
| `logscope-ingest` | source adapters, Import Profiles, streaming readers, log normalizer |
| `logscope-store` | Parquet segment writers, FTS5 index maintenance |
| `logscope-query` | hardened DuckDB engine, bounded queries, cancellation, analysis |
| `logscope-otlp` | EXPERIMENTAL OTLP spike (isolated; nothing depends on it except tests/bench) |
| `logscope-app` | application services + typed command DTOs |
| `logscope-testsupport` | fixtures access, deterministic generators, bench harness |
| `apps/desktop/src-tauri` | thin Tauri shell over `logscope-app` |

Rules: React owns presentation only; Tauri commands call `logscope-app`
services; adapters never query; DuckDB appears only in `logscope-query`;
rusqlite appears in `logscope-workspace` (metadata) and `logscope-store`
(FTS index). Shared code stays platform-neutral; platform specifics
(WebView2 lookup, app-data paths) live in the desktop shell.

## Consequences

The future CLI/Agent API reuse `logscope-app` unchanged. The OTLP spike can
be deleted or promoted without touching the product path. Compile times stay
manageable because DuckDB/tonic are isolated behind crate boundaries.
