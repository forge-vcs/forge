CREATE TABLE IF NOT EXISTS embargo_workflows (
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT NOT NULL CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (
        state IN (
            'active',
            'accepted_under_embargo',
            'released_under_embargo',
            'revealed',
            'published',
            'closed'
        )
    ),
    public_projection_mode TEXT CHECK (
        public_projection_mode IN ('provenance_only', 'sanitized_source', 'full_source')
    ),
    public_actor_ref TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (repo_id, work_package_kind, work_package_id)
);

CREATE INDEX IF NOT EXISTS idx_embargo_workflows_state
ON embargo_workflows(repo_id, state, updated_at_ms);

CREATE TABLE IF NOT EXISTS embargo_release_authorizations (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT NOT NULL CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT NOT NULL,
    recipient TEXT NOT NULL,
    authority TEXT NOT NULL,
    policy_revision INTEGER NOT NULL,
    content_classes_json TEXT NOT NULL,
    reason TEXT,
    created_at_ms INTEGER NOT NULL,
    revoked_at_ms INTEGER,
    UNIQUE (repo_id, work_package_kind, work_package_id, recipient)
);

CREATE INDEX IF NOT EXISTS idx_embargo_release_authorizations_effective
ON embargo_release_authorizations(repo_id, work_package_kind, work_package_id, recipient, revoked_at_ms);

CREATE TABLE IF NOT EXISTS embargo_workflow_events (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT NOT NULL CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT NOT NULL,
    action TEXT NOT NULL CHECK (
        action IN (
            'mark',
            'grant',
            'revoke',
            'accept',
            'release_attempt',
            'release',
            'reveal',
            'publish',
            'close'
        )
    ),
    actor TEXT NOT NULL,
    authority TEXT NOT NULL,
    prior_state TEXT CHECK (
        prior_state IN (
            'active',
            'accepted_under_embargo',
            'released_under_embargo',
            'revealed',
            'published',
            'closed'
        )
    ),
    new_state TEXT CHECK (
        new_state IN (
            'active',
            'accepted_under_embargo',
            'released_under_embargo',
            'revealed',
            'published',
            'closed'
        )
    ),
    policy_revision INTEGER NOT NULL,
    reason TEXT,
    recipient TEXT,
    capability TEXT CHECK (
        capability IN ('see_stub', 'inspect_content', 'inspect_evidence', 'sync_materialize', 'publish_reveal')
    ),
    release_authorization_id TEXT,
    public_projection_mode TEXT CHECK (
        public_projection_mode IN ('provenance_only', 'sanitized_source', 'full_source')
    ),
    public_actor_ref TEXT,
    content_classes_json TEXT,
    check_summary_json TEXT,
    bundle_digest TEXT,
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_embargo_workflow_events_work_package
ON embargo_workflow_events(repo_id, work_package_kind, work_package_id, created_at_ms);

INSERT OR IGNORE INTO embargo_workflows (
    repo_id,
    work_package_kind,
    work_package_id,
    state,
    public_projection_mode,
    public_actor_ref,
    created_at_ms,
    updated_at_ms
)
SELECT
    repo_id,
    work_package_kind,
    work_package_id,
    'active',
    NULL,
    NULL,
    created_at_ms,
    updated_at_ms
FROM work_package_visibility
WHERE visibility = 'embargoed';

INSERT OR IGNORE INTO embargo_workflow_events (
    id,
    repo_id,
    work_package_kind,
    work_package_id,
    action,
    actor,
    authority,
    prior_state,
    new_state,
    policy_revision,
    reason,
    recipient,
    capability,
    release_authorization_id,
    public_projection_mode,
    public_actor_ref,
    content_classes_json,
    check_summary_json,
    bundle_digest,
    created_at_ms
)
SELECT
    'embargo_event_migration_021_' || rowid,
    repo_id,
    work_package_kind,
    work_package_id,
    'mark',
    'migration',
    'migration',
    NULL,
    'active',
    0,
    'backfill existing embargoed visibility',
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    NULL,
    updated_at_ms
FROM work_package_visibility
WHERE visibility = 'embargoed';
