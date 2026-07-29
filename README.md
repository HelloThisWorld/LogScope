# LogScope

LogScope is an open-source, Windows-first, fully offline, local-first
telemetry investigation desktop application. It turns exported or supplied
logs (and later metrics and traces) into deterministic, source-traceable
datasets you can query on your own machine — no account, no server, no
network access.

**Status: 0.2.0 — interactive Log Explorer.** Import a log export
(generic JSONL, CSV, or Elasticsearch/ECS JSONL), then investigate it with
a versioned query language (free text, phrases, typed comparisons, boolean
logic, wildcards, linear-time regexes), a UTC histogram with brush zoom,
facets and field summaries, source-order context with raw excerpts, saved
searches/columns/recents, and bounded atomic CSV/JSONL export — all under
one compiled filter, fully offline. See `docs/user/log-explorer.md` and
`docs/user/query-language.md`.

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

See `docs/development/v0.2-implementation-plan.md` (live status and the
0.2 acceptance matrix), `docs/adr/` (architecture decisions 0001–0015),
`fixtures/README.md` (synthetic golden fixtures), `CHANGELOG.md`, and
`docs/benchmarks/` (measured baselines, incl. the 1M/5M query baseline).

## Building

Prerequisites: Rust (stable, MSVC target on Windows), Node.js 22+.

```
# tests
cargo test --workspace --exclude logscope-desktop

# desktop shell (dev)
cd apps/desktop && npm install && npm run tauri dev

# portable Windows archive (unsigned, no installer)
pwsh scripts/package-portable.ps1 -Version 0.2.0
```

Benchmarks (deterministic corpora, generated on demand, never committed):

```
cargo run --release -p logscope-testsupport --bin bench_import -- logs 1000000
cargo run --release -p logscope-testsupport --bin bench_query -- 1000000
```

## License

License selection is pending and intentionally not decided by tooling; see
`docs/THIRD-PARTY-NOTICES.md` for bundled component licenses.
