-- Migration 0001: initial workspace metadata schema.
-- Forward-only. Applied inside a single transaction by the runner.

CREATE TABLE workspace_info (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE sources (
    source_id      TEXT PRIMARY KEY,
    kind           TEXT NOT NULL,  -- static_file_set | archive_bundle | watched_folder | otlp_session
    display_name   TEXT NOT NULL,
    retention_mode TEXT NOT NULL DEFAULT 'referenced',  -- referenced | managed_copy
    created_at     TEXT NOT NULL,
    detail_json    TEXT NOT NULL DEFAULT '{}'
) STRICT;

CREATE TABLE source_files (
    file_id                TEXT PRIMARY KEY,
    source_id              TEXT NOT NULL REFERENCES sources(source_id) ON DELETE CASCADE,
    path                   TEXT NOT NULL,
    managed_rel_path       TEXT,
    archive_parent_file_id TEXT,
    archive_entry          TEXT,
    size_bytes             INTEGER NOT NULL,
    modified_at            TEXT,
    content_hash           TEXT NOT NULL,
    created_at             TEXT NOT NULL
) STRICT;
CREATE INDEX idx_source_files_source ON source_files(source_id);
CREATE INDEX idx_source_files_hash ON source_files(content_hash);

CREATE TABLE datasets (
    dataset_id         TEXT PRIMARY KEY,
    name               TEXT NOT NULL,
    signal             TEXT NOT NULL,  -- logs | metrics | spans
    status             TEXT NOT NULL,  -- staging | published | failed | removing
    created_at         TEXT NOT NULL,
    published_at       TEXT,
    profile_id         TEXT,
    profile_version    TEXT,
    parser_id          TEXT,
    parser_version     TEXT,
    normalizer_version TEXT,
    model_version      TEXT,
    notes              TEXT
) STRICT;

CREATE TABLE dataset_sources (
    dataset_id TEXT NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    source_id  TEXT NOT NULL REFERENCES sources(source_id),
    PRIMARY KEY (dataset_id, source_id)
) STRICT;

-- A row in `segments` means the segment file is published and visible.
-- Staged segments exist only as files under staging/ and have no row.
CREATE TABLE segments (
    segment_id     TEXT PRIMARY KEY,
    dataset_id     TEXT NOT NULL REFERENCES datasets(dataset_id) ON DELETE CASCADE,
    signal         TEXT NOT NULL,
    file_name      TEXT NOT NULL,  -- relative to data/<dataset_id>/
    row_count      INTEGER NOT NULL,
    byte_size      INTEGER NOT NULL,
    min_event_time INTEGER,        -- unix nanos, NULL when no timestamps
    max_event_time INTEGER,
    created_at     TEXT NOT NULL,
    fts_indexed    INTEGER NOT NULL DEFAULT 0
) STRICT;
CREATE INDEX idx_segments_dataset ON segments(dataset_id);

CREATE TABLE resources (
    resource_id            TEXT PRIMARY KEY,
    canonical_json         TEXT NOT NULL,
    service_name           TEXT,
    service_namespace      TEXT,
    service_instance_id    TEXT,
    deployment_environment TEXT,
    schema_url             TEXT,
    first_seen_at          TEXT NOT NULL
) STRICT;

CREATE TABLE scopes (
    scope_id       TEXT PRIMARY KEY,
    canonical_json TEXT NOT NULL,
    name           TEXT,
    version        TEXT,
    schema_url     TEXT,
    first_seen_at  TEXT NOT NULL
) STRICT;

CREATE TABLE jobs (
    job_id      TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    status      TEXT NOT NULL,  -- pending | running | paused | cancelled | failed | completed
    dataset_id  TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    detail_json TEXT NOT NULL DEFAULT '{}',
    error_json  TEXT
) STRICT;
CREATE INDEX idx_jobs_status ON jobs(status);

CREATE TABLE ingest_ledger (
    ledger_id         INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id            TEXT NOT NULL REFERENCES jobs(job_id),
    source_id         TEXT NOT NULL,
    file_id           TEXT NOT NULL,
    checkpoint_json   TEXT NOT NULL,
    records_accepted  INTEGER NOT NULL DEFAULT 0,
    records_rejected  INTEGER NOT NULL DEFAULT 0,
    records_unparsed  INTEGER NOT NULL DEFAULT 0,
    records_duplicate INTEGER NOT NULL DEFAULT 0,
    updated_at        TEXT NOT NULL
) STRICT;
CREATE INDEX idx_ledger_job ON ingest_ledger(job_id);

CREATE TABLE rejected_records (
    reject_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    dataset_id      TEXT NOT NULL,
    source_id       TEXT NOT NULL,
    file_id         TEXT NOT NULL,
    locator_json    TEXT NOT NULL,
    reason_code     TEXT NOT NULL,
    message         TEXT NOT NULL,
    raw_excerpt     BLOB,
    parser_id       TEXT,
    parser_version  TEXT,
    profile_id      TEXT,
    profile_version TEXT,
    retryable       INTEGER NOT NULL DEFAULT 0,
    created_at      TEXT NOT NULL
) STRICT;
CREATE INDEX idx_rejected_dataset ON rejected_records(dataset_id);
