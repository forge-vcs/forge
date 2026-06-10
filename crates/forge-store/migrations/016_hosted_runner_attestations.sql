CREATE TABLE ledger_signatures_016 (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    subject_kind TEXT NOT NULL CHECK (
        subject_kind IN ('evidence', 'decision', 'commit', 'sync_merge_commit')
    ),
    subject_id TEXT NOT NULL,
    signed_digest TEXT NOT NULL,
    signature_alg TEXT NOT NULL CHECK (signature_alg = 'ed25519'),
    public_key TEXT NOT NULL,
    key_fingerprint TEXT NOT NULL,
    signature TEXT NOT NULL,
    trust_level TEXT NOT NULL CHECK (
        trust_level IN ('locally_signed', 'hosted_runner_signed')
    ),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(repo_id, subject_kind, subject_id, signed_digest, key_fingerprint, trust_level)
);

INSERT INTO ledger_signatures_016 (
    id,
    repo_id,
    subject_kind,
    subject_id,
    signed_digest,
    signature_alg,
    public_key,
    key_fingerprint,
    signature,
    trust_level,
    created_at_ms
)
SELECT
    id,
    repo_id,
    subject_kind,
    subject_id,
    signed_digest,
    signature_alg,
    public_key,
    key_fingerprint,
    signature,
    trust_level,
    created_at_ms
FROM ledger_signatures;

DROP TABLE ledger_signatures;
ALTER TABLE ledger_signatures_016 RENAME TO ledger_signatures;

CREATE INDEX idx_ledger_signatures_subject
ON ledger_signatures(repo_id, subject_kind, subject_id);

CREATE TABLE signing_keys_016 (
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    key_fingerprint TEXT NOT NULL,
    public_key TEXT NOT NULL,
    trust_origin TEXT NOT NULL CHECK (trust_origin IN ('local', 'peer', 'hosted_runner')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (repo_id, key_fingerprint)
);

INSERT INTO signing_keys_016 (
    repo_id,
    key_fingerprint,
    public_key,
    trust_origin,
    created_at_ms,
    updated_at_ms
)
SELECT
    repo_id,
    key_fingerprint,
    public_key,
    trust_origin,
    created_at_ms,
    updated_at_ms
FROM signing_keys;

DROP TABLE signing_keys;
ALTER TABLE signing_keys_016 RENAME TO signing_keys;

CREATE INDEX idx_signing_keys_origin
ON signing_keys(repo_id, trust_origin);
