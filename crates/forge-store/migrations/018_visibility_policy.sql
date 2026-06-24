CREATE TABLE IF NOT EXISTS visibility_policy (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    default_work_package_visibility TEXT NOT NULL CHECK (
        default_work_package_visibility IN ('private', 'team', 'public', 'embargoed')
    ),
    updated_at_ms INTEGER NOT NULL
);

INSERT OR IGNORE INTO visibility_policy (
    singleton,
    default_work_package_visibility,
    updated_at_ms
) VALUES (
    1,
    'public',
    CAST(strftime('%s', 'now') AS INTEGER) * 1000
);

CREATE TABLE IF NOT EXISTS work_package_visibility (
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT NOT NULL CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT NOT NULL,
    visibility TEXT NOT NULL CHECK (visibility IN ('private', 'team', 'public', 'embargoed')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (repo_id, work_package_kind, work_package_id)
);

CREATE INDEX IF NOT EXISTS idx_work_package_visibility_lookup
ON work_package_visibility(repo_id, work_package_kind, work_package_id, visibility);

CREATE TABLE IF NOT EXISTS path_visibility_labels (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT NOT NULL CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT NOT NULL,
    path TEXT NOT NULL,
    visibility TEXT NOT NULL CHECK (visibility IN ('private', 'team', 'public', 'embargoed')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (repo_id, work_package_kind, work_package_id, path)
);

CREATE TABLE IF NOT EXISTS visibility_grants (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT NOT NULL CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT NOT NULL,
    recipient TEXT NOT NULL,
    capability TEXT NOT NULL CHECK (
        capability IN ('see_stub', 'inspect_content', 'inspect_evidence', 'sync_materialize', 'publish_reveal')
    ),
    created_at_ms INTEGER NOT NULL,
    revoked_at_ms INTEGER,
    UNIQUE (repo_id, work_package_kind, work_package_id, recipient, capability)
);

CREATE INDEX IF NOT EXISTS idx_visibility_grants_effective
ON visibility_grants(repo_id, work_package_kind, work_package_id, recipient, capability, revoked_at_ms);

CREATE TABLE IF NOT EXISTS visibility_audit (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT,
    action TEXT NOT NULL CHECK (
        action IN ('set_default', 'set_visibility', 'grant_capability', 'revoke_capability', 'reveal_publish')
    ),
    actor TEXT NOT NULL,
    prior_visibility TEXT CHECK (prior_visibility IN ('private', 'team', 'public', 'embargoed')),
    new_visibility TEXT CHECK (new_visibility IN ('private', 'team', 'public', 'embargoed')),
    recipient TEXT,
    capability TEXT CHECK (
        capability IN ('see_stub', 'inspect_content', 'inspect_evidence', 'sync_materialize', 'publish_reveal')
    ),
    reason TEXT,
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_visibility_audit_work_package
ON visibility_audit(repo_id, work_package_kind, work_package_id, created_at_ms);
