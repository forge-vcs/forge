CREATE TABLE IF NOT EXISTS trust_policy (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    min_accept_trust TEXT NOT NULL CHECK (min_accept_trust IN ('self_reported', 'locally_observed', 'locally_signed')),
    min_export_trust TEXT NOT NULL CHECK (min_export_trust IN ('self_reported', 'locally_observed', 'locally_signed')),
    updated_at_ms INTEGER NOT NULL
);

INSERT OR IGNORE INTO trust_policy (
    singleton,
    min_accept_trust,
    min_export_trust,
    updated_at_ms
) VALUES (
    1,
    'self_reported',
    'self_reported',
    CAST(strftime('%s', 'now') AS INTEGER) * 1000
);
