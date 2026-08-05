# ADR-0019 — Deterministic reports, disclosure projection, and portable case bundles

Status: accepted (v0.3.0) · Date: 2026-08-05

## Context

v0.3 adds the three surfaces through which investigation content leaves a
workspace: generated reports, the disclosure (redaction) projection, and
portable case bundles. Everything before them (ingest, canonical storage,
query) is inward-facing; these are outward-facing, so their contracts are
dominated by two risks — an artifact silently differing from what the user
previewed, and content escaping that the user chose to withhold.

## Decisions

### 1. Artifacts are deterministic; the preview IS the final bytes

- No wall-clock time, random value, or environment detail is ever written
  into artifact bytes. Rendering is a pure function of the captured
  snapshot (one investigation revision, captured once per generation).
- Markdown is LF-only with fence lengths derived from content (longest
  backtick run + 1, minimum 3), so hostile content cannot escape a fence.
- HTML is self-contained: CSP `default-src 'none'; style-src
  'unsafe-inline'`, zero scripts, zero external references, all content
  escaped. Hostile-rendering tests assert inertness in both formats.
- `render_preview` and `generate_report` call one shared render path over
  the same snapshot type, and test asserts preview bytes == published
  bytes. Generation writes to a staged `.partial-<uuid>` file, renames
  atomically, records SHA-256 on an immutable artifact row, and refuses
  existing destinations. Regenerating a definition yields byte-identical
  output (asserted end to end).
- Artifact rows are two-phase (`running` inserted before the first byte,
  finished exactly once). Workspace open completes interrupted rows as
  `failed (job/interrupted)` — tombstones are finished, never deleted, so
  an interrupted generation stays on record.

### 2. Disclosure projection: one projection, default-closed, counted

- One compiled projection (`redact.rs`) is shared by report preview,
  report generation, and bundle export. There is no second
  implementation to drift.
- Rules are ordered and typed: `omit_field`, `mask_field` (one fixed
  token, so a mask can never smuggle data), `replace_exact`,
  `replace_regex` (linear-time engine, pattern ≤ 512 chars, compiled
  program ≤ 1 MiB), `pseudonymize` (deterministic, labeled — correlation
  survives, the value does not). Unknown rule kinds, unknown keys,
  oversized lists, and invalid regexes are structured refusals
  (`redaction/invalid-profile`), never skipped.
- Posture is default-closed where it matters: provenance paths are
  omitted unless the profile explicitly widens them; deny always beats
  allow; a non-empty allow list switches fields to allowlist mode.
- **Posture JSON must be a JSON object** (clarified by the v0.3 test
  sweep): serde's derived struct deserialization also accepts a
  positional array, which would have let `["include"]` widen the path
  policy without ever looking like a posture. Arrays are refused.
- Every omission, mask, replacement, pseudonym, truncation, and path
  redaction is counted, and the counts are rendered into the artifact and
  the bundle's disclosure note — an omission can never look like
  completeness.
- The projection never mutates canonical data. It shapes what leaves the
  workspace, nothing else. An unparseable snapshot is projected as opaque
  text so raw bytes cannot bypass the text rules.

### 3. Portable case bundles: deterministic out, hostile in

- Export is a deterministic ZIP (fixed entry timestamps, stable entry
  order) with an exhaustive manifest: schema version, min-compatible
  version, investigation identity/revision, reproduction scope, and the
  exact entry list with per-entry SHA-256. Import enforces exact
  entry-set equality — no hidden payloads in either direction.
- Each hypothesis entry carries its evidence links and state (added by
  the v0.3 sweep after the round-trip test caught them being dropped);
  import restores them once the evidence rows exist. A link to evidence
  the bundle does not contain is a refusal; a state that does not parse
  stays `unverified` — the import never upgrades a claim it cannot read.
- A bounded parquet subset of referenced canonical records (≤ 200k) may
  be included; under a disclosure profile **no raw data is exported at
  all** (`snapshot_only` scope). The subset lands in the destination as
  an inert file and is not registered as a queryable dataset — the
  imported case stays readable through captured snapshots, and
  verification honestly reports `dataset_revision_unavailable` instead of
  pretending `verified`. Previously generated reports travel as inert
  files and are never opened or re-executed; a report file modified after
  generation fails the export (`bundle/report-modified`) rather than
  bundling bytes that no longer match their recorded checksum.
- Import validates the entire central directory before extracting
  anything: platform-independent path hardening by string inspection
  (backslash, colon/ADS, absolute, traversal, control characters,
  reserved device names, per-segment trailing dot/space, depth ≤ 8,
  length ≤ 512, case-insensitive collisions), entry/total/inflate size
  cutoffs enforced during streaming, checksum verification, and an
  actionable min-compatible-version gate. Extraction goes to a staging
  directory and the new isolated workspace is renamed into place only
  after every validation and insert held — a refused import leaves
  nothing behind.
- Bundles import into a NEW workspace only, never into an existing one.
  Import provenance (original path, bundle checksum, manifest, summary)
  is recorded in the destination.

## Consequences

- The user can trust that what they previewed is what left the machine,
  byte for byte, and that the artifact itself discloses how much was
  withheld.
- Bundle compatibility can evolve additively (new optional keys import as
  absent on older bundles) without version bumps; incompatible changes
  bump `min_compatible_version` and fail with an actionable message.
- Determinism is load-bearing: byte-equality tests double as integrity
  tests, and any nondeterminism (ordering, timestamps, locale) is caught
  immediately.
- Report definitions deliberately do not travel in bundles: a regenerated
  report in a destination without the canonical dataset could silently
  differ from what was shared. The generated artifacts travel instead;
  new reports can still be authored in the destination from captured
  snapshots.
