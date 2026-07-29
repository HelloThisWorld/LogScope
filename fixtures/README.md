# LogScope Golden Fixtures

Small, reviewable, fully synthetic fixtures. No real company, customer,
host, user, path, token, or operational data appears here; every value was
invented for these tests.

Byte fidelity: `.gitattributes` marks `fixtures/**` as binary (`-text`) so
CRLF, BOM, UTF-16, and partial final records survive exactly.

## Inventory

| File | Covers | Consumed by |
|------|--------|-------------|
| `logs/ecs.jsonl` | ECS-shaped JSON logs (`@timestamp`, `log.level`, nested fields) | v0.0 JSONL path (`builtin.jsonl.generic`) |
| `logs/nodejs-structured.jsonl` | Node.js (pino-style) numeric-level JSON logs, epoch-ms timestamps | v0.0 JSONL path |
| `logs/dotnet-structured.jsonl` | .NET structured JSON logs (`Timestamp`, `Level`, `EventId`) | v0.1 profile target (fields preserved as attributes in v0.0) |
| `logs/go-structured.jsonl` | Go slog/zap-style JSON logs | v0.0 JSONL path |
| `logs/python-json.jsonl` | Python JSON logs incl. multiline traceback inside a JSON string | v0.0 JSONL path |
| `logs/python-text-exception.log` | Python plain-text traceback (multiline) | v0.1 text/multiline parser |
| `logs/java-logback-multiline.log` | Logback/Log4j2 text with a multiline stack trace | v0.1 text/multiline parser |
| `logs/quarkus-jboss.log` | Quarkus/JBoss console format | v0.1 text parser |
| `logs/kubernetes-cri.log` | Kubernetes CRI (`<ts> <stream> <P/F> <line>`) | v0.1 text parser |
| `logs/generic.csv` | Generic CSV logs with headers (`builtin.csv.basic`) | v0.0 CSV path |
| `logs/es-discover.csv` | Elasticsearch Discover CSV export shape | v0.1 profile target |
| `logs/business-events.csv` | Generic CSV business events (typed columns) | v0.1 profile target |
| `logs/malformed.jsonl` | Invalid JSON line + truncated final record (no newline) | v0.0 reject path |
| `logs/malformed.csv` | Ragged rows and unbalanced quote | v0.0 reject path |
| `logs/utf8-bom-crlf.jsonl` | UTF-8 BOM + CRLF line endings | v0.0 JSONL path |
| `logs/utf16le-bom.log` | UTF-16 LE with BOM (documented variant) | v0.1 encoding support |
| `logs/missing-timezone.log` | Naive local timestamps (no zone info) | v0.1 text parser + zone policy |
| `logs/dst-boundary.jsonl` | Naive timestamps inside the Europe/Berlin DST overlap/gap | timestamp normalizer tests |
| `traces/kafka-spans-links.jsonl` | OTLP JSONL producer/consumer spans joined by links | OTLP spike |
| `traces/problem-spans.jsonl` | Orphan, duplicate, incomplete, out-of-order, clock-skewed spans | OTLP spike + graph reconstruction |

Large corpora are never committed; they are generated on demand from fixed
seeds by `logscope-testsupport` (see `src/gen_*.rs` and the
`bench_import` binary).
