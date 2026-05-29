---
title: "Verify the workspace-to-attempt binding before a write, and confine the worktree adapter behind a trait while it still leaks"
date: 2026-05-29
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: write-binding-and-backend-boundary
severity: high
applies_when:
  - A command records "the current workspace" under a caller-supplied target id that may not match what the workspace actually holds
  - More than one path can change the worktree, and only some of them update the record of "what is materialized"
  - You are moving a concrete dependency (here git) behind a trait boundary but the dependency must keep doing the work for now
  - A new machine-visible error code is added to a CLI with a published, drift-guarded contract
tags: [write-binding, attached-attempt, content-backend, trait-boundary, git-leak, typed-errors, drift-guard, defense-in-depth, ner-134]
---

# Verify the workspace-to-attempt binding before a write, and confine the worktree adapter behind a trait while it still leaks

## Context

NER-134 (Forge M2 Phase 3) closed the **cross-attempt contamination footgun**: `forge save --attempt X`
snapshotted whatever was in the worktree and recorded it under attempt `X` with no check that the
worktree actually reflected `X`. It was masked in tests only because every test either used a single
attempt or ran `attempt attach <id>` immediately before `save --attempt <id>`. The same change moved
git-worktree/base operations behind the `ContentBackend` trait so git semantics stop leaking into core
lifecycle code (PRD §23.4). This captures the non-obvious learnings so the next "record the workspace
under an id" feature — and the Phase 7 native walker — does not re-derive them. Builds on the M1
substrate ([Related](#related)).

## Guidance

### 1. The soundness anchor is the *binding record*, not content equality

There was already a column recording "which attempt the worktree was last materialized to":
`current_state.attached_attempt_id`, written by exactly the two commands that materialize an attempt
(`forge start`, `attempt attach`). The correct check for `save --attempt X` is **identity**:
`attached_attempt_id == X` (with `None` allowed — nothing materialized means the single-attempt v0 path).
Content-equality is *wrong* here: `save`'s entire purpose is to capture divergence from the baseline, so
the live worktree legitimately differs from the attempt's last snapshot. Don't reach for a content hash
when an identity record already states the invariant. Use the **raw** `attached_attempt_id` (not a
re-resolved "active attempt") — if the worktree holds attempt `W`'s content, recording it under `X != W`
is contamination even if `W` was later abandoned.

### 2. Enumerate *every* path that mutates the protected state — the one you forget is the second hole

The plan's first draft asserted "`restore` operates within the resolved attempt, so it never desyncs the
binding." A doc-review feasibility pass falsified that against the code: `restore_response` materializes an
*arbitrary* `snapshot_id`'s content and does **not** update `attached_attempt_id`. So `restore <B's
snapshot>` while attached to `A` leaves the worktree holding `B`'s content with the binding still naming
`A` — a *second* contamination vector. The guarantee in §1 holds only once **all** materialization paths
are accounted for. The discipline: when a fix rests on an invariant about state, grep the full set of
writers of the *record* and the full set of mutators of the *thing the record describes* (here: writers of
`attached_attempt_id` = {start, attach}; mutators of worktree content = {start, attach, **restore**}). The
mismatch between those two sets *is* the bug surface. This became "Piece 1b" and reused the same error code
with coherent field semantics — no second code needed.

### 3. Two-site check: a CLI fast-fail pre-check + an authoritative in-store guard, sharing one helper

Put the **authoritative** check on the production write path that cannot be bypassed (inside
`save_snapshot`, the store function every caller funnels through). Add a **fast-fail pre-check** at the CLI
*before* the expensive/dirtying step (`snapshot_worktree`) so a mismatch writes **no orphan content
objects**. Make them agree by construction: the pre-check (`verify_save_target`) resolves the target and
returns the resolved id, which the CLI passes back to `save_snapshot` as an **explicit selector** — so the
in-store guard re-resolves the *same* id and runs the *same* `verify_worktree_binding` helper. One source
of truth, no logic drift, and the in-store guard is real defense-in-depth (a non-CLI caller still can't
bypass it). Do not "optimize away" the second check because the first always fires first — that deletes the
only guard a future direct caller would hit.

```rust
// CLI: fail fast, write no objects on mismatch
let resolved = forge_store::verify_save_target(&cwd, args.attempt.as_deref())?;
let content = selected_backend(&cwd)?.snapshot_worktree(&cwd)?;   // only reached if binding holds
let saved = forge_store::save_snapshot(&cwd, request_id, Some(resolved.as_str()), /* … */)?;
// save_snapshot re-runs verify_worktree_binding(context, attempt) — authoritative, non-bypassable
```

For materialize-then-record commands (`restore`), the check has **only one valid site**: *before*
materialization, at the CLI. A post-materialization "authoritative" guard is structurally impossible — once
`restore_snapshot` runs, the worktree is already clobbered. So the right placement is asymmetric across
commands; match it to where the irreversible step is, not to a uniform pattern.

### 4. Confine a leaking adapter behind a trait *before* you replace it — and forbid the premature "fix"

Phase 3 is not the native walker; git stays the materialization adapter. But the core lifecycle was calling
`forge_content_git::current_head` / `content_ref_for_commit_tree` **directly** in five+ sites. Adding
`current_base` / `base_content_ref` to the `ContentBackend` trait and routing core through
`selected_backend(&cwd)?` confines the leak now and leaves a clean seam for Phase 7 — *without* doing
Phase 7's work. The native backend's impls **intentionally** shell to git and return `git-tree:` refs,
which `backend_for_content_ref` routes back to the git backend for restore. This looks like a bug to a
future implementer ("native emits git-tree refs?!"). Mark it loudly (`// Phase 7 (NER-138): replace with
native base anchoring`) and state the constraint in the trait doc: an implementer **must not** "fix" native
to emit `forge-tree:` base refs before the native walker exists. A documented, behavior-preserving seam
beats a half-done reversal. (Deliberately accepted the ~3-line git-shell duplication across both backends
over a new `forge-content-git` crate dependency: the trait crate stays git-free, the dependency graph stays
acyclic, and the duplication vanishes in Phase 7.)

### 5. Push security invariants into the trait doc where the type system can't

Two leak surfaces the boundary introduced, pinned as named invariants on the new trait methods so the
Phase 7 reimplementation can't silently regress them:
- **S1 — no filesystem paths in `anyhow` context.** A `.with_context(|| format!("… {path}"))` on these
  methods would bubble a path into the *untyped* envelope `message`, bypassing the typed-error secret-path
  redaction that only protects `details`. The methods return opaque revision ids; keep paths out of their
  error context.
- **S2 — `base_content_ref` must reference a policy-excluded tree.** Materialization must keep honoring the
  shared `forge_content::is_ignored_by_policy` so `.env`/keys are never written into a competing attempt's
  worktree. Today this is inherited from git's tree + the git backend's filtered restore; the trait doc
  makes it a contract the native walker must satisfy.
These are convention-enforced, not type-enforced — so they live as `// S1`/`// S2` comments and trait docs,
and the follow-ups (a path-free-error test, a cross-backend planted-secret round-trip) are noted for
Phase 7.

### 6. A new error code is additive only when it lands on every contract surface at once

Adding `ForgeError::AttemptWorktreeMismatch` meant touching, in one change: the enum, `code()`,
`details()` (keys `requested_attempt`/`attached_attempt`), `retryable()`, `after_ms()`, `Display`, the
`error_registry()` `ErrorCodeSpec` (with `details_keys` matching what `details()` actually emits), the two
in-`error.rs` drift-guard tests (`registry_covers_every_variant`'s `all` array **and** its exhaustive
`match` arm; `codes_match_the_pre_change_registry`'s `assert_eq!` stanza), and the `tests/forge_schema.rs`
`FORGE_ERROR_CODES` list. Miss one and the published `forge.cli.v0` contract drifts; the drift-guard tests
exist precisely to fail when it does. The variant's `details` carries only opaque minted attempt ids, so it
is *deliberately* exempt from path-redaction — pinned by a dedicated `details_carry_only_ids` test so
"details never leak" stays auditable as the error set grows. `schema_version` stays `forge.cli.v0`: one new
code is purely additive.

## Why This Matters

The wedge is "multiple agents attempt, the best is selected." A `save --attempt X` that silently records a
different attempt's files is a trust-destroying autonomous-loop failure, and it corrupts the inputs to the
Phase 6 compare/rank differentiator (which ranks attempts by their bound snapshot `content_ref`s). The
non-obvious parts — that the invariant rests on a *record* not content, that `restore` was a second
unguarded mutator the plan initially missed, that the two-site check must share one resolved id, and that
the git seam must be confined-but-not-yet-replaced with the premature fix explicitly forbidden — are
exactly what a green test suite hides. The contamination was invisible to 150 passing tests because they
always attached first; the restore vector was caught only by reconstructing the actual `restore` code path
during doc-review, not by any test.

## When to Apply

- Any command that records "the current workspace/buffer/state" under a caller-supplied target id — verify
  the workspace's binding to that id before the write, using whatever record already tracks materialization.
- Any time a correctness fix rests on an invariant about mutable state: enumerate every writer of the
  *record* and every mutator of the *thing recorded*; the asymmetry is the bug surface.
- Any move of a concrete dependency behind a trait where the dependency must keep working for now — confine,
  mark, and forbid the premature reversal; don't half-do the replacement.
- Any new machine-visible error code in a CLI/daemon with a published contract: land it on every surface in
  one change and lean on a drift-guard test.

## Scope boundaries (deferred)

Physical per-attempt worktrees → Phase 8 (NER-139). Native base anchoring / walker (remove all git
shell-outs, make `base_head` a native tree id) → Phase 7 (NER-138). Unifying `save`'s and `restore`'s
"bound attempt" definition behind a shared `verify_restore_target` store helper, the S1 path-free-error
test, the S2 cross-backend planted-secret round-trip, and adding `repo_id` scoping to the pre-existing
`snapshot_content_ref` — all deferred follow-ups in the code-review triage. See
`docs/code-reviews/2026-05-29-ner-134-phase-3.md`.

## Related

- Plan: `docs/plans/completed/2026-05-29-008-feat-phase-3-write-binding-verification-plan.md`
- Code-review triage: `docs/code-reviews/2026-05-29-ner-134-phase-3.md` (the restore second-vector and the convergent restore-binding finding were doc-review / code-review findings, not pre-merge test failures)
- Requirements: `docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md` (the wedge: single-checkout proposal-level competition, NOT physical worktrees)
- Substrate this builds on: `docs/solutions/architecture-patterns/schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md` (the additive-error + drift-guard pattern §6 extends; the "verify the production caller graph, not a green test on a plausible function" lesson §2/§3 applies), `docs/solutions/architecture-patterns/crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md` (advisory-lock acquire-once — the new checks are pure reads inside already-locked commands), `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`
- Implementation: `crates/forge-store/src/lib.rs` (`verify_worktree_binding`, `verify_save_target`, `snapshot_owner_attempt_id`, `save_snapshot`), `crates/forge-store/src/error.rs` (`AttemptWorktreeMismatch`), `crates/forge-content/src/lib.rs` (`ContentBackend::current_base`/`base_content_ref` + S1/S2 docs), `crates/forge-content-git/src/lib.rs`, `crates/forge-content-native/src/lib.rs` (Phase-7-marked delegation), `crates/forge-cli/src/main.rs` (`save_response`, `restore_response`, backend routing)
- Eval: `scripts/e2e-eval.sh`
