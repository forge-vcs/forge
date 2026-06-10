CREATE TABLE trust_policy_015 (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    min_accept_trust TEXT NOT NULL CHECK (
        min_accept_trust IN (
            'self_reported',
            'locally_observed',
            'locally_signed',
            'hosted_runner_observed',
            'hosted_runner_signed',
            'third_party_attested'
        )
    ),
    min_export_trust TEXT NOT NULL CHECK (
        min_export_trust IN (
            'self_reported',
            'locally_observed',
            'locally_signed',
            'hosted_runner_observed',
            'hosted_runner_signed',
            'third_party_attested'
        )
    ),
    updated_at_ms INTEGER NOT NULL
);

INSERT INTO trust_policy_015 (
    singleton,
    min_accept_trust,
    min_export_trust,
    updated_at_ms
)
SELECT
    singleton,
    min_accept_trust,
    min_export_trust,
    updated_at_ms
FROM trust_policy;

DROP TABLE trust_policy;
ALTER TABLE trust_policy_015 RENAME TO trust_policy;
