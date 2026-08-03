# LogScope portable (Windows x64)

LogScope is a local-first telemetry investigation tool that makes no network
requests while running.

**Early development.** LogScope is not feature complete and is not a 1.0
product. Import and the interactive Log Explorer work; deterministic analysis,
investigations, reports, case bundles, metrics, traces, dashboards, the CLI and
the Agent API are not built yet. See the project README for the full list.

## Running

1. Extract this ZIP anywhere — a folder with spaces or non-ASCII characters
   is fine. No installation or administrator rights are required, and the
   application makes no network requests once running. See the WebView2 note
   below for the one first-launch prerequisite.
2. Run `logscope.exe`.

The application directory can be treated as read-only. All mutable state
lives elsewhere:

- application configuration (for example the recent-workspace list):
  `%APPDATA%\org.logscope.desktop`
- workspaces: only the folders you explicitly create or open. Workspace
  paths are never derived from the application directory, and replacing or
  upgrading the application never touches a workspace.

## WebView2

Check `package-manifest.json` in this folder for the `webview2` field.

- `"fixed-runtime-bundled"` — this archive contains a `webview2` folder with a
  bundled fixed-version runtime. First launch performs no installation or
  download, and the archive is fully offline.
- `"evergreen-required"` — this archive does **not** bundle a runtime and
  relies on the Microsoft Edge WebView2 Evergreen runtime already being
  present. It ships with Windows 11, so first launch normally works with no
  download. On a machine without it — some Windows 10 installations, or images
  where it was removed — LogScope will not start until the runtime is
  installed, and this archive is therefore not a fully offline first-launch
  artifact.

Either way, LogScope itself makes no network requests.

## Verifying the download

Compare the archive's SHA-256 with the accompanying `.sha256` file. The
archive is intentionally unsigned; verification is by checksum.

## Removing

Delete the extracted folder. Optionally delete
`%APPDATA%\org.logscope.desktop` and any workspaces you created.
