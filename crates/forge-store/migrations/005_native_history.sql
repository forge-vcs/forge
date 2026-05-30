-- NER-138 Phase 7 slice 2: native history substrate.
--
-- The native commit-object format (forge-content-native) is a near-permanent format
-- commitment (Phase 9 sync/signing will anchor on it), so it is versioned from day one
-- via this registry rather than being implicit in the binary. A future format bump is a
-- new migration that updates the singleton row, and readers branch on the recorded tag.
--
-- Singleton table (mirrors integrity_marker): one row, CHECK(singleton = 1), seeded with
-- INSERT OR IGNORE so a concurrent first-init cannot collide. created_at_ms is seeded 0
-- (the runner stamps schema_migrations with the real time -- this row is format metadata,
-- not an event). NOTE: the migration applier splits naively on the statement terminator,
-- so neither the DDL nor these comments may contain that terminator inside text.
CREATE TABLE IF NOT EXISTS native_object_format (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    format_tag TEXT NOT NULL,
    hash_algo TEXT NOT NULL,
    commit_schema_version INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL
);

INSERT OR IGNORE INTO native_object_format
    (singleton, format_tag, hash_algo, commit_schema_version, created_at_ms)
    VALUES (1, 'f1', 'sha256', 1, 0);
