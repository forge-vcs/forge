# NER-382: attempt attach silently discards pre-attach edits made inside .forge/worktrees/<attempt>/

`forge attempt start --intent <id>` returns a `workspace_path` (`.forge/worktrees/<attempt_id>`) in its JSON payload. The path exists on disk and contains a full materialized tree, so it looks like the place to work. But it is only a materialization target: edits written into it before `forge attempt attach <id>` are silently overwritten when attach re-materializes the workspace. No warning, no error — the work is gone.

Repro: (1) temp repo, forge init --content-backend native; (2) forge start "intent A" (attempt 1 attached); (3) forge attempt start --intent <id> → attempt 2, attached:false, workspace_path .forge/worktrees/attempt_...; (4) echo edit >> .forge/worktrees/<attempt2>/src/App.css; (5) forge attempt attach <attempt2> → success; (6) forge save --attempt <attempt2> → changed_paths: [] — the edit is gone.

Desired: (a) the payload/help should stop presenting workspace_path as editable; (b) attach should detect the workspace dir drifted from its recorded materialized content and refuse with a typed error naming the drifted paths, unless an explicit discard flag is passed; (c) integration tests covering the repro, the override, the no-drift path, and never-materialized workspaces.
