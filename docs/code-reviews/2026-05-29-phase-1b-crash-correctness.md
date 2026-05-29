# Code review — Phase 1b crash-correctness (NER-132)

- **Date:** 2026-05-29
- **Branch:** `ner-132-phase-1b-crash-correctness`
- **base-sha:** `c3e2ce7` (`docs: add M1 Phase 1b + Phase 2 kickoff handoff (#8)`)
- **head-sha:** `04f4fcc` (review fixes applied on the branch)
- **Plan:** `docs/plans/2026-05-29-006-fix-phase-1b-crash-correctness-plan.md` (U1–U8)
- **Tracking:** Linear **NER-132**

Scope reviewed: the full Phase 1b branch diff (~1100 lines code+tests across `forge-store`, `forge-content`, `forge-content-native`, `forge-content-git`, `forge-cli`) — advisory `.forge` write lock (std `try_lock`, typed `LockTimeout`), lock wiring at the `command_result` boundary, in-txn `check` verdict, crash-atomic worktree restore, store-before-DB contract, race-safe concurrent init, extended `doctor`, and the crash-injection harness.

Review team (10): correctness, adversarial, security, reliability, testing, maintainability, project-standards, api-contract, ce-learnings-researcher, ce-agent-native. (No Rust-specific stack persona exists in the catalog.) Verify trio green at both review and post-fix: `fmt` clean / 85 tests / `clippy -D warnings` clean.

## Requirements completeness (plan, explicit)

All 12 plan requirements (R1–R12) ship with code + tests:
- **R1** store-before-DB ordering — documented on the `ContentBackend` trait; proven by U6's `after_db_commit` crash boundary.
- **R2** crash-atomic restore — `materialize_tree` temp+rename+fsync (U4) + the ancestor-dir fsync added in review fix C.
- **R3/R4** advisory lock + cross-read closure — `repo_lock` + `command_result` wiring; `check` verdict closed **in-txn** (not by the lock, since `run` is lock-free), `accept` STALE_BASE under the lock.
- **R5** `run` lock-free — `requires_repo_lock` excludes `run`; tested in `forge_repo_lock.rs`.
- **R6** race-safe init — lock + `INSERT OR IGNORE`; 8-process concurrent-init test.
- **R7** crash-injection on Linux+macOS — `forge_crash_injection.rs`, 3 boundaries.
- **R8** doctor dangling-refs + half-applied worktrees — `DoctorReport` fields + worktree scan.
- **R9** lock model documented + golden-tested.
- **R10** in-txn `replay_guard` coverage — `forge-store/tests/replay_guard.rs`.
- **R11** Phase 1a invariants preserved — confirmed by ce-learnings-researcher (zero regressions across WAL/PRAGMA/IMMEDIATE/517/RequestIdReplay/UUIDv7/rowid).
- **R12** lock-file + sidecar exclusion — blanket `.forge/` prefix, regression-tested.

## Real-actionable — fixed on this branch (head `04f4fcc`)

1. **`.forge-restore-*` temps were not excluded from snapshots/exports** (security, P1, anchor 90). Restore temps live in *worktree* directories, not under `.forge`, so the blanket `.forge/` prefix never matched them — an orphan from a crash-interrupted restore could be captured by a later `save`. Fixed: shared `RESTORE_TEMP_PREFIX` + `is_restore_temp_path` predicate in `forge-content`, excluded in **both** backends' `is_ignored_by_policy`, with symmetric regression tests. (`snapshot_candidate_paths` and `materialize_tree` both filter through `is_ignored_by_policy`, so this is the load-bearing gate.)
2. **`doctor` scan descended into nested `.git`/submodule dirs** (adversarial P2/anchor 100; corroborated by reliability + correctness + maintainability). `scan_restore_temps` skipped `.git`/`.forge` only at the worktree root (`dir == root` qualifier), so it walked unbounded submodule object trees and could misclassify internals. Fixed: skip `.git`/`.forge` at any depth.
3. **Restore did not fsync newly-created intermediate worktree dirs** (correctness, P2/anchor 75). The crash-atomic restore fsynced only the immediate parent, not newly-created ancestors — `write_object` (the pattern the plan said to mirror) does both. Fixed: capture `missing_dirs` before `create_dir_all` and fsync each newly-created ancestor's parent, deduped across the restore via `synced_dirs`.

## Defer-able — filed on NER-132 (not blocking)

1. **`LOCK_TIMEOUT` ships `retry.retryable=false`** (api-contract P2/anchor 100; ce-agent-native "critical"). The envelope's `retry` field exists but is uniformly `false` for *every* error today — the structured retryable taxonomy (and `errors[].details`, `warnings[]`) is **NER-133 (Phase 2)** scope per the plan's Scope Boundaries. Setting only `LOCK_TIMEOUT` to `true` now would pre-empt and fragment that taxonomy. → **NER-133.** (The error *code* `LOCK_TIMEOUT` is surfaced and documented retryable; only the structured boolean is deferred.)
2. **No concurrent test proving the in-txn `check` verdict** (testing P2/anchor 75). The verdict is correct by construction (computed from the same in-txn evidence row it binds as `evidence_id`); the sequential `forge_propose_check.rs` tests pass under both the old and new paths, so they don't *prove* the in-txn semantics. A deterministic interleaved test (commit rival evidence between a held txn's read and write) needs a test hook. → **NER-132 testing follow-up.**
3. **Minor robustness/test items** (P3): `scan_restore_temps` swallows `read_dir` errors (`Err(_) => continue`) — could hide a permission issue; `acquire_repo_lock`'s `Ok(None)` is a tri-state the call site reads via doc, not type; `configured_timeout` env-parse/clamp path is only covered via the pre-clamped unit path; `crash_after_db_commit` proves object hash-validity but not an end-to-end content round-trip; a `doctor` run concurrent with an in-flight restore can momentarily false-positive a half-applied worktree (lock-free read). → **NER-132 follow-up.**

## Reviewed-and-rejected (do not re-flag)

- **`LockTimeout` / `maybe_crash` as "custom error type / non-test public API"** — `LockTimeout` is the same accepted single-sentinel pattern as `RequestIdReplay` (Phase 1a code review accepted it deliberately); `maybe_crash` is debug-gated scaffolding folded into NER-133's typed errors. Not a "no custom error types" violation. (project-standards: clean.)
- **init error-code narrowing** (api-contract P2/anchor 75) — `init_response` no longer masks every error as `NOT_A_GIT_REPOSITORY`; it now returns `LOCK_TIMEOUT` on contention and falls through `error_code` (still `NOT_A_GIT_REPOSITORY` for the genuine case). This is an intentional **improvement** — the old catch-all was a latent bug. Accepted.
- **`RESTORE_TEMP_PREFIX` cross-crate reference / backoff-jitter duplication** (maintainability, anchor 50) — the const now lives in shared `forge-content`; the lock backoff is deadline-aware (a real behavioral difference from `sleep_backoff`). Acceptable.

## Verdict

**Ready to merge.** Correctness and security reviewers found one real issue each, both fixed; the adversarial reviewer refuted 5 of 7 attack vectors (lock re-entrancy, run-starvation, check TOCTOU, accept STALE_BASE, shard-dir race, abort/Drop, concurrent-init) and the 2 it confirmed are fixed. The deferred `retry.retryable` item is a deliberate NER-133 scope boundary, not a regression. Verify trio green; zero Phase 1a invariant regressions.
