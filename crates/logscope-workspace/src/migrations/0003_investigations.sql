-- Migration 0003: investigation workbench control plane (v0.3).
-- Forward-only. Applied inside a single transaction by the runner.
--
-- Additive only: no v2 table or row is modified and no bulk data is
-- rewritten, so a failed migration rolls back to a schema-v2 workspace
-- that 0.2.0 builds can still open.
--
-- Conventions: opaque prefixed TEXT ids (inv-/hyp-/item-/iscope-/ev-/
-- evg-/mark-/rep-/art-/red-/bnd-/bimp-<uuid>), RFC3339 TEXT wall-clock
-- stamps, unix-nanos INTEGER event instants (UTC, half-open intervals),
-- `*_json` typed payloads. Enum values are validated in the service
-- layer; every mutation bumps `revision` and writes a `case_history`
-- row in the same transaction (non-destructive history). No OS identity
-- is ever captured; `owner_text` is stored exactly as the user typed it.

CREATE TABLE investigations (
    investigation_id    TEXT PRIMARY KEY,
    entity_version      INTEGER NOT NULL,   -- investigation entity schema, currently 1
    title               TEXT NOT NULL,
    description         TEXT,
    status              TEXT NOT NULL,      -- open|investigating|mitigated|resolved|archived
    severity            TEXT,               -- generic documented scale: sev1..sev4
    owner_text          TEXT,               -- user-entered text; never inferred from OS/git
    tags_json           TEXT NOT NULL DEFAULT '[]',
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    status_changed_at   TEXT,
    incident_started_at INTEGER,            -- optional incident instants (unix nanos UTC)
    mitigated_at        INTEGER,
    resolved_at         INTEGER,
    window_start        INTEGER,            -- relevant time window [start, end)
    window_end          INTEGER,
    revision            INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE investigation_scope_refs (
    scope_ref_id     TEXT PRIMARY KEY,
    investigation_id TEXT NOT NULL REFERENCES investigations(investigation_id) ON DELETE CASCADE,
    kind             TEXT NOT NULL,         -- dataset|resource_selector|saved_query|embedded_query|time_window|label
    dataset_id       TEXT,                  -- kind=dataset
    dataset_revision TEXT,                  -- dsrev-<hex> captured at attach time
    selector_json    TEXT,                  -- kind=resource_selector
    saved_search_id  TEXT,                  -- kind=saved_query (fails honestly if deleted)
    query_json       TEXT,                  -- kind=embedded_query (versioned query definition)
    window_start     INTEGER,               -- kind=time_window
    window_end       INTEGER,
    label            TEXT,                  -- kind=label (generic system/component label)
    position         INTEGER NOT NULL DEFAULT 0,
    created_at       TEXT NOT NULL
) STRICT;
CREATE INDEX idx_scope_refs_inv ON investigation_scope_refs(investigation_id, position);

CREATE TABLE evidence_groups (
    group_id         TEXT PRIMARY KEY,
    investigation_id TEXT NOT NULL REFERENCES investigations(investigation_id) ON DELETE CASCADE,
    name             TEXT NOT NULL,
    position         INTEGER NOT NULL DEFAULT 0,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE evidence (
    evidence_id      TEXT PRIMARY KEY,
    investigation_id TEXT NOT NULL REFERENCES investigations(investigation_id) ON DELETE CASCADE,
    envelope_version INTEGER NOT NULL,      -- evidence envelope schema, currently 1
    kind             TEXT NOT NULL,         -- event|selection|query|explorer_group|histogram_interval|item_ref
    signal           TEXT NOT NULL,         -- log|manual
    title            TEXT NOT NULL,
    annotation       TEXT,
    relevance        TEXT,                  -- explicit relevance explanation
    captured_investigation_revision INTEGER NOT NULL,
    group_id         TEXT REFERENCES evidence_groups(group_id) ON DELETE SET NULL,
    position         INTEGER NOT NULL DEFAULT 0,
    supersedes_evidence_id TEXT REFERENCES evidence(evidence_id),
    archived         INTEGER NOT NULL DEFAULT 0,   -- normal removal archives; no UI hard delete
    resolver_state   TEXT NOT NULL DEFAULT 'unverified',
    resolver_detail_json TEXT NOT NULL DEFAULT '{}',
    last_verified_at TEXT,
    -- Live canonical reference (re-resolution/navigation) and bounded
    -- captured snapshot (what the investigator saw at pin time) are
    -- separate payloads; verification never rewrites the snapshot.
    reference_json   TEXT NOT NULL,
    snapshot_json    TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE INDEX idx_evidence_inv ON evidence(investigation_id, position);
CREATE INDEX idx_evidence_group ON evidence(group_id);

CREATE TABLE hypotheses (
    hypothesis_id    TEXT PRIMARY KEY,
    investigation_id TEXT NOT NULL REFERENCES investigations(investigation_id) ON DELETE CASCADE,
    statement        TEXT NOT NULL,
    rationale        TEXT,
    state            TEXT NOT NULL,         -- unverified|supported|rejected|confirmed (manual only)
    position         INTEGER NOT NULL DEFAULT 0,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE INDEX idx_hypotheses_inv ON hypotheses(investigation_id, position);

CREATE TABLE hypothesis_evidence (
    hypothesis_id TEXT NOT NULL REFERENCES hypotheses(hypothesis_id) ON DELETE CASCADE,
    evidence_id   TEXT NOT NULL REFERENCES evidence(evidence_id) ON DELETE CASCADE,
    linked_at     TEXT NOT NULL,
    PRIMARY KEY (hypothesis_id, evidence_id)
) STRICT;

CREATE TABLE investigation_items (
    item_id          TEXT PRIMARY KEY,
    investigation_id TEXT NOT NULL REFERENCES investigations(investigation_id) ON DELETE CASCADE,
    kind             TEXT NOT NULL,         -- note|task|finding|question
    content          TEXT NOT NULL,
    task_status      TEXT,                  -- kind=task: todo|doing|done|dropped
    question_status  TEXT,                  -- kind=question: open|answered|deferred
    authored_by_user INTEGER NOT NULL DEFAULT 1,  -- v0.3: always 1 (manual authorship)
    -- Reserved for deterministic v0.4 findings (rule/calculation
    -- provenance); NULL for every v0.3 row so manual findings never
    -- change meaning.
    finding_provenance_json TEXT,
    position         INTEGER NOT NULL DEFAULT 0,
    archived         INTEGER NOT NULL DEFAULT 0,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE INDEX idx_items_inv ON investigation_items(investigation_id, kind, position);

CREATE TABLE timeline_markers (
    marker_id        TEXT PRIMARY KEY,
    investigation_id TEXT NOT NULL REFERENCES investigations(investigation_id) ON DELETE CASCADE,
    kind             TEXT NOT NULL,         -- deployment|config_change|operator_action|custom
    label            TEXT NOT NULL,
    description      TEXT,
    at_nanos         INTEGER,               -- UTC instant; NULL renders in the undated section
    end_nanos        INTEGER,               -- optional bounded interval end (exclusive)
    original_tz_offset_min INTEGER,         -- user-supplied zone offset, preserved
    original_time_text TEXT,                -- timestamp text exactly as entered
    position         INTEGER NOT NULL DEFAULT 0,
    archived         INTEGER NOT NULL DEFAULT 0,
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1
) STRICT;
CREATE INDEX idx_markers_inv ON timeline_markers(investigation_id, at_nanos);

CREATE TABLE redaction_profiles (
    profile_id      TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    profile_version INTEGER NOT NULL DEFAULT 1,  -- bumps on any rule/posture change
    rules_json      TEXT NOT NULL DEFAULT '[]',
    posture_json    TEXT NOT NULL DEFAULT '{}',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    revision        INTEGER NOT NULL DEFAULT 1
) STRICT;

CREATE TABLE report_definitions (
    report_def_id    TEXT PRIMARY KEY,
    investigation_id TEXT NOT NULL REFERENCES investigations(investigation_id) ON DELETE CASCADE,
    title            TEXT NOT NULL,
    subtitle         TEXT,
    sections_json    TEXT NOT NULL,         -- ordered sections; narrative content is user-authored
    selected_evidence_json TEXT NOT NULL DEFAULT '[]',  -- [{evidence_id, revision}] exact revisions
    selected_markers_json  TEXT NOT NULL DEFAULT '[]',
    redaction_profile_id   TEXT REFERENCES redaction_profiles(profile_id),
    options_json     TEXT NOT NULL DEFAULT '{}',
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    revision         INTEGER NOT NULL DEFAULT 1
) STRICT;

-- Immutable generation records. A cancelled/failed generation keeps its
-- row (status) but never a published artifact.
CREATE TABLE report_artifacts (
    artifact_id      TEXT PRIMARY KEY,
    report_def_id    TEXT NOT NULL REFERENCES report_definitions(report_def_id),
    investigation_id TEXT NOT NULL,
    format           TEXT NOT NULL,         -- markdown|html
    destination_path TEXT NOT NULL,
    snapshot_json    TEXT NOT NULL,         -- consistent investigation snapshot metadata
    checksum_sha256  TEXT,                  -- NULL for failed/cancelled rows
    byte_size        INTEGER,
    status           TEXT NOT NULL,         -- running|completed|failed|cancelled
    error_json       TEXT,
    created_at       TEXT NOT NULL,
    finished_at      TEXT
) STRICT;

CREATE TABLE bundle_exports (
    bundle_id        TEXT PRIMARY KEY,
    investigation_id TEXT NOT NULL,
    destination_path TEXT NOT NULL,
    manifest_json    TEXT,                  -- as written (completed rows)
    checksum_sha256  TEXT,
    byte_size        INTEGER,
    status           TEXT NOT NULL,         -- running|completed|failed|cancelled
    error_json       TEXT,
    created_at       TEXT NOT NULL,
    finished_at      TEXT
) STRICT;

-- Import provenance recorded in the DESTINATION workspace of a bundle
-- import (bundles are imported into a new isolated workspace).
CREATE TABLE bundle_imports (
    import_id            TEXT PRIMARY KEY,
    original_bundle_path TEXT NOT NULL,
    bundle_checksum      TEXT NOT NULL,
    manifest_json        TEXT NOT NULL,
    imported_at          TEXT NOT NULL,
    detail_json          TEXT NOT NULL DEFAULT '{}'
) STRICT;

-- Single non-destructive history/activity ledger for all case entities.
-- payload_json is the full entity state AFTER the action, so any prior
-- revision is retrievable. Hashes stored anywhere in case data are
-- integrity aids, never signatures or proof of authorship.
CREATE TABLE case_history (
    history_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    investigation_id TEXT,                  -- NULL for workspace-global entities (redaction profiles)
    entity_kind      TEXT NOT NULL,         -- investigation|scope_ref|hypothesis|item|evidence|evidence_group|marker|report_definition|report_artifact|redaction_profile|bundle_export
    entity_id        TEXT NOT NULL,
    revision         INTEGER NOT NULL,
    action           TEXT NOT NULL,         -- created|edited|status_changed|state_changed|pinned|reordered|linked|unlinked|superseded|archived|restored|verified|report_generated|bundle_exported|removed
    payload_json     TEXT NOT NULL,
    detail_json      TEXT NOT NULL DEFAULT '{}',
    created_at       TEXT NOT NULL
) STRICT;
CREATE INDEX idx_history_entity ON case_history(entity_kind, entity_id, revision);
CREATE INDEX idx_history_inv ON case_history(investigation_id, history_id);
