# ADR-0004: Canonical Resource/Scope/signal/provenance model

Status: accepted (v0.0) · Date: 2026-07-29

## Context

Every source (files today, OTLP eventually) must normalize into one
lossless, OTel-aligned model with deterministic identity, without inventing
or silently dropping data.

## Decision

- Typed `AnyValue` mirrors OTLP (str/bool/int/double/bytes/array/map/empty)
  with `BTreeMap` ordering, bit-exact doubles (NaN/±Inf survive JSON), and
  a tagged canonical JSON form used in storage.
- `ResourceDescriptor`/`ScopeDescriptor` are identified by BLAKE3 over
  attributes + schema URL (+ name/version for scopes); derived fields
  (`service.name`, deployment environment, host/container/pod/process)
  record the original key used for each derivation.
- Log/metric/span records carry complete typed attributes, dropped counts,
  and full `IngestProvenance` (dataset, logical source, physical origin,
  exact locator, parser/profile/normalizer versions, protocol, content
  type, ingest time, raw hash, timestamp precision, quality flags).
- Deterministic hashing uses an explicit tagged byte encoding (never JSON
  text). Record IDs (`log-/met-/spn-<32 hex>`) cover canonical content plus
  stable provenance (raw hash, locator, code versions, `MODEL_VERSION`) and
  **exclude** wall-clock and workspace-instance fields (ingest time,
  observed time, dataset/source/file IDs) so re-imports hash identically.
- A golden pin test (`tests/golden/record_id.txt`) fails on any encoding
  change; changing the encoding requires bumping `MODEL_VERSION`.
- Unknown metric types are rejected with a reason, never coerced to gauges.
  Imperfect input (missing timestamps, unmapped severities, invalid IDs,
  duplicates, DST assumptions) is flagged, not repaired.

## Consequences

Duplicates are detectable across imports and rotated copies by content
identity; canonical equivalence across OTLP transports is testable by
stripping provenance; every stored value can be traced to exact source
bytes.
