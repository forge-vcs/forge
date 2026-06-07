-- Phase 8 Slice 4: physical per-attempt workspaces.
--
-- Workspace rows are additive metadata. They do not replace
-- current_state.attached_attempt_id, which remains the repo-root materialization
-- compatibility binding.
CREATE TABLE IF NOT EXISTS attempt_workspaces (
    attempt_id TEXT PRIMARY KEY REFERENCES attempts(id) ON DELETE CASCADE,
    repo_id TEXT NOT NULL REFERENCES repositories(id),
    workspace_rel_path TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'abandoned')),
    materialized_content_ref TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_attempt_workspaces_repo
ON attempt_workspaces(repo_id);
