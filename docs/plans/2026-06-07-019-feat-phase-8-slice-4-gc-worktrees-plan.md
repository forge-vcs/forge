---
title: "feat: Phase 8 Slice 4 - real GC deletion and physical attempt worktrees"
status: active
date: 2026-06-07
origin: docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md
---

# Phase 8 Slice 4: Real GC Deletion and Physical Attempt Worktrees

## Problem Frame

Forge now has native `forge-tree:` storage, native history roots, conflict metadata, and advisory conflict suggestions. The remaining unsafe gap before full native dogfooding is that garbage collection is still a dry-run report and all mutating worktree operations still share the repository root as the physical checkout.

Slice 4 makes two related safety guarantees real:

1. Unreachable native objects and abandoned attempt workspaces can be reclaimed only after a complete, locked, doctor-clean reachability pass.
2. Each active attempt gets its own physical workspace under `.forge/`, so multiple agents can work on the same source repository without destructively switching the same checkout.

The implementation must preserve existing git-backed `git-tree:` compatibility and keep the JSON envelope stable: additive fields are acceptable; existing fields and error behavior must not be removed or renamed.

## Source Requirements Trace

- R23: Replace dry-run-only GC with real mark-sweep deletion. Roots are ledger tip, reachable-from-HEAD, op-log `state_json` commit IDs, per-attempt worktrees, and checkout targets. Hold the repo-level advisory lock across scan and unlink sweep. Add real `gc` to lock-requiring commands.
- R24: Real deletion is gated on a doctor-clean precondition. `doctor` must parse every `views.state_json`; any corrupt shape that makes `gc` fail closed must also be a doctor finding.
- R25: Deletion requires `--yes` and otherwise returns typed `CONFIRMATION_REQUIRED`. Deletion is preceded by a mandatory dry-run diff, honors a default protection window of at least 7 days, and is crash-safe: a crash mid-sweep over-protects rather than under-protects.
- R26: Fuzz reachability to prove GC reclaims only unreachable objects and never deletes reachable refs, recent operations, decisions, worktrees, or checkout targets.
- R27: Add physical per-attempt workspaces under `.forge/`. Switching must be non-destructive and concurrency-safe. Materialization filters secret-risk paths before writing, and JSON readback reapplies the same filter.
- R28: Add a worktree-level advisory lock around per-attempt workspace materialization.
- R29: Surface the per-attempt workspace path in attempt list/show JSON.
- AE4: `forge gc --yes` refuses deletion when `doctor` reports corruption.
- AE5: Objects reachable via per-attempt worktree or recent operation are not deleted.
- AE9: Secret-risk paths such as `.env` are not materialized into physical workspaces.
- AE10: A crash during `gc --yes` between root enumeration and unlink leaves `doctor` clean/no dangling, and torn accept replay still resolves.

## Scope

In scope:

- Native loose-object deletion for unreachable, old-enough objects under `.forge/objects`.
- Reclamation of abandoned physical attempt workspaces under `.forge/worktrees`.
- Workspace-aware snapshot/materialize paths for native repositories.
- Doctor extensions needed to make deletion fail closed.
- CLI JSON additions for `attempt list`, `attempt show`, and `gc`.
- Focused dogfood coverage in `scripts/dogfood-typescript-native.sh`.

Out of scope:

- Pack files, indexes, retention policies, compaction, and command-output/evidence retention tuning. Those belong to S5.
- Deleting ledger rows, evidence rows, decision rows, or proposal rows. The ledger remains the audit spine.
- Changing git-backed `git-tree:` semantics beyond preserving existing behavior through the shared command paths.
- Deleting local `dogfood/*` branches.

## Design Decisions

### 1. Keep the shared ledger, add an effective worktree

Add workspace-aware repository resolution instead of replacing the repository root model. `open_repository(cwd)` should still identify the owner repo, but it should also know whether the command is running from an attempt workspace. The owner repo continues to hold `.forge/forge.db`, native objects, refs, and repo-level locks. The effective worktree path is the directory that snapshot/materialize commands should read and write.

Recommended shape:

- Extend `RepositoryContext` in `crates/forge-store/src/lib.rs` with an optional effective workspace descriptor.
- Create physical workspaces under `.forge/worktrees/<attempt_id>/`.
- Write a small workspace marker inside each workspace, for example `.forge-workspace.json`, containing the owner repo and attempt id. The native walker must treat that marker as policy-excluded so it is never captured as source content.
- Teach root resolution to recognize the marker and open the owner repo while setting the effective worktree to the workspace path.

Rationale: agents can `cd .forge/worktrees/<attempt_id>` and use normal local tooling while Forge still has one authoritative ledger and object store. Existing repo-root workflows continue to work.

### 2. Store workspace metadata additively

Add a migration for attempt workspace metadata rather than overloading `current_state.attached_attempt_id`.

Recommended table:

- `attempt_workspaces(attempt_id PRIMARY KEY, repo_id, workspace_rel_path, status, materialized_content_ref, created_at_ms, updated_at_ms)`

`workspace_rel_path` should be repo-relative, for example `.forge/worktrees/<attempt_id>`. JSON should surface this value as `workspace_path` in `AttemptSummary` and therefore in both `attempt list` and `attempt show`.

Rationale: `attached_attempt_id` is a compatibility concept for the repo-root worktree. A separate table lets S4 preserve that behavior while adding isolated workspaces and GC roots.

### 3. Materialize with two locks

Per-attempt workspace materialization should hold:

- The repo-level lock for ledger/state consistency.
- A worktree-level advisory lock for `.forge/worktree-locks/<attempt_id>.lock` while the physical workspace is created or restored.

The worktree lock can reuse the implementation style from `crates/forge-store/src/repo_lock.rs`, but it should be path-scoped so parallel agents on different attempts do not block each other unnecessarily.

Rationale: R28 is specifically about closing the concurrent materialize window, while R23 requires `gc` to own the repo-level scan/sweep lock.

### 4. GC deletion uses a dry-run plan digest

Keep existing `forge gc --dry-run` behavior and add a deletion handshake:

- `forge gc --dry-run` returns the current report plus a deterministic `plan_digest`, `protection_window_days`, and `protected_native_objects`.
- `forge gc --yes --plan-digest <digest>` recomputes reachability while holding the repo lock. If the recomputed digest differs, fail closed with a typed `GC_PLAN_CHANGED` error.
- `forge gc --yes` without a plan digest fails with `CONFIRMATION_REQUIRED`.

Rationale: this satisfies the mandatory dry-run-before-delete requirement without adding an interactive prompt and keeps headless JSON workflows deterministic.

### 5. Default protection window is 7 days

Objects and abandoned workspaces newer than the retention cutoff are protected even if not reachable. Use filesystem metadata as the first implementation source for loose native objects and workspace directories. Default to 7 days, and make any override explicit and test-only or hidden until the product wants a public retention flag.

Rationale: the protection window covers torn-accept and recent-op orphan windows. S5 can introduce richer retention policy.

### 6. Doctor and GC share root parsing rules

Extract root enumeration into one internal helper used by both `doctor` and `gc`. Any malformed `views.state_json` or unparseable root that would make GC fail closed must produce a doctor finding with path-free details.

Rationale: R24 requires the same corrupt-root class to be visible before users try real deletion.

## Implementation Units

### Unit 1: Workspace Metadata and JSON Surface

Files:

- `crates/forge-store/src/migrations.rs`
- `crates/forge-store/src/lib.rs`
- `crates/forge-cli/tests/forge_attempt_worktrees.rs`
- `crates/forge-cli/tests/forge_schema.rs`

Work:

- Add the `attempt_workspaces` migration.
- Create workspace metadata when an attempt starts or is first materialized.
- Add `workspace_path` to `AttemptSummary`.
- Include `workspace_path` in `attempt list` and `attempt show` output.
- Preserve existing `attached` semantics.

Tests:

- Starting an attempt in a native repo creates/surfaces a repo-relative workspace path.
- `attempt list` and `attempt show` include `workspace_path` without removing any existing JSON keys.
- Git-backed repos still pass existing attempt list/show tests.
- Schema allowlists include new additive fields and new typed errors.

### Unit 2: Effective Worktree Resolution

Files:

- `crates/forge-store/src/lib.rs`
- `crates/forge-cli/src/main.rs`
- `crates/forge-content-native/src/lib.rs`
- `crates/forge-cli/tests/forge_attempt_worktrees.rs`

Work:

- Extend repository opening to detect workspace markers and return both owner repo root and effective worktree path.
- Update CLI snapshot/materialize call sites to use effective worktree paths while using the owner repo path for the native object store.
- Exclude workspace marker files from native snapshot/diff/materialize readback policy.
- Keep repo-root attach/save/restore/check/accept flows compatible.

Tests:

- Two attempts materialize the same source file into different workspace directories.
- Editing attempt A's workspace does not change attempt B's workspace or the repo root.
- Running `forge save`, `forge check`, and `forge propose` from inside a workspace binds to that workspace's attempt.
- Running existing repo-root workflows still behaves as before.
- `.env` in a stored tree is not materialized into a workspace and does not appear in JSON diff/readback surfaces.

### Unit 3: Worktree-Level Locking

Files:

- `crates/forge-store/src/repo_lock.rs`
- `crates/forge-store/src/lib.rs`
- `crates/forge-cli/tests/forge_repo_lock.rs`
- `crates/forge-cli/tests/forge_attempt_worktrees.rs`

Work:

- Add a reusable lock helper for path-scoped advisory lock files.
- Lock `.forge/worktree-locks/<attempt_id>.lock` around workspace creation and materialization.
- Ensure stale lock behavior matches the existing repo lock contract.

Tests:

- A held worktree lock makes concurrent materialization for the same attempt fail with the existing lock-style typed failure.
- A held worktree lock for attempt A does not block materialization for attempt B.
- A held repo lock still blocks mutating commands, including real `gc`.

### Unit 4: Doctor-Clean Deletion Preconditions

Files:

- `crates/forge-store/src/lib.rs`
- `crates/forge-cli/tests/forge_doctor_gc.rs`

Work:

- Extract reachability/root enumeration so doctor and GC parse `views.state_json` consistently.
- Add a doctor report field for corrupt ledger view rows, carrying only row ids/kinds and closed error categories.
- Make `gc --yes` call doctor first and refuse deletion unless `ok == true`.
- Keep `gc --dry-run` fail-closed behavior for corrupt root enumeration.

Tests:

- A malformed `views.state_json` appears in `doctor` and makes `ok=false`.
- The same malformed row makes `gc --yes` refuse deletion before unlinking anything.
- Existing dangling native content still remains a doctor finding while dry-run GC stays tolerant where the current contract says it is tolerant.

### Unit 5: Real Native Object and Workspace GC

Files:

- `crates/forge-cli/src/main.rs`
- `crates/forge-store/src/lib.rs`
- `crates/forge-content-native/src/lib.rs`
- `crates/forge-cli/tests/forge_doctor_gc.rs`
- `crates/forge-cli/tests/forge_native_history.rs`

Work:

- Add `--yes` and `--plan-digest` to `GcArgs`.
- Add `gc` to the lock-requiring command set.
- Extend `GcDryRunReport` additively with plan digest, protection window, protected objects/workspaces, and abandoned workspaces.
- Implement deletion only for old, unreachable native loose objects and abandoned physical workspaces.
- Keep deletion path-free in errors and JSON: expose opaque object ids and repo-relative workspace paths only.
- Add a debug crash point during sweep to test crash-safe ordering.

Tests:

- `forge gc` without `--dry-run` or `--yes` returns `CONFIRMATION_REQUIRED`.
- `forge gc --yes` without the prior plan digest returns `CONFIRMATION_REQUIRED`.
- `forge gc --yes --plan-digest <digest>` deletes only objects/workspaces listed in the matching dry-run plan.
- A changed plan digest fails with `GC_PLAN_CHANGED`.
- Recent unreachable objects are protected by the default 7-day window.
- Objects reachable from native HEAD, decisions, op-log checkout targets, snapshots, proposal revisions, and attempt workspaces are not deleted.
- Crash injection after the Nth unlink leaves `doctor` clean and a repeated GC succeeds.

### Unit 6: Reachability Fuzz and Dogfood Harness

Files:

- `crates/forge-store/src/lib.rs`
- `crates/forge-cli/tests/forge_doctor_gc.rs`
- `scripts/dogfood-typescript-native.sh`

Work:

- Add a deterministic reachability fuzz/property-style test around the shared root enumerator and native object store.
- Extend the TypeScript dogfood harness to run at least two attempt workspaces against the same temporary repo.
- Include a conflict/suggestion loop from workspace paths and a GC dry-run/delete cycle after abandoning a workspace.

Tests:

- Fuzz proves no reachable object from HEAD, decisions, view JSON, snapshots/proposals, recent ops, workspace metadata, or checkout targets is classified deletable.
- Dogfood harness shows two isolated TypeScript attempts can edit the same files independently, run `tsc --noEmit`, propose/check/accept, and then GC abandoned workspace data.
- Dogfood confirms `.env` is not materialized into physical workspaces.

## Sequencing

1. Add metadata and JSON surfaces first. This is additive and validates the shape of workspaces before changing command behavior.
2. Add effective worktree resolution and materialization. This unlocks real isolated dogfood without deletion risk.
3. Add worktree-level locking.
4. Extend doctor and shared reachability parsing.
5. Add real GC deletion gates and sweep implementation.
6. Add fuzz, crash, and TypeScript dogfood coverage.
7. Run the full verification gate: format, workspace tests, clippy, dogfood harness, then CE code review in autofix mode.

## Compatibility Notes

- Existing JSON envelope structure stays stable. New fields are additive.
- Git-backed `git-tree:` repositories should keep using existing backend behavior. Workspace JSON can still surface, but native-only deletion must skip git object storage.
- Existing repo-root attach flow remains available for backward compatibility. Workspace-aware commands add isolation without forcing old scripts to change immediately.
- Secret filtering must continue to use `forge_content::is_ignored_by_policy`; no second secret list should be introduced.

## Risks and Mitigations

- Risk: a workspace under `.forge/` is accidentally treated as internal metadata and skipped entirely. Mitigation: effective worktree paths are passed explicitly to snapshot/materialize; only owner repo metadata remains under the owner root.
- Risk: real GC misses a root and deletes a live object. Mitigation: shared root enumerator, doctor-clean gate, plan digest handshake, 7-day protection window, reachability fuzz, and crash tests.
- Risk: repo-root compatibility regresses because workspace support changes `attached_attempt_id`. Mitigation: keep workspace metadata separate and preserve existing attach semantics.
- Risk: workspace marker leaks into snapshots or diffs. Mitigation: add the marker to the same policy exclusion path as `.forge`, `.git`, restore temps, and secret-risk paths.
- Risk: deleting abandoned workspaces removes user edits that were never saved. Mitigation: deletion only applies to abandoned/closed workspace metadata older than the protection window, with dry-run digest confirmation. Active attempt workspaces are roots.

## Verification

Required before opening the implementation PR:

- `rtk cargo fmt --all --check`
- `rtk cargo test --workspace`
- `rtk cargo clippy --workspace --all-targets -- -D warnings`
- `rtk bash scripts/dogfood-typescript-native.sh`
- CE code review in `mode:autofix`

Implementation is complete only when the new S4 tests pass and the dogfood harness demonstrates two isolated TypeScript attempts working against the same temporary repo.
