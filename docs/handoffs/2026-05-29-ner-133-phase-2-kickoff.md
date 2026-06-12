# Handoff — Forge M1 next: Phase 2 (NER-133), the last M1 track

**Date:** 2026-05-29 · **Milestone:** M1 — Bulletproof the ledger · **Ticket:** Linear **NER-133** (Phase 2) · **Forge project:** id `2b5e82f7-7a78-4354-af7d-68609e6e77bc`, team **SE Engineers**, prefix **NER**

## Where things stand

Forge is an agent-first local change-control CLI over Git (Rust workspace, `crates/forge-*`; state in `.forge/forge.db` via rusqlite "bundled"). **M1's crash-correctness story is now complete and merged:**

- **Phase 1a (NER-131):** WAL + `busy_timeout` + `synchronous=NORMAL`; `BEGIN IMMEDIATE` + bounded busy/517 retry on every writer; in-txn `replay_guard` (`RequestIdReplay` sentinel); UUIDv7 ids + `rowid` tiebreaks. (PRs #5, #6.)
- **Phase 1b (NER-132):** repo-level advisory write lock on `.forge/forge.lock` (std `File::try_lock`, zero deps) with a typed retryable `LockTimeout`; `check` verdict closed **in-txn**; `accept` `STALE_BASE` under the lock; crash-atomic worktree restore (temp+rename+fsync); store-before-DB durability contract; race-safe concurrent `init`; extended `doctor`; crash-injection harness. **Merged as PR #9** (`8a8485f` on `main`). Status: **Done.**

`main` is clean and synced. Phase 1b's plan is at `docs/plans/completed/2026-05-29-006-fix-phase-1b-crash-correctness-plan.md`; its code review is `docs/code-reviews/2026-05-29-phase-1b-crash-correctness.md`; its learnings are captured in `docs/solutions/architecture-patterns/crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md` (**read it** — it pins the invariants 2 must not regress). Phase 1a's solution doc (`…/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`) still applies.

Verify trio is the gate (`cargo fmt --all --check` · `cargo test --workspace` · `cargo clippy --workspace --all-targets -- -D warnings`); CI runs the same trio on every PR. Toolchain pinned `1.92.0`. `gh` authenticated; remote repository configured; squash-merge convention `(#N)`.

## What's next — NER-133 (Phase 2), the last open M1 track

**No plan doc exists yet.** Run the full lifecycle: branch off `main` (e.g. `ner-133-phase-2-migrations-typed-errors`) → `/ce-plan NER-133` → **doc-review gate** (`/ce-doc-review`) → `/ce-work` → **code-review gate** (`/ce-code-review` with `plan:<path>`) + verify trio → `/ce-commit-push-pr` referencing NER-133.

### Roadmap scope (from the ticket — `docs/ROADMAP.md` Phase 2)

- **Numbered `.sql` migration runner** — each migration discrete, ordered, recorded in `schema_migrations`, DDL + version stamp in **one** txn, **gated under Phase 1b's write lock**, replacing the unconditional `apply_migrations` (`crates/forge-store/src/lib.rs`). Unify fresh-init and upgrade paths (today `attached_attempt_id` is an upgrade-only `ALTER` absent from `001`'s DDL); per-migration checksums; unknown future version ⇒ read-only refuse.
- **Typed `ForgeError` enum** in `forge-store` (`STALE_BASE`, `DIRTY_WORKTREE`, `AMBIGUOUS_ATTEMPT`/`PROPOSAL`, `REQUEST_ID_CONFLICT`, `NOT_ACCEPTED`, `LOCK_TIMEOUT`…) replacing the substring-matched `error_code()` (`crates/forge-cli/src/main.rs`) and `bail!` string contracts. Fold in the two existing sentinels — `RequestIdReplay` and **`LockTimeout`** (both already `impl Error` + downcast at the CLI) — as the first members.
- **Populate `errors[].details`** (candidate ids, expected-vs-actual head, offending paths), **meaningful `retry.retryable`**, `warnings[]`.
- **`forge schema` / `--capabilities`** emitting `schema_version`, per-command shapes, and the error-code registry as published JSON Schema.
- **Launch blocker A — secret-export default-deny:** gate evidence/content export on secret-risk by default; each dropped path → a `warnings[]` entry. (Full redactor stays Phase 5.)
- **Launch blocker B — conflict-set metadata persistence:** on `current_head != base_head` at accept/export, write a `ConflictSet`/`PathConflict` row (PRD §15). Metadata insert — no merge engine.
- **Exit criteria:** adding a schema change needs only a numbered `.sql` file; crash-atomic & idempotent; a schema-diff test proves fresh-init-v2 and upgraded-via-ALTER converge; no error code is string-derived; `forge schema` emits a versioned contract; export refuses secret-risk by default with a warning; a stale-base bail writes a persisted conflict-set row.

### Deferred findings NER-133 owns (filed as Linear comments on NER-133)

1. **Transient CAS conflict poisons a `--request-id`** (Phase 1a PR-B review). A lost `current_state` singleton CAS (`current operation changed`) is recorded by `record_failed_operation` under the request-id, so a later retry replays the failure (status-aware replay). The typed-error taxonomy is where this is fixable: classify `current operation changed` **and** `LOCK_TIMEOUT` as **transient/retryable** so a recorded transient failure is re-runnable, distinct from a deterministic domain failure.
2. **`LOCK_TIMEOUT` ships `retry.retryable=false`** (Phase 1b code review — api-contract + agent-native). The error *code* is surfaced and documented retryable, but the structured `retry` field is uniformly `false` for every error today. When the typed enum lands, set `LOCK_TIMEOUT.retry.retryable = true` and surface `LockTimeout.waited_ms` in `errors[].details` for adaptive backoff. (Deliberately NOT done in 1b — setting one error's retryable in isolation pre-empts the taxonomy.)

## Carry-over context 2 must honor

- **The migration runner must run *under* Phase 1b's advisory write lock** — these two tracks meet at the migration/lock boundary. Acquire via `forge_store::acquire_repo_lock` / the `repo_lock` module; do not nest a second acquisition (std re-entrancy caveat).
- **Absorb the `apply_migrations` shim.** Phase 1b made the `schema_migrations` version-row inserts `INSERT OR IGNORE` as a minimal race-safety shim, flagged in-code for this runner to subsume. The new runner must carry that idempotency invariant forward.
- **Changing error codes/shapes is itself a contract break** — ride the `schema_version` bump. The `forge.cli.v0` envelope is otherwise frozen; **additive** new fields/codes (as 1b did for `LOCK_TIMEOUT` and the new `doctor` fields) did not bump it, but reclassifying or renaming existing codes does.
- **Do not regress Phase 1a/1b invariants** (see both solution docs): WAL persistence + per-connection PRAGMAs, `IMMEDIATE` everywhere, 517/busy retry, `RequestIdReplay`/`LockTimeout` sentinels, UUIDv7 + `rowid` tiebreaks; store-before-DB ordering; the §10.6 `run` lock carve-out; acquire-the-lock-once; crash injection is consistency-not-power-loss.
- **Security defaults are load-bearing** (CLAUDE.md): snapshot/export exclude `.forge` (incl. `-wal`/`-shm` sidecars **and** the new `.forge/forge.lock` and `.forge-restore-*` worktree temps — tested in both backends), `.env`, keys; `EXCERPT_LIMIT`; redaction. Launch-blocker A tightens export to default-deny.
- **Stale line numbers:** the ticket cites `lib.rs:1799-1843` (apply_migrations) and `main.rs:739-775` (error_code); both shifted after Phase 1b. Navigate by symbol (`apply_migrations`, `error_code`, `requires_repo_lock`, `acquire_repo_lock`).

## Process

1. Branch off updated `main`: `ner-133-phase-2-migrations-typed-errors`.
2. `/ce-plan NER-133` → land a plan in `docs/plans/<date>-NNN-<type>-<name>-plan.md` (next seq is 007) → **doc-review gate** before `/ce-work`.
3. `/ce-work` → **code-review gate** (`/ce-code-review` with `plan:<path>`) + verify trio → `/ce-commit-push-pr` referencing NER-133.
4. On merge: flip the plan to `status: completed`, move to `docs/plans/completed/`, set NER-133 → Done, `/ce-compound` the non-obvious learnings (the migration-runner-under-lock recipe and the typed-error-taxonomy migration are strong candidates).
5. **Scope discipline:** keep 2 to its ticket scope; the full redactor is Phase 5, the merge engine is later, `AUTOINCREMENT`/gc hardening is Phase 8. File new defer-able findings as Linear comments on the right ticket.
