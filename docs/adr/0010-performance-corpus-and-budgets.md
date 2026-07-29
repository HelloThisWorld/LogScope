# ADR-0010: Performance corpus and provisional budgets

Status: accepted (v0.0 baseline) · Date: 2026-07-29

## Context

Performance claims must come from executed, reproducible runs on documented
hardware — never asserted from intuition.

## Decision

- Deterministic generators (`logscope-testsupport`, ChaCha8 fixed seed)
  produce the corpora on demand: JSONL logs, OTLP JSONL metrics with
  controlled cardinality, OTLP JSONL spans with realistic defects. Corpora
  are never committed.
- The `bench_import` harness runs the real pipeline (generate → import →
  publish → index → query) and emits machine-readable results including
  peak working set.
- Baseline (2026-07-29, i9-12900KF / 64 GB / NVMe, Windows 11 26100,
  release build, seed 20260729 — see
  `docs/benchmarks/2026-07-29-v0.0-baseline.md` for the full table):
  1M logs imported in 24.0 s (41.7k rec/s) with 175 MiB peak WS against a
  240 MiB input; 1M metric points in 12.9 s (77.7k pts/s) with peak WS
  scale-invariant (513 MiB at 1M vs 489 MiB at 250k); 250k spans at 100.5k
  spans/s; first-page/severity/FTS queries 233–261 ms; cancellation
  latency 4.7 ms with a clean workspace.
- Provisional budgets derived from ×2 headroom on that baseline:
  logs ≥ 20k rec/s; metrics ≥ 35k pts/s; peak import memory ≤ 1 GiB and
  never proportional to input size; first-page and FTS queries ≤ 1 s on a
  1M-record dataset; cancellation acknowledged ≤ 250 ms; recovery on open
  ≤ 5 s for interrupted staged imports. CI smoke uses reduced counts;
  budget regressions require a new measured baseline in this ADR's report.

## Consequences

Every future optimization or regression is argued against recorded
measurements; budgets are honest floors rather than aspirations.
