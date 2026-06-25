CREATE TABLE IF NOT EXISTS org_encryption_key_bindings (
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
    UNIQUE (repo_id, key_fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_org_encryption_key_bindings_principal
ON org_encryption_key_bindings(repo_id, principal_id, state);

CREATE TABLE IF NOT EXISTS private_path_labels (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT NOT NULL CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT NOT NULL,
    path_hash TEXT NOT NULL,
    encrypted_display_path TEXT NOT NULL,
    visibility TEXT NOT NULL CHECK (visibility IN ('private', 'team', 'public', 'embargoed')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    UNIQUE (repo_id, id, work_package_kind, work_package_id, path_hash),
    UNIQUE (repo_id, work_package_kind, work_package_id, path_hash)
);

CREATE INDEX IF NOT EXISTS idx_private_path_labels_work_package
ON private_path_labels(repo_id, work_package_kind, work_package_id, visibility);

CREATE TABLE IF NOT EXISTS encrypted_private_payloads (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT NOT NULL CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT NOT NULL,
    snapshot_id TEXT REFERENCES snapshots(id),
    path_label_id TEXT NOT NULL REFERENCES private_path_labels(id),
    path_hash TEXT NOT NULL,
    envelope_format TEXT NOT NULL,
    recipient_fingerprint TEXT NOT NULL,
    ciphertext_digest TEXT NOT NULL,
    private_object_path TEXT NOT NULL,
    encrypted_metadata_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    FOREIGN KEY (repo_id, path_label_id, work_package_kind, work_package_id, path_hash)
        REFERENCES private_path_labels(repo_id, id, work_package_kind, work_package_id, path_hash),
    UNIQUE (repo_id, snapshot_id, path_label_id, recipient_fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_encrypted_private_payloads_snapshot
ON encrypted_private_payloads(repo_id, snapshot_id, recipient_fingerprint);

CREATE TABLE IF NOT EXISTS private_content_audit (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    work_package_kind TEXT CHECK (work_package_kind IN ('intent', 'attempt', 'proposal')),
    work_package_id TEXT,
    snapshot_id TEXT REFERENCES snapshots(id),
    path_label_id TEXT REFERENCES private_path_labels(id),
    principal_id TEXT,
    key_fingerprint TEXT,
    action TEXT NOT NULL CHECK (
        action IN (
            'private_payload_encrypted',
            'private_payload_materialized',
            'private_payload_omitted',
            'grant_private_materialize',
            'revoke_private_materialize',
            'bind_encryption_key',
            'revoke_encryption_key'
        )
    ),
    reason TEXT,
    created_at_ms INTEGER NOT NULL,
    FOREIGN KEY (repo_id, principal_id) REFERENCES org_principals(repo_id, id)
);

CREATE INDEX IF NOT EXISTS idx_private_content_audit_work_package
ON private_content_audit(repo_id, work_package_kind, work_package_id, created_at_ms);
