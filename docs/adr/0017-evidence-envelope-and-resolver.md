# ADR-0017: Evidence envelope, pin capture, and integrity resolution

Status: accepted (v0.3)

## Context

v0.3 turns Explorer state into durable investigation evidence. Evidence
must stay honest when the workspace changes underneath it: sources get
deleted or edited, datasets are re-imported, saved searches are renamed,
catalogs drift. v0.2 already owns canonical record ids, ingest
provenance, source fingerprints, and one authoritative query pipeline;
the evidence layer must reuse those guarantees, not re-implement them.

## Decision

### One versioned envelope, two separate payloads

Every evidence row stores an `envelope_version` (currently 1) plus two
JSON payloads defined in `logscope-case::envelope`:

- the **live reference** (`EvidenceReference`) — typed identity used to
  re-resolve canonical data and jump back to the Explorer;
- the **captured snapshot** (`EvidenceSnapshot`) — a bounded record of
  what was visible at pin time, readable even if every source vanishes.

Decoding is version-gated: payloads written by a newer build are refused
(`UnsupportedVersion`), never reinterpreted. Bounds are hard contract:
500 selection ids, 20 representative ids, 50 snapshot rows, 4 KiB per
captured field, 64 KiB per snapshot.

### Pinning goes through the authoritative pipeline

`logscope-app::case` owns the six pin services (event, bounded
selection, query, Explorer group, histogram interval, manual item). All
query-shaped captures run `analyze → compile → execute` through the same
catalog/language/engine path the Explorer uses; an invalid scope is a
structured error, never stored evidence. Relative time strategies are
pinned to the concrete resolved bounds in effect. Group pins render
their predicate with `group_predicate` + `compose_group_query`
(parenthesized base ∧ predicate, `NOT field:*` for the missing-value
group); verification recomposes with the same functions so pin and
verify can never drift apart.

### Dataset revisions are recomputable fingerprints

`dataset_revision` digests the sorted published segment set
(segment_id, row_count, byte_size), the dataset id, and the storage
schema version into `dsrev-<hex>`. It is computed on demand from
`segments` — no new state to migrate, identical inputs always agree.

### One batched, read-only, cancellable resolver

`verify_evidence` maps every evidence item to exactly one of the ten
documented resolver states (ADR table in the v0.3 plan). Structure:

1. decode all references (version gate first);
2. one dataset-fact pass (existence, current dsrev, segment files,
   parser/profile versions);
3. **one id-set lookup per dataset** for all event/selection ids using
   the temp-table pattern — never one query per evidence item;
4. per-file source probes cached for the run: size fast-path, then
   full-file BLAKE3 (`fingerprint_file`) only when sizes match;
5. query/group/interval evidence re-validates and re-runs one bounded
   count through the query service, comparing fingerprints, counts, and
   omitted-untimestamped totals against the capture.

Verification writes only `resolver_state`, `resolver_detail_json`, and
`last_verified_at` — never a snapshot, reference, or row revision — and
records one investigation-level `verified` history event per run.
Cancellation between items leaves finished items at their fresh state
and unreached items untouched.

### State-mapping refinements fixed during implementation

- A reference that fails to decode **at a supported version** with a
  readable snapshot is `unsupported_reference_version`
  (`cause: "undecodable"`): the spec state covers "cannot be interpreted
  safely", and the evidence is not lost. `broken` is reserved for
  reference **and** snapshot both unreadable.
- A canonical record that disappeared from a still-present dataset is
  `dataset_revision_unavailable` with `record_found: false` — the
  captured revision no longer resolves; content-hashed ids make "same
  revision, missing record" impossible.
- `source_missing` / `source_changed` apply generically to what an
  evidence kind references: for item evidence the "source" is the item
  (deleted item → missing; advanced revision → changed with both
  revisions recorded).
- A pinned query that no longer validates (catalog drift) is
  `query_drift` with `validates: false` and the diagnostics — the
  environment drifted so far the query cannot even run; details keep the
  conditions distinct.
- Secondary conditions that do not change the primary state (dataset
  revision advanced while the record still verifies, parser/profile
  version drift, saved-search renamed or deleted, uncheckable non-exact
  count) are recorded under `detail.secondary`.

## Consequences

- Later signal kinds (metric point/range, span, trace, comparison,
  deterministic finding) arrive as new envelope versions; v1 log
  evidence semantics never change.
- Reports and bundles can trust `resolver_state` + detail as the single
  integrity vocabulary; no second green/red boolean exists anywhere.
- Verification cost stays bounded: one id lookup per dataset, one hash
  per changed-size-exempt file per run, one count per query-shaped item.
- Source fingerprints are full-file BLAKE3 integrity aids — documented
  as change detection, not signatures, authorship, or custody.
