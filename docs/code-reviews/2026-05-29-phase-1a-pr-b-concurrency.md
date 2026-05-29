# Code review — Phase 1a PR B (concurrency + idempotent replay)

- **Date:** 2026-05-29
- **Branch:** `ner-131-phase-1a-pr-b-concurrency`
- **base-sha:** `0f64f45` (`docs: add Claude session-relay design brainstorm`)
- **head-sha:** working tree at review time (committed as the PR B squash)
- **Plan:** `docs/plans/2026-05-28-005-fix-substrate-phase-1a-plan.md` (U3–U6)
- **Tracking:** Linear **NER-131**

Scope reviewed: U3 (WAL + `busy_timeout` + `synchronous=NORMAL`), U4 (`BEGIN IMMEDIATE` + bounded busy-retry on all 12 writer txns; determining reads moved inside the txn for propose/record_check/record_evidence), U5 (serialized `--request-id` replay via in-txn `replay_guard` + `RequestIdReplay` sentinel), U6 (multi-process exit-criteria tests).

Review team (10): correctness, adversarial, reliability, security, testing, maintainability, project-standards, performance, ce-learnings-researcher, ce-agent-native. Stack-specific Rust personas: none in catalog. Verify trio green (fmt / 70 tests / clippy `-D warnings`).

## Requirements completeness (plan, explicit)

- **R2** (WAL + `synchronous=NORMAL` + `busy_timeout` every open) — met (`open_connection`); `init_opens_database_in_wal_mode` asserts it.
- **R3** (every writer IMMEDIATE + bounded busy/517 retry) — met (12 writer sites via `with_immediate_retry`; `busy_classification_retries_only_busy_class_errors` unit test).
- **R6** (concurrent same `--request-id` → exactly one domain row, replays original; different command → `REQUEST_ID_CONFLICT`) — met (`replay_guard` + `replay_response`; `concurrent_same_request_id_creates_exactly_one_snapshot` / `_for_different_command_conflicts`).
- **R8** (envelope unchanged) — met; replay envelope byte-identical to pre-existing pre-flight path.
- R1/R4/R5 shipped in PR A; U6 adds the cross-process collision + static no-swallowed-sync assertions.

## Real-actionable — fixed in this PR

These review-surfaced items were fixed before opening the PR:

1. **Narrow backoff jitter** (reliability, adversarial, performance) — `sleep_backoff` jitter was `subsec_nanos() % 8` (0–7 ms, 8 buckets), too narrow to desynchronize concurrent **processes** whose clocks read the same coarse nanosecond. Now mixes `std::process::id()` with the nanosecond over a 0–24 ms window. Dependency-free.
2. **Busy-retry classification untested** (testing, performance, correctness — cross-corroborated) — added `busy_classification_retries_only_busy_class_errors`: asserts `is_retryable_busy` retries SQLITE_BUSY (5) and SQLITE_BUSY_SNAPSHOT (517), walks the wrapped error chain, and does **not** retry SQLITE_CONSTRAINT (19), plain anyhow errors, or the `RequestIdReplay` sentinel. Closes the "absence-of-surface can't distinguish busy-never-fired from busy-retried" gap.
3. **Git-backend WAL-sidecar exclusion untested** (security, project-standards) — the native backend got `wal_sidecars_are_excluded_by_policy`; added the symmetric test to `forge-content-git` so the two `is_ignored_by_policy` implementations can't drift on `.forge/forge.db-wal` / `-shm`.
4. **`replayed_request_id` misleading binding** (maintainability) — inlined to `request_id.as_deref().unwrap_or_default()`; removes a clone and the misleading name.
5. **FnMut/clone rationale undocumented** (maintainability) — documented on `with_immediate_retry` why writer closures `.clone()` captured inputs per attempt.

## Defer-able — out of PR B scope (file follow-ups)

Triaged against the plan's explicit scope (U3–U6) and its deferred-to-Phase-1b list. None are regressions introduced by this PR.

1. **Concurrent `forge init` of the same repo** (adversarial, reliability — medium). `read_init_repository` runs before the IMMEDIATE txn, so two simultaneous first-inits can both pass the short-circuit; the loser hits the `repositories.root_path` UNIQUE constraint (and the `schema_migrations` version insert is similarly non-idempotent), surfacing as `NOT_A_GIT_REPOSITORY` with raw SQLite text. **Out of the compete-loop exit criteria** (the wedge inits once, then fans out). Cheap hardening exists (`INSERT OR IGNORE` on the version row; move `read_init_repository` inside the txn or catch the UNIQUE violation → `already_initialized`). → **NER-132** (Phase 1b cross-process init/first-open race).
2. **`check` verdict TOCTOU** (adversarial — medium). `check_response` reads `show().latest_evidence.exit_code` on a separate connection to compute pass/fail, then `record_check` re-reads the latest evidence inside its IMMEDIATE txn for the staleness verdict + `evidence_id`. A concurrent `run` between the two reads can attribute the externally-computed pass/fail to a different evidence row. This is exactly the **CLI-layer cross-read atomicity the plan defers to Phase 1b** (§ Scope Boundaries / System-Wide Impact `STALE_BASE` caveat). The in-DB determining read *was* moved into the txn per U4; the residual CLI-layer read is deferred. → **NER-132**.
3. **Transient CAS conflict poisons a `--request-id`** (adversarial — medium). A `current operation changed` CAS loss is recorded by `record_failed_operation` under the request-id, so a later retry of the same id replays the failure. This is the **pre-existing command-aware/status-aware replay contract** (plan 002) that Phase 1a explicitly preserves; distinguishing transient from deterministic failures belongs to the typed-error work in **Phase 2 (NER-133)**.
4. **In-txn `replay_guard` / failure-replay coverage under concurrency** (testing, maintainability). The exactly-one-snapshot and conflict tests prove the end-to-end R6 contract, but most racing workers are caught by the CLI pre-flight after the winner commits, so the in-txn `RequestIdReplay` branch and the concurrent `status == "failed"` replay are not deterministically asserted (the failure-replay path is covered sequentially in `forge_start_save.rs`). Deferred testing enhancement (forge-store-level thread test with separate connections). → note on **NER-131/NER-132**.

## Defense-in-depth — optional, non-blocking

- Assert `PRAGMA synchronous` = `normal` after open (mirrors the WAL-mode test) — completes R2 coverage.
- End-to-end `save`/`export branch` test against a repo whose `.forge/forge.db-wal` exists on disk, asserting the sidecars never appear in the produced tree/branch (unit-level exclusion is asserted in both backends today).
- `record_failed_operation` UNIQUE-violation swallow path test (two concurrent same-id failing writers → no crash, exactly one failed row). The swallow is intentional and commented; a test would lock it in.

## Reviewed-and-rejected (do not re-flag)

- **`RequestIdReplay` "custom error type"** (project-standards, confidence 75 vs CLAUDE.md "no custom error types"). **Accepted, deliberate.** The rule's intent is "no typed-error *taxonomy* for domain errors" — which the plan explicitly defers to Phase 2 (NER-133). `RequestIdReplay` is a single control-flow sentinel carried inside `anyhow::Error` and recovered via `downcast_ref`, which is idiomatic `anyhow` usage. The only error-free alternative threads a `Committed | Replayed` enum through all 12 writer return types *and* every `command_result` closure (15+ call sites), which is materially more coupling and complexity — a worse outcome than one minimal sentinel struct. Kept; flagged here so it is not re-litigated.
- **`record_check` closure formatting outlier** (maintainability, low). The multi-line `with_immediate_retry(` wrap in `record_check` is `rustfmt`'s deterministic output (the LHS destructuring tuple is long); collapsing it to one line would fail `cargo fmt --check`. Not a manual inconsistency.
- **`open_connection`-per-call / `apply_migrations` PRAGMA probes / `proposal_metadata_for_attempt` N+1** (performance, low, `pre_existing`). Not introduced by this PR; negligible for a short-lived solo-dev CLI. Defer until measured.

## Verdict

**Ready to merge.** No P0/P1 defects; correctness and security reviewers found zero issues. All five fixed items are clearly-correct and verify-green (fmt / 70 tests / clippy `-D warnings`). The medium adversarial/reliability findings are pre-existing edges outside the U3–U6 compete-loop scope, mapped to NER-132 (Phase 1b) and NER-133 (Phase 2) above — none is a regression.
