# ADR-0009: OTLP receiver/file equivalence and forward compatibility

Status: accepted (experimental spike, v0.0) · Date: 2026-07-29

## Context

Local OTel Session (v0.7) will ingest live OTLP. v0.0 must prove that live
receipt and file import can share one canonical semantic path, without
shipping a production receiver.

## Decision

- Official schemas via `opentelemetry-proto` (tonic/prost types, OTLP-spec
  JSON serde with hex IDs and u64-as-string). One converter
  (`logscope-otlp::convert`) maps protobuf types to canonical records for
  all four transports: HTTP protobuf, HTTP JSON, gRPC, JSONL file.
- Receivers are loopback-only, disabled by default, with bounded bodies
  (413), bounded buffering (429/RESOURCE_EXHAUSTED), strict content-type
  checks (415), and graceful decode failures (400). gRPC and HTTP share the
  same envelope channel.
- Equivalence is enforced by a golden test: identical telemetry through all
  four transports must produce equal canonical values after stripping
  provenance; transport provenance (protocol, raw envelope hash, locator)
  must differ and is asserted separately.
- Forward compatibility: unknown JSON fields are tolerated (serde ignores;
  a synthetic future field is part of the test); JSONL file import retains
  the source file itself (referenced source) so raw envelopes remain
  reprocessable; unknown metric types and invalid span identities reject
  individual records with reasons, never whole batches when avoidable, and
  event-time ordering is never assumed.
- Limitation (accepted for the spike): the network receivers retain the
  envelope hash, not raw bytes; raw-envelope retention for live sessions is
  a v0.7 concern.

## Consequences

v0.7 can promote the receiver by adding lifecycle/UI around an
already-proven conversion path; canonical equality is the regression net
for any future OTLP schema upgrade.
