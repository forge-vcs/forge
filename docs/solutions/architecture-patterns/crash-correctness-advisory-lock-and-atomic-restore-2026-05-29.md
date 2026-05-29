---
title: Make a crash-correct local store — explicit advisory lock, atomic file replace, honest crash injection
date: 2026-05-29
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: durability
severity: high
applies_when:
  - A local store must guarantee "if a command returns Ok, its effects survive a crash"
  - Concurrent short-lived processes need an explicit serialization point, not an accidental one
  - A CLI-layer determining read (exit code, git HEAD) feeds a write but lives on a separate connection
  - Files are replaced in place and a crash mid-write must not leave a torn file
tags: [advisory-lock, flock, file-locking, std-try-lock, fsync, atomic-rename, temp-rename, crash-injection, toctou, durability, sqlite, lock-timeout, idempotent-init]
---

# Make a crash-correct local store — explicit advisory lock, atomic file replace, honest crash injection

## Context

Phase 1a (`docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`) made Forge's `.forge/forge.db` safe for many concurrent processes (WAL + `busy_timeout` + `BEGIN IMMEDIATE` + in-txn replay guard) but named three boundaries it left open: cross-read atomicity for determining reads done at the CLI layer, an explicit serialization point (rather than an accidental property of SQLite locking), and crash-atomic worktree restore. Phase 1b (NER-132, PR #9) closed them. This captures the non-obvious learnings so the next durability layer does not re-derive them.

## Guidance

### 1. Make the serialization point explicit with a repo-level advisory file lock — and use the standard library

`BEGIN IMMEDIATE` serializes *commits within one connection's transaction*. It does **not** make atomic a determining read a command performs on a *separate connection at the CLI layer* (reading an exit code to compute a verdict, a git `HEAD` for a stale-base check), nor does it serialize cross-process directory creation in an object store. A repo-level advisory lock on a **separate** lock file (`.forge/forge.lock`) is the construct that does, and it makes the serialization point observable instead of incidental.

Use the standard library: `std::fs::File::try_lock` / `unlock` stabilized in **Rust 1.89** (`flock` on Unix, `LockFileEx` on Windows). On a toolchain ≥ 1.89 this is **zero new dependencies** — no `fs2`/`fd-lock`, which matters under a supply-chain-age gate. Acquire in a bounded, jittered `try_lock` backoff loop and return a typed, retryable `LockTimeout` on deadline (mirror the existing sentinel pattern; carry it in `anyhow::Error`, recover by `downcast_ref`, map to a `LOCK_TIMEOUT` error code).

```rust
// acquire once at the command boundary; the guard releases on Drop / OS reclaim
let file = OpenOptions::new().read(true).write(true).create(true).open(&lock_path)?;
loop {
    match file.try_lock() {
        Ok(()) => return Ok(RepoLock { file }),
        Err(TryLockError::WouldBlock) => { if Instant::now() >= deadline { return Err(LockTimeout{..}.into()); } backoff(); }
        Err(TryLockError::Error(e)) => return Err(e.into()),
    }
}
```

Two hard caveats:
- **Acquire exactly once per command — never nested.** The std API leaves re-locking the *same* file handle (or a clone) "unspecified, including the possibility that it will deadlock." Acquire at the single command funnel; do not let a store function called inside the locked section re-acquire. (Independent `open()` handles across processes contend correctly — that is the cross-process case you want.)
- **Carve out long-running child-exec commands (PRD §10.6).** A command that runs a user child (`forge run`) must **not** hold the global lock while the child runs. Exclude it from the lock predicate; its single DB write is already serialized by `IMMEDIATE`/WAL. Also clamp any `*_TIMEOUT_MS` env override to a floor so a `0` value cannot silently turn the lock into try-once-fail and re-open the races it exists to close.

### 2. A lock only helps when *both* racers take it — otherwise close the TOCTOU in-transaction

The most non-obvious finding: the `check`-verdict TOCTOU could **not** be closed by the advisory lock. The racing writer (`forge run`, recording evidence) is deliberately lock-free (§1 carve-out), so a lock held by `check` cannot serialize against it. The fix was to move the determining read **and** the decision into the same `IMMEDIATE` transaction — compute the pass/fail verdict from the very evidence row `record_check` already binds as `evidence_id`, instead of from a prior CLI-layer `show()` read.

**Lesson:** match the mechanism to where the race actually is. A lock serializes only processes that take it; if one party is intentionally lock-free, push the read-then-decide into its transaction. Reserve the lock for races where both sides are mutating commands that hold it (here: two `accept`s, and cross-process object-dir creation in `save`/`propose`).

### 3. Crash-atomic file replace = temp + fsync + atomic rename + parent-dir fsync — including newly-created ancestors

Replace in-place `fs::write` on the restore path with the object-write durability pattern: write a temp in the **destination's own parent directory** (guarantees a same-filesystem rename even when `.forge` is a separate mount), `set_mode` on the temp, `sync_all()` it, `persist`/rename into place, then `sync_dir(parent)`. Mirror the object store fully: capture `missing_dirs(parent)` before `create_dir_all` and `fsync` each newly-created ancestor's parent too (a fresh dir's entry is not durable until the dir it lives in is fsynced). Dedup the dir-fsyncs across a multi-file operation so each dir is synced at most once.

Two consequences specific to worktree-resident temps:
- **They escape a `.forge`-prefix exclusion.** A restore temp lives in a worktree directory (e.g. `src/.forge-restore-XXXX`), not under `.forge`, so the blanket exclusion does not cover it. Give it a distinctive prefix and exclude that prefix in **every** content backend's ignore policy, or a crash-orphaned temp gets captured into a later snapshot/export.
- **`doctor` is the reclamation net.** Scan the worktree for the temp prefix (skipping `.git`/`.forge` at **any** depth — a submodule's nested `.git` is an unbounded object tree) and report a half-applied worktree. This is the only way to find an orphan, because (see §4) the temp's `Drop`-based auto-delete does not run on a hard kill.

### 4. Crash injection via `abort()` proves crash-*consistency*, not power loss — and it skips `Drop`

Inject crashes with a `cfg!(debug_assertions)`-gated env hook that calls `std::process::abort()` at instrumented durability boundaries (between object-fsync and DB-commit; mid-restore; after commit). Gating on `cfg!(debug_assertions)` makes the whole check dead code in release — zero overhead, no abuse surface — while `cargo test` (a debug build) exercises it.

Be honest about what it proves and design around what it skips:
- **`abort()` runs no destructors and flushes nothing** — it models SIGKILL / sandbox teardown / OOM, the failure agents actually hit. So **lock release must not depend on `Drop`** (rely on the OS reclaiming `flock` on process death — a crashed holder never wedges a peer) and **temp cleanup must not depend on `Drop`** (rely on the `doctor` scan from §3).
- It proves **crash-consistency of the ordering given the OS's fsync guarantees**, not block-device power-loss fault injection. State that boundary explicitly in the plan and the harness comments rather than implying durability you did not test. Assert the safe states: a crash before commit leaves *object-present / ref-absent* (never the inverse); a crash after commit leaves *ref-present and object durable* after WAL recovery on reopen.

### 5. Race-safe first-init = serialize under the lock + idempotent version-row inserts

Two concurrent first-inits of the same repo both pass a pre-txn short-circuit and one hits the `root_path` UNIQUE constraint (surfacing as raw SQLite text under the wrong error code). Fix: acquire the repo lock **inside** the init path (after creating the `.forge` dir that holds the lock file, and outside the CLI command funnel so it never double-locks), so the loser observes the winner's committed row and returns `already_initialized`. Make the `schema_migrations` version-row inserts `INSERT OR IGNORE` as a constraint-safe shim for any residual race. Also stop masking every init error as one code — map the lock timeout to its own retryable code and let the genuine not-a-git-repo case fall through.

## Why This Matters

Each closes a trust hole that only appears under the failure mode agents live in (killed mid-operation): without the explicit lock, a `check` verdict can attribute to the wrong evidence and a stale-base `accept` is a CLI-layer read-then-act; without atomic restore, a crash leaves a torn file; without honest crash injection, you ship a durability claim you never tested. The fixes are mostly small, but the *reasons* — why a lock can't close a race against a lock-free writer, why `abort()` invalidates `Drop`-based cleanup, why a worktree temp escapes a `.forge` exclusion — are exactly what is non-obvious at 2am.

## When to Apply

- Any local store that must survive a hard kill with "Ok implies durable."
- Any CLI/daemon where a determining read on one connection feeds a write — decide per-race whether a coarse lock or an in-transaction read is the right close (§2).
- Any in-place file replacement that must be crash-atomic (config writers, caches, materializers).
- Any crash/fault test harness — be explicit about consistency-vs-power-loss and about destructors not running.

## Scope boundaries (deferred)

The typed `ForgeError` taxonomy, a *structured* `retry.retryable` envelope field (today every error ships `retryable: false`; classifying them together is the taxonomy's job — setting one in isolation pre-empts it), populated `errors[].details`, and the numbered `.sql` migration runner are **NER-133 (Phase 2)**. The `INSERT OR IGNORE` init shim (§5) is flagged in-code for that runner to absorb. `AUTOINCREMENT`/rowid-reuse hardening before real `gc` is Phase 8.

## Related

- Phase 1a substrate: `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`
- Plan: `docs/plans/completed/2026-05-29-006-fix-phase-1b-crash-correctness-plan.md` (U1–U8)
- Code-review triage: `docs/code-reviews/2026-05-29-phase-1b-crash-correctness.md`
- Implementation: `crates/forge-store/src/repo_lock.rs`, `crates/forge-store/src/lib.rs` (`acquire_repo_lock`, `record_check` in-txn verdict, init lock, `doctor`/`scan_restore_temps`), `crates/forge-cli/src/main.rs` (`requires_repo_lock`, `command_result` lock guard, `forge_content::maybe_crash`), `crates/forge-content-native/src/lib.rs` (`materialize_tree`), `crates/forge-content/src/lib.rs` (`RESTORE_TEMP_PREFIX`, `is_restore_temp_path`, `maybe_crash`)
- Tests: `crates/forge-cli/tests/forge_crash_injection.rs`, `forge_repo_lock.rs`, `crates/forge-store/tests/replay_guard.rs`
- External: Rust std `File` locking (stabilized 1.89), PRD §10.6 (Locking and Concurrency)
