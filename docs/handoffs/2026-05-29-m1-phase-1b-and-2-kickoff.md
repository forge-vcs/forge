# Handoff — Forge M1 next: Phase 1b (NER-132) + Phase 2 (NER-133)

**Date:** 2026-05-29 · **Milestone:** M1 — Bulletproof the ledger · **Tickets:** Linear **NER-132** (Phase 1b), **NER-133** (Phase 2) · **Forge project:** id `2b5e82f7-7a78-4354-af7d-68609e6e77bc`, team **SE Engineers**, prefix **NER**

## Where things stand

Forge is an agent-first local change-control CLI over Git (Rust workspace, `crates/forge-*`; state in `.forge/forge.db` via rusqlite "bundled"). **Phase 1a (NER-131) is complete and merged** — both halves:

- **PR A (#5):** U1 crash-safe object-write durability + U2 UUIDv7 ids and `, rowid DESC` tiebreaks.
- **PR B (#6):** U3 WAL + `busy_timeout(5s)` + `synchronous=NORMAL`; U4 `BEGIN IMMEDIATE` + bounded busy/517 retry on all 12 writer txns with determining reads moved inside the txn; U5 serialized `--request-id` replay (in-txn `replay_guard` + `RequestIdReplay` anyhow sentinel → `command_result`/`replay_response`); U6 real-multi-process exit-criteria tests.

The plan is completed and archived at `docs/plans/completed/2026-05-28-005-fix-substrate-phase-1a-plan.md`. The first `docs/solutions/` entry now exists: `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` (read it — it encodes the WAL/IMMEDIATE/replay/UUIDv7 learnings and the **scope boundaries** that bound the work below). Code-review triage: `docs/code-reviews/2026-05-29-phase-1a-pr-b-concurrency.md`.

`main` is clean. Verify trio is the gate (`cargo fmt --all --check` · `cargo test --workspace` · `cargo clippy --workspace --all-targets -- -D warnings`); CI runs the same trio. Toolchain pinned `1.92.0`. `gh` authed as `freezscholte`; remote `freezscholte/forge`; squash-merge convention `(#N)`.

## What's next — two L-effort tracks (can run in parallel)

Neither has a plan doc yet. **Each needs its own `/ce-plan` → doc-review gate → `/ce-work` → code-review gate → `/ce-commit-push-pr` cycle** (the compound-engineering lifecycle in CLAUDE.md). Recommended order: **start NER-132 (Phase 1b)** — it completes the crash-correctness story for M1 and owns most of the deferred code-review findings; NER-133 (Phase 2) coordinates with 1b's write lock and can proceed alongside.

### NER-132 — Phase 1b: crash-correctness (store-before-DB, atomic restore, repo write-lock) [L]

Roadmap scope (from the ticket):
- **Strict store-before-DB durability order:** object file + its directory entry durable *before* the SQLite txn that commits the referencing `content_ref`.
- **Crash-atomic worktree restore:** `materialize_tree` per-file temp-file+rename instead of in-place `fs::write` (native `crates/forge-content-native/src/lib.rs` ~387/399).
- **Repo-level advisory file lock** on `.forge` with a typed `LOCK_TIMEOUT` (retryable) error (PRD §10.6) — makes the serialization point explicit rather than an accidental property of SQLite locking.
- **Crash-injection + concurrency harness;** `doctor` reports zero dangling `content_refs` and zero half-applied worktrees.
- **Exit criteria:** crash-injection passes on Linux + macOS at every durability boundary; a committed `content_ref` provably implies a durably-retained object; the locking model + its contention error are documented and golden-tested.

Phase 1a code-review findings filed here (see the NER-132 comment dated 2026-05-29 and the code-review doc):
1. **Concurrent `forge init` of the same repo** — `read_init_repository` (and the `apply_migrations` version-row insert) run *before* the `BEGIN IMMEDIATE` txn, so two first-inits race; the loser hits the `repositories.root_path` UNIQUE constraint and surfaces as `NOT_A_GIT_REPOSITORY` with raw SQLite text. Cheap fixes: `INSERT OR IGNORE` on the `schema_migrations` version row; move `read_init_repository` inside the IMMEDIATE txn (or catch the UNIQUE violation → `already_initialized: true`).
2. **`check` verdict TOCTOU (CLI-layer cross-read)** — `check_response` (`crates/forge-cli/src/main.rs`) reads `show().latest_evidence.exit_code` on a separate connection to compute pass/fail, then `record_check` re-reads the latest evidence inside its IMMEDIATE txn for the staleness verdict + `evidence_id`; a concurrent `forge run` between the two reads can attribute the verdict to a different evidence row. This is exactly the residual cross-read atomicity the advisory lock should close (or pass `exit_code` into `record_check` and evaluate in-txn).
3. **In-txn `replay_guard` test coverage** — R6 is proven end-to-end, but most racing workers are caught by the CLI pre-flight after the winner commits, so the in-txn `RequestIdReplay` branch and concurrent `status == "failed"` replay aren't deterministically asserted. Add a forge-store-level thread test (separate connections).

### NER-133 — Phase 2: migration framework + typed machine-actionable contract + launch-blocker gates [L]

Roadmap scope (from the ticket):
- **Numbered `.sql` migration runner** (each migration discrete, ordered, recorded in `schema_migrations`, DDL+version stamp in one txn, gated under the 1b write lock), replacing the unconditional `apply_migrations` (`crates/forge-store/src/lib.rs`). Unify fresh-init and upgrade paths (today `attached_attempt_id` is an upgrade-only ALTER absent from 001's DDL); per-migration checksums; unknown future version ⇒ read-only refuse.
- **Typed `ForgeError` enum** from `forge-store` (STALE_BASE, DIRTY_WORKTREE, AMBIGUOUS_ATTEMPT/PROPOSAL, REQUEST_ID_CONFLICT, NOT_ACCEPTED, LOCK_TIMEOUT…) replacing substring-matched `error_code()` (`crates/forge-cli/src/main.rs`) and `bail!` string contracts. Populate `errors[].details`, meaningful `retry.retryable`, `warnings[]`.
- **`forge schema` / `--capabilities`** emitting `schema_version`, per-command shapes, and the error-code registry as published JSON Schema.
- **Launch blocker A — secret-export default-deny:** gate evidence/content export on secret-risk by default; surface each dropped path as a `warnings[]` entry. (Full redactor stays Phase 5.)
- **Launch blocker B — conflict-set metadata persistence:** on `current_head != base_head` at accept/export, write a ConflictSet/PathConflict row (PRD §15). Metadata insert — no merge engine.
- **Exit criteria:** adding a schema change needs only a numbered `.sql` file; crash-atomic & idempotent; a schema-diff test proves fresh-init-v2 and upgraded-via-ALTER converge; no error code is string-derived; `forge schema` emits a versioned contract; export refuses secret-risk by default with a warning; a stale-base bail writes a persisted conflict-set row.

Phase 1a code-review finding filed here (see the NER-133 comment dated 2026-05-29):
- **Transient CAS conflict poisons a `--request-id`** — a lost `current_state` singleton CAS (`current operation changed`) is recorded by `record_failed_operation` under the request-id, so a later retry replays the failure. Phase 1a deliberately preserves the plan-002 command-aware/status-aware replay contract, so the fix belongs with the typed-error work: classify `current operation changed` (and `LOCK_TIMEOUT`) as **transient/retryable** so a recorded transient failure is re-runnable, distinct from a deterministic domain failure.

## Carry-over context the next session must honor

- **Read the new solution doc first** (`docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`). It pins the SQLite invariants Phase 1a established (WAL persistent vs per-connection PRAGMAs; IMMEDIATE everywhere; 517 handling; determining-reads-inside-txn; the `RequestIdReplay` sentinel; `rowid` tiebreaks) — 1b/2 must not regress these.
- **1b's advisory lock + WAL interact subtly across OSes** (per the ticket's risk note). The lock is what finally closes the CLI-layer cross-reads (`check` verdict, `accept`'s `STALE_BASE` git `current_head` read) that IMMEDIATE alone can't.
- **2's migration runner replaces `apply_migrations`** and must be gated under 1b's write lock — these two tracks meet at the migration/lock boundary; coordinate if both are in flight.
- **Changing error codes/shapes (NER-133) is itself a contract break** — ride the `schema_version` bump; the `forge.cli.v0` envelope is otherwise frozen.
- **Security defaults are load-bearing** (CLAUDE.md § Security defaults): snapshot/export exclude `.forge` (incl. the WAL `-wal`/`-shm` sidecars — tested in both backends), `.env`, keys; `EXCERPT_LIMIT`; redaction. Launch-blocker A tightens export to default-deny.

## Process

1. Branch off updated `main` per item (e.g. `ner-132-phase-1b-crash-correctness`, `ner-133-phase-2-migrations-typed-errors`).
2. `/ce-plan` the chosen ticket → land a plan in `docs/plans/<date>-NNN-<type>-<name>-plan.md` → **doc-review gate** (`/ce-doc-review`) before `/ce-work`.
3. `/ce-work` → **code-review gate** (`/ce-code-review` with `plan:<path>`) + verify trio → `/ce-commit-push-pr` → reference the ticket.
4. On merge: flip the plan to `status: completed`, move to `docs/plans/completed/`, set the ticket → Done, `/ce-compound` the non-obvious learnings.
5. **Scope discipline:** keep 1b and 2 to their ticket scope; file new defer-able findings as Linear comments on the right ticket rather than scope-creeping.
