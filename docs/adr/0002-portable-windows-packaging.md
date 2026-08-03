# ADR-0002: Portable-first Windows packaging, fixed WebView2, optional signing

Status: accepted (v0.0) · Date: 2026-07-29

## Context

The primary artifact must be an ordinary relocatable ZIP: no MSI, NSIS,
self-extractor, bootstrapper, registry entries, services, or shortcuts, and
no signing identity anywhere in build/test/release. First launch of the
fully offline artifact must not install or download WebView2. A macOS path
must stay open.

## Decision

1. `tauri.conf.json` sets `bundle.active = false`; Tauri's bundler is never
   used. The exe is produced by `npm run tauri build -- --no-bundle`.

   **Corrected 2026-08-03 (ADR-0018):** this step previously said
   `cargo build --release -p logscope-desktop`. A plain cargo build does
   not embed `frontendDist`, so the packaged exe loaded the UI from
   `devUrl` and every artifact from 0.0.0 through 0.2.1 shipped a
   development-mode binary that showed a WebView2 error page instead of
   LogScope. Packaging now goes through the Tauri CLI and asserts the
   frontend is embedded before archiving.
2. `scripts/package-portable.ps1` assembles
   `LogScope-<version>-windows-x64-portable.zip` from an explicit file list,
   producing a machine-readable `package-manifest.json` (per-file SHA-256)
   and a `.sha256` for the archive. ZIP entries use fixed order and fixed
   timestamps so identical inputs produce identical archives.
3. Fixed WebView2: the packager optionally embeds a Microsoft fixed-version
   runtime under `webview2/`. At startup (before the webview exists) the
   shell sets `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` to that folder when
   present — the wry/WebView2-documented fixed-version mechanism. Without
   the folder an existing system WebView2 is used; nothing is ever
   installed or downloaded at runtime.
4. Read-only app dir: mutable state (recent-workspace list, logs) lives in
   the platform app-config dir (`%APPDATA%\org.logscope.desktop`);
   workspaces only where the user chooses. No path is derived from the exe
   location except the optional bundled runtime lookup.
5. Signing is an optional post-process on the finished ZIP contents and is
   not part of any acceptance gate.
6. The package script is target-aware (`-Target` extension point); macOS
   later assembles `.app.zip`/DMG from the same shared code.

## Consequences

No installer/uninstaller lifecycle to maintain; upgrade = extract new
folder. Archive with bundled fixed runtime grows by roughly the runtime
size (~150-200 MiB script-measured when bundled); without it the archive
stays small but requires an existing WebView2 (standard on Windows 11) —
the manifest records which flavor was built.
