CREATE TABLE IF NOT EXISTS signing_keys (
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    key_fingerprint TEXT NOT NULL,
    public_key TEXT NOT NULL,
    trust_origin TEXT NOT NULL CHECK (trust_origin IN ('local', 'peer')),
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (repo_id, key_fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_signing_keys_origin
ON signing_keys(repo_id, trust_origin);

INSERT OR IGNORE INTO signing_keys (
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
    MIN(public_key),
    'peer',
    MIN(created_at_ms),
    CAST(strftime('%s', 'now') AS INTEGER) * 1000
FROM ledger_signatures
GROUP BY repo_id, key_fingerprint;
