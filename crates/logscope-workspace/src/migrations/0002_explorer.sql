-- Migration 0002: Log Explorer control-plane state (v0.2).
-- Forward-only. Applied inside a single transaction by the runner.

-- Derived, rebuildable per-dataset attribute field catalog. Never the sole
-- copy of anything: rebuilt from Parquet segments on demand.
CREATE TABLE field_stats (
    dataset_id            TEXT NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    display               TEXT NOT NULL,              -- dotted display path
    path_json             TEXT NOT NULL,              -- JSON array of segments
    types_json            TEXT NOT NULL,              -- JSON array of observed type tags
    present_count         INTEGER NOT NULL,
    distinct_est          INTEGER,
    distinct_is_exact     INTEGER NOT NULL DEFAULT 0,
    examples_json         TEXT NOT NULL DEFAULT '[]', -- bounded, display-safe examples
    queryable             INTEGER NOT NULL DEFAULT 1, -- 0: name unreachable in language v1
    catalog_version       INTEGER NOT NULL,
    built_at              TEXT NOT NULL,
    PRIMARY KEY (dataset_id, display)
) STRICT;

-- Rebuildable-index lifecycle (FTS, field catalog). One row per index kind
-- and dataset.
CREATE TABLE index_state (
    kind        TEXT NOT NULL,              -- fts | field_catalog
    dataset_id  TEXT NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    version     INTEGER NOT NULL,
    status      TEXT NOT NULL,              -- pending | building | ready | failed
    updated_at  TEXT NOT NULL,
    detail_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (kind, dataset_id)
) STRICT;

CREATE TABLE saved_searches (
    saved_search_id TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    query_text      TEXT NOT NULL,
    language_version INTEGER NOT NULL,
    fingerprint     TEXT NOT NULL,
    -- JSON: {"kind":"all"} or {"kind":"explicit","dataset_ids":[…]}
    dataset_selection_json TEXT NOT NULL,
    -- JSON TimeStrategy: all | absolute{start,end} | relative_to_latest{duration_nanos}
    time_strategy_json TEXT NOT NULL,
    description     TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;

CREATE TABLE column_sets (
    column_set_id TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    -- JSON array of {field: canonical-or-attr display identity, width: px|null}
    columns_json  TEXT NOT NULL,
    is_default    INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
) STRICT;

CREATE TABLE recent_searches (
    recent_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    query_text      TEXT NOT NULL,
    language_version INTEGER NOT NULL,
    fingerprint     TEXT NOT NULL,
    dataset_selection_json TEXT NOT NULL,
    time_strategy_json TEXT NOT NULL,
    run_count       INTEGER NOT NULL DEFAULT 1,
    last_run_at     TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX idx_recent_identity
    ON recent_searches(fingerprint, dataset_selection_json, time_strategy_json);

CREATE TABLE export_jobs (
    export_id     TEXT PRIMARY KEY,
    job_id        TEXT NOT NULL,
    format        TEXT NOT NULL,             -- csv | jsonl
    destination   TEXT NOT NULL,
    query_text    TEXT NOT NULL,
    fingerprint   TEXT NOT NULL,
    dataset_selection_json TEXT NOT NULL,
    time_strategy_json TEXT NOT NULL,
    resolved_start INTEGER,
    resolved_end   INTEGER,
    row_limit     INTEGER NOT NULL,
    byte_limit    INTEGER NOT NULL,
    rows_written  INTEGER NOT NULL DEFAULT 0,
    bytes_written INTEGER NOT NULL DEFAULT 0,
    truncated     INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL,             -- running | completed | cancelled | failed
    error_json    TEXT,
    created_at    TEXT NOT NULL,
    finished_at   TEXT
) STRICT;

-- Existing datasets need their derived indexes (re)built under v0.2 rules:
-- the FTS tokenizer changed (v2) and the field catalog is new. Log datasets
-- start `pending`; the application schedules the builds as cancellable jobs.
INSERT INTO index_state (kind, dataset_id, version, status, updated_at)
SELECT 'fts', dataset_id, 2, 'pending', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM datasets WHERE signal = 'logs';

INSERT INTO index_state (kind, dataset_id, version, status, updated_at)
SELECT 'field_catalog', dataset_id, 1, 'pending', strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
FROM datasets WHERE signal = 'logs';
