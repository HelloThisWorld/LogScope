# Third-party notices

LogScope bundles the following principal third-party components. The full,
exact dependency inventory for a given build can be produced with
`cargo tree` / `cargo license` and `npm ls` at that revision; this file
summarizes the components that ship inside the portable archive and their
licenses.

## Native / Rust

| Component | Use | License |
|-----------|-----|---------|
| DuckDB (bundled via `duckdb` crate) | analytical query engine | MIT |
| SQLite (bundled via `rusqlite`) | workspace metadata, FTS5 index | Public domain |
| Apache Arrow / Parquet (`arrow`, `parquet`) | columnar segment storage | Apache-2.0 |
| Tauri, wry, tao | desktop shell / WebView host | MIT OR Apache-2.0 |
| tonic, prost | OTLP gRPC/protobuf (experimental spike) | MIT / Apache-2.0 |
| opentelemetry-proto | official OTLP schemas | Apache-2.0 |
| axum, tokio, hyper | loopback OTLP HTTP receiver (spike) | MIT |
| BLAKE3 (`blake3`) | content hashing | CC0-1.0 OR Apache-2.0 |
| zstd (via `parquet`) | segment compression | BSD-3-Clause |
| serde, thiserror, chrono, csv, flate2, zip and other Rust crates | see `Cargo.lock` | MIT OR Apache-2.0 (typical) |

## Frontend

| Component | Use | License |
|-----------|-----|---------|
| React, ReactDOM | UI | MIT |
| @tauri-apps/api, plugin-dialog | command/event bridge | MIT OR Apache-2.0 |
| Vite, TypeScript | build tooling (not shipped) | MIT / Apache-2.0 |

## Microsoft Edge WebView2 Fixed Version Runtime (optional)

When the portable archive contains a `webview2` folder, it is the
Microsoft Edge WebView2 Fixed Version runtime, redistributed under the
Microsoft Edge WebView2 distribution terms. See
`webview2/LICENSE` inside that folder when present.

No component in the shipped application performs network access at
runtime; build tooling (cargo, npm) downloads dependencies at build time
only.
