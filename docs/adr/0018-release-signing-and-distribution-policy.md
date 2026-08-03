# ADR-0018: Release signing policy and Eclipse-style graphical extractor

Status: accepted (v0.2.2) · Date: 2026-08-03
Supersedes the "optional signing" ambiguity in ADR-0002 and fixes the
distribution design left open since v0.0.

## Context

Two release questions had stayed open since the foundation milestone and
were blocking any General Availability claim:

1. **Signing.** ADR-0002 described signing as "optional", which left the
   GA gate ambiguous: a release policy that *requires* a signed artifact
   would block GA until a certificate exists, and no certificate has ever
   been issued for this project. An ambiguous policy cannot be satisfied,
   only argued about.
2. **The second distribution artifact.** Every milestone from v0.0 onward
   requires the graphical setup and the Portable ZIP to derive from one
   payload and extract to byte-equivalent trees (`v0.0-G004` and its
   restatement in all eleven later stages). Only the ZIP path was ever
   built, so that gate has never been satisfiable. The full stage audit
   established this as the oldest unpaid gate in the repository and the
   single highest-leverage item in the remediation chain.

## Decision

### 1. LogScope releases are unsigned, by settled policy

Signing is **not** a GA prerequisite and its absence is **not** a release
blocker. LogScope ships a relocatable payload that requires no installer,
no elevation, no service, no registered uninstaller and no mandatory
registry state, so Authenticode signing buys no integrity property that
the published checksums do not already provide.

Release integrity is established by:

- the SHA-256 checksum published beside each artifact;
- the per-file SHA-256 hashes inside the artifact's
  `package-manifest.json`;
- deterministic, reproducible unsigned builds from declared inputs.

Consequences:

- Windows will display Unknown Publisher / SmartScreen warnings. This is
  expected and is documented in the README and release notes rather than
  worked around.
- If signing is ever added it is an isolated post-build stage producing
  **separately identified** artifacts with their own checksums.
- An unsigned build is never relabelled as signed. No placeholder
  certificate, empty signature block, or "Signed" label is ever produced.
  A signing workflow without a certificate is not signing.

### 2. The graphical setup is an Eclipse-style extractor over one payload

The canonical payload is assembled **once**. Both public artifacts are
containers around that same byte-identical tree:

```
canonical payload tree  (the only source of truth)
    ├── LogScope-<version>-windows-x64-portable.zip   ordinary ZIP
    └── LogScope-<version>-windows-x64-setup.exe      extractor + same ZIP
```

The setup executable is a **shell around a ZIP payload**, in the same
spirit as a self-contained Java archive: a small launcher stub with the
compressed payload appended, which the stub locates, verifies and unpacks.
It is explicitly **not**:

- an MSI, MSIX, NSIS or Windows Installer transaction;
- a network bootstrapper;
- a registered uninstaller;
- anything that creates a service, scheduled task, startup entry,
  firewall rule, `PATH` entry, file association, or mandatory registry
  state.

Required behavior:

- prompt for a destination and refuse unsafe or non-writable targets;
- show overall progress, the current relative path, and a detailed
  extraction log;
- verify every extracted file against `package-manifest.json` before
  reporting success;
- leave no launchable partial installation after cancellation, disk-full,
  permission failure, corrupt payload, or checksum mismatch;
- run without administrator rights and without any network access;
- never overwrite unknown files in a non-empty destination without an
  explicit, precise confirmation.

Because both artifacts wrap the same payload, byte-equivalence between
them is a property of the design rather than a coincidence to be tested
after the fact — but it is still asserted by an executed check, because a
design property that nothing verifies is an assumption.

### 3. Packaging must build through the Tauri CLI

Recorded here because ADR-0002 stated the wrong command and every artifact
produced between v0.0 and 0.2.1 was defective as a result.

`cargo build --release -p logscope-desktop` does **not** embed
`frontendDist`. The resulting executable loads the UI from `devUrl`
(`http://localhost:5173`), which nothing serves on an end-user machine, so
the packaged application starts and then displays a WebView2
"can't reach this page" error instead of LogScope. Packaging therefore
builds through the Tauri CLI (`npm run tauri build -- --no-bundle`), and
`scripts/package-portable.ps1` **fails closed**: it parses the asset
references out of the built `index.html` and asserts each is present
inside the executable before packaging.

## Consequences

- The GA signing gate is **satisfied by policy**, not deferred. It can be
  marked PASS with this ADR as its evidence.
- `v0.0-G004` and its eleven restatements remain **open** until
  `tools/logscope-setup` exists, but they are now unblocked: the design is
  decided and no external dependency remains.
- The license is MIT (see `LICENSE`), so payload redistribution inside the
  setup executable carries no additional obligation beyond the existing
  third-party notices.
