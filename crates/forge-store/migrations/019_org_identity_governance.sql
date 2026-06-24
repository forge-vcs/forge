CREATE TABLE IF NOT EXISTS org_authority_profile (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    org_id TEXT,
    policy_revision INTEGER NOT NULL,
    bootstrap_actor_id TEXT,
    bootstrap_key_fingerprint TEXT,
    recovery_status TEXT NOT NULL CHECK (recovery_status IN ('normal', 'recovery_needed')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

INSERT OR IGNORE INTO org_authority_profile (
    singleton,
    enabled,
    org_id,
    policy_revision,
    bootstrap_actor_id,
    bootstrap_key_fingerprint,
    recovery_status,
    created_at_ms,
    updated_at_ms
) VALUES (
    1,
    0,
    NULL,
    0,
    NULL,
    NULL,
    'normal',
    CAST(strftime('%s', 'now') AS INTEGER) * 1000,
    CAST(strftime('%s', 'now') AS INTEGER) * 1000
);

CREATE TABLE IF NOT EXISTS org_principals (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    kind TEXT NOT NULL CHECK (kind IN ('human', 'service', 'external')),
    state TEXT NOT NULL CHECK (state IN ('active', 'rotated', 'expired', 'revoked')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (repo_id, id)
);

CREATE INDEX IF NOT EXISTS idx_org_principals_repo_kind
ON org_principals(repo_id, kind, state);

CREATE TABLE IF NOT EXISTS org_principal_aliases (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    principal_id TEXT NOT NULL,
    alias_kind TEXT NOT NULL,
    alias_value TEXT NOT NULL,
    visibility TEXT NOT NULL CHECK (visibility IN ('private', 'team', 'public')),
    state TEXT NOT NULL CHECK (state IN ('active', 'rotated', 'expired', 'revoked')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (repo_id, principal_id) REFERENCES org_principals(repo_id, id),
    UNIQUE (repo_id, alias_kind, alias_value)
);

CREATE INDEX IF NOT EXISTS idx_org_principal_aliases_principal
ON org_principal_aliases(repo_id, principal_id, state);

CREATE TABLE IF NOT EXISTS org_key_bindings (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    principal_id TEXT NOT NULL,
    key_fingerprint TEXT NOT NULL,
    public_key TEXT NOT NULL,
    binding_authority TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'rotated', 'expired', 'revoked')),
    valid_from_revision INTEGER NOT NULL,
    valid_until_revision INTEGER,
    revocation_reason TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (repo_id, principal_id) REFERENCES org_principals(repo_id, id),
    FOREIGN KEY (repo_id, binding_authority) REFERENCES org_principals(repo_id, id),
    FOREIGN KEY (repo_id, key_fingerprint) REFERENCES signing_keys(repo_id, key_fingerprint),
    UNIQUE (repo_id, key_fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_org_key_bindings_principal
ON org_key_bindings(repo_id, principal_id, state);

CREATE TABLE IF NOT EXISTS org_role_bindings (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    principal_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'maintainer', 'member', 'external_reviewer', 'service')),
    authority TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('active', 'rotated', 'expired', 'revoked')),
    valid_from_revision INTEGER NOT NULL,
    valid_until_revision INTEGER,
    revocation_reason TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (repo_id, principal_id) REFERENCES org_principals(repo_id, id),
    FOREIGN KEY (repo_id, authority) REFERENCES org_principals(repo_id, id),
    UNIQUE (repo_id, principal_id, role)
);

CREATE INDEX IF NOT EXISTS idx_org_role_bindings_principal
ON org_role_bindings(repo_id, principal_id, state);

CREATE TABLE IF NOT EXISTS org_issuer_bindings (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    issuer_kind TEXT NOT NULL CHECK (issuer_kind IN ('hosted_runner', 'third_party')),
    issuer TEXT NOT NULL,
    key_fingerprint TEXT NOT NULL,
    scope TEXT,
    state TEXT NOT NULL CHECK (state IN ('active', 'rotated', 'expired', 'revoked')),
    valid_from_revision INTEGER NOT NULL,
    valid_until_revision INTEGER,
    revocation_reason TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY (repo_id, key_fingerprint) REFERENCES signing_keys(repo_id, key_fingerprint),
    UNIQUE (repo_id, issuer_kind, issuer, key_fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_org_issuer_bindings_lookup
ON org_issuer_bindings(repo_id, issuer_kind, issuer, state);

CREATE TABLE IF NOT EXISTS org_policy_audit (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    action TEXT NOT NULL,
    actor_id TEXT,
    acting_key_fingerprint TEXT,
    authority TEXT,
    prior_state_json TEXT,
    new_state_json TEXT,
    policy_revision INTEGER NOT NULL,
    reason TEXT,
    created_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_org_policy_audit_repo_revision
ON org_policy_audit(repo_id, policy_revision, created_at_ms);
