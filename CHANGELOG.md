# Changelog

## 0.2.2 — 2026-08-04

**The first release whose user interface actually loads, and the first to
ship both public artifacts.** Every archive before this one — 0.0.0, 0.2.0
and 0.2.1 — shipped an executable that started and then displayed the
WebView2 "Hmmm… can't reach this page / localhost refused to connect" error
page instead of LogScope.

### Added
- **MIT license.** LogScope is released under the MIT License (`LICENSE`).
  `license = "MIT"` is declared in the workspace package metadata, inherited
  by all 13 crates, and set in `apps/desktop/package.json`. This closes the
  long-standing "license selection is pending" gap, which blocked any GA
  claim regardless of code readiness.
- **ADR-0018 — release signing policy and Eclipse-style graphical extractor.**
  Settles two questions open since v0.0:
  - **Signing is not a GA prerequisite.** LogScope releases are unsigned by
    settled policy, not by omission. Integrity comes from published SHA-256
    checksums, per-file hashes in `package-manifest.json`, and deterministic
    reproducible builds. An unsigned build is never relabelled as signed.
  - **The graphical setup is a shell around the same ZIP payload** — a
    launcher stub with the compressed payload appended, in the spirit of a
    self-contained Java archive. Explicitly not an MSI, MSIX, NSIS,
    bootstrapper or registered uninstaller. Both public artifacts wrap one
    canonical payload, so byte-equivalence is a design property rather than
    a coincidence.
- **The graphical setup extractor exists** (`tools/logscope-setup`),
  closing `v0.0-G004` — the oldest open gate in the repository — under the
  ADR-0018 design. A small Win32 launcher (destination picker, progress
  window, cancel) reads the ZIP payload appended to its own executable via
  a fixed 70-byte trailer, verifies the payload's SHA-256 **before**
  extraction begins, extracts to a staging directory next to the
  destination, verifies every file against `package-manifest.json`, and
  only then moves it into place; failure or cancel rolls back and leaves
  nothing behind. It installs no registry state, service, shortcut or
  uninstaller and needs no administrator rights.
- **Packaging emits both public artifacts and fails closed on divergence.**
  `scripts/package-portable.ps1` now also produces
  `LogScope-<version>-windows-x64-setup.exe` by appending the portable ZIP
  to the launcher stub — through the same Rust code that later reads it,
  so the trailer has exactly one definition — and aborts unless the
  embedded payload's SHA-256 equals the portable ZIP's.

### Fixed
- **The setup executable crashed at launch** with
  `STATUS_ENTRYPOINT_NOT_FOUND` (`0xC0000139`), before `main` ran. rfd is
  built with its `common-controls-v6` feature, so the stub imports
  `TaskDialogIndirect` — which only comctl32 **v6** exports, and Windows
  only binds v6 when the executable's manifest declares the dependency. No
  manifest was embedded, the loader bound v5, and the import could not be
  resolved. A minimal application manifest is now embedded at link time
  (`tools/logscope-setup/manifest.xml` via `build.rs`, MSVC-only),
  `InitCommonControlsEx(ICC_PROGRESS_CLASS)` is called before the progress
  window is created as comctl32 v6 requires, and the stub is built with
  `windows_subsystem = "windows"` so it no longer drags a console window
  behind its dialogs. Caught by a launch smoke test **before** this
  version was published; every unit test passed throughout, because unit
  tests exercise the library, not executable loading.
- **`-SkipBuild` could package a stale extractor stub.** The flag skipped
  the stub build along with the expensive Tauri build and then silently
  packaged whatever `target/release/logscope-setup.exe` was lying around —
  the same fail-open species as the development-mode packaging defect
  below. The stub (a seconds-long build) is now always rebuilt;
  `-SkipBuild` only skips the Tauri build.
- **Packaged executables were built in development mode.**
  `scripts/package-portable.ps1` built the shell with
  `cargo build --release -p logscope-desktop`. A plain cargo build does not
  embed `frontendDist`; the resulting binary loads the UI from `devUrl`
  (`http://localhost:5173`), which nothing is serving on an end-user machine.
  Packaging now builds through the Tauri CLI
  (`npm run tauri build -- --no-bundle`), which embeds the frontend.
- **Packaging now fails closed.** The script parses the asset references out
  of the built `index.html` and asserts every one is present inside the
  executable, so a development-mode binary can no longer be packaged. Verified
  to reject the previously shipped 0.2.0 executable and accept the corrected
  one. This defect was invisible until first launch, which is why it survived
  from 0.0.0: the packaging step succeeded and the application did start.

### Note on 0.2.1
Tag `v0.2.1` exists but has no published release. Its archive carried the same
unusable executable and was withdrawn before use rather than being replaced in
place. The 0.2.1 source changes below are all included here.

## 0.2.1 — 2026-08-03

Patch release. One user-visible correctness fix in the bounded query path.

### Fixed
- **Cancellation raised before a query entered execution was silently
  dropped.** DuckDB clears any pending interrupt when a query begins, so a
  cancel landing in the window between the request and the engine starting
  work was discarded and the query then ran to completion. Both call sites
  cancel exactly once from a polling thread and never retry, so the
  cancellation was ignored for the whole remaining budget — up to 600 s for
  field statistics and 24 h for export. `run_bounded` now short-circuits
  when cancellation was already requested, and its watchdog re-asserts the
  interrupt while cancellation is pending. A query that finishes before the
  interrupt lands still returns its value; cancellation stays best-effort.

### Added
- Dispatch-gated Linux shared-core CI leg (`linux-core`), covering path
  canonicalization, encodings, timezones, line endings and file identity on
  a case-sensitive filesystem. Not yet executed: GitHub-hosted minutes are
  unavailable under the Actions billing limit.
- `docs/development/v1.0-implementation-plan.md` — the v1.0 preflight
  record, public contract census and 160-item acceptance assessment.

### Correction to the 0.2.0 entry below
The 0.2.0 "Unchanged / preserved" line claims packaging is "setup EXE +
portable ZIP from one payload". **There is no setup EXE.** Only the portable
ZIP path exists (`scripts/package-portable.ps1`); the graphical extractor is
unbuilt. The entry is left as written rather than rewritten, and corrected
here.

## 0.2.0 — 2026-07-29

First usable Log Explorer: import a log export, query it interactively,
entirely offline.

### Added
- **Query language v1** (`logscope-query-lang`): free text and phrases,
  typed field comparisons, boolean logic with parentheses and implicit
  AND, existence/missing tests, severity bands, timestamp bounds,
  bounded wildcards and linear-time regexes, aliases (OTel/ECS flavored),
  spanned diagnostics with stable codes and hints, deterministic
  fingerprints, versioned grammar.
- **Log Explorer**: highlighted editor with debounced authoritative
  validation, UTC histogram with brush zoom, virtualized keyset-paged
  event table, facets and field summaries, record detail (canonical
  fields, typed attributes, Resource/Scope, timestamp quality,
  provenance), source-order context with raw-byte excerpts and honest
  changed/missing-file reporting.
- **Field catalog**: derived, rebuildable per-dataset attribute statistics
  (types, counts, bounded distinct, safe examples) powering trusted field
  resolution, suggestions, and type-conflict detection.
- **Saved state**: saved searches (query text + dataset selection + time
  strategy incl. relative-to-newest-event), column sets, bounded local
  recent searches.
- **Export**: streamed, bounded, atomic CSV/JSONL of the exact current
  filter and order, with truncation marking, overwrite protection,
  cancellation cleanup, and a spreadsheet formula-injection guard.
- Built-in generic **Elasticsearch export** import profile (ECS names).
- Workspace **migration 0002** (transactional, forward-only) with derived
  index lifecycle; pre-0.2 workspaces stay searchable without re-import
  (exact-scan text search until the one-time FTS rebuild).
- Benchmark harness `bench_query` with recorded 1M/5M measurements.

### Changed
- FTS index v2 tokenizer (`unicode61 remove_diacritics 0`) so indexed and
  fallback text search share one documented token semantics; v1 index
  files are detected and rebuilt as a cancellable, resumable job.
- Queries run on a pooled set of hardened DuckDB connections with
  per-request cancellation; DuckDB's `compressed_materialization`
  optimizer is disabled (bundled-build assertion with NULL integer sort
  keys).

### Unchanged / preserved
- Parquet storage schema v1 (no data rewrite), portable-first packaging
  (setup EXE + portable ZIP from one payload), zero network access at
  runtime, platform-neutral shared core.
