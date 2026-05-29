---
title: "fix: Phase 1b crash-correctness — store-before-DB ordering, atomic restore, repo write lock"
type: fix
status: active
date: 2026-05-29
origin: docs/ROADMAP.md (Phase 1b) · Linear NER-132
---

# fix: Phase 1b crash-correctness — store-before-DB ordering, atomic restore, repo write lock

## Summary

Lock the full durability invariant for Forge — *if a command returns `Ok`, its effects survive a crash* — and make the serialization point explicit. We add a repo-level advisory write lock on `.forge` (Rust std `File::try_lock`, zero new dependencies) with a typed retryable `LockTimeout` error, hold it across the residual CLI-layer cross-reads that `BEGIN IMMEDIATE` alone cannot make atomic (`check` verdict, `accept` `STALE_BASE`), make worktree restore crash-atomic via per-file temp+rename+fsync, formalize the store-before-DB object-durability ordering, and prove all of it with a `cfg!(debug_assertions)`-gated crash-injection harness plus extended `doctor` checks. Phase 1a's WAL/IMMEDIATE/replay/UUIDv7 invariants are preserved unchanged.

---

## Problem Frame

Phase 1a (NER-131) made `.forge/forge.db` safe for many concurrent processes (WAL + `busy_timeout` + `synchronous=NORMAL` + `BEGIN IMMEDIATE` on every writer + in-txn `replay_guard` + UUIDv7 ids). But it deliberately stopped at three boundaries, named explicitly in the Phase 1a solution doc's *Scope boundaries* section:

1. **Cross-read atomicity.** `IMMEDIATE` serializes *commits within one connection*. It does **not** make atomic a determining read done on a *separate connection at the CLI layer* — e.g. `check` reading `latest_evidence.exit_code` to compute a pass/fail verdict, or `accept` reading git `current_head` for a stale-base check — then writing based on it. A concurrent `forge run` between the read and the write can attribute a verdict to the wrong evidence row.
2. **Durability ordering is incidental, not enforced.** Objects are fsynced inside `write_object` before `save_snapshot` commits the referencing `content_ref`, but only by call ordering — there is no documented, tested barrier coupling the object-store fsync to the SQLite commit, and a cross-process shard-dir creation race (`missing_dirs` inferring ancestor durability from `exists()`) can leave a committed `content_ref` pointing at an object whose directory entry a crashed peer never fsynced.
3. **Restore is not crash-atomic.** `materialize_tree` writes each restored file in place with `fs::write` (no temp+rename, no fsync), so a crash mid-restore can leave a torn file.

Agents are routinely killed mid-operation (timeouts, sandbox teardown). For a change-control tool, trust *is* the product: an agent must never get `Ok` and then lose the object on power loss, and a second concurrent writer should queue on an explicit, retryable lock rather than hard-fail on an accidental property of SQLite locking. This plan closes all three boundaries and the deferred concurrency findings the NER-132 ticket owns. See origin: `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` (§ Scope boundaries), `docs/code-reviews/2026-05-29-phase-1a-pr-b-concurrency.md`.

---

## Requirements

- **R1.** A committed `content_ref` provably implies a durably-retained object: the object file *and* its directory entry are durable before the SQLite txn that commits the referencing row. The ordering is documented and enforced, not incidental. (ticket exit criteria; ROADMAP §1b)
- **R2.** Worktree restore is crash-atomic per file: each restored file is written via temp+rename+fsync so it is either fully old or fully new — never torn — and no orphaned temp artifact survives a crash. (ticket; `materialize_tree`)
- **R3.** A repo-level advisory **write** lock on `.forge` makes the serialization point explicit, with a **typed, retryable `LOCK_TIMEOUT`** contention error (not substring-derived). (PRD §10.6; ticket)
- **R4.** The residual CLI-layer cross-reads are closed by the right mechanism for each: `accept`'s `STALE_BASE` decision runs under the advisory lock (serializing concurrent Forge `accept`s), and the `check` verdict is made atomic by computing pass/fail **inside** `record_check`'s IMMEDIATE txn from the same in-txn evidence row it already re-reads — **not** by the lock, because `run` (which commits the racing evidence) is deliberately lock-free (R5) and the lock therefore cannot serialize against it. (deferred findings 1a-PRB #2, doc-review; solution-doc scope boundary 1)
- **R5.** Long-running evidence commands do **not** hold the global lock while the child process runs (`forge run`). (PRD §10.6)
- **R6.** Concurrent `forge init` of the same repo is race-safe: exactly one initializes; the rest return `already_initialized: true`; no raw SQLite constraint text or misleading `NOT_A_GIT_REPOSITORY` surfaces. (deferred finding 1a-PRB #1)
- **R7.** Crash-injection passes on **Linux and macOS** at every durability boundary: between object-fsync and DB-commit, mid-restore, and between DB-commit and WAL checkpoint. (ticket exit criteria)
- **R8.** `doctor` reports zero dangling `content_refs` and zero half-applied worktrees. (ticket)
- **R9.** The locking model and its contention error are **documented and golden-tested**. (ticket exit criteria)
- **R10.** *(test-coverage requirement — exercises existing Phase 1a production code; no new production code)* The in-txn `replay_guard` branch and concurrent `status == "failed"` replay are deterministically asserted at the `forge-store` level (separate connections). (deferred finding 1a-PRB #4)
- **R11.** All Phase 1a SQLite invariants are preserved unchanged: WAL persistent vs per-connection PRAGMAs, `BEGIN IMMEDIATE` on every writer, 517/busy retry classification, the `RequestIdReplay` sentinel, UUIDv7 ids, `rowid DESC` tiebreaks. (carry-over; solution doc)
- **R12.** The advisory lock file (`.forge/forge.lock`) and the WAL sidecars (`forge.db-wal` / `-shm`) are excluded from snapshots and exports in **both** backends — already guaranteed by the existing blanket `.forge/` prefix in each `is_ignored_by_policy`; this requirement is locked in by a regression assertion, **not** a new exclusion rule. (CLAUDE.md § Security defaults; doc-review finding)

**Origin actors:** competing local agents (fanned-out processes) + the solo developer driving the CLI.
**Origin flows:** `save` (object write → content_ref commit), `run` (child exec → evidence write), `check` (verdict), `accept` (stale-base gate → decide), `restore`/`attempt attach` (materialize), `init`, `doctor`.

---

## Scope Boundaries

- **No finer-grained concurrency.** A single coarse repo-level write lock (PRD §10.6 "conservative repository-level write locking"), not per-table or optimistic concurrency. Reads do not block.
- **No typed-error *taxonomy*.** `LockTimeout` is a single typed sentinel struct mirroring the existing `RequestIdReplay`, surfaced via the existing `error_code` ladder as `"LOCK_TIMEOUT"`. The full typed `ForgeError` enum, populated `errors[].details`, and a structured `retry.retryable` envelope field are **NER-133 (Phase 2)** — coordinate, do not pre-build. The lock is designed so Phase 2 can fold `LockTimeout` into the enum cleanly.
- **No media-level power-loss simulation.** The crash-injection harness simulates a hard process kill (`std::process::abort()`) at instrumented durability boundaries — it proves *crash-consistency of the ordering* given the OS's fsync guarantees (and the `sync_all`/`F_FULLFSYNC` Phase 1a already lands), not block-device fault injection. Stated explicitly per the solution doc's "name the boundaries you didn't deliver" discipline.
- **No whole-restore transactionality.** Restore is crash-atomic *per file* (temp+rename) and leaves no orphaned temps; a multi-file restore interrupted mid-way is not rolled back as a unit. We do not claim restore-level atomicity — only per-file atomicity + no torn writes + `doctor`-detectable cleanliness.
- **No external-git serialization.** The advisory lock serializes Forge writers; it cannot prevent an external `git` process from moving `HEAD` between `accept`'s `current_head` read and its `decide` write. That residual window is inherent and documented; closing it would require something Forge does not own.
- **No checkpoint-before-copy helper yet.** No DB-copy/backup path exists in v0 (exports materialize worktree content, never the `.db` file), so a `PRAGMA wal_checkpoint(TRUNCATE)`-before-copy helper is not built here; the crash harness still accounts for WAL recovery on reopen. → Deferred.

### Deferred to Follow-Up Work

- **Typed `ForgeError` enum + `retry.retryable` + `errors[].details`** (replacing the `error_code` substring ladder): **NER-133 / Phase 2**. `LockTimeout` and `RequestIdReplay` are its first two members.
- **`AUTOINCREMENT` / explicit sequence column on the nine `rowid DESC`-tiebreak tables** before real `gc` lands (WAL commit-order divergence + rowid reuse after delete): **Phase 8 / NER-139**. Noted in NER-132 comment; not triggered until `gc` deletes rows.
- **`checkpoint-before-copy` helper** (`PRAGMA wal_checkpoint(TRUNCATE)` + close): introduce when a DB backup/copy path is first added.
- **Fold `forge-evidence`'s private `now_ms` onto `forge_core::now_ms`**: opportunistic cleanup; do only if trivially adjacent during implementation, else leave to a follow-up.

---

## Context & Research

### Relevant Code and Patterns

- **`crates/forge-content-native/src/lib.rs`**
  - `write_object` — the durability template to mirror: `missing_dirs(parent)` → `create_dir_all` → `NamedTempFile::new_in(tmp_dir)` → `write_all` → `temp.as_file_mut().sync_all()` → `temp.persist(path)` → `sync_dir(parent)` → per-newly-created-ancestor `sync_dir(grandparent)`. Errors propagate (no `let _ =`).
  - `materialize_tree` — the `TreeEntryKind::File` arm uses in-place `fs::write(&full, bytes)` + `set_file_mode` (the crash hole, R2).
  - `sync_dir`, `missing_dirs`, `tmp_dir` (`.forge/tmp`), `is_ignored_by_policy` (local copy).
  - `restore_snapshot` (trait impl) is verify-then-mutate: `verify_content_ref` (full reachability + hash) before `materialize_tree`.
- **`crates/forge-content/src/lib.rs`** — `trait ContentBackend { snapshot_worktree; restore_snapshot }`; `is_secret_risk_path`; `redact_secret_like_text`. No durability/lock surface on the trait today.
- **`crates/forge-content-git/src/lib.rs`** — git backend defers durability to git; its own local `is_ignored_by_policy` (must also exclude the lock file, R12); `current_head` (used by `accept`).
- **`crates/forge-store/src/lib.rs`**
  - `open_connection` (per-open PRAGMAs; **do not regress**, R11), `with_immediate_retry` / `run_immediate_once`, `is_retryable_busy`, `sleep_backoff` (jitter from `process::id()` ⊕ nanos — reuse for lock backoff), `replay_guard`, `RequestIdReplay` (the typed-sentinel precedent for `LockTimeout`).
  - `open_repository` → `RepositoryContext` (the single funnel for non-`init` commands; no long-lived guard today).
  - Writers committing `content_ref`: `save_snapshot`, `propose`. Other writers: `record_check`, `decide`, `record_evidence`, `record_restore`.
  - `init_repository` + `read_init_repository` (the short-circuit that races, R6); `apply_migrations` (version-row insert that is non-idempotent, R6).
  - `doctor` + `DoctorReport { ok, issues, schema_version, dangling_temp_files }` — already verifies each `content_ref` and checks `.forge/tmp` dangling temps; extend for R8.
- **`crates/forge-cli/src/main.rs`**
  - `command_result` — the mutating-command funnel (pre-flight replay check → closure `f` → `RequestIdReplay` downcast → failed-op record). The lock wraps here (R3/R4).
  - `check_response` (verdict from `show()` then `record_check`), `decision_response` (the `accept` `current_head` read + `decide`), `run_response` (`capture_with_timeout` **inside** the closure — the §10.6 carve-out, R5), `restore_response`, `init_response`, `doctor_response`.
  - `error_code(command, message)` substring ladder (`"stale base"→STALE_BASE`, etc.) — add `LockTimeout` mapping (via downcast, structurally) → `"LOCK_TIMEOUT"`; `is_mutating_command`.
- **`crates/forge-cli/tests/`** — `common/mod.rs` (`TestRepo::new_git`, `forge()`); `forge_concurrency.rs` (real `std::process::Command` children from threads; `run_forge`, `assert_no_busy`, `BUSY_MARKERS`, `open_db`, `assert_no_id_collisions`, `no_swallowed_sync_remains_on_durability_paths` static scan); `forge_doctor_gc.rs` (`native_restore_verifies_reachable_objects_before_mutating_worktree`, `object_path_containing`).

### Institutional Learnings

- `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`:
  - **§6 (durability)** — propagate (never `let _ =`) the file *and* parent-dir `sync_all()`; fsync newly-created ancestors on first creation; `File::sync_all()` gives `F_FULLFSYNC` on macOS for the file, but there is no portable directory `F_FULLFSYNC` (`File::open(dir)?.sync_all()` is the best the OS exposes). The store-before-DB barrier *is* this fsync completing before the IMMEDIATE commit.
  - **§1 (PRAGMA split)** — per-open `busy_timeout` + `synchronous=NORMAL`; `synchronous=NORMAL` failure envelope = "only the last commit can be lost on power loss, never the database." The crash harness must expect exactly that envelope.
  - **§7 (test concurrency with real processes)** — spawn real OS children; assert the user-visible contract; back any new retry predicate with a direct unit test ("the integration test can't distinguish busy-never-fired from busy-retried"); derive backoff jitter from `process::id()` ⊕ nanos.
  - **§ Scope boundaries** — names this exact ticket: the advisory lock is what retroactively makes the `check`-exit-code and `accept`-`HEAD` cross-reads atomic; the transient-CAS reclassification is **not** ours (NER-133).
- `docs/plans/completed/2026-05-28-003-feat-native-forge-content-store-plan.md` — R4 already framed store-before-DB ("temp path, flush, sync, atomic rename … best-effort parent-directory sync **before SQLite metadata points at the object**"); cautioned "do not claim database-grade durability until crash simulation exists on target platforms" — this plan's harness is that simulation.

### External References

- Rust std `File` advisory locking — `lock` / `lock_shared` / `try_lock` / `try_lock_shared` / `unlock`, **stabilized 1.89.0** (toolchain pinned **1.92.0** → available, **no new dependency**). `try_lock` returns `Result<(), TryLockError>` with `WouldBlock` / `Error(io::Error)` variants. Unix `flock`, Windows `LockFileEx`. **Caveats that shape the design:** (a) re-locking the *same* file handle in the same process is unspecified and may deadlock → acquire **exactly once** per command at the boundary, never nested; (b) on Windows the lock file must be opened with read+write (not append-only); (c) locks release on `unlock()` or on file close (a `Drop` guard suffices). Advisory-vs-mandatory is platform-dependent — fine here because every writer is a cooperating Forge process.

---

## Key Technical Decisions

- **Use std `File::try_lock`, not a crate.** Toolchain 1.92 ≥ 1.89, so `fs2`/`fd-lock` add zero value and a supply-chain surface the repo's `min-release-age` posture actively avoids. The lock is a `.forge/forge.lock` file opened read+write; acquisition is a `try_lock` backoff loop (reusing the Phase 1a `sleep_backoff` jitter) until a deadline.
- **Typed `LockTimeout` sentinel mirroring `RequestIdReplay`.** A small `struct LockTimeout` in `forge-store` implementing `std::error::Error` + `Display`, carried inside `anyhow::Error` and recovered by `downcast_ref` at the CLI — the exact precedent the Phase 1a code review accepted as deliberate. Surfaced as `"LOCK_TIMEOUT"` via `error_code` by **downcast** (structural), not substring. Default timeout 10s, overridable via `FORGE_LOCK_TIMEOUT_MS` (env, like the existing busy posture), **clamped to a minimum floor** (≥ ~50 ms / at least one real retry) so a `0`/`1` value cannot silently disable serialization and re-open the cross-read races. Marked retryable in documentation; the structured `retry.retryable` envelope field is NER-133.
- **Lock at the `command_result` boundary, once, for mutating commands except `run`.** The guard is acquired before the closure `f` runs (so `accept`'s `current_head`→`decide` read-then-write inside `f` is atomic, and `save`/`propose` object writes are serialized) and dropped after. `run` is the PRD §10.6 carve-out: it executes its child *inside* `f`, so it stays **lock-free** — its single `record_evidence` write is already serialized at the DB level by `IMMEDIATE`/WAL. Read-only commands (`show`, `proposal list`, `doctor`) never lock (reads don't block). This is a single coarse lock; nesting is forbidden by the std re-entrancy caveat, so no store-level function acquires it independently.
- **`check` verdict computed in-txn, not under the lock.** Because `run` is lock-free (R5), a lock held by `check` cannot exclude a concurrent `run` committing new evidence between `check`'s read and write — so the lock is the *wrong* tool here. Instead, move the pass/fail derivation into `record_check`'s IMMEDIATE txn: it already re-reads `latest_evidence_on(tx)` for the staleness verdict, so compute the verdict from that same in-txn evidence's exit code and drop the CLI-layer `show()`-based verdict entirely. The verdict is then atomic with the evidence row it names, independent of the lock. (This is the handoff's offered alternative: "pass `exit_code` into `record_check` and evaluate in-txn.")
- **`accept`: the existing `current_head` read becomes the under-lock CAS — no new read added.** `decision_response` already reads `current_head` immediately before `decide` inside the closure; wrapping the closure in the repo lock makes that read-then-`decide` atomic against concurrent Forge `accept`s. The HEAD-vs-`base_head` comparison guards against *external git* moving HEAD — an inherent, uncloseable window (Scope Boundaries); the lock does not (and cannot) close that.
- **Adapt (don't verbatim-copy) `write_object`'s temp+rename+fsync pattern in `materialize_tree`'s file arm.** One intentional difference from `write_object`: write the temp in the *destination file's parent directory* — **not** `.forge/tmp` — to guarantee a same-filesystem atomic rename even when `.forge` is a separate mount (sandboxed-agent bind-mounts), and give it a Forge-owned name prefix (`tempfile::Builder::prefix(".forge-restore-")`) so a crash-orphaned temp is discoverable by `doctor` (U7) and never collides with a user file. `set_file_mode` on the temp before `persist`, then `sync_dir(parent)`. To bound per-ancestor fsync cost (ticket risk note), track parent dirs already fsynced within a single `materialize_tree` invocation and fsync each once.
- **Shard-dir durability race closed by the lock.** Because every object-writing command (`save`, `propose`) now holds the repo write lock across its critical section, only one process creates shard/ancestor dirs at a time and completes its fsyncs before releasing — a peer that later sees `exists()` can trust durability. The lock-independent alternative (unconditional full-ancestor-chain fsync per object) is documented but not taken, to avoid per-object latency.
- **Crash injection gated on `cfg!(debug_assertions)` + `FORGE_CRASH_POINT` env.** Instrumented points `abort()` only in debug builds (which is what `cargo test --workspace` builds, so the standard verify trio exercises them) and only when the env names a known point; release builds optimize the branch to dead code → zero production overhead. No cargo feature flag (a non-default feature would not be exercised by the plain `cargo test --workspace` trio).
- **Extend `doctor`, don't add a command.** "Zero dangling `content_refs`" formalizes the existing per-`content_ref` verification into a distinct `DoctorReport` category. "Zero half-applied worktrees" is detected by scanning the **worktree** (the restore target tree) for the Forge-owned restore-temp prefix (`.forge-restore-*`) — *not* the `.forge/tmp` scan, since U4's restore temps live in worktree dirs — alongside the existing `.forge/tmp` check (which still covers object-write temps).

---

## Open Questions

### Resolved During Planning

- **Lock crate vs std?** → std (1.92 ≥ 1.89). No dependency.
- **Lock granularity / placement?** → single coarse lock at `command_result`, mutating-except-`run`; reads lock-free (PRD §10.6).
- **`check`/`accept` fix: lock vs push-read-in-txn?** → the lock (generalizes to both cross-reads and the shard-dir race and satisfies the ticket's explicit "advisory lock" deliverable). `accept` additionally re-reads `HEAD` under the lock.
- **Crash-injection mechanism?** → `abort()` at `cfg!(debug_assertions)`-gated env-named points; honest scope boundary on what it proves.
- **`LockTimeout`: typed sentinel vs `bail!` string?** → typed sentinel (solution doc: "detected structurally, not by substring"; sets up NER-133).

### Deferred to Implementation

- **Exact lock-acquire/release call site inside `command_result`** relative to the pre-flight replay check (acquire-then-preflight vs preflight-then-acquire) — resolve when wiring; the determining reads must be inside the lock, the pre-flight read need not be.
- **Whether `restore`/`attempt attach` need the lock around the worktree mutation or only around the DB write** — both are mutating; default to holding across the critical section, confirm no §10.6-style long operation is inside.
- **Precise `FORGE_CRASH_POINT` names and their exact placement** in the save/restore/commit flows — fix during harness implementation.
- **Whether a `wal_checkpoint(TRUNCATE)` on reopen is needed in the harness** to make the post-crash assertion deterministic, or whether default WAL recovery suffices.
- **`forge-policy` ↔ `forge-store` dependency direction for the in-txn `check` verdict (U2):** confirm `record_check` can call `forge_policy::evaluate` without introducing a dependency cycle, or replicate the trivial exit-code policy (`exit_code == 0 → pass`) inside `forge-store`. Resolve when wiring.

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

**Lock lifetime at the command boundary (R3/R4/R5):**

```
command_result(command, request_id, f):
    if requires_repo_lock(command):          # is_mutating_command(command) && command != "run"
        _guard = repo_lock::acquire(forge_dir, timeout)?   # -> Err(LockTimeout) on deadline
        # _guard held for the whole critical section below; unlock on return (Drop)
    pre-flight replay check (optional fast path)
    result = f(cwd, request_id)              # accept's HEAD-read + decide run HERE, under the lock
    # _guard drops here

# run goes THROUGH command_result like every mutating command — but requires_repo_lock("run")
# is FALSE, so no lock is taken; the child executes inside f, LOCK-FREE (PRD §10.6). The
# pre-flight replay guard + failed-op recording still apply (they live in command_result):
run_response = command_result("run", id, |cwd, id| {
    captured = capture_with_timeout(...)     # child runs with NO repo lock held
    record_evidence(...)                     # single writer txn; serialized by IMMEDIATE/WAL
})

# check's verdict moves INTO record_check's IMMEDIATE txn, so it is atomic with the evidence
# row it names even though the racing `run` writer is lock-free:
record_check(tx, ...): ev = latest_evidence_on(tx); verdict = evaluate(ev.exit_code); ...
```

**Durability boundaries the crash harness injects at (R1/R7):**

```
save:    write objects (fsync file + dir) ──┤CRASH after_object_fsync_before_db_commit├── commit content_ref (WAL)
                                            reopen ⇒ object durable, ref absent      (never: ref present, object gone)
         commit content_ref (WAL) ──┤CRASH after_db_commit_before_checkpoint├── checkpoint
                                    reopen ⇒ WAL replayed ⇒ ref present AND object present
restore: file[0]..file[k] renamed ──┤CRASH mid_restore├── file[k+1]..
                                    reopen ⇒ each renamed file whole (never torn); no orphan temp ⇒ doctor clean
```

---

## Implementation Units

### U1. Repo-level advisory write lock (`forge-store`)

**Goal:** A `RepoLock` guard + `acquire` that serializes Forge writers on `.forge/forge.lock` using std `File::try_lock`, returning a typed retryable `LockTimeout` on deadline.

**Requirements:** R3, R9, R11.

**Dependencies:** None.

**Files:**
- Create: `crates/forge-store/src/repo_lock.rs` (`RepoLock` guard, `acquire(forge_dir, timeout) -> Result<RepoLock>`, `LockTimeout` sentinel).
- Modify: `crates/forge-store/src/lib.rs` (module decl + re-export `RepoLock`, `LockTimeout`, `acquire_repo_lock`; lock-file path helper under `.forge`).
- Test: unit tests in `crates/forge-store/src/repo_lock.rs` (or `lib.rs` test module).

**Approach:**
- Open `.forge/forge.lock` with read+write+create (Windows caveat). Loop `try_lock()`; on `WouldBlock`, `sleep_backoff` (reuse the Phase 1a jitter: `process::id()` ⊕ nanos) until the deadline, then return `LockTimeout` (anyhow-wrapped sentinel). On `Error(io::Error)`, propagate. On success, return the `RepoLock` holding the `File`.
- `RepoLock` releases on `Drop` (explicit `unlock()` or rely on close; prefer explicit `unlock()` for determinism). Acquire **once** per command — never nested (std re-entrancy caveat).
- Default deadline 10s; `FORGE_LOCK_TIMEOUT_MS` override, **clamped to a minimum floor** (≥ ~50 ms / at least one real retry) so a `0`/`1` value cannot silently turn the lock into try-once-fail and re-open the cross-read races (R3/R4).

**Patterns to follow:** `RequestIdReplay` (typed sentinel + `Display`/`Error`); `sleep_backoff`/`is_retryable_busy` jitter and bounded-retry shape in `lib.rs`.

**Test scenarios:**
- Happy path: `acquire` on an uncontended lock returns a guard; a second `acquire` from another thread/process blocks then succeeds once the first guard drops.
- Edge case: two guards in sequence (acquire → drop → acquire) both succeed (release works).
- Error path: with the lock held and `FORGE_LOCK_TIMEOUT_MS` set very low, a second `acquire` returns `LockTimeout` (the typed sentinel, downcastable) — *not* a busy/io error, and within ~the configured deadline.
- Edge case: `acquire` creates `.forge/forge.lock` if absent (read+write open).
- Error path / clamp: `FORGE_LOCK_TIMEOUT_MS=0` is clamped to the floor (not honored as try-once) — under contention it still waits ≥ the floor before `LockTimeout`, so serialization cannot be silently disabled.

**Verification:** A direct unit test proves `LockTimeout` fires on contention within the deadline and that the guard is re-acquirable after release (per solution-doc §7: back the contention behavior with a direct predicate test, since an integration test can't distinguish "never contended" from "contended and waited").

---

### U2. Wire the lock at the CLI boundary + close the `check` verdict in-txn (`forge-cli`, `forge-store`)

**Goal:** Hold the repo write lock across the critical section of mutating commands except `run` — making `accept`'s `STALE_BASE` decision atomic against concurrent Forge `accept`s and serializing `save`/`propose` object writes — and close the `check` verdict TOCTOU by computing pass/fail **inside** `record_check`'s IMMEDIATE txn (the lock cannot help `check`: the racing `run` writer is deliberately lock-free, R5). Surface `LockTimeout` as `"LOCK_TIMEOUT"`.

**Requirements:** R4, R5, R3, R9.

**Dependencies:** U1.

**Files:**
- Modify: `crates/forge-cli/src/main.rs` (`command_result` acquires the lock for `requires_repo_lock(command)`; `check_response` stops deriving the verdict from its `show()` read and lets `record_check` evaluate in-txn; `error_code` maps the `LockTimeout` downcast to `"LOCK_TIMEOUT"`; a `requires_repo_lock` helper = `is_mutating_command(command) && command != "run"`).
- Modify: `crates/forge-store/src/lib.rs` (`record_check` computes the pass/fail verdict from its in-txn `latest_evidence_on(tx)` read instead of accepting a CLI-precomputed status).
- Test: `crates/forge-cli/tests/forge_concurrency.rs` (cross-read serialization); `crates/forge-cli/tests/forge_propose_check.rs` (in-txn verdict).

**Approach:**
- In `command_result`, when `requires_repo_lock(command)`, acquire the guard before running `f`; bind it so it drops after `f`. On `LockTimeout`, return an error envelope with code `"LOCK_TIMEOUT"`.
- **`check`:** move the pass/fail derivation into `record_check`'s IMMEDIATE txn. `record_check` already re-reads `latest_evidence_on(tx)` for the staleness verdict — derive the verdict from that same in-txn row's exit code and drop the CLI-layer `show()`-based verdict, so the verdict is atomic with the evidence `evidence_id` it binds. The lock is *not* relied on for `check`.
- **`accept`:** no new read — `decision_response` already reads `current_head` immediately before `decide` inside `f`; wrapping `f` in the lock makes that read-then-`decide` atomic against concurrent Forge `accept`s. The external-git HEAD-move window remains inherent (Scope Boundaries).
- **`run`:** stays a `command_result("run", …)` call, but `requires_repo_lock("run")` is `false`, so the child runs lock-free (R5); the pre-flight replay guard and failed-op recording in `command_result` still apply.
- Map `LockTimeout` to `"LOCK_TIMEOUT"` by `downcast_ref` (structural), consistent with the `RequestIdReplay` handling — not a new substring arm.

**Patterns to follow:** existing `RequestIdReplay` downcast in `command_result`; the in-txn determining-read pattern (`latest_evidence_on`, Phase 1a U4); the `error_code` ladder (reached via downcast).

**Test scenarios:**
- Integration: a `check` racing a `forge run` records a verdict whose pass/fail and `evidence_id` derive from the *same* in-txn evidence row — even though `run` holds no lock. (Covers the deferred TOCTOU finding via in-txn evaluation, not the lock.)
- Integration: two concurrent `accept`s on the same proposal — exactly one decides; the loser sees a coherent outcome (already-decided / stale-base), serialized by the lock.
- Error path: with the lock held by another process, a mutating command returns envelope `error.code == "LOCK_TIMEOUT"` after the (clamped) deadline.
- Happy path / regression: `forge run` of a slow child does **not** block a concurrent mutating command for the child's full duration (R5) — `run` holds no repo lock during execution.
- Regression: read-only `show`/`doctor` never block on the write lock.

**Verification:** the in-txn `check` verdict and the `accept` serialization tests pass deterministically; `LOCK_TIMEOUT` is observable in the envelope; `run` demonstrably does not serialize behind a peer for its child's lifetime; verify trio green.

---

### U3. Formalize store-before-DB durability ordering + sidecar/lock-file exclusions (`forge-content-native`, `forge-content-git`, `forge-content`)

**Goal:** Make the object-durable-before-`content_ref`-commit ordering an explicit, documented contract; close the shard-dir durability race via the lock; exclude the lock file and WAL sidecars from snapshots/exports in both backends.

**Requirements:** R1, R12, R11.

**Dependencies:** U1, U2 (the lock that serializes object-writing commands closes the shard-dir race).

**Files:**
- Modify: `crates/forge-content/src/lib.rs` (document the durability-ordering contract on `trait ContentBackend::snapshot_worktree`: "returns only after all written objects are durable").
- Modify: `crates/forge-content-native/src/lib.rs` (doc the `write_object` barrier as the contract's basis — no `is_ignored_by_policy` rule change needed, see Approach).
- Test: `crates/forge-content-native/src/lib.rs` + `crates/forge-content-git/src/lib.rs` unit tests (regression assertion that `.forge/forge.lock` is excluded, symmetric across backends, alongside the existing WAL-sidecar assertion); the cross-boundary durability proof lives in U6.

**Approach:**
- The ordering is already correct by call sequence (`snapshot_worktree` fsyncs all objects before `save_snapshot`/`propose` commit). This unit *formalizes* it: a documented trait contract + the shard-dir race closed by U2's lock (cross-reference). No new per-object fsync (avoid latency); the lock is the cross-process serialization.
- `.forge/forge.lock` is **already** excluded by the existing blanket `.forge/` prefix in both backends' `is_ignored_by_policy` (the same mechanism that already covers the WAL sidecars, verified at `forge-content-native` ~L494 and `forge-content-git` ~L190). The deliverable is a **regression assertion** that pins it — **not** a new exclusion rule, **not** a named-match; do not narrow the blanket prefix while editing (R12).

**Patterns to follow:** the existing `wal_sidecars_are_excluded_by_policy` tests in both backends; `write_object`'s documented fsync barrier.

**Test scenarios:**
- Edge case: `.forge/forge.lock` is excluded by `is_ignored_by_policy` in the native backend.
- Edge case: `.forge/forge.lock` is excluded by `is_ignored_by_policy` in the git backend (symmetric — guards against drift).
- (Documentation-only change to the trait contract: `Test expectation: none -- doc/contract comment; the ordering is proven by U6's crash boundary.`)

**Verification:** Both backends exclude the lock file and sidecars; the trait documents the store-before-DB contract; U6's `after_db_commit_before_checkpoint` boundary proves a committed `content_ref` implies a durable object.

---

### U4. Crash-atomic worktree restore (`forge-content-native`)

**Goal:** Replace the in-place `fs::write` in `materialize_tree`'s file arm with temp+rename+fsync so each restored file is atomic and durable.

**Requirements:** R2, R8.

**Dependencies:** None (independent of the lock).

**Files:**
- Modify: `crates/forge-content-native/src/lib.rs` (`materialize_tree` `TreeEntryKind::File` arm).
- Test: `crates/forge-cli/tests/forge_doctor_gc.rs` (extend restore tests) and/or `forge-content-native` unit tests.

**Approach:**
- For each file: `create_dir_all(parent)`; create the temp in the *destination's parent dir* with a Forge-owned prefix — `tempfile::Builder::new().prefix(".forge-restore-").tempfile_in(parent)` — so the rename is same-filesystem (even when `.forge` is a separate mount under a sandboxed agent) **and** a crash-orphaned temp is discoverable by `doctor` (U7) without colliding with a user file; `write_all(bytes)`; `set_file_mode` on the temp; `sync_all()` the temp; `persist(full)`; then `sync_dir(parent)`. Adapts `write_object`'s pattern — the prefix + worktree-parent location is the intentional difference (the `abort()`-based crash model in U6 skips `Drop`, so temp cleanup cannot rely on `NamedTempFile`'s auto-delete; doctor's scan is what reclaims a leak).
- Bound fsync cost: track parent dirs already `sync_dir`'d within this `materialize_tree` invocation and fsync each once (ticket risk note on large worktrees).
- Preserve existing semantics: remove a dir-at-path before writing a file (the `full.is_dir()` branch), policy-skip, recursion for `Dir` entries.

**Patterns to follow:** `write_object` temp+rename+fsync; existing `materialize_tree` structure.

**Test scenarios:**
- Happy path: restoring a snapshot reproduces every file's content and mode (existing behavior unchanged).
- Edge case: restoring over an existing file replaces it atomically (no intermediate truncated/torn state observable) — assert content is exactly old or exactly new.
- Edge case: restoring a file where a directory currently exists at that path still succeeds (dir removed first).
- Integration: after a successful restore, no `.forge-restore-*` temp file remains in any restored directory (clean rename path).
- (Crash-mid-restore atomicity is proven in U6.)

**Verification:** Restore round-trips content+mode; no torn writes; no orphaned temps; existing `native_restore_verifies_reachable_objects_before_mutating_worktree` still passes; verify trio green.

---

### U5. Race-safe concurrent `forge init` (`forge-store`)

**Goal:** Two simultaneous first-inits of the same repo both succeed cleanly — one initializes, the other returns `already_initialized: true` — with no raw SQLite constraint text or misleading error code.

**Requirements:** R6, R9, R11.

**Dependencies:** U1 (init participates in the lock after `.forge` creation).

**Files:**
- Modify: `crates/forge-store/src/lib.rs` (`init_repository`: create `.forge`, acquire the repo lock, move the `read_init_repository` short-circuit inside the IMMEDIATE txn or catch the `repositories.root_path` UNIQUE violation → `already_initialized: true`; `apply_migrations`: `INSERT OR IGNORE` the `schema_migrations` version row).
- Modify (if needed): `crates/forge-cli/src/main.rs` (`init_response` surfaces `already_initialized` coherently).
- Test: `crates/forge-cli/tests/forge_concurrency.rs` (concurrent init) and/or `forge_init.rs`.

**Approach:**
- After creating the `.forge` directory, acquire the repo lock (the lock file lives in `.forge`, now created) so concurrent inits serialize; the loser, under the lock, observes the winner's committed repository row and returns `already_initialized: true`. Note: `init` does not route through `command_result`, so it acquires the lock **inside** `init_repository` directly — outside the `requires_repo_lock` path (and so never double-locks).
- Make the version-row insert idempotent (`INSERT OR IGNORE`) and move the determining init read inside the IMMEDIATE txn (the determining-read-inside-the-txn pattern from Phase 1a — `docs/plans/completed/2026-05-28-005-fix-substrate-phase-1a-plan.md`, **not** this plan's U4) so even without the lock the path is constraint-safe.
- **NER-133 coordination:** keep the `apply_migrations` change to the absolute minimum (the `INSERT OR IGNORE` line only) and mark it in-code as a temporary idempotency shim with a comment pointing at NER-133, so when NER-133's numbered-migration runner replaces `apply_migrations` it knowingly re-applies the invariant rather than silently dropping it.

**Patterns to follow:** Phase 1a "move the determining read inside the IMMEDIATE txn"; `already_initialized` envelope shape if it exists, else mirror existing init success shape.

**Test scenarios:**
- Integration: N concurrent `forge init` processes on one fresh repo → exactly one reports a fresh init, the rest report `already_initialized: true`; **no** output contains raw SQLite UNIQUE-constraint text or `NOT_A_GIT_REPOSITORY`.
- Edge case: sequential re-`init` of an already-initialized repo still returns `already_initialized: true` (no regression).
- Edge case: `INSERT OR IGNORE` leaves exactly one `schema_migrations` version-2 row after concurrent inits.

**Verification:** Concurrent-init test green; no SQLite text leaks; single version row; verify trio green.

---

### U6. Crash-injection + concurrency harness (`forge-cli`, gated hooks)

**Goal:** Prove R1/R2/R7 on Linux and macOS by killing the process at each durability boundary and asserting post-crash invariants + `doctor` cleanliness.

**Requirements:** R7, R1, R2, R9.

**Dependencies:** U1–U5 (it tests them).

**Files:**
- Modify: `crates/forge-cli/src/main.rs` (and/or `forge-content-native`/`forge-store`) — `cfg!(debug_assertions)`-gated `FORGE_CRASH_POINT` checks calling `std::process::abort()` at the three boundaries.
- Create: `crates/forge-cli/tests/forge_crash_injection.rs`.

**Approach:**
- Crash points: `after_object_fsync_before_db_commit` (in the `save` flow, between `snapshot_worktree` and `save_snapshot` — this hook lives in the **CLI** `save_response` layer, since neither library crate spans the snapshot-then-commit boundary; place it after `snapshot_worktree` returns and before `save_snapshot` begins), `mid_restore` (inside `materialize_tree` after k files), `after_db_commit_before_checkpoint` (after a writer commit, before any checkpoint). Each: `if cfg!(debug_assertions) && env FORGE_CRASH_POINT == <name> { abort() }`. Release builds → dead branch, zero overhead.
- **`abort()` intentionally skips `Drop`** — it models a hard kill (SIGKILL / sandbox teardown / OOM), the failure mode agents actually hit. Two consequences the harness relies on: (a) `flock` is reclaimed by the OS on process death, so lock release does **not** depend on the `RepoLock` `Drop` running (a crashed holder never wedges peers — the harness asserts a peer can acquire after the crash); (b) `tempfile`'s auto-delete does **not** run, so a crash-orphaned restore temp persists — which is exactly why doctor (U7) reclaims it by scanning for the `.forge-restore-*` prefix rather than relying on `Drop`.
- Tests spawn the binary (debug, via `assert_cmd::cargo::cargo_bin`) with `FORGE_CRASH_POINT` set, assert the child aborts (signal/non-zero), then reopen the repo and assert the boundary invariant + `doctor` clean. Portable across Linux/macOS (`abort()` + the existing `std::process::Command` harness).

**Patterns to follow:** `forge_concurrency.rs` (real child processes, JSON-envelope parsing, `open_db`); solution-doc §7.

**Test scenarios:**
- Crash `after_object_fsync_before_db_commit` → reopen → the object is durably present, the `content_ref` is absent; **never** the inverse (committed ref, missing object). `doctor` clean.
- Crash `after_db_commit_before_checkpoint` → reopen → WAL replayed → the `content_ref` is present **and** its object is durable. `doctor` reports zero dangling `content_refs`.
- Crash `mid_restore` → reopen → every already-renamed file is whole (not torn); no orphaned temp; `doctor` reports zero half-applied worktrees.
- Edge case: crash hooks are inert when `FORGE_CRASH_POINT` is unset (normal commands unaffected).

**Verification:** All three boundaries pass on macOS (dev) and Linux (CI); `doctor` clean after each; the `synchronous=NORMAL` failure envelope (lost last commit OK, never corruption) holds; verify trio green on both OSes.

---

### U7. Extend `doctor` for dangling `content_refs` + half-applied worktrees (`forge-store`)

**Goal:** `doctor` explicitly reports zero dangling `content_refs` and zero half-applied worktrees.

**Requirements:** R8, R9.

**Dependencies:** U4 (restore-temp accounting).

**Files:**
- Modify: `crates/forge-store/src/lib.rs` (`DoctorReport` + `doctor`: a distinct `dangling_content_refs` category; a `half_applied_worktree` check scanning the worktree for `.forge-restore-*` temps, alongside the existing `.forge/tmp` scan).
- Modify (if needed): `crates/forge-cli/src/main.rs` (`doctor_response` serialization).
- Test: `crates/forge-cli/tests/forge_doctor_gc.rs`.

**Approach:**
- Promote the existing "verify every `content_ref`" loop's failures into a named `dangling_content_refs` field/category (vs the generic `issues`), so the exit criterion is machine-checkable.
- Add a half-applied-worktree check that scans the **worktree** (repo root, excluding `.git`/`.forge`) for leftover `.forge-restore-*` temp files (U4's prefix). The existing `.forge/tmp` dangling-temp scan stays for object-write temps. The distinctive prefix avoids false positives on user `.tmp*` files.

**Patterns to follow:** existing `doctor` checks (`foreign_key_check`, schema version, per-`content_ref` verify, `dangling_temp_files`); `DoctorReport` serde shape.

**Test scenarios:**
- Happy path: `doctor` on a clean initialized repo → `ok: true`, zero dangling refs, zero half-applied worktrees.
- Error path: with an artificially deleted object that a `content_ref` references → `doctor` flags it as a dangling `content_ref` (not `ok`).
- Edge case: a leftover `.forge-restore-*` temp in a worktree directory → `doctor` flags a half-applied worktree; a leftover `.forge/tmp` artifact is still flagged too.

**Verification:** `doctor` distinguishes the two new categories; clean repo is `ok`; injected damage is flagged; U6 reuses these checks post-crash; verify trio green.

---

### U8. Deterministic in-txn `replay_guard` coverage (`forge-store`)

**Goal:** Deterministically exercise the in-txn `RequestIdReplay` branch and concurrent `status == "failed"` replay at the store level (not just the end-to-end CLI path).

**Requirements:** R10, R11.

**Dependencies:** None.

**Files:**
- Test: `crates/forge-store/src/lib.rs` test module (or a `forge-store` integration test) — a thread test with **separate connections**.

**Approach:**
- Drive two writers on separate connections at the same `(repo_id, request_id)` so the loser hits the in-txn `replay_guard` (the `RequestIdReplay` branch) after the winner commits — not the CLI pre-flight. Assert exactly one domain row and that the loser observes the replay.
- Add a case where the recorded operation has `status == "failed"` and assert the replay reproduces the failure (the path currently covered only sequentially in `forge_start_save.rs`).

**Patterns to follow:** solution-doc §7 (separate connections, real concurrency); existing `concurrent_same_request_id_creates_exactly_one_snapshot` end-to-end test.

**Test scenarios:**
- Integration: two separate-connection writers, same `request_id` → exactly one domain row; the loser's result is a replay via the in-txn guard (assert the in-txn branch, not the pre-flight).
- Integration: a recorded `failed` operation replayed by the same `request_id` → reproduces the failure deterministically.

**Verification:** The in-txn `RequestIdReplay` branch and failed-replay are asserted deterministically at the store level; verify trio green.

---

## System-Wide Impact

- **Interaction graph:** the lock guard sits in `command_result` (the single mutating-command funnel) and `init_repository`; the durability/restore changes are inside `forge-content-native`; `doctor` reads across `snapshots`/`proposal_revisions` + the object store + `.forge/tmp`.
- **Error propagation:** `LockTimeout` flows store → CLI as an anyhow-wrapped sentinel, recovered by `downcast_ref` and mapped to `"LOCK_TIMEOUT"` (structural, not substring) — same channel as `RequestIdReplay`. Lock-acquire failure short-circuits before the closure runs.
- **State lifecycle risks:** partial object writes (closed by the existing temp+rename + the new ordering proof), torn restore files (closed by U4), orphaned temps (detected by U7/`doctor`), half-initialized repos (closed by U5). The advisory lock guard must always release (Drop) even on panic/early-return.
- **API surface parity:** the lock-file + sidecar exclusion must stay symmetric across `forge-content-native` and `forge-content-git` (two `is_ignored_by_policy` copies).
- **Integration coverage:** the cross-read (U2), concurrent-init (U5), and crash-boundary (U6) behaviors are only provable with real multi-process / kill tests, not in-process mocks.
- **Unchanged invariants:** the `forge.cli.v0` envelope shape, all Phase 1a SQLite PRAGMA/IMMEDIATE/retry/replay/UUIDv7/rowid behavior, and the security-default exclusions are **not** changed except to *add* the lock file to the exclusion set; `run`'s evidence-capture semantics are unchanged (it simply remains lock-free).

---

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Advisory lock + WAL interact differently across Linux/macOS | Med | High | Lock is a *separate* `.forge/forge.lock` file, independent of SQLite's WAL locks; harness (U6) runs the lock + crash boundaries on both OSes; CI (Linux) + dev (macOS). |
| Same-process re-entrant lock → deadlock (std caveat) | Med | High | Acquire **exactly once** at the command boundary; no store-level fn acquires independently; `requires_repo_lock` excludes nesting; documented. |
| `run` regresses to holding the lock during child exec (PRD §10.6) | Low | High | `run` explicitly excluded from `requires_repo_lock`; U2 test asserts a slow `run` does not serialize a peer for the child's lifetime. |
| Per-file temp+rename + per-ancestor fsync slows large-worktree restore | Med | Med | fsync each parent dir once per `materialize_tree` (track fsynced dirs); ancestors fsynced only on first creation; benchmark note; accept for v0 (crash-correctness > restore speed). |
| Crash hooks ship in release | Low | Med | Gated on `cfg!(debug_assertions)` → dead branch in release; honored only when `FORGE_CRASH_POINT` env is set. |
| Over-claiming durability the harness doesn't prove | Med | Med | Scope Boundaries states `abort()`-injection proves ordering-consistency given OS fsync, not block-device fault injection. |
| Touching `apply_migrations` collides with NER-133's migration-runner rewrite | Med | Med | U5 keeps the change to the `INSERT OR IGNORE` line only, marked in-code as a temporary shim with a comment pointing at NER-133 so the runner rewrite knowingly re-applies the invariant; the runner itself stays NER-133. |

---

## Documentation / Operational Notes

- Document the locking model and `LOCK_TIMEOUT` contention error (golden-tested per R9): what acquires the lock, when, the §10.6 `run` carve-out, the timeout + `FORGE_LOCK_TIMEOUT_MS` override, and the inherent external-git window for `accept`.
- On merge: this is a strong `/ce-compound` candidate — the temp+rename worktree-restore pattern, "advisory lock closes CLI-layer cross-reads," and the crash-injection harness recipe are each currently undocumented in `docs/solutions/`.
- Coordinate with NER-133 at the migration/write-lock boundary: the migration runner must run *under* this lock; `LockTimeout` and `RequestIdReplay` are candidates for NER-133's typed error taxonomy (NER-133 owns the final enum shape — don't pre-encode it here).

---

## Sources & References

- **Origin:** `docs/ROADMAP.md` (Phase 1b); Linear **NER-132** (+ its three deferred-findings comment threads); `docs/handoffs/2026-05-29-m1-phase-1b-and-2-kickoff.md`.
- Carry-over invariants: `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` (§1, §6, §7, § Scope boundaries).
- Code-review triage: `docs/code-reviews/2026-05-29-phase-1a-pr-b-concurrency.md`.
- Prior art: `docs/plans/completed/2026-05-28-003-feat-native-forge-content-store-plan.md` (R4 store-before-DB framing), `docs/plans/completed/2026-05-28-005-fix-substrate-phase-1a-plan.md` (U1–U6 Phase 1a).
- PRD: §10.6 (Locking and Concurrency), § Security defaults (CLAUDE.md).
- Related PRs: #5, #6 (Phase 1a); #7, #8 (Phase 1a close-out + handoff).
- External: Rust std `File` locking (stabilized 1.89.0); SQLite WAL (sqlite.org/wal.html).
