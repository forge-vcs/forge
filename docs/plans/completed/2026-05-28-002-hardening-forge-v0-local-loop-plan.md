---
title: "hardening: Forge v0 local loop"
type: hardening
status: completed
date: 2026-05-28
origin: docs/brainstorms/forge-v0-wedge-requirements.md
previous_plan: docs/plans/completed/2026-05-28-001-feat-forge-v0-local-loop-plan.md
review_artifact: ce-code-review run forge-v0-autofix-001
---

# hardening: Forge v0 local loop

## Summary

Harden the working Forge v0 prototype into a safer local wedge by addressing the high-severity `ce-code-review` residual findings: secret-safe snapshots and evidence, complete JSON behavior for agents, correct request-id idempotency, compare-and-advance operation/view writes, safer restore/export recovery behavior, and proposal-revision binding for checks/decisions/publications.

The previous plan established the Cargo workspace, SQLite metadata, CLI loop, Git-backed snapshots, evidence capture, proposal/check/decision/export commands, doctor, PR-body export, and integration harness. This plan does not expand Forge beyond that wedge. It tightens the existing workflow so the v0 loop is trustworthy enough to keep dogfooding.

---

## Problem Frame

Forge now has the full local loop, but the first review pass found safety issues that undermine the product promise. A tool that helps agents make changes must be more conservative than a shell script around Git: it must avoid leaking secrets, preserve agent-readable contracts on every error path, make retries safe, detect stale or mismatched lifecycle state, and avoid losing user work during restore/export operations.

This plan turns the prototype from "works end to end" into "fails safely enough for v0 dogfooding." The priority is not adding features. The priority is closing correctness, reliability, and security holes in the existing command surface while keeping the vertical integration harness as the proof mechanism.

---

## Requirements

**Security and evidence safety**

- R1. Snapshots and branch exports must exclude secret-bearing files by default, including `.env`, `.env.*`, private keys, credential files, and configured secret-risk paths.
- R2. Evidence excerpts must redact common secret patterns before being returned in JSON or persisted in SQLite.
- R3. Secret-risk evidence must carry sensitivity metadata that downstream PR-body export can omit or redact.
- R4. Evidence capture must remain bounded without buffering unbounded child output in memory.

**Agent JSON contract**

- R5. `--json` must return the Forge response envelope for parser/usage errors, including missing subcommands, missing required arguments, and unknown flags.
- R6. Public error codes must be stable and typed rather than inferred from English error messages.
- R7. Golden/schema-style tests must cover representative success and error envelopes for the public command surface.

**Idempotency and operation/view safety**

- R8. Request-id replay must be command-aware and status-aware.
- R9. Retrying a failed request ID must replay or report the original failure, not return success.
- R10. Reusing a request ID for a different mutating command must return a stable conflict error.
- R11. Mutating commands must compare-and-advance `current_operation -> current_view` so concurrent writers cannot silently fork or overwrite current state.
- R12. SQLite connections must enforce foreign keys, and doctor/tests must detect referential integrity failures.

**Restore, export, and recovery**

- R13. `forge restore` must refuse to overwrite dirty work by default.
- R14. Restore must materialize the selected snapshot exactly, including removing files absent from the target tree where safe.
- R15. Restore and branch export must record enough pending/final state for `forge doctor` to identify interrupted operations.
- R16. Export retries must reconcile an already-created branch when it points at the expected commit, while still refusing silent overwrite.

**Proposal lifecycle binding**

- R17. Evidence, check results, decisions, and publications must bind to the exact proposal revision they validate or publish.
- R18. `forge check` must mark evidence stale or missing when it does not correspond to the current proposal revision.
- R19. `forge accept` must fail safely on stale base before recording an accepted decision.
- R20. `forge export branch` must require an accepted decision for the exact proposal revision being exported.

**Local testability**

- R21. Each hardening unit must extend integration tests under `crates/forge-cli/tests`.
- R22. Tests must assert CLI behavior, JSON shape, filesystem state, `.forge` metadata, and Git refs where relevant.
- R23. The full dogfood loop must pass after hardening, including `forge doctor` on healthy metadata.

---

## Key Technical Decisions

- **Security first:** The P0 finding about sensitive files entering exported trees is the first implementation unit. A local agent workflow cannot be trusted if it snapshots `.env` or key material by default.
- **Typed error boundaries:** Stable JSON cannot rest on substring matching. Introduce a typed error code path at the CLI/store/content/export boundaries, then migrate command handlers incrementally.
- **Idempotency is a store concern:** Request ID semantics depend on persisted operation command/status/result metadata. The CLI may format replay responses, but the store should decide whether a request ID is a replay, failed replay, or conflict.
- **Operation/view CAS is the transaction root:** Every mutating command must advance current state with an expected current operation check. This aligns implementation with the operation/view design in the previous plan.
- **Restore/export need pending state:** Commands that mutate the worktree or Git refs outside SQLite cannot be fully atomic. v0 should record pending intent and finalization state so doctor can flag interrupted work.
- **Proposal revision is the review unit:** Checks, decisions, and exports must not float against "latest proposal" or repository-wide latest decision. The proposal revision is the stable object being reviewed.
- **Keep scope in v0:** No native Forge content backend, hosted features, automatic snapshots, semantic merge, team review, or full policy language in this hardening pass.

---

## Current System Context

The current implementation already includes:

- Cargo workspace crates under `crates/`.
- CLI entrypoint in `crates/forge-cli/src/main.rs`.
- JSON envelope types in `crates/forge-protocol/src/lib.rs`.
- SQLite store and migration in `crates/forge-store/src/lib.rs` and `crates/forge-store/migrations/001_init.sql`.
- Git content backend in `crates/forge-content-git/src/lib.rs`.
- Evidence capture in `crates/forge-evidence/src/lib.rs`.
- Basic policy and export crates in `crates/forge-policy/src/lib.rs` and `crates/forge-export-git/src/lib.rs`.
- Integration tests under `crates/forge-cli/tests`.

The `ce-code-review mode:autofix` pass already applied safe automatic fixes for human-mode restore confirmation, README command standards, and a narrow doctor current-view validation gap. The remaining findings are gated or manual and need planned implementation.

---

## Implementation Units

### U1. Secret-Safe Snapshots, Export, and Evidence

- **Goal:** Prevent Forge from capturing or publishing common secret-bearing files and redact secret-like command output before persistence or JSON emission.
- **Requirements:** R1, R2, R3, R4, R21, R22.
- **Dependencies:** None.
- **Files:**
  - `crates/forge-content/Cargo.toml`
  - `crates/forge-content/src/lib.rs`
  - `crates/forge-content-git/src/lib.rs`
  - `crates/forge-evidence/src/lib.rs`
  - `crates/forge-store/src/lib.rs`
  - `crates/forge-cli/src/main.rs`
  - `crates/forge-cli/tests/forge_start_save.rs`
  - `crates/forge-cli/tests/forge_run_evidence.rs`
  - `crates/forge-cli/tests/forge_pr_body.rs`
- **Approach:** Add a small v0 sensitivity policy shared by content and evidence paths. The policy should deny snapshot/export of `.env`, `.env.*`, private-key-like files, credential files, and configured secret-risk paths by default. Evidence capture should redact common token/password/key patterns before storing excerpts and should mark redacted evidence with a secret-risk sensitivity label. Keep the policy intentionally small and explicit.
- **Execution note:** Test-first. Start with failing integration tests for `.env` exclusion and redacted command output.
- **Patterns to follow:** Keep Git-specific filtering in `forge-content-git`, but define policy concepts outside the Git adapter so later content backends can reuse them.
- **Test scenarios:**
  - Saving a repo with `.env` present does not include `.env` in changed paths, snapshot content, proposal changed paths, or exported branch contents.
  - Saving tracked `.env` fails safely or excludes it by policy; the behavior must be explicit and covered.
  - `forge run -- sh -c 'echo TOKEN=secret'` returns and stores a redacted excerpt, not the raw token.
  - PR-body export omits or redacts secret-risk evidence.
  - Evidence still records command metadata, exit code, truncation flags, trust, and sensitivity.
- **Verification:** `cargo test --workspace`, plus a dogfood save/export check that `.forge` and secret-risk files are not included.

### U2. Complete JSON Contract and Typed Errors

- **Goal:** Ensure every agent-facing failure path, including parser and usage errors, returns the stable Forge JSON envelope when `--json` is supplied.
- **Requirements:** R5, R6, R7, R21, R22.
- **Dependencies:** None.
- **Files:**
  - `crates/forge-protocol/src/lib.rs`
  - `crates/forge-cli/src/main.rs`
  - `crates/forge-store/src/lib.rs`
  - `crates/forge-content-git/src/lib.rs`
  - `crates/forge-export-git/src/lib.rs`
  - `crates/forge-cli/tests/forge_init.rs`
  - `crates/forge-cli/tests/forge_start_save.rs`
  - `crates/forge-cli/tests/common/mod.rs`
- **Approach:** Replace `Cli::parse()` with a parse path that detects `--json` before clap exits. Parser errors in JSON mode should serialize as `ResponseEnvelope` with stable codes such as `USAGE_ERROR`, `UNKNOWN_ARGUMENT`, and `MISSING_ARGUMENT`. Add typed internal error codes for known domain failures and map those directly to `ErrorObject` instead of scanning messages.
- **Execution note:** Characterization-first for current JSON envelopes, then tighten parser and error paths.
- **Patterns to follow:** `forge-protocol` owns public response types; command handlers should return structured errors or typed results rather than English strings.
- **Test scenarios:**
  - `forge --json` with no subcommand returns a JSON envelope.
  - `forge --json export branch` with missing branch name returns a JSON envelope.
  - `forge --json --unknown init` returns a JSON envelope.
  - Known domain errors use stable codes without relying on message text.
  - Human-mode parser errors remain concise and useful.
- **Verification:** JSON contract/golden tests cover representative success and failure envelopes.

### U3. Correct Request-ID Idempotency

- **Goal:** Make mutating command retries safe and predictable by scoping request IDs to command and status.
- **Requirements:** R8, R9, R10, R21, R22.
- **Dependencies:** U2 is preferred but not strictly required.
- **Files:**
  - `crates/forge-store/src/lib.rs`
  - `crates/forge-store/migrations/001_init.sql`
  - `crates/forge-cli/src/main.rs`
  - `crates/forge-cli/tests/forge_start_save.rs`
  - `crates/forge-cli/tests/forge_init.rs`
- **Approach:** Extend operation lookup so request-id replay includes stored command and status. A matching successful operation may replay a stable response shape. A matching failed operation should replay/report the original failure. A different command using the same request ID should return `REQUEST_ID_CONFLICT`. The schema may need a small persisted result/error summary if the existing operation rows are insufficient.
- **Execution note:** Test-first around failed replay and cross-command reuse before changing store semantics.
- **Patterns to follow:** Keep idempotency state in SQLite, not process memory.
- **Test scenarios:**
  - Repeating the same successful `forge start --request-id X` does not create a second attempt or operation.
  - Repeating a failed mutating command with the same request ID returns an error, not success.
  - Reusing request ID `X` for a different command returns `REQUEST_ID_CONFLICT`.
  - Operation row counts and `current_state` are unchanged after replay.
- **Verification:** Integration tests inspect SQLite row counts and JSON envelopes.

### U4. Operation/View Compare-and-Advance and SQLite Integrity

- **Goal:** Make operation/view advancement concurrency-safe and enforce SQLite referential integrity.
- **Requirements:** R11, R12, R21, R22.
- **Dependencies:** U2 for typed conflict errors is preferred.
- **Files:**
  - `crates/forge-store/src/lib.rs`
  - `crates/forge-store/migrations/001_init.sql`
  - `crates/forge-cli/tests/forge_doctor_gc.rs`
  - `crates/forge-cli/tests/forge_start_save.rs`
- **Approach:** Route every store connection through a helper that enables `PRAGMA foreign_keys = ON`. Thread the expected current operation into operation/view writes and update `current_state` with `WHERE singleton = 1 AND current_operation_id = ?`. If no row advances, return a retryable conflict. Add `doctor` checks for `PRAGMA foreign_key_check`, current operation/view mismatch, and dangling references.
- **Execution note:** Test-first where possible; use direct SQLite corruption fixtures for doctor checks.
- **Patterns to follow:** The previous plan's operation/view write shape is the authority: compare-and-advance current operation inside the transaction.
- **Test scenarios:**
  - Foreign key violations are rejected on store connections.
  - `forge doctor` reports foreign-key violations or broken current state.
  - A simulated stale expected operation returns a structured retryable conflict.
  - Concurrent mutating commands do not silently fork current state.
- **Verification:** Integration tests plus a focused store unit test if direct concurrency setup is clearer there.

### U5. Restore and Export Recovery Safety

- **Goal:** Prevent restore/export from losing user work or leaving undetected partial state.
- **Requirements:** R13, R14, R15, R16, R21, R22.
- **Dependencies:** U2 and U4.
- **Files:**
  - `crates/forge-content/src/lib.rs`
  - `crates/forge-content-git/src/lib.rs`
  - `crates/forge-export-git/src/lib.rs`
  - `crates/forge-store/src/lib.rs`
  - `crates/forge-cli/src/main.rs`
  - `crates/forge-cli/tests/forge_start_save.rs`
  - `crates/forge-cli/tests/forge_accept_export.rs`
  - `crates/forge-cli/tests/forge_doctor_gc.rs`
- **Approach:** Add dirty-worktree preflight before restore. Restore should materialize the target tree exactly enough for v0 trust, including removing files absent from the target snapshot unless doing so would delete protected control files. Add pending/final states for restore and branch export so doctor can detect interrupted operations. Export retry should reconcile a branch that already points at the expected commit, while refusing unrelated existing branches.
- **Execution note:** Test-first for dirty restore refusal and exact-tree restore before changing Git plumbing.
- **Patterns to follow:** Keep current-branch safety invariant: export must not switch or mutate the current branch.
- **Test scenarios:**
  - Restore without `--yes` fails in JSON and human modes.
  - Restore refuses when dirty work would be overwritten.
  - Restoring an earlier snapshot removes a file that was created after that snapshot.
  - Interrupted restore/export fixtures are reported by doctor.
  - Retrying export after branch creation but before publication finalization reconciles if the branch points at the expected commit.
  - Export still refuses to overwrite an unrelated existing branch.
- **Verification:** Full integration path exercises restore and branch export without changing the current branch.

### U6. Proposal Revision Binding and Stale Evidence Protection

- **Goal:** Make proposal checks, decisions, and publications refer to the exact proposal revision being validated or exported.
- **Requirements:** R17, R18, R19, R20, R21, R22.
- **Dependencies:** U2, U3, and U4.
- **Files:**
  - `crates/forge-store/src/lib.rs`
  - `crates/forge-policy/src/lib.rs`
  - `crates/forge-cli/src/main.rs`
  - `crates/forge-cli/tests/forge_propose_check.rs`
  - `crates/forge-cli/tests/forge_accept_export.rs`
  - `crates/forge-cli/tests/forge_run_evidence.rs`
- **Approach:** Replace repository-wide "latest" authorization with explicit proposal revision relationships. Evidence should record the snapshot/content context it was captured against where possible. `forge check` should bind check results to the proposal revision and evidence IDs used. `forge accept` should verify the current target base before recording an accepted decision. `forge export branch` should require an accepted decision for the exact proposal revision being exported.
- **Execution note:** Characterization-first for the current happy path, then add stale and mismatch tests.
- **Patterns to follow:** Accept records a decision; export publishes that exact accepted revision.
- **Test scenarios:**
  - Evidence captured before a later save/propose is marked stale or missing for the new proposal revision.
  - Accept fails with stale base before writing an accepted decision.
  - Export fails when the latest proposal is not the same revision that was accepted.
  - Rejected proposal revisions cannot be exported.
  - PR-body summarizes the exact proposal/check/decision/publication relationship.
- **Verification:** Integration tests inspect proposal revision IDs in JSON and SQLite.

### U7. Evidence Execution Bounds

- **Goal:** Prevent hung or noisy child processes from blocking or exhausting Forge.
- **Requirements:** R4, R21, R22.
- **Dependencies:** U1 for redaction integration is preferred.
- **Files:**
  - `crates/forge-evidence/src/lib.rs`
  - `crates/forge-store/src/lib.rs`
  - `crates/forge-cli/src/main.rs`
  - `crates/forge-cli/tests/forge_run_evidence.rs`
- **Approach:** Add a default timeout and a small CLI override such as `forge run --timeout-ms <n> -- <command>`. Capture timed-out commands as persisted evidence with an explicit timeout status. Replace whole-output buffering with capped collection where practical in v0.
- **Execution note:** Test-first with a sleeping command and a noisy command.
- **Patterns to follow:** Do not capture full environment or raw long logs by default.
- **Test scenarios:**
  - A sleeping command times out, is killed, and persists timed-out evidence.
  - JSON output reports timeout without raw long output.
  - A noisy command does not allocate unbounded output before truncation.
- **Verification:** Integration tests complete quickly and deterministically.

### U8. Final Contract and Dogfood Verification

- **Goal:** Prove the hardened v0 loop through automated tests, doctor checks, and local dogfooding.
- **Requirements:** R21, R22, R23.
- **Dependencies:** U1 through U7.
- **Files:**
  - `README.md`
  - `crates/forge-cli/tests/forge_init.rs`
  - `crates/forge-cli/tests/forge_start_save.rs`
  - `crates/forge-cli/tests/forge_run_evidence.rs`
  - `crates/forge-cli/tests/forge_propose_check.rs`
  - `crates/forge-cli/tests/forge_accept_export.rs`
  - `crates/forge-cli/tests/forge_doctor_gc.rs`
  - `crates/forge-cli/tests/forge_pr_body.rs`
- **Approach:** Consolidate JSON contract helpers/goldens, update README to describe safety behavior honestly, run the full integration suite, rerun `ce-code-review`, and dogfood the hardened loop in this repository.
- **Execution note:** Verification-first. This unit should mostly expose gaps left by earlier units.
- **Test scenarios:**
  - Full local loop passes with a non-secret file change.
  - Full local loop refuses or redacts secret-risk input.
  - `forge doctor` passes after a healthy full loop and reports expected corruption fixtures.
  - Exported branch contains exactly the accepted safe proposal contents.
  - PR-body includes useful intent/evidence/check/decision/publication context without secrets.
- **Verification:** `rtk cargo fmt --all --check`, `rtk cargo test --workspace`, `rtk cargo clippy --workspace --all-targets -- -D warnings`, `ce-code-review mode:autofix`, and a final dogfood loop.

---

## Phased Delivery

- **Milestone 1:** U1 and U2 close the largest security and agent-contract gaps.
- **Milestone 2:** U3 and U4 make mutating commands retry-safe and concurrency-aware.
- **Milestone 3:** U5 and U6 make restore/export/proposal lifecycle behavior safe enough for v0 dogfooding.
- **Milestone 4:** U7 and U8 complete execution bounds, verification, review, and documentation.

The milestones are intentionally sequenced by risk. Secret handling and JSON contract reliability come before deeper lifecycle cleanup because unsafe capture and non-agent-readable failures undermine every workflow.

---

## Scope Boundaries

### In Scope

- Hardening the existing CLI-first local workflow.
- Secret-safe default snapshot/export behavior.
- Evidence redaction and bounded capture.
- Typed JSON error behavior.
- Request-id idempotency correctness.
- SQLite integrity and operation/view compare-and-advance.
- Restore/export recovery detection.
- Proposal revision binding.
- Integration tests and dogfood verification.

### Deferred to Follow-Up Work

- Native Forge content backend.
- Native Git protocol or packfile writing.
- Hosted review or remote sync.
- Automatic snapshots.
- Multiple competing attempts UI.
- Team permissions or multi-user workflows.
- Rich policy language.
- Semantic merge.
- Destructive GC execution beyond dry-run.

### Outside v0 Product Identity

- General-purpose Git replacement.
- GitHub clone.
- CI platform.
- Agent orchestration platform.

---

## Risks and Mitigations

- **Secret filtering can create false confidence:** Static deny lists will miss some secrets. Mitigation: start with conservative default exclusions, redaction tests, and visible sensitivity metadata rather than claiming perfect detection.
- **Restore exactness can delete user work:** Exact tree restore must be paired with dirty-worktree refusal and clear confirmation behavior. Mitigation: test dirty refusal before exact materialization.
- **Typed errors can sprawl:** A large error taxonomy would slow the hardening pass. Mitigation: start with public error codes required by current commands and evolve the enum only as tests require.
- **Concurrency tests can become flaky:** Direct simultaneous CLI tests may be nondeterministic. Mitigation: add a store-level stale-operation fixture first, then one narrow CLI-level concurrency smoke test if stable.
- **Review findings are interdependent:** Idempotency, CAS, and proposal binding touch the same store flows. Mitigation: keep unit order explicit and run the full integration suite after every unit.

---

## Verification Checklist

- `rtk cargo fmt --all --check`
- `rtk cargo test --workspace`
- `rtk cargo clippy --workspace --all-targets -- -D warnings`
- `ce-code-review mode:autofix base:origin/main plan:docs/plans/completed/2026-05-28-002-hardening-forge-v0-local-loop-plan.md`
- Dogfood: `forge init -> start -> save -> run -> propose -> check -> accept -> export branch -> export pr-body -> doctor`
- Confirm exported branch does not include `.forge`, `.env`, or secret-risk files.
- Confirm PR body does not include raw secret-like output.

---

## Sources

- Origin requirements: `docs/brainstorms/forge-v0-wedge-requirements.md`.
- Previous implementation plan: `docs/plans/completed/2026-05-28-001-feat-forge-v0-local-loop-plan.md`.
- Review artifact: `ce-code-review` run `forge-v0-autofix-001`.
- Current implementation files under `crates/forge-*`.
