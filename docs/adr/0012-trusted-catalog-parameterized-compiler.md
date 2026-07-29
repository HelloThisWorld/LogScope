# ADR-0012: Trusted field catalog and fully parameterized compiler

Status: accepted (v0.2) · Date: 2026-07-29

## Context

User queries reference dynamic attribute fields that only exist as keys
inside tagged canonical JSON. Field identifiers cannot be bound as SQL
parameters, which historically invites string-built SQL.

## Decision

- A derived, rebuildable **field catalog** (`field_stats`, one row per
  dataset+field) is computed by streaming `attributes_json` through the
  same `AnyValue` model the importer wrote: nested maps to depth 6,
  observed types, presence counts, bounded distinct tracking (exact ≤ 1024,
  flagged approximate beyond), display-safe bounded examples. Fields whose
  names cannot be written in language v1 are catalogued as unqueryable.
- The compiler emits SQL from exactly two identifier sources: a
  compiled-in canonical column map, and JSON **path parameters** built from
  catalog-resolved segments (`$."k1".v."k2".v`) — the path travels as a
  bound value, never as SQL text. All user values are bound parameters.
- Every leaf predicate collapses SQL three-valued logic with
  `COALESCE(…, false)`, giving the documented null policy: missing fields
  never match positive predicates and do match their negations.
- Free-text terms/phrases have one token definition (Unicode alphanumeric
  runs, case-insensitive, diacritics kept). Execution is either FTS v2
  candidates (SQLite `unicode61 remove_diacritics 0`, ids joined through a
  temp table) or an equivalent RE2 token-boundary regex. The compiler picks
  per predicate: index ready AND ≤ 20 000 hits (measured cutoff: at 57k
  candidates the scan is 4× faster). Overflow falls back to the exact
  scan — results are never truncated to fit an index.
- Type conflicts across datasets fail resolution loudly (int+double
  promote to double; anything else is `lang/type-conflict`).
- `index_state` tracks fts/field_catalog per dataset (pending → building →
  ready/failed); rebuilds are cancellable jobs; migration 0002 seeds
  pending states for pre-0.2 datasets, whose queries use the exact
  fallback until rebuilt.

## Consequences

SQL injection is structurally impossible at the query boundary (proved by
hostile-input tests); indexes stay disposable; unindexed data is slower
but never wrong.
