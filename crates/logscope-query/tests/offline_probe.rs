//! Executable evidence for the offline storage/query architecture (ADR-0006).
//!
//! Proves, on the exact binaries we ship:
//! 1. parquet and json work with extension auto-install/auto-load disabled
//!    (statically linked into the bundled build);
//! 2. the DuckDB `fts` extension is NOT available offline (would require a
//!    runtime INSTALL, i.e. a download) — which is why full-text search is
//!    built on SQLite FTS5 instead;
//! 3. SQLite FTS5 is available in the bundled rusqlite;
//! 4. in-flight DuckDB queries can be interrupted from another thread.

use logscope_query::EngineConnection;

#[test]
fn hardening_disables_extension_autoinstall_and_autoload() {
    let engine = EngineConnection::open_in_memory().unwrap();
    let state = engine.hardening_state().unwrap();
    assert_eq!(
        state,
        vec![
            (
                "autoinstall_known_extensions".to_string(),
                "false".to_string()
            ),
            ("autoload_known_extensions".to_string(), "false".to_string()),
        ]
    );
}

#[test]
fn parquet_and_json_work_without_any_install_or_load() {
    let engine = EngineConnection::open_in_memory().unwrap();
    let conn = engine.raw();

    // JSON functions are statically available.
    let v: String = conn
        .query_row(
            "SELECT json_extract_string('{\"a\": {\"b\": 7}}', '$.a.b')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, "7");

    // Parquet write + read round trip on disk, no extension loading.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("probe.parquet");
    let path_sql = path.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!(
        "COPY (SELECT range AS n, 'row-' || range AS label FROM range(100)) \
         TO '{path_sql}' (FORMAT PARQUET);"
    ))
    .unwrap();
    let count: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM read_parquet('{path_sql}')"),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 100);
}

#[test]
fn duckdb_fts_extension_is_not_available_offline() {
    let engine = EngineConnection::open_in_memory().unwrap();
    let conn = engine.raw();

    // Not loaded, not installed in the bundled build.
    let mut stmt = conn
        .prepare(
            "SELECT loaded, installed FROM duckdb_extensions() \
             WHERE extension_name = 'fts'",
        )
        .unwrap();
    let state: Option<(bool, bool)> = stmt
        .query_map([], |r| Ok((r.get::<_, bool>(0)?, r.get::<_, bool>(1)?)))
        .unwrap()
        .next()
        .transpose()
        .unwrap();
    if let Some((loaded, installed)) = state {
        assert!(!loaded, "fts must not be loaded in the bundled build");
        assert!(!installed, "fts must not be installed in the bundled build");
    }

    // LOAD without a prior INSTALL must fail (we never run INSTALL: with
    // auto-install disabled it would be a hard error offline, and running it
    // in CI would attempt a download).
    let err = conn.execute_batch("LOAD fts;").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.to_ascii_lowercase().contains("fts"),
        "unexpected error: {msg}"
    );
}

#[test]
fn httpfs_is_absent_so_no_network_path_exists() {
    let engine = EngineConnection::open_in_memory().unwrap();
    let conn = engine.raw();
    let loaded: Option<bool> = conn
        .prepare("SELECT loaded FROM duckdb_extensions() WHERE extension_name = 'httpfs'")
        .unwrap()
        .query_map([], |r| r.get::<_, bool>(0))
        .unwrap()
        .next()
        .transpose()
        .unwrap();
    assert!(
        loaded != Some(true),
        "httpfs must never be loaded in LogScope"
    );
}

#[test]
fn sqlite_fts5_is_available_in_bundled_rusqlite() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE VIRTUAL TABLE probe USING fts5(message, record_ref UNINDEXED);
         INSERT INTO probe(message, record_ref) VALUES
           ('connection timeout while calling upstream', 'seg1:1'),
           ('user login succeeded', 'seg1:2'),
           ('upstream connection refused', 'seg1:3');",
    )
    .unwrap();
    let hits: Vec<String> = conn
        .prepare("SELECT record_ref FROM probe WHERE probe MATCH ?1 ORDER BY rank")
        .unwrap()
        .query_map(["connection upstream"], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.contains(&"seg1:1".to_string()));
    assert!(hits.contains(&"seg1:3".to_string()));
}

#[test]
fn long_running_query_can_be_interrupted() {
    use std::time::{Duration, Instant};

    let engine = EngineConnection::open_in_memory().unwrap();
    let handle = engine.interrupt_handle();

    let interrupter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        handle.interrupt();
    });

    let started = Instant::now();
    // Large cross join: far longer than the interrupt delay if uncancelled.
    let result: Result<i64, _> = engine.raw().query_row(
        "SELECT count(*) FROM range(200000) a, range(200000) b",
        [],
        |r| r.get(0),
    );
    let elapsed = started.elapsed();
    interrupter.join().unwrap();

    assert!(result.is_err(), "query should have been interrupted");
    assert!(
        elapsed < Duration::from_secs(30),
        "interrupt took too long: {elapsed:?}"
    );
    // Connection stays usable after an interrupt.
    let ok: i64 = engine
        .raw()
        .query_row("SELECT 41 + 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, 42);
}
