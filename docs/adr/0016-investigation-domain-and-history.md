# ADR-0016: Investigation domain, optimistic revisions, and the shared history ledger

Status: accepted (v0.3)

## Context

v0.3 turns a searchable workspace into a durable investigation workbench.
The domain (investigations, hypotheses, typed items, scope refs, evidence,
markers, reports, redaction profiles, bundles) needs identity, concurrency,
and history rules that later milestones (deterministic findings in v0.4,
more signal kinds) can extend without changing the meaning of existing
records.

## Decision

1. **Control plane in SQLite, one migration.** All v0.3 entities live in
   the existing metadata database via forward-only migration
   `0003_investigations` (schema v3). The migration is additive only: it
   creates tables and rewrites nothing, so a failed migration rolls back
   to a schema-v2 workspace that 0.2.0 builds still open. Parquet/DuckDB
   are untouched; investigations never copy or mutate canonical log data.
2. **Opaque prefixed ids.** Every entity id is `<prefix>-<uuid4>`
   (`inv-`, `hyp-`, `item-`, `ev-`, …), minted in `logscope-case::ids` —
   never derived from titles, paths, row numbers, or display order.
3. **Optimistic revisions.** Every mutable row carries `revision`.
   Updates run `… WHERE id = ? AND revision = ?expected`; zero affected
   rows returns the structured conflict `workspace/stale-revision`
   (distinct from `workspace/missing-entity`). No silent last-write-wins.
4. **One non-destructive history ledger.** Every material mutation
   inserts a `case_history` row — entity kind/id, new revision, action,
   and the full post-action entity payload — in the same transaction as
   the mutation. Any prior revision is retrievable; "removal" for
   investigations, hypotheses, items, and evidence is archival, not
   deletion (scope refs, being plain pointers, may be removed with their
   final payload retained in history). The ledger doubles as the
   per-investigation activity feed.
5. **No OS identity.** Nothing captures the OS account, hostname, git
   config, or paths-as-identity. `owner_text` is stored exactly as typed.
   Hashes stored in case data are integrity aids, never signatures or
   proof of authorship.
6. **Manual state semantics.** Investigation statuses and hypothesis
   states change only through explicit calls that record from→to detail.
   `Supported` and `Confirmed` remain distinct. v0.3 findings are always
   user-authored (`authored_by_user = 1`,
   `finding_provenance_json = NULL`); the reserved provenance column lets
   v0.4 deterministic findings arrive without re-interpreting manual ones.
7. **Vocabulary in `logscope-case`.** Enum strings (snake_case) are the
   single wire/storage form, parsed and validated in the service layer;
   the repositories store validated strings and stay mechanical.

## Consequences

- Concurrent UI edits surface as structured conflicts the UI can resolve
  by reloading — required by the v0.3 gate on stale writes.
- History grows monotonically; it is bounded in practice by being
  metadata-only (payloads are entity rows, evidence snapshots are bounded
  at pin time). A future compaction policy, if ever needed, must preserve
  retrievability of referenced revisions (report definitions reference
  exact evidence revisions).
- Because the migration is pre-release for v0.3, `0003_investigations`
  may be amended on the feature branch until 0.3.0 ships; after release
  it is frozen like 0001/0002.
