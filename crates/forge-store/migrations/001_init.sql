CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS repositories (
    id TEXT PRIMARY KEY,
    root_path TEXT NOT NULL UNIQUE,
    git_head TEXT,
    content_backend TEXT NOT NULL DEFAULT 'git',
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS operations (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    request_id TEXT,
    command TEXT NOT NULL,
    status TEXT NOT NULL,
    kind TEXT NOT NULL,
    parent_operation_id TEXT REFERENCES operations(id),
    resulting_view_id TEXT,
    error_json TEXT,
    created_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_operations_request_id
ON operations(repo_id, request_id)
WHERE request_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS views (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    operation_id TEXT NOT NULL REFERENCES operations(id),
    kind TEXT NOT NULL,
    state_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS current_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    current_operation_id TEXT NOT NULL REFERENCES operations(id),
    current_view_id TEXT NOT NULL REFERENCES views(id),
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS intents (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    text TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attempts (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    intent_id TEXT NOT NULL REFERENCES intents(id),
    base_head TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS snapshots (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    parent_snapshot_id TEXT REFERENCES snapshots(id),
    content_ref TEXT NOT NULL,
    changed_paths_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS evidence (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    snapshot_id TEXT REFERENCES snapshots(id),
    command TEXT NOT NULL,
    args_json TEXT NOT NULL,
    cwd TEXT NOT NULL,
    exit_code INTEGER NOT NULL,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER NOT NULL,
    stdout_excerpt TEXT NOT NULL,
    stderr_excerpt TEXT NOT NULL,
    stdout_truncated INTEGER NOT NULL,
    stderr_truncated INTEGER NOT NULL,
    timed_out INTEGER NOT NULL DEFAULT 0,
    sensitivity TEXT NOT NULL,
    visibility TEXT NOT NULL,
    trust TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS proposals (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    attempt_id TEXT NOT NULL REFERENCES attempts(id),
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    base_head TEXT NOT NULL,
    content_ref TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS proposal_revisions (
    id TEXT PRIMARY KEY,
    proposal_id TEXT NOT NULL REFERENCES proposals(id),
    snapshot_id TEXT NOT NULL REFERENCES snapshots(id),
    content_ref TEXT NOT NULL,
    changed_paths_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS check_results (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    proposal_id TEXT NOT NULL REFERENCES proposals(id),
    proposal_revision_id TEXT NOT NULL REFERENCES proposal_revisions(id),
    status TEXT NOT NULL,
    reason TEXT NOT NULL,
    evidence_id TEXT REFERENCES evidence(id),
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS decisions (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    proposal_id TEXT NOT NULL REFERENCES proposals(id),
    proposal_revision_id TEXT NOT NULL REFERENCES proposal_revisions(id),
    decision TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS publications (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    proposal_id TEXT NOT NULL REFERENCES proposals(id),
    proposal_revision_id TEXT NOT NULL REFERENCES proposal_revisions(id),
    branch_name TEXT NOT NULL,
    commit_id TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS conflict_sets (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    context TEXT NOT NULL,
    paths_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);
