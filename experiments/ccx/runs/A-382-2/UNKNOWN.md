# UNKNOWN — ccx-task-382-2-drift-guard (revision 1)

## 1. The mandated diff primitive mutates the workspace dir the invariants say must stay untouched

**Kind:** blocking

**What I need to know:** Which requirement wins — the invariant that the drift
check "reads the workspace dir only; it never mutates it" (and that "a refused
attach leaves both the workspace dir and current_state untouched"), or the
interface + negative-constraint requirement to use the existing
working-vs-tree diff primitive (`forge_content_native::diff_working_vs_tree`)?
As the code stands, both cannot be satisfied at once.

**Why the brief does not answer it:** `diff_working_vs_tree` unconditionally
(best-effort) writes a status cache into the *scanned* root as a side effect.
The drift check must scan the attempt workspace dir
(`.forge/worktrees/<attempt_id>`), so the check would create
`<workspace>/.forge/tmp/` and write `<workspace>/.forge/status-cache.json` —
a mutation of the workspace dir, on the refusal path included. Every escape
route is closed by the contract itself:

- I may not add a read-only/no-cache variant of the primitive:
  `crates/forge-content-native/**` is in `forbidden_paths`.
- I may not write a different diff: negative constraint "Do not write a new
  diff; use the existing working-vs-tree diff" (single diff engine invariant).
- The obvious workaround — snapshot the workspace into the object store and
  compare content refs via the existing tree-vs-tree diff
  (`diff_native_content_refs`) — is barred by negative constraint 3: it would
  persist the drifted workspace content into the store, which is exactly the
  "auto-snapshot drifted content" recovery layer the ticket explicitly defers.

The cache file is metadata-only and policy-excluded from all snapshots/diffs
(`is_ignored_by_policy` skips `.forge/`), so it never affects content
semantics — the contract authors may consider it acceptable. But the
invariants as written ("never mutates it", "workspace dir … untouched") are
violated byte-for-byte, and the neighbor tests task (ccx-task-382-3, contract
missing) could plausibly assert exactly that. Choosing a reading weakens a
normative clause either way, so I stopped instead of guessing.

**Evidence:**
- `crates/forge-content-native/src/lib.rs:97-108` — `diff_working_vs_tree`
  calls `status_cache::working_fingerprints(repo_root)` (line 105), where
  `repo_root` is the scanned root, not the repository root.
- `crates/forge-content-native/src/status_cache.rs:91-97` —
  `working_fingerprints` always ends with `let _ = write_cache(repo_root, …)`.
- `crates/forge-content-native/src/status_cache.rs:107-119` — `write_cache`
  creates `<scanned-root>/.forge/` and `<scanned-root>/.forge/tmp/`, then
  persists the cache file.
- `crates/forge-content-native/src/status_cache.rs:193-195` — `cache_path` is
  `<scanned-root>/.forge/status-cache.json`.
- `crates/forge-cli/src/commands/core.rs:291-298` — existing caller precedent:
  the second argument is the directory being scanned
  (`context.worktree_path`), confirming the drift check would have to pass the
  workspace dir there.
- `crates/forge-store/src/attempts.rs:236-238` — the workspace dir the check
  must scan (`.forge/worktrees/<attempt_id>`).
- Task contract: invariants 2 and 3; interface paragraph 1; negative
  constraints 2 and 3; `allowed_changes.forbidden_paths`.

**Possible resolutions (for the reviewer to pick, not me):**
1. Amend invariants 2/3 to "never mutates workspace *content*" and accept the
   policy-excluded status-cache side effect of the mandated primitive.
2. Relax `forbidden_paths` to allow a read-only entry point (or a
   `write_cache: bool` option) in `forge-content-native`.
3. Some other mechanism I shouldn't invent under the current contract.

## 2. Semantics when the workspace dir is missing or emptied

**Kind:** assumption (not acted on)

The brief covers `materialized_content_ref` NULL (skip the check) but not a
recorded ref with the workspace dir deleted by the user (`rm -rf
.forge/worktrees/<id>`). A strict "compare and refuse if different" reading
makes total deletion maximal drift (every recorded path deleted →
`WORKSPACE_DRIFT`, recoverable via `--discard-workspace-changes`), but the
walk of a missing root may instead surface an untyped I/O error
(`crates/forge-content-native/src/lib.rs:1551-1580`, `walk_worktree` /
`map_walk_error`). If unblocked on item 1, I would treat "dir missing" the
same as drift only if the diff primitive naturally reports it, and surface
any walk error as-is; noting it here since the brief is silent.

## 3. Neighbor contracts missing

**Kind:** observation

The brief marks the neighbor contracts `ccx-task-382-1-payload-honesty` and
`ccx-task-382-3-tests` as MISSING and instructs surfacing rather than
guessing. Not blocking for implementing the guard itself (this task's
acceptance only requires the existing `forge-store` / `forge_attempts` suites
to pass; new-behavior tests appear to be 382-3's scope), but item 1's
resolution likely affects what 382-3 asserts about "workspace dir untouched."

## Work not done

No code changes were made. The planned implementation (for reference once
unblocked): new `ForgeError::WorkspaceDrift { paths }` appended to the enum,
`code() = "WORKSPACE_DRIFT"`, `details() = redact_paths(paths)` (same
secret-risk filtering as `DIRTY_WORKTREE`, `crates/forge-store/src/error.rs:576-593`),
Display message naming `--discard-workspace-changes`, registry entry appended
in `error_registry()` (additive-only), drift-check fn in
`crates/forge-store/src/attempts.rs` reading
`attempt_workspaces.materialized_content_ref`, CLI flag on
`AttemptCommand::Attach` (`crates/forge-cli/src/args.rs:225-227`), check
invoked in the attach arm of `crates/forge-cli/src/commands/core.rs:169-216`
after the existing `DIRTY_WORKTREE` guard and before
`restore_effective_worktree`/`materialize_attempt_workspace`.
