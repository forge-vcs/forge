-- Phase 8 Slice 2a: conflict-as-data substrate.
--
-- Keep this additive so pre-008 stale-base metadata rows continue to open. New
-- S2a writers populate the content refs, operation owner, status, resolver, and
-- content_hash. Legacy rows keep NULLs.
CREATE TABLE IF NOT EXISTS conflict_sets (
    id TEXT PRIMARY KEY,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    context TEXT NOT NULL,
    paths_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

ALTER TABLE conflict_sets ADD COLUMN base_content_ref TEXT;
ALTER TABLE conflict_sets ADD COLUMN ours_content_ref TEXT;
ALTER TABLE conflict_sets ADD COLUMN theirs_content_ref TEXT;
ALTER TABLE conflict_sets ADD COLUMN generated_by_operation_id TEXT REFERENCES operations(id);
ALTER TABLE conflict_sets ADD COLUMN resolver_backend TEXT;
ALTER TABLE conflict_sets ADD COLUMN status TEXT NOT NULL DEFAULT 'unresolved' CHECK (status IN ('unresolved', 'partially_resolved', 'resolved', 'abandoned'));
ALTER TABLE conflict_sets ADD COLUMN content_hash TEXT;

CREATE TABLE IF NOT EXISTS path_conflicts (
    id TEXT PRIMARY KEY,
    conflict_set_id TEXT NOT NULL REFERENCES conflict_sets(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    path_fingerprint TEXT NOT NULL,
    base_path TEXT,
    ours_path TEXT,
    theirs_path TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('content', 'binary', 'delete_modify', 'rename', 'dir_file', 'mode', 'symlink')),
    base_ref TEXT,
    ours_ref TEXT,
    theirs_ref TEXT,
    base_status TEXT,
    ours_status TEXT,
    theirs_status TEXT,
    base_mode TEXT,
    ours_mode TEXT,
    theirs_mode TEXT,
    resolution_ref TEXT,
    status TEXT NOT NULL DEFAULT 'unresolved' CHECK (status IN ('unresolved', 'partially_resolved', 'resolved', 'abandoned')),
    created_at_ms INTEGER NOT NULL
);
