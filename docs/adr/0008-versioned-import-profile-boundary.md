# ADR-0008: Versioned declarative Import Profile boundary

Status: accepted (v0.0 draft contract) · Date: 2026-07-29

## Context

Heterogeneous log formats need per-source mapping rules without letting
mapping become code execution, and organization-specific mappings must stay
out of the public repository.

## Decision

- `ImportProfile` is pure serializable data: format spec (CSV
  delimiter/headers, JSONL), timestamp rule (candidate fields + format +
  timezone policy), severity/message/trace/span candidate fields, and
  generic-field mappings (operation, outcome, event_type, request_id,
  transaction_id, message_id, entity_id). No expressions, no scripts, no
  native code — the contract is sandboxed by construction.
- `contract_version` gates the shape (newer contracts are rejected with a
  stable error); `version` is the profile content version; a deterministic
  BLAKE3 `fingerprint()` over canonical serialization gives every profile a
  stable identity recorded in provenance and dataset stamps.
- Built-in profiles are generic and public (`builtin.jsonl.generic`,
  `builtin.csv.basic` in v0.0; the Elasticsearch/ECS/Logback/Quarkus set
  lands with the v0.1 parser work). Organization-specific profiles live
  outside the repository in `workspace/profiles/` using this same public
  contract.
- v0.1 extends the same struct family with encodings, multiline rules,
  header rules, redaction hints, and detection hints — additive, gated by
  `contract_version`.

## Consequences

Profiles are diffable, hashable, and safe to share; determinism claims can
name exact profile identities; the core never learns company field names.
