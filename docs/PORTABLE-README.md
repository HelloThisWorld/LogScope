# LogScope portable (Windows x64)

LogScope is a fully offline, local-first telemetry investigation tool.

## Running

1. Extract this ZIP anywhere — a folder with spaces or non-ASCII characters
   is fine. No installation, administrator rights, or network access is
   required or used.
2. Run `logscope.exe`.

The application directory can be treated as read-only. All mutable state
lives elsewhere:

- application configuration (for example the recent-workspace list):
  `%APPDATA%\org.logscope.desktop`
- workspaces: only the folders you explicitly create or open. Workspace
  paths are never derived from the application directory, and replacing or
  upgrading the application never touches a workspace.

## WebView2

If this archive contains a `webview2` folder, LogScope uses that bundled
fixed-version runtime and first launch performs no installation or
download. Without the folder, an existing system WebView2 (standard on
Windows 11) is used; nothing is downloaded either way.

## Verifying the download

Compare the archive's SHA-256 with the accompanying `.sha256` file. The
archive is intentionally unsigned; verification is by checksum.

## Removing

Delete the extracted folder. Optionally delete
`%APPDATA%\org.logscope.desktop` and any workspaces you created.
