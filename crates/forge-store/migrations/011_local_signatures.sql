CREATE TABLE IF NOT EXISTS ledger_signatures (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('evidence', 'decision', 'commit')),
    subject_id TEXT NOT NULL,
    signed_digest TEXT NOT NULL,
    signature_alg TEXT NOT NULL CHECK (signature_alg = 'ed25519'),
    public_key TEXT NOT NULL,
    key_fingerprint TEXT NOT NULL,
    signature TEXT NOT NULL,
    trust_level TEXT NOT NULL CHECK (trust_level = 'locally_signed'),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(repo_id, subject_kind, subject_id, signed_digest, key_fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_ledger_signatures_subject
ON ledger_signatures(repo_id, subject_kind, subject_id);

CREATE TABLE IF NOT EXISTS signature_marker (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    evidence_high_water INTEGER NOT NULL,
    decision_high_water INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL
);

INSERT OR IGNORE INTO signature_marker (
    singleton,
    evidence_high_water,
    decision_high_water,
    created_at_ms
) VALUES (
    1,
    (SELECT COALESCE(MAX(rowid), 0) FROM evidence),
    (SELECT COALESCE(MAX(rowid), 0) FROM decisions),
    CAST(strftime('%s', 'now') AS INTEGER) * 1000
);
