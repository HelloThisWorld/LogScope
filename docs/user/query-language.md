# LogScope query language (v1)

One line of query text filters everything at once: the event table, the
histogram, facets, field summaries, counts, and exports. The Rust engine
is the only interpreter; the editor merely displays its tokens and
diagnostics.

## Quick reference

```
timeout                                  free-text token
"connection timed out"                   phrase (consecutive tokens)
service.name:workflow                    field equals value
service.name = "workflow"                same thing (`:` and `=` are aliases)
severity != DEBUG                        not equal (missing values match !=)
duration_ms >= 500                       numeric comparison
timestamp >= "2026-07-29T00:00:00Z"      timestamp bound (RFC 3339 or YYYY-MM-DD)
trace_id:*                               field exists (and is not empty-valued)
NOT user.id:*                            field missing
message:/timeout|deadline/i              regular expression (`i` = ignore case)
host.name:web-*                          wildcard (* any run, ? one character)
(severity:ERROR OR severity:WARN) AND service.name:checkout
severity:(ERROR OR WARN)                 value group, same as the line above
```

## Rules that matter

- **Precedence**: parentheses, then `NOT`, then `AND`, then `OR`.
  Writing predicates next to each other means `AND`.
- **Keywords** are uppercase `AND` / `OR` / `NOT` only; lowercase `and`
  is a search term.
- **Free text** matches whole tokens in the message, case-insensitively.
  Tokens are runs of letters/digits; `web-01` is the tokens `web`,`01` in
  order. Diacritics are significant (`café` does not match `cafe`).
  Substring matching is what regexes are for.
- **`message:`** uses the same token semantics; every other string field
  compares exactly (and case-sensitively, except severity).
- **Severity** understands levels: `severity:ERROR` matches the ERROR
  band (OTLP numbers 17–20, or the literal text when a record has no
  number); `severity >= WARN` and `severity >= 13` also work.
- **Missing vs empty**: `field:*` means the field is present with a real
  value; `field:""` matches a present-but-empty string; a bare `NOT
  field:0` also matches records without the field at all.
- **Field names**: canonical names and common aliases (`level`,
  `log.level` → severity; `@timestamp`, `time` → timestamp; `msg`, `body`
  → message; `trace.id`, `span.id`, …) resolve automatically. Dynamic
  attributes use their dotted path. If an alias hides one of your real
  attributes, `attr.level:` reaches the attribute. Unknown fields are
  errors with suggestions — never silently empty results.
- **Types come from your data**: comparing a string field with `>=` or a
  number against a boolean is a diagnostic that names the field, its
  type, and the offending value.
- **Escaping**: inside quotes use `\"` `\\` `\n` `\t` `\r`. In bare
  words, `\*` and `\?` are literal characters instead of wildcards.
  Values containing spaces or punctuation belong in quotes.
- **Regular expressions** run on a linear-time engine: backreferences and
  lookaround do not exist here, patterns are length- and size-limited,
  and a leading `.*` or wildcard earns a cost warning. Regexes match
  anywhere in the raw field text (not tokens).
- **Time**: the time-range control and any `timestamp` predicates
  combine with AND. Ranges are half-open `[start, end)` in UTC. Records
  without a valid timestamp appear in "all data" queries, are excluded
  from bounded ranges, and the omitted count is always shown.

## Limits

Query text ≤ 4096 bytes, ≤ 512 tokens, nesting ≤ 32, ≤ 128 predicates,
wildcards ≤ 128 chars, regexes ≤ 512 chars (compiled size capped).
Execution carries a deadline and can always be cancelled; queries never
freeze the app just because their syntax was valid.

## Not supported (v1)

Field-to-field comparison, arithmetic, `IN` lists (use `f:(a OR b)`),
array element addressing, per-query case-sensitivity switches, and field
names containing spaces, quotes, or wildcard characters (such fields
remain visible in details; their names just cannot be written in v1).
