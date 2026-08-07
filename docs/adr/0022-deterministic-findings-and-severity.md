# ADR-0022 — Deterministic findings and what severity means

Status: accepted (v0.4 WP5) · Date: 2026-08-08

## Context

A finding is the most dangerous object LogScope produces. It is the
thing a reader is most likely to paste into an incident review, and the
thing most likely to be read as *"the tool found the cause"*. Patterns,
comparisons and correlations at least look like measurements. A finding
looks like a conclusion.

Everything below is therefore about limiting what a finding is able to
say, not about what it is able to detect.

## Decision

### 1. Severity ranks evidence, and the type cannot express impact

`FindingSeverity` is four generic bands — `informational`, `low`,
`medium`, `high`. There is no `critical`, no `customer_impacting`, no
`security`. **The type has no field in which a business claim could be
placed, so no rule can make one.**

Severity is a pure function of two things: the canonical log severity
ceiling of the records involved, and the size of the measured change.

| ceiling ＼ magnitude | small | moderate | large |
| --- | --- | --- | --- |
| fatal | medium | high | high |
| error | low | medium | high |
| warn | low | low | medium |
| info / debug / trace / unknown | informational | informational | low |

Two properties fall out of the table and are pinned by tests:

- **Nothing below `warn` can reach `medium` or `high`**, whatever the
  magnitude. A million new DEBUG lines is a large change and still not
  a severe one. Severity is capped by the evidence, not by size.
- The ceiling comes from **OTLP severity numbers**, not from source
  text, so it means the same thing whatever words the source used.
  Numbers outside the defined 1–24 bands are `Unknown` rather than
  guessed.

Every rendered finding carries `SEVERITY_MEANING` verbatim: *"Severity
ranks findings by the canonical log severity involved and the size of
the measured change. It is not a statement about customer, financial, or
security impact."* Without that sentence, "high" is an invitation to
read impact into a log count.

### 2. A finding is never more confident than its evidence

`FindingConfidence` is carried in from the analysis that produced the
input and a rule cannot promote it. Signal-derived findings map straight
across from `sig-rules`' own ladder (`documented` → `measured`,
`corroborated`, `indicative`).

`Measured` means the underlying values are exact counts — confidence in
the *measurement*, never in an interpretation of it. The limitations
carry that distinction.

### 3. Limitations are carried, never invented and never dropped

Each rule has its own limitation naming the thing a reader is most
likely to over-read from it, and signal-derived findings additionally
carry the *signal's* own limitation from `sig-rules`. The standing
`FINDING_LIMITATION` follows every one of them.

This is why a finding is a struct with a `limitations: Vec<String>`
rather than prose assembled at render time: a limitation that can be
dropped by a caller is not a limitation.

### 4. The forbidden-wording test needs two lists, not one

The obvious guard — ban `impact`, `customer`, `root cause`, … from every
generated string — **fails against its own disclaimer.** The sentence
that keeps a finding honest is *"It is not a statement about customer,
financial, or security impact"*, and it necessarily contains `customer`
and `impact`.

A disclaimer has to be allowed to name the thing it disclaims. So the
test splits:

- **Cause and certainty claims** (`confirmed`, `root cause`, `caused
  by`, `therefore`, `because of`, `proves`, `resulted in`, `must be
  fixed`, `we recommend`) are banned from *every* string, in any form.
  These cannot appear even in a denial without reading as a claim.
- **Business-impact vocabulary** (`impact`, `customer`, `revenue`,
  `breach`, `outage`, `critical`, `urgent`, `resolved`, `mitigat`,
  `incident`) is banned from the text that *asserts* something — the
  title, the subject, the calculation, the severity label — where it
  could only ever be a claim.

Banning the second list everywhere would have deleted the guard instead
of enforcing it. This was found by the test failing against the
implementation, which is the right way round.

### 5. Findings are reproducible from their own record

Gate 28 asks that a finding preserve rule, inputs, calculation,
thresholds, severity, confidence, records, limitations and origin. Each
is a field of `Finding`, so the obligation is met by construction rather
than by remembering to populate a log line:

- `inputs` are named values sufficient to recompute the decision;
- `calculation` states how the decision was reached in the same integer
  terms the rule used — never a restatement of the title;
- `thresholds` are the values actually applied, so a reader can see what
  would have had to differ for the rule not to fire;
- `origin` names the analysis kind, run ID, and the source rule set and
  version that produced the input.

All arithmetic is integer. Concentration shares are basis points
computed through `u128`, never a float.

Supporting records are bounded at `MAX_FINDING_RECORDS` (20) with the
remainder counted in `records_truncated`. A finding points at evidence;
it does not reproduce a dataset.

### 6. Ranking is deterministic and severity-led

Severity descending, then confidence descending, then rule name, then
subject. Never discovery order, and stable across runs and machines.

## Consequences

- LogScope will under-report. A rule fires only when its thresholds are
  met, magnitude bands are conservative, and nothing below `warn` can
  reach `medium`. That is the deliberate trade against ever
  over-stating, and it is the same trade ADR-0020 makes for templates
  and ADR-0021 for correlation.
- Severity is comparable across findings but is **not** a work-ordering
  signal. Ordering work needs impact, and impact is exactly what this
  design refuses to guess.
- The seven `find-rules` v1 rules are additive: a v2 may add rules or
  re-band magnitudes, but changing the severity table changes the
  meaning of stored findings and therefore requires a version bump, not
  an edit.
- Accepted limitation, to be resolved when execution lands: the severity
  ceiling has to come from somewhere. Reading it from a bounded
  drill-down sample can only ever *miss* a higher severity, so a
  sampled ceiling can understate a finding's severity but never
  overstate it. That one-way error is acceptable; the alternative —
  aggregating severity into the comparison artifact — is a WP3 schema
  change and is the better long-term answer.
