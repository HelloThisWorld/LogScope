-- Migration 0004: deterministic analysis control plane (v0.4 WP1).
-- Forward-only. Applied inside a single transaction by the runner.
--
-- Additive only: no v3 table or row is modified and no bulk data is
-- rewritten, so a failed migration rolls back to a schema-v3 workspace
-- that 0.3.0 builds can still open. Existing manual investigation items
-- keep authored_by_user = 1 untouched; deterministic findings live in
-- their own table and never rewrite them.
--
-- Conventions (unchanged from 0003): opaque prefixed TEXT ids
-- (adef-/arun-<uuid>; afind-/pat-/stk- ids are deterministic BLAKE3
-- content addresses minted in logscope-case), RFC3339 TEXT wall-clock
-- stamps, unix-nanos INTEGER event instants (UTC, half-open intervals),
-- `*_json` typed payloads validated at the service layer. Definition
-- mutations bump `revision` and write a `case_history` row in the same
-- transaction. Runs are two-phase and immutable once finished: a row is
-- inserted `pending` before any derived byte exists and is finished
-- exactly once; crash recovery completes interrupted runs as failed
-- tombstones, never deletes them.

CREATE TABLE analysis_definitions (
    definition_id        TEXT PRIMARY KEY,
    definition_schema_version INTEGER NOT NULL,  -- currently 1
    kind                 TEXT NOT NULL,   -- message_pattern|stack_fingerprint|comparison|correlation|finding_rules
    name                 TEXT NOT NULL,
    description          TEXT,
    dataset_selection_json TEXT NOT NULL DEFAULT '[]',
    query_text           TEXT NOT NULL DEFAULT '',
    query_language_version INTEGER NOT NULL,
    query_fingerprint    TEXT,            -- qry-<hex>; NULL for an empty (all-data) query
    time_strategy_json   TEXT NOT NULL,
    field_selection_json TEXT NOT NULL DEFAULT '{}',
    algorithm_id         TEXT NOT NULL,
    algorithm_version    INTEGER NOT NULL,
    config_json          TEXT NOT NULL DEFAULT '{}',   -- normalized configuration
    config_fingerprint   TEXT NOT NULL,   -- acfg-<hex> over the canonical config
    masking_profile_json TEXT NOT NULL DEFAULT '{}',
    thresholds_json      TEXT NOT NULL DEFAULT '{}',
    limits_json          TEXT NOT NULL DEFAULT '{}',
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    revision             INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE analysis_runs (
    run_id               TEXT PRIMARY KEY,
    definition_id        TEXT NOT NULL REFERENCES analysis_definitions(definition_id),
    definition_revision  INTEGER NOT NULL,
    semantic_fingerprint TEXT NOT NULL,   -- asem-<hex>: deterministic semantic-input identity
    state                TEXT NOT NULL DEFAULT 'pending',  -- pending|running|completed|cancelled|failed|stale
    dataset_revs_json    TEXT NOT NULL,   -- exact [{dataset_id, dataset_revision}] sorted by dataset_id
    query_fingerprint    TEXT,
    query_language_version INTEGER NOT NULL,
    bounds_json          TEXT NOT NULL,   -- concrete UTC-nanos half-open intervals
    algorithm_id         TEXT NOT NULL,
    algorithm_version    INTEGER NOT NULL,
    config_fingerprint   TEXT NOT NULL,
    progress_stage       TEXT,
    counts_json          TEXT NOT NULL DEFAULT '{}',  -- accepted/excluded/untimestamped/invalid/truncated/sampled
    started_at           TEXT NOT NULL,
    finished_at          TEXT,
    warnings_json        TEXT NOT NULL DEFAULT '[]',
    manifest_json        TEXT,            -- result summary + derived references, set on completion
    error_json           TEXT,            -- structured failure, set on failure/cancellation
    invalidation_reason  TEXT             -- set when state becomes stale
) STRICT;
CREATE INDEX idx_analysis_runs_def ON analysis_runs(definition_id, started_at);
CREATE INDEX idx_analysis_runs_sem ON analysis_runs(semantic_fingerprint);

CREATE TABLE analysis_findings (
    finding_id           TEXT PRIMARY KEY,  -- afind-<hex>, deterministic semantic id
    origin               TEXT NOT NULL DEFAULT 'deterministic',
    finding_schema_version INTEGER NOT NULL,
    rule_id              TEXT NOT NULL,
    rule_version         INTEGER NOT NULL,
    run_id               TEXT NOT NULL REFERENCES analysis_runs(run_id),
    subject_json         TEXT NOT NULL,   -- semantic identity of the subject result
    title                TEXT NOT NULL,
    explanation          TEXT NOT NULL,
    calculation_json     TEXT NOT NULL,   -- inputs + formula + thresholds
    severity             TEXT NOT NULL,
    severity_rule_json   TEXT NOT NULL,
    confidence           TEXT,            -- correlation-derived findings only
    contributing_json    TEXT NOT NULL,   -- bounded record ids or reproducible scope
    examples_json        TEXT NOT NULL DEFAULT '[]',
    state_json           TEXT NOT NULL DEFAULT '{}', -- exact/approximate/truncated + limitations
    created_at           TEXT NOT NULL,   -- from run completion metadata, not event time
    revision             INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE INDEX idx_analysis_findings_run ON analysis_findings(run_id);

CREATE TABLE derived_artifacts (
    artifact_id          TEXT PRIMARY KEY,
    run_id               TEXT NOT NULL REFERENCES analysis_runs(run_id) ON DELETE CASCADE,
    kind                 TEXT NOT NULL,   -- pattern_membership|bucket_series|comparison_domain|correlation_edges
    rel_path             TEXT NOT NULL,   -- under derived/analysis/<run_id>/, forward slashes
    row_count            INTEGER NOT NULL,
    byte_size            INTEGER NOT NULL,
    sha256               TEXT NOT NULL,
    schema_version       INTEGER NOT NULL,
    created_at           TEXT NOT NULL
) STRICT;
CREATE INDEX idx_derived_artifacts_run ON derived_artifacts(run_id);
