# LogScope

LogScope is an open-source, Windows-first, fully offline, local-first
telemetry investigation desktop application. It turns exported or supplied
logs (and later metrics and traces) into deterministic, source-traceable
datasets you can query on your own machine — no account, no server, no
network access.

**Status: v0.0 foundation / architecture proof.** The current build proves
the offline architecture (Tauri + React + Rust + DuckDB + Parquet + SQLite),
the canonical OTel-aligned data model, atomic imports with cancellation and
crash recovery, offline full-text search, and OTLP transport equivalence.
It is not yet the interactive Log Explorer.

## Product profiles

1. **Offline Log Case** — investigate exported/supplied logs without
   requiring metrics or traces (first wedge).
2. **Local OTel Session** — eventually ingest local OpenTelemetry logs,
   metrics, and traces into the same durable workspace (the v0.0 OTLP
   receiver is an experimental spike, disabled by default, loopback-only).

## Guarantees

- Fully offline at runtime: no dependency downloads, no telemetry, no
  update checks; DuckDB extension auto-install/auto-load is disabled and
  covered by automated probes.
- Portable-first delivery: an ordinary relocatable ZIP; no installer, no
  registry entries, no signing requirement (ADR-0002).
- Nothing is silently dropped or invented: unknown fields survive as typed
  attributes, imperfect records carry quality flags, rejected records keep
  exact source locators, and no synthetic spans/timestamps are fabricated.
- Deterministic re-imports: identical source + identical parser/profile/
  normalizer versions produce identical canonical values and record hashes.

## Repository layout

See `docs/development/v0.0-v0.1-implementation-plan.md` (live status),
`docs/adr/` (architecture decisions 0001–0010), `fixtures/README.md`
(synthetic golden fixtures), and `docs/benchmarks/` (measured baselines).

## Building

Prerequisites: Rust (stable, MSVC target on Windows), Node.js 22+.

```
# tests
cargo test --workspace --exclude logscope-desktop

# desktop shell (dev)
cd apps/desktop && npm install && npm run tauri dev

# portable Windows archive (unsigned, no installer)
pwsh scripts/package-portable.ps1 -Version 0.0.0
```

Benchmarks (deterministic corpora, generated on demand, never committed):

```
cargo run --release -p logscope-testsupport --bin bench_import -- logs 1000000
```

## License

License selection is pending and intentionally not decided by tooling; see
`docs/THIRD-PARTY-NOTICES.md` for bundled component licenses.
