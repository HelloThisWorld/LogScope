# LogScope

[![CI](https://github.com/HelloThisWorld/LogScope/actions/workflows/ci.yml/badge.svg)](https://github.com/HelloThisWorld/LogScope/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/HelloThisWorld/LogScope?display_name=tag&label=release)](https://github.com/HelloThisWorld/LogScope/releases/latest)

## Download the latest LogScope release

[**Open the latest release and download LogScope for x64**](https://github.com/HelloThisWorld/LogScope/releases/latest)

The latest Release page provides the Setup executable, the Portable ZIP,
release notes, and SHA-256 checksums together.

**Setup is a graphical extractor, not an installer.** It is a small launcher
with the same payload appended — the same idea as a self-contained Java
archive. It asks where to put LogScope, extracts, verifies every file against
`package-manifest.json`, and stops. It creates no MSI/MSIX transaction, no
registered uninstaller, no service, scheduled task, startup entry, firewall
rule, `PATH` entry, file association or mandatory registry state, and it needs
no administrator rights. To remove LogScope, delete the folder.

Both artifacts carry a byte-identical payload, so it makes no difference which
you choose. Nothing is signed, so Windows may display an Unknown Publisher or
SmartScreen warning. Download only from the official release above and verify
the `.sha256` sidecar published alongside each artifact.

**Status: 0.2.2 — early development.** LogScope is not feature complete and is
not a 1.0 product. Only the Log Explorer half of one profile works today; see
[Product profiles](#product-profiles) for exactly what is and is not available.

LogScope is an open-source, Windows-first, fully offline, local-first telemetry
case workbench published by `HelloThisWorld`. It turns exported or supplied logs
into deterministic, source-traceable datasets you can query on your own machine
— no account, no server, no network access, no built-in AI.

## Distribution

Both distributions are unsigned and relocatable. For a version `<version>`:

- `LogScope-<version>-windows-x64-setup.exe` — graphical extractor: pick a
  folder, watch progress, every file verified against its manifest;
- `LogScope-<version>-windows-x64-portable.zip` — ordinary ZIP: extract and run.

The setup executable embeds that exact ZIP byte-for-byte, and packaging fails
closed if the two ever diverge, so the resulting application tree is identical
either way.

The current source version is `0.2.2`. See the
[latest official Release](https://github.com/HelloThisWorld/LogScope/releases/latest)
for the published asset list and checksum.

Every release is unsigned and its notes say so explicitly. No MSI, MSIX, NSIS,
Inno Setup, registered uninstaller, service, scheduled task, startup entry,
firewall rule, `PATH` modification, file association, package manager, or
administrator access is required to run a release archive.

Each archive carries a `package-manifest.json` recording the exact packaged
file list with per-file SHA-256 hashes, and declares `installer: false` and
`signed: false`.

## Product profiles

LogScope has two, and only two, planned public product profiles. Their current
state is:

1. **Offline Log Case** — investigate exported or supplied logs without
   requiring metrics or traces. **Partially available.** Import and the
   interactive Log Explorer work; deterministic analysis, investigations,
   reports, case bundles and the bounded Agent interface are not built yet.
2. **Local OTel Session** — ingest local OpenTelemetry logs, metrics and traces
   into the same durable workspace. **Not available.** The OTLP receiver in the
   tree is an experimental spike (ADR-0009): disabled by default, loopback-only,
   unauthenticated, and its acknowledgements are not durability-backed. There is
   no metric pipeline, span store, watched-folder ingestion or rotation
   handling.

Adding a signal later will not require recreating a workspace.

## Core features

Available today at 0.2.2:

- import of generic JSONL, CSV, and Elasticsearch/ECS JSONL log exports through
  a versioned, declarative import-profile boundary;
- a versioned query language (free text, phrases, typed field comparisons,
  boolean logic with parentheses and implicit AND, existence and missing tests,
  severity bands, timestamp bounds, bounded wildcards, linear-time regexes,
  OTel/ECS-flavored aliases) with spanned diagnostics, stable error codes, and
  deterministic query fingerprints;
- a highlighted query editor with debounced authoritative validation, a UTC
  histogram with brush zoom, a virtualized keyset-paged event table, facets and
  field summaries, and record detail showing canonical fields, typed attributes,
  Resource/Scope, timestamp quality and provenance;
- source-order context with raw-byte excerpts and honest changed-file and
  missing-file reporting;
- a derived, rebuildable per-dataset field catalog powering trusted field
  resolution, suggestions and type-conflict detection;
- saved searches, column sets and bounded local recent searches, including a
  relative-to-newest-event time strategy;
- streamed, bounded, atomic CSV/JSONL export of the exact current filter and
  order, with truncation marking, overwrite protection, cancellation cleanup and
  a spreadsheet formula-injection guard;
- durable, cancellable, resumable jobs for import and index rebuild, with
  interrupted work discarded rather than half-applied on reopen.

Not built yet: deterministic analysis and findings, investigations and evidence
UX, timelines, reports, portable case bundles, redaction and disclosure
projection, metrics, traces, dashboards, the machine CLI, the Agent Query API,
the extension registry, backup and restore, integrity scanning and safe mode.
See [known limitations](docs/development/v1.0-implementation-plan.md).

## Portable mode

Extract the complete Portable ZIP to a writable directory and run
`logscope.exe`. The application directory is treated as read-only and
relocatable; workspaces are created wherever you choose and are never written
inside the application directory.

Portable use does not modify the registry, create shortcuts, register an
uninstaller, or require administrator access. Removing LogScope means deleting
the extracted folder; your workspaces, sources and exports are untouched.

If the archive was built without a bundled fixed WebView2 runtime, its
`package-manifest.json` records `"webview2": "evergreen-required"` and the
Microsoft Edge WebView2 Evergreen runtime must already be present on the
machine. That build is therefore **not** a fully offline first-launch artifact.
Building with `-WebView2FixedDir` records `"webview2":
"fixed-runtime-bundled"` and is offline from first launch.

## Build and test

Prerequisites: Rust stable with the MSVC target on Windows, Node.js 22+.

```powershell
cargo test --workspace --exclude logscope-desktop
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cd apps/desktop; npm install; npm run tauri dev
pwsh scripts/package-portable.ps1 -Version 0.2.2
```

Benchmarks use deterministic ChaCha8 corpora that are generated on demand and
never committed (ADR-0010):

```powershell
cargo run --release -p logscope-testsupport --bin bench_import -- logs 1000000
cargo run --release -p logscope-testsupport --bin bench_query -- 1000000
```

Routine CI (format, lint, full tests, desktop shell smoke) runs on a
self-hosted macOS runner. The Linux shared-core leg runs there too, inside a
`linux/arm64` container, and the Windows packaging job runs on a hosted
Windows runner. Both are `workflow_dispatch`-gated so they do not serialize
behind every pull request on a single runner.

## Privacy and security

LogScope does not collect log content, query text, file paths, workspace
contents, usage analytics or crash reports. Nothing is uploaded, because
nothing can be: there is no outbound network path in the product.

- fully offline at runtime — no dependency downloads, no telemetry, no account,
  no cloud service, no update check, and no background updater;
- DuckDB extension auto-install and auto-load are disabled at every connection
  open, `httpfs` is not compiled in, and automated probes assert this;
- the experimental OTLP receiver is disabled by default and binds loopback only
  when explicitly started. It is **unauthenticated** and must not be exposed;
- nothing is silently dropped or invented: unknown fields survive as typed
  attributes, imperfect records carry quality flags, rejected records keep exact
  source locators, and no synthetic spans or timestamps are fabricated;
- deterministic re-imports — identical source plus identical parser, profile and
  normalizer versions produce identical canonical values and record hashes;
- no operating-system identity is captured anywhere, asserted by test.

There is no built-in AI, model runtime or remote inference, and no Model Context
Protocol server, adapter or dependency. These are excluded by standing product
decision, not merely absent.

## Code signing policy

**LogScope releases are unsigned by design.** This is a settled project
decision, not a pending task: LogScope is distributed as a relocatable payload
that requires no installer, no elevation and no registered system state, so
Authenticode signing is not a release prerequisite. Windows may display an
Unknown Publisher or SmartScreen warning; that is expected.

Release integrity is established by:

- the SHA-256 checksum published beside each artifact;
- the per-file SHA-256 hashes recorded inside the artifact's
  `package-manifest.json`;
- deterministic, reproducible unsigned builds from declared inputs.

If signing is ever added it will be an isolated post-build stage producing
separately identified artifacts with their own checksums. An unsigned build will
never be relabelled as signed, and no placeholder certificate or "Signed" label
is ever used. See [ADR-0018](docs/adr/0018-release-signing-and-distribution-policy.md).

## License

LogScope is released under the **[MIT License](LICENSE)**.

See [docs/THIRD-PARTY-NOTICES.md](docs/THIRD-PARTY-NOTICES.md) for bundled
component licenses.

## Status

Architecture decisions live in [docs/adr/](docs/adr/) (0001–0017). Milestone
plans, acceptance matrices and the current blocker chain live in
[docs/development/](docs/development/); the newest is the
[v1.0 preflight record](docs/development/v1.0-implementation-plan.md), which
records why LogScope is not yet 1.0. Measured baselines are in
[docs/benchmarks/](docs/benchmarks/), and synthetic golden fixtures in
[fixtures/](fixtures/).
