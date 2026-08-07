# ADR-0021 — Correlation confidence classes and behavioural signals

Status: accepted (v0.4 WP4) · Date: 2026-08-08

## Context

v0.4 relates log records to each other: which records concern the same
request, which look like retries, which repeat, where a clock moved
backwards, where the record stream went quiet.

Everything in that list is one careless sentence away from a claim the
data does not support. Two records sharing an ID is not an ordering.
Two records saying the same thing is not a retry. A quiet interval is
not an absence of activity. A timestamp that moves backwards is not a
proven clock problem. LogScope is used to reconstruct what happened
during incidents, so a result that reads as stronger than its evidence
is worse than no result at all.

The gates this ADR answers (v0.4 plan, 19–27) are therefore mostly
about what the system must *refuse* to say.

## Decision

### 1. Three confidence classes, and the type system enforces the ceiling

`Exact` requires canonical telemetry identity — a 32-hex trace ID, or a
trace+span pair, validated at ingest and re-checked in the rule layer.
`Correlated` requires a typed, stable application or transport
identifier matched exactly on its canonical value. `Probable` is a
documented bounded rule over compatible fields and time proximity.

Two of these are enforced structurally rather than by a check:

- **There is no `SpanId` selector variant.** A span ID is unique only
  within its trace, so it cannot select a group. Rather than accept one
  and reject it at runtime, the variant does not exist; the `span_id`
  config name is refused *by name* with the reason and the fix
  (`trace_span`), because it is a common enough mistake to deserve a
  real answer instead of "unknown key".
- **`ProbableNeighbor` has no confidence field and no constructor that
  sets one.** `confidence()` returns `Probable` and there is no other
  value it could return. Gate 22 — time proximity alone never reaches
  Exact or Correlated — is thus a property of the type rather than a
  rule someone could forget to apply.

Normalization is explicit, versioned, off by default, and **refused
outright on canonical identifiers**: those are normalized at ingest, and
altering them here would make an exact relationship mean something
else. On correlated keys the steps that actually changed a value travel
into the explanation.

### 2. Candidate generation is key-partitioned, never pairwise

One bounded streaming pass buckets records by validated key. Cost is
linear in records scanned, and no join can blow up on a hot key. Edges
connect consecutive records only — the bounded honest representation of
"previous/next in this group".

The duplicate rule follows the same discipline: members are bucketed on
a message digest and only consecutive bucket members are compared.
Inside a 256-member group the pairwise form would be 32,640 comparisons
for the same answer.

Every cap (events per group, groups per run, edges per event, total
edges, total signals, records scanned) reports what it dropped. Signals
are budgeted separately from edges because they answer a different
question, and one shared budget would let a noisy group's signals
silently consume the sequence's edges.

Ordering is event time, then record ID — a deterministic content
address, so the tie-break is stable across machines. **Records with no
event time are counted but never sequenced**: ordering them by import
time would manufacture a plausible story the data does not support.

### 3. Signals carry an evidence ladder, and only the top rung is an observation

`sig-rules` v1 defines four signals, each with its own rule ID: retry,
operational duplicate, clock skew, gap. Each result reports an
`EvidenceStrength`:

| Rung | Meaning | Reported as |
| --- | --- | --- |
| `documented` | the source stated the fact in a typed field | an observation |
| `corroborated` | several independent typed fields agree | investigative lead |
| `indicative` | one weak but deterministic indicator | investigative lead |

Every result names the fields that matched **and the fields the rule
looked for but did not find**, so a reader sees how much of the
evidence was actually present rather than only what survived.

Consequences that fall out of the ladder:

- **Retry has nowhere to put a message.** `RetryFacts` carries
  attempt/operation/outcome and no text at all, so "these two records
  say the same thing" cannot become a retry however alike they read
  (gate 24, by construction). Only an advancing typed attempt counter
  reaches `documented` — and a counter that does *not* advance returns
  no signal at all, because the source is saying these are the same
  attempt logged twice, which is better evidence than the operation
  match is.
- **A gap can never reach `documented`.** Absence of records is not a
  record of absence; the limitation names collection failures,
  filtering, sampling and retention explicitly.
- **Skew never reaches `documented` either.** The arithmetic is exact,
  but attributing it to a clock is a reading, not a record.

### 4. An ingestion duplicate is not an operational duplicate

The same source line imported twice says something about LogScope. The
same message emitted twice by the system says something about the
system. Conflating them would turn an import artefact into a finding.

`classify_duplicate` therefore **classifies rather than filters**,
returning a distinct `Ingestion` variant — for the same source
position, and for a same-source pair whose distinctness cannot be shown
because neither carries a record number. Import's own
`QualityFlag::DuplicateRecord` verdict also excludes a record from the
rule. Both paths are counted in their own `ingest_duplicates_excluded`
run counter, so the exclusion stays visible instead of vanishing.

### 5. Time is reported, never rewritten

`TimeObservation` carries both original timestamps, the measured delta,
the tolerance it was judged against, and both quality values. **It has
no field in which an adjusted time could be returned** (gate 26).

Timestamp quality comes from ingest rather than being re-derived:
`TimezoneAssumed` and `TimestampUnparsed` drop a time-based claim from
`corroborated` to `indicative`, and `OutOfOrderTimestamp` corroborates
a skew finding. Import already decided these things once; deciding them
again in analysis would let the two disagree.

### 6. Neighborhoods are drill-downs, not artifacts

A probable neighborhood is anchored to one record a person selected, so
materialising every neighborhood would be a cross product written to
disk. Instead it is a bounded two-pass query against the run's frozen
scope: locate the anchor, then narrow the run's own window to the
anchor's tolerance interval so the engine's time filter does the work.
A drill-down can never widen the run.

A required compatible field that a candidate does not carry is a
rejection, never a wildcard. Ordering is absolute distance from the
anchor first, so a truncated neighborhood drops its least relevant
members rather than an arbitrary tail.

## Consequences

- Correlation results are reproducible and explainable, and every
  explanation states the rule, the key, the applied normalization, and
  the standing limitation that a shared identifier is not ordering,
  parent/child structure, completeness, or causation. Trace groups add
  that this is not a reconstructed trace.
- A forbidden-wording assertion covers every generated string in the
  correlation and signal modules and every reason surfaced by a run
  (`caused by`, `root cause`, `therefore`, `proves`, `confirms that`,
  `resulted in`).
- Accepted limitation: LogScope will under-report. A retry the source
  did not label, a duplicate whose text differs by a request ID, a
  skewed clock in a source without record numbers — these produce
  nothing. That is the deliberate trade against ever over-stating, and
  it is the same trade ADR-0020 makes for templates.
- Per-member signal detail is bounded by
  `max_events_per_group × max_groups` and held in memory for the
  duration of a run. Interning the repeated dataset/source strings is
  the known fix and belongs with the WP8 reliability work.
- `RESULTS_SCHEMA_VERSION` is 2. A run produced before signals existed
  is refused with a re-run instruction rather than returning an empty
  page: "none were computed" and "none were found" are different
  answers and must not look alike.
