# ADR-0020 — Rule-based message templates and stack fingerprints

Status: accepted (v0.4 WP2) · Date: 2026-08-05

## Context

v0.4 needs deterministic, explainable pattern extraction over
multi-million-record log workspaces. The determinism gate is strict:
identical inputs must produce identical pattern IDs and memberships
across repeated runs, allowed parallelism, randomized input-partition
order, close/reopen, and cold/warm cache. Every active rule must be
visible; results must never ask the user to trust an opaque score.

## Decision

### 1. Templates are rule-based masks — no adaptive clustering

Classic template miners (Drain-style parse trees, online clustering)
evolve state as records arrive: the resulting clusters depend on
insertion order, which directly violates the randomized-partition gate
and makes explanations circular ("these merged because the tree said
so"). LogScope instead normalizes **every record independently** under a
versioned rule set (`template.mask` v1, mask-set v1): the template IS
the masked token sequence, and a pattern is the set of records sharing
it byte-for-byte.

- Order-independence holds by construction — no coordination, no merge
  phase, trivially parallelizable.
- Every mask rule is independently switchable and visible; disabling a
  rule changes the configuration fingerprint and therefore the identity
  of every result (never a silent relabel).
- The accepted limitation, stated in the UI and docs: messages that
  differ beyond the mask rules form distinct patterns. There is no
  fuzzy merge. This is the honest trade against the determinism and
  explainability contracts.

Normalization contract (identity-bearing, versioned together):
canonical UTF-8 as ingested (no additional Unicode normalization in v1
— combining-form variants stay distinct, documented); case-sensitive;
whitespace runs collapse; tokens are whitespace-split with leading
`([{'"<` / trailing `)]}.,;:!?'">` decoration detached, classified, and
re-attached; rules apply per token in a fixed order (quoted → url →
path → timestamp → uuid → trace/span hex → ip:port → ip → 0x-hex →
duration → byte-size → number → bare-hex), first enabled match wins;
messages beyond 8 KiB / 512 tokens are cut with an explicit
`<truncated>` token that participates in identity, so a truncated
message never collides with an untruncated prefix. Masking is analysis
identity ONLY — it is not redaction, and the disclosure projection
(ADR-0019) remains the sole authority for outbound artifacts.

### 2. Stack fingerprints parse frames; volatile locations never count

`stack.frames` v1 parses the stack-trace text found inside log records
(five textual forms: Java, .NET, Python, Go panics, Node.js; detection
runs every parser and picks the first fully-parsed result in a fixed
order). Identity = exception type + ordered normalized frames + nested
cause chain. Java/.NET frames keep the fully-qualified method and drop
the file/line; Python/Node/Go frames keep `callable@file-basename`.
Line numbers, columns, and addresses never participate, so equivalent
traces with volatile locations share one fingerprint; distinct
exception types never merge, whatever the frames look like. Frame and
cause bounds (128 / 8) set an explicit truncation flag that
participates in identity. Malformed/partial traces fingerprint what
parsed with the parse quality reported honestly. This is log-body
analysis — never JVM dump diagnostics.

### 3. Execution: one streaming pass, order-free aggregation, atomic publish

The runner streams the run's FROZEN scope once (`stream_query` over the
compiled filter and the run's own stored bounds — never re-resolved),
folds each record into an order-independent accumulator (counts,
min/max extremes with record-id tie-breaks, bucket and per-resource
maps with explicit caps), and publishes a summaries parquet under
`derived/analysis/<run>/` — cataloged with its SHA-256 — before the
two-phase run record is completed with its manifest. Missing fields,
malformed stacks, over-limit patterns, and record-cap truncation are
counted, never silently dropped. Representative examples are
deterministic (earliest / latest / peak-bucket / per-resource, ties by
canonical event ID) and are labeled as examples, not the contributing
set.

### 4. Drill-down re-derives; the membership file is an optimization

Drill-down re-streams the frozen scope and re-normalizes with the same
versioned configuration, returning the records whose identity matches —
deterministic and always consistent with the run. It is refused with
`analysis/stale-run` when the run's inputs moved (a moved dataset would
silently change the answer). A cached membership parquet is a WP8
optimization behind the same contract, invalidated by the existing
derived-artifact checksum machinery; it is deliberately NOT the source
of truth.

## Consequences

- Determinism tests can assert byte-identical summaries across reruns
  and shuffled inputs without tolerance windows.
- Pattern quality depends on mask-rule coverage; improving rules bumps
  the mask-set version and produces explicitly new identities rather
  than silently reshaping history.
- Drill-down costs a bounded re-scan until WP8 lands the membership
  cache; at the measured v0.3 scan rates this is interactive for the
  1M development corpus and bounded by run limits everywhere else.
