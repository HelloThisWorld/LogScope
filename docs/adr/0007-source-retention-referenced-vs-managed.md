# ADR-0007: Source retention — referenced versus managed-copy mode

Status: accepted (draft in v0.0, implementation completes in v0.1) · Date: 2026-07-29

## Context

Investigations need the original source bytes (re-parse, audit, byte-exact
locator resolution), but copying every input can be prohibitive.

## Decision

Two per-source retention modes, recorded on the `sources` row:

- `referenced` (v0.0 default): the workspace stores the absolute path,
  size, mtime, and full-content BLAKE3 of each file. The original file is
  never modified; a changed hash at re-access is surfaced, never silently
  accepted.
- `managed_copy` (v0.1): the exact original bytes are copied under
  `sources/` before parsing; `managed_rel_path` records the location. The
  workspace is then self-contained and locators resolve without the
  original media.

Record-level provenance (locators + raw hashes) is identical in both
modes; only the availability of the original bytes differs. Archive
entries always parse from controlled staging extractions regardless of
mode.

## Consequences

`referenced` keeps imports fast and disk-light but ties byte-exact
re-resolution to the original file's continued existence; `managed_copy`
trades disk for durability. The schema supports both from v0.0 so no
migration is needed when v0.1 activates managed copies.
