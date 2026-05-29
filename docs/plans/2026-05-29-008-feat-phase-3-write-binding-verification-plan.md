---
title: "Forge M2 Phase 3 — Write-Binding Verification + ContentBackend Worktree Isolation"
type: feat
status: in_progress
date: 2026-05-29
deepened: 2026-05-29
origin: docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md
ticket: NER-134
---

# Forge M2 Phase 3 — Write-Binding Verification + ContentBackend Worktree Isolation

## Problem statement

`forge save --attempt X` today snapshots **whatever is in the worktree** and records it
under attempt `X`, without checking that the worktree actually reflects `X`. If a
*different* attempt `Y` was most recently materialized into the checkout (via
`attempt attach Y`, or simply because a freshly-created `attempt start` does **not**
materialize anything), then `save --attempt X` silently records `Y`'s content as `X`'s
snapshot. This is the **cross-attempt contamination footgun**.

It is masked today only because every passing test either (a) operates on a single
attempt, or (b) calls `attempt attach <id>` immediately before `save --attempt <id>`
so the worktree already matches. The `competing_attempt_loop_exports_selected_proposal`
integration test does exactly (b) — it attaches `second_attempt` before saving to it.
Nothing exercises "attempt Y materialized, then `save --attempt X`."

This is the v0 wedge's core trust property: "multiple agents attempt, the best is
selected." A wrong `--attempt` silently recording the wrong files is a trust-destroying
autonomous-loop failure, and it would corrupt the inputs to the headline Phase 6
compare/rank differentiator (NER-137), which ranks attempts by their bound snapshot
`content_ref`s.

Secondarily, the core CLI lifecycle (`crates/forge-cli/src/main.rs`) computes worktree
baselines by calling `forge_content_git::` functions **directly** — git-specific
`current_head` and `content_ref_for_commit_tree` are open-coded in the `attach`, `start`,
`attempt start`, `accept`, and `export` paths. This leaks git-worktree semantics into
core lifecycle code (PRD §23.4 "git adapter leaks into the core"), making the future
Phase 7 native walker harder to seam in and the native backend silently git-dependent.

## Goals

- **G1.** `save --attempt X` provably records `X`'s content, never a different attempt's.
  On a binding mismatch it returns a **new, additive, typed `ForgeError`** (with a
  `forge schema` registry entry + drift-guard assertion), not a string bail.
- **G2.** Worktree/base materialization is confined behind the `ContentBackend` boundary:
  no `forge_content_git::` worktree-management calls remain in core lifecycle code.
- **G3.** A regression test proves G1 directly (Y materialized → `save --attempt X` fails
  with the typed code, recording nothing under X).
- **G4.** Zero regressions to M1 invariants (Phase 1a/1b/2) and the existing
  single-attempt and competing-attempt flows.

## Non-goals (hard scope boundaries)

- **No physical per-attempt worktrees / workspace directories.** Explicitly deferred to
  Phase 8 (NER-139). The competing-attempts brainstorm scopes the wedge as single-checkout
  proposal-level competition (decision: "Start with proposal-level competition … uses the
  current checkout rather than creating separate physical worktrees").
- **No native base anchoring / native walker.** Making `base_head` a native tree id and
  replacing `git ls-files`/`git diff` is Phase 7 (NER-138). At this stage git remains the
  materialization adapter (per ROADMAP Phase 3: "Keep git as the materialization adapter at
  this stage").
- **No new merge/diff/compare engine.** Phase 6/8.
- **No change to the `forge.cli.v0` schema_version** — every contract change here is purely
  additive (one new error code), so the version is retained.

## Current state (cited)

### The footgun
- `crates/forge-cli/src/main.rs` `save_response` (~line 360): calls
  `selected_backend(&cwd)?.snapshot_worktree(&cwd)?` then
  `forge_store::save_snapshot(&cwd, request_id, args.attempt.as_deref(), …)`. No binding
  check.
- `crates/forge-store/src/lib.rs` `save_snapshot` (~line 647): calls
  `resolve_attempt_in_context(&context, attempt_id)?.attempt` to pick the target attempt,
  then writes the snapshot row bound to it. **No check that the worktree reflects that
  attempt.**

### The workspace→attempt binding already exists
- `current_state.attached_attempt_id` (nullable FK to `attempts(id)`, added by
  `crates/forge-store/migrations/002_columns.sql`) records which attempt the worktree was
  last materialized to. It is loaded into `RepositoryContext::attached_attempt_id`
  (`lib.rs:63`).
- It is written in exactly two places, both `UPDATE current_state SET attached_attempt_id`:
  `create_attempt` (only when `attach: true`) and `attach_attempt`.
- **`restore` is a third worktree-materialization path that does NOT update the binding.**
  `restore_response` materializes an *arbitrary* `snapshot_id`'s content and calls
  `record_restore`, but it neither sets `attached_attempt_id` nor verifies the snapshot
  belongs to the bound attempt. Its only guard is a dirty-check against the *resolved*
  attempt's latest snapshot. So `restore <B-snapshot>` while attached to `A` (worktree clean
  vs `A`'s latest) leaves the worktree holding `B`'s content with `attached_attempt_id = A`
  — a second cross-attempt contamination vector, closed by Piece 1b.
- **`forge start`** → `create_attempt(attach: true)` → attaches A1.
- **`attempt start --intent`** → `start_attempt_for_intent` → `create_attempt(attach: false)`
  → does **not** attach and does **not** materialize. This is the contamination entry
  point: after `attempt start`, `attached_attempt_id` still points at the *previous*
  attempt while the worktree still holds the previous attempt's content.
- **`attempt attach`** → `attach_attempt` → materializes the target's latest snapshot
  (or base) and sets `attached_attempt_id`.
- `resolve_attempt_in_context` (`lib.rs:1519`) resolves: explicit `--attempt` id → by id;
  else attached (if active) → attached; else sole active attempt → it; else
  `AmbiguousAttempt`/`NoActiveAttempt`.

### The git-worktree leaks in core lifecycle (`main.rs`)
`grep forge_content_git:: crates/forge-cli/src/main.rs`:
- `current_head` at `:259` (start base_head), `:279` (attempt start base_head),
  `:327` (attach dirty-check baseline), `:490` (accept stale-base), `:591` (export stale-base).
- `content_ref_for_commit_tree` at `:320`, `:328`, `:341` (attach baseline materialization).
- `branch_exists` at `:614` and `create_branch_from_*` (export branch creation) — the
  **git-export interop adapter**.

### Error/contract plumbing to extend (additive)
- `crates/forge-store/src/error.rs`: `ForgeError` enum + `code()` + `retryable()` +
  `after_ms()` + `details()` + `Display`; the `error_registry()` slice; and the in-file
  drift-guard test `registry_covers_every_variant` (exhaustive `match` + length assertion).
- `crates/forge-cli/src/schema.rs`: derives the registry from `forge_store::error_registry()`
  — automatically picks up a new variant.
- `crates/forge-cli/tests/forge_schema.rs`: `const FORGE_ERROR_CODES` array + assertions
  must list the new code.

## Proposed approach

Two coordinated pieces. Piece 1 is the correctness fix; Piece 2 is the boundary isolation.
They share the attach/baseline code paths, so they are planned together but are
independently verifiable.

### Piece 1 — Write-binding verification

**Binding rule (identity-based).** The record of "which attempt the worktree currently
reflects" is `attached_attempt_id` — set by `forge start` and `attempt attach`. The one
command that can desync it from worktree content is `restore` (see Current state); Piece 1b
closes that, after which `attached_attempt_id` is a faithful binding. Content-equality
cannot be the check: `save`'s whole purpose is to capture divergence from the base, so the
live worktree `content_ref` legitimately differs from the attempt's last snapshot.
**Identity (attached attempt == save target) is the sound check, given Piece 1b.**

The verdict:

| attached_attempt_id | save target (resolved) | outcome |
|---|---|---|
| `None` | any | **allow** (single-attempt / nothing materialized — preserves v0) |
| `Some(W)` | `W` | **allow** |
| `Some(W)` | `X ≠ W` | **`ATTEMPT_WORKTREE_MISMATCH`** |

Rationale for `None → allow`: contamination requires a *different* attempt to have been
materialized, and materialization always sets `attached_attempt_id`. `None` means the
worktree reflects the base / the implicit sole attempt — safe, and required by R2
(preserve single-attempt workflows). The `ambiguous_attempt_requires_explicit_selector`
test force-clears the attachment and then does `save --attempt first`; that must keep
working (it does, since attached is `None`). Use the **raw** `attached_attempt_id`
(regardless of the attached attempt's active/abandoned status): if the worktree was
materialized to `W`, recording its content under `X ≠ W` is contamination even if `W` was
later abandoned.

**Where the check lives (caller-graph lesson, NER-133 §5).** The authoritative check goes
**inside `save_snapshot`** (the production write path that cannot be bypassed), as a shared
helper operating on `&RepositoryContext` + the resolved `AttemptRecord`. To honor the
ticket's "**before snapshotting**" wording and avoid writing orphan content objects on the
error path, the CLI `save_response` also calls a lightweight pre-check
(`forge_store::verify_save_target`) *before* `snapshot_worktree`. Both call the same
private helper, so there is one source of truth and no logic drift. **Both sites are kept**
(committed, not optional): the in-`save_snapshot` check is the non-negotiable authoritative
guard on the production write path; the CLI pre-check is the fail-fast that avoids writing
orphan content objects. `verify_save_target` returns the resolved id, which is passed to
`save_snapshot` as an explicit `--attempt`, so `save_snapshot`'s own resolution becomes a
by-id lookup — the double-resolution is intended and idempotent; the in-`save_snapshot`
check must not be "optimized away."

**New typed error.** Add `ForgeError::AttemptWorktreeMismatch { requested_attempt: String,
attached_attempt: String }`:
- `code()` → `"ATTEMPT_WORKTREE_MISMATCH"`
- `retryable()` → `false` (deterministic — re-running without re-materializing repeats it)
- `after_ms()` → `None`
- `details()` → `json!({ "requested_attempt": …, "attached_attempt": … })`. Both fields are
  **minted opaque attempt ids** (`new_id("attempt")`, UUIDv7-based — never user input,
  paths, or secret content), so this variant is **deliberately exempt from path-redaction**
  (unlike `DirtyWorktree`/`StaleBase` which carry paths). A `details_carry_expected_keys`-
  style test will assert the payload carries only these two id keys, keeping the
  "details never leak" invariant auditable as the error set grows.
- `Display` → e.g. `"worktree is materialized for attempt {attached_attempt}, not the
  requested {requested_attempt}; attach it first"`

Registry/contract updates (all additive):
- add the `ErrorCodeSpec` entry to `error_registry()`;
- extend the drift-guard `all` array + exhaustive `match` in `registry_covers_every_variant`;
- extend `codes_match_the_pre_change_registry`;
- add `"ATTEMPT_WORKTREE_MISMATCH"` to `FORGE_ERROR_CODES` in `tests/forge_schema.rs`.

### Piece 1b — Restore binding verification (closes the second contamination vector)

`restore` is the other path that can leave the worktree reflecting an attempt other than
the bound one (see Current state). Without closing it, the Piece 1 "identity is sound"
guarantee is only true for the start/attach flows — and the ticket's own title is "close
cross-attempt contamination footgun," of which restore is an instance. This is **not**
physical-worktree scope creep; it is the same single-checkout proposal-level binding
correctness, and it is small.

Rule: `restore <snapshot_id>` may only materialize a snapshot that **belongs to the bound
(resolved) attempt**. The dirty-check already *assumes* this (it compares against the
resolved attempt's latest snapshot); Piece 1b makes it enforced. On violation, reuse
`ForgeError::AttemptWorktreeMismatch { requested_attempt: <snapshot's owning attempt>,
attached_attempt: <resolved/bound attempt> }` — coherent field semantics ("the attempt
whose content you'd bring in" vs "the attempt the worktree is bound to"), so **no second new
error code**.

Placement: the check must run **before** `restore_snapshot` materializes (otherwise the
worktree is already clobbered with the wrong content), so it lives in `restore_response`
before the `backend_for_content_ref(...).restore_snapshot(...)` call, backed by a small
store helper `snapshot_owner_attempt_id(cwd, snapshot_id) -> Result<String>` (or fold the
ownership check into the existing `snapshot_content_ref` lookup). `restore` already holds
the per-command advisory lock; the check is a pure read.

### Piece 2 — ContentBackend worktree isolation

Extend the `ContentBackend` trait (`crates/forge-content/src/lib.rs`) with the two
worktree/base operations the core lifecycle currently open-codes against git:

```rust
pub trait ContentBackend {
    fn snapshot_worktree(&self, repo_root: &Path) -> Result<SnapshotContent>;
    fn restore_snapshot(&self, repo_root: &Path, content_ref: &str) -> Result<()>;
    // NEW (NER-134):
    /// The backend's current base revision anchor for a fresh attempt / stale-base check.
    fn current_base(&self, repo_root: &Path) -> Result<String>;
    /// The restorable content_ref that materializes `base` into the worktree.
    fn base_content_ref(&self, repo_root: &Path, base: &str) -> Result<String>;
}
```

- **Git backend** (`forge-content-git`): `current_base` = existing `current_head`;
  `base_content_ref` = existing `content_ref_for_commit_tree`. Thin wrappers over code
  that already exists.
- **Native backend** (`forge-content-native`): at this stage native still anchors bases on
  git (existing reality — `base_head` is a git SHA even for native repos, and native
  already shells to git for `ls-files`/`diff`). So the native impls delegate to git via the
  backend's existing git shell-out path, marked `// Phase 7 (NER-138): replace with native
  base anchoring`. This is the ROADMAP-sanctioned "keep git as the materialization adapter
  at this stage" — now confined *behind the trait* instead of leaking into core.
  - **Resolved (was O3):** native shells git directly via its existing `git()` helper
    (it already shells git for `ls-files`/`diff`), so **no new `forge-content-git` crate
    dependency**. The ~2–3 lines duplicated (`git rev-parse --verify HEAD`, `git rev-parse
    {commit}^{tree}` → `git-tree:` prefix) are tolerated because they vanish in Phase 7 when
    native stops using git; the trait crate `forge-content` stays git-free by design, so a
    shared helper cannot live there. The native `base_content_ref` therefore returns a
    `git-tree:` ref **intentionally** — `backend_for_content_ref` routes that ref's restore
    back to the git backend (existing behavior; an implementer must NOT "fix" native to emit
    `forge-tree:` base refs — that is Phase 7).

Route the **core lifecycle** call sites through `selected_backend(&cwd)?`:
- `start` / `attempt start` base_head ← `selected_backend.current_base`
- `attach` baseline + dirty-check ← `selected_backend.current_base` /
  `selected_backend.base_content_ref`
- `accept` / `export` stale-base ← `selected_backend.current_base`

**Stays as the explicit git-export interop adapter** (not core worktree management; ROADMAP
keeps git export as interop): `branch_exists` + `create_branch_from_*` in the `export
branch` arm. These will be the *only* remaining `forge_content_git::` references in
`main.rs`, and will carry a comment naming them the sanctioned git-export adapter boundary.
*Open question O2 (for doc-review):* confirm leaving export-branch creation as an explicit
adapter (vs. abstracting it now) is the right [S] boundary — I believe it is, since native
publication is a Phase 6/9 concern.

## Implementation steps

1. **Add `ForgeError::AttemptWorktreeMismatch`** in `error.rs`: variant + `code()` +
   `retryable()`/`after_ms()` + `details()` + `Display`; add its `error_registry()` entry;
   extend both in-file drift-guard tests (`registry_covers_every_variant`,
   `codes_match_the_pre_change_registry`). *Verify:* `cargo test -p forge-store error::`.
2. **Add the binding helper + check in `save_snapshot`** (`lib.rs`): private
   `fn verify_worktree_binding(context, attempt) -> Result<()>` implementing the table
   above; call it in `save_snapshot` right after resolving the attempt, before the snapshot
   row write. Add `pub fn verify_save_target(cwd, attempt_id) -> Result<String>` that opens
   the context, resolves the attempt, runs the helper, and returns the resolved attempt id.
3. **Wire the CLI fail-fast pre-check** in `save_response` (`main.rs`): call
   `forge_store::verify_save_target` before `snapshot_worktree`; pass the resolved id into
   `save_snapshot`. (`save_snapshot` keeps its own authoritative check — defense in depth.)
4. **Close the restore vector (Piece 1b)**: add `pub fn snapshot_owner_attempt_id(cwd,
   snapshot_id) -> Result<String>` in `lib.rs`; in `restore_response` (`main.rs`), before
   `restore_snapshot` materializes, resolve the worktree's bound attempt and reject if the
   snapshot's owning attempt differs, raising `AttemptWorktreeMismatch`.
5. **Extend `ContentBackend`** with `current_base` + `base_content_ref`
   (`forge-content/src/lib.rs`), implement in git + native backends (Piece 2; native shells
   git directly, Phase-7-marked).
6. **Route core lifecycle through the backend**: replace the `forge_content_git::`
   worktree/base calls in `main.rs` (`start`, `attempt start`, `attach`, `accept`,
   `export` stale-base) with `selected_backend(&cwd)?` calls. Leave the export-branch
   adapter calls (`branch_exists`, `create_branch_from_*`) with a boundary comment — they
   are the only remaining `forge_content_git::` refs in `main.rs`.
7. **Tests** (see strategy: save-mismatch, restore-mismatch, error-contract, regressions).
8. **Contract test** (`tests/forge_schema.rs`): add `"ATTEMPT_WORKTREE_MISMATCH"`.
9. **Full verify**: `bash scripts/ci.sh` (fmt + test + clippy `-D warnings` + e2e eval).

## Testing strategy

- **G3 exit-criterion test** (`tests/forge_attempts.rs`, new
  `save_records_target_attempt_not_materialized_attempt`): init → `start "compete"` (A1) →
  write+`save` (A1) → `attempt start --intent` (A2, attached still A1) →
  `save --attempt A2` **must fail** with `ATTEMPT_WORKTREE_MISMATCH`, and the store must
  contain **no** snapshot for A2 — assert `attempt show A2`'s `data.latest_snapshot` is
  `null`. Then `attempt attach A2` → `save --attempt A2` **succeeds** and records A2's
  content.
- **Piece 1b restore test** (`tests/forge_attempts.rs`, new
  `restore_rejects_cross_attempt_snapshot`): with A1 (snapshot S1) and A2 attached + clean,
  `restore <S1> --yes` **must fail** with `ATTEMPT_WORKTREE_MISMATCH` and leave the worktree
  unchanged (A2's content); restoring an *own*-attempt snapshot still succeeds.
- **Error contract test** (`tests/forge_errors.rs`): the mismatch envelope carries
  `errors[0].code == "ATTEMPT_WORKTREE_MISMATCH"`, `retry.retryable == false`, and
  `details.requested_attempt` / `details.attached_attempt`.
- **Details-shape test** (`forge-store` `error::tests`, mirroring
  `details_carry_expected_keys`): `AttemptWorktreeMismatch.details()` carries exactly the
  two id keys (security invariant: no path/secret leak).
- **Regression**: existing `competing_attempt_loop_exports_selected_proposal`,
  `ambiguous_attempt_requires_explicit_selector`, `attach_*`, and single-attempt
  start/save tests must stay green unchanged.
- **Piece 2**: a `grep` assertion (manual, recorded in the code-review doc) that
  `forge_content_git::` no longer appears in `main.rs` outside the `export branch` arm;
  run the existing suite under both `FORGE_CONTENT_BACKEND=git` and `=native` if the
  harness supports it (the e2e eval covers the default).
- **Drift guards**: `forge-store` `registry_covers_every_variant` + `forge-cli`
  `forge_schema` tests enforce the new code is wired end-to-end.

## Risks & open questions

- **O1 — `attached=None` with multiple attempts + explicit `--attempt X`.** The plan allows
  this (no binding to contradict). It is the documented preserve-v0 path and matches the
  existing force-clear test. Doc-review: confirm this is acceptable (alternative: require an
  attached attempt once >1 active attempt exists — rejected as over-strict for v0).
- **O2 — export-branch adapter boundary** (see Piece 2). Confirm leaving
  `create_branch_from_*`/`branch_exists` as the explicit git-interop adapter.
- **O3 — native base-op delegation** (see Piece 2). Confirm approach (a) (native shells git
  directly) vs (b) (crate dep).
- **Orphan objects on the error path.** If only the in-`save_snapshot` check fired (no CLI
  pre-check), `snapshot_worktree` would already have written unreferenced content objects —
  harmless (same state as a crash-before-commit; `doctor` does not flag unreachable
  objects; real GC is Phase 8). The CLI pre-check avoids this in the common case. Documented,
  not blocking.
- **M1 invariants.** No connection/transaction/lock changes: the binding check is a pure
  read of already-loaded `RepositoryContext` state inside the existing flow; no new lock
  acquisition (the check runs within `save`, which already holds the per-command lock via
  `requires_repo_lock`), preserving acquire-once/never-nested. No ID/format/WAL/IMMEDIATE
  changes. Trait additions are compile-time only.

## Out of scope / future work

- Physical per-attempt worktrees → Phase 8 (NER-139).
- Native base anchoring + native walker (remove all git shell-outs) → Phase 7 (NER-138).
- Abstracting git-export publication behind a backend boundary → Phase 6/9.
- Content-level (not identity) worktree verification → not needed; identity is sound for the
  single-checkout model.
