---
title: "Build a content-bound multi-gate check engine: fail closed, redact every egress, and sweep every version pin"
date: 2026-05-29
category: architecture-patterns
module: forge-policy
problem_type: architecture_pattern
component: check-engine-and-enforcement
severity: high
applies_when:
  - A green "check" authorizes an autonomous actor to self-select, so the gate must not be defeatable by the actor itself
  - An enforcement decision is read from persisted config that could be NULL, corrupt, or written by a newer binary
  - A gate verdict is re-evaluated at a second command (accept) that races a deliberately lock-free writer
  - Argv / command identities are surfaced on more than one machine-visible egress (error details AND a success envelope)
  - A numbered-migration head bump changes a value that is hard-coded as a literal across unit tests, integration tests, and shell evals
tags: [check-engine, policy, content-binding, fail-closed, secret-redaction, toctou, sqlite-migration, version-pin, emit-only, ner-135]
---

# Build a content-bound multi-gate check engine: fail closed, redact every egress, and sweep every version pin

## Context

NER-135 (Forge M2 Phase 4) replaced a single `exit==0`-on-latest-evidence check policy with a declarative, content-bound, multi-gate engine: per-intent gates (`forge start --require "cargo test"`), aggregation over the proposed snapshot's full evidence set, and an `accept` gate that requires a passing check by default. The headline footguns it closes — `run -- true` satisfying any intent, `run -- echo ok` flipping a failing gate green — are the easy part. The non-obvious parts are how an *enforcement* gate fails, where its identities leak, and how a head-version bump fans out across a test suite. The doc-review and code-review gates each caught a hole a green test suite hid. This captures them so the Phase 5/6 work (tamper-evidence, compare/rank) does not re-derive them. Builds on the M1/M2 substrate ([Related](#related)).

## Guidance

### 1. The verdict rule that closes the footgun: latest-matching-evidence-per-gate-on-the-proposed-snapshot

A gate is a `(program, args)` identity. Its verdict is decided by the **latest** evidence row whose identity matches *and* whose `snapshot_id` is the proposed snapshot (ordered `created_at_ms DESC, rowid DESC`). This one rule does all the work:
- `run -- true` cannot satisfy a `cargo test` gate — different identity, so the gate is `missing`.
- `echo ok` after a failing `cargo test` cannot flip the gate — `echo ok` is a different identity; the latest *matching* `cargo test` row is still the failure.
- A legitimate same-tree re-run supersedes (latest matching wins).
- Off-snapshot-only matching evidence is `stale` (it ran, but not on this tree); no matching evidence is `missing`. Four verdicts, one rule.

Keep the engine a **pure function** with zero IO (`evaluate(spec, proposed_snapshot_id, &[EvidenceFact]) -> CheckOutcome`) so every verdict/precedence permutation is a fast unit test, and the store is its only caller.

### 2. Default mode (no declared gates) is an intentional behavior *change*, not back-compat — say so

With no `--require` gates, the engine synthesizes one gate per distinct identity observed on the proposed snapshot. This is **not** the old single-latest-row behavior — it closes the `echo ok` footgun for *undeclared* intents too. It happens to reproduce the three prior outcomes (passed/missing/stale, including the exact `"does not match proposal revision"` reason substring an existing test pins), which makes it *look* like back-compat. A scope-guardian reviewer correctly flagged the "fall back to single-latest-evidence for NULL spec" simplification — but that would **re-open the footgun for the common (undeclared) path**, violating an exit criterion. Resolution: keep the aggregate default, and document it as a deliberate behavior change (a dedicated requirement), not "back-compat." When a "simplification" would re-open the very hole the phase exists to close, the simplification is wrong; name the behavior change instead of hiding it.

### 3. An enforcement gate read from persisted config must fail CLOSED on corruption — and the schema-version gate does not save you

The spec is persisted as `intents.check_spec_json`. The first implementation read it as `serde_json::from_str(&json).ok().unwrap_or_default()` — which silently collapses a **non-NULL but unparseable** value to an empty spec, i.e. permissive default mode, where a lone `run -- true` passes. That is the one place the whole enforcement feature can be silently defeated by data drift, and it fails **open**. Distinguish the two cases: `NULL` is a legitimately un-gated intent (default mode); a present-but-unparseable value is corruption (manual edit, partial write, or a richer spec shape written by a newer binary) and must **error**. Crucially, the migration/`schema_version` gate does **not** catch this — the migration only adds a *column*, not a value-*shape* version, so a forward-version spec passes the version check and then fails to deserialize. Any enforcement decision derived from stored bytes needs an explicit fail-closed branch; `.ok().unwrap_or_default()` on a security-relevant read is a fail-open bug.

### 4. Redact secret-like identities PER-TOKEN, on EVERY egress — the success path is the one the threat model forgets

Gate command identities are argv that an agent might (mis)declare with a secret (`--require "deploy --token=ghp_x"`), and unlike captured evidence they are persisted and surfaced *without execution*. Two non-obvious traps:
- **`redact_secret_like_text` keys on the first `=`/`:` of its input.** A *joined* identity (`cargo test FOO=bar --token=ghp_x`) only ever has its first token inspected as the key — `FOO=bar` is not secret-like, so the line is kept verbatim and the real secret leaks. Redact **per argv token** (each arg independently), not on the space-joined string, so a secret in any position is caught.
- **The error path is not the only egress.** It is natural to redact the `CHECK_NOT_PASSED.unmet` *error* and stop. But the per-gate verdicts are also emitted on the **success** envelope (`check --json data.gates[]`), and a `missing` declared gate copies the spec's argv verbatim into that surface. Redact the success egress too. The discipline: enumerate *every* surface a sensitive identity reaches (error details, success data, at-rest DB, logs) and apply the same redactor at each; the threat model that only lists the error surface is incomplete.

(At-rest: the raw spec must stay un-redacted in `check_spec_json` because the gate's identity has to *match* the raw evidence identity; the `.forge/forge.db` snapshot/export exclusion is what protects it at rest. Redaction is an egress-only transform.)

### 5. A re-evaluated gate at a second command must run IN the same transaction, not on its own connection

`accept` re-evaluates the check (rather than trusting a stored `check_results` row) so the decision binds to current evidence. The tempting implementation is a read-only `evaluate_proposal_check(cwd, …)` that opens its own connection as a CLI-layer pre-flight before `decide`. That **reopens the NER-132 U2 TOCTOU**: `forge run` is deliberately lock-free, so between the own-connection read and the `decide` write, a concurrent `run` can change the evidence the decision is supposed to be bound to. The fix is to evaluate the gate **inside `decide`'s `IMMEDIATE` transaction**, reading facts on the same `&tx` (a `&Connection`-taking helper shared with `record_check` via deref coercion). The reusable rule from Phase 1b holds here verbatim: when a determining read feeds a write and the racing party is lock-free, push the read into the writer's transaction — a coarse lock or a separate-connection pre-flight cannot close it. Doc-review caught this *before* implementation by reasoning about the lock-free writer; it would not have shown up as a failing test.

### 6. A migration head bump fans out to every hard-coded version literal — grep the real test graph, do not trust the plan's list

Adding migration `003` makes `schema_head()` become 3. That single change broke pins the plan's feasibility pass only partially enumerated: inline `#[cfg(test)]` tests in `migrations.rs` (`schema_head_is_max_version`, `fresh_apply_reaches_head_with_checksums`), integration assertions in `tests/migrate.rs`, **CLI** integration tests in a *separate* file the plan never named (`forge_migration_upgrade.rs`, `forge_init.rs`, `forge_concurrency.rs`), and the shell e2e eval. Two classes are easy to miss:
- **"At-head" tests** that assert the DB sits at the *old* head — after the bump they are now "behind" and the runner will upgrade them; the fixture must be advanced to the new head (apply `003` + stamp version 3) to stay "at head."
- **"DB-ahead-of-binary refuses" fixtures** that stamp `head+1` as a literal (`3`) — once 3 is real, they must stamp `4`, or they stop testing the refusal (and assert wrong numbers). Some self-adjust via `schema_head()+1`; the literal ones do not.

A version-2 literal that still passes after the bump (e.g. counting `version = 2` rows when there is exactly one) has *lost its meaning* without failing — update it to the new head so it keeps testing what it was written to test. Find them by grepping the whole test tree for the old literal, not by trusting a partial list. This is the Phase-2 "real test graph, not a plausible green" lesson applied to a head bump.

### 7. Emit speculative downstream data; do not persist it until a consumer exists

Per-gate verdicts are emitted in the `check` response but **not** written to a DB column, even though Phase 6 (compare/rank) will want them. Persisting now would add a column whose JSON shape Phase 6 must then stay compatible with — a forward-compat contract for an unbuilt consumer, and an orphaned column if Phase 6 is restructured. Phase 6 adds the column when it consumes it (the same cheap additive-ALTER pattern). Emit-now / persist-when-consumed keeps the schema honest.

## Why This Matters

A green check is what authorizes an agent to self-select a winning attempt; the gate must be un-gameable *by the agent itself*. The footgun closures are visible and testable. The holes that a green suite hides are the ones that bite: an enforcement read that fails open on corruption (§3), a redactor that silently passes a secret because of where the `=` falls (§4), a re-evaluation that reopens a closed race because it used a fresh connection (§5), and a head bump that leaves a test green-but-meaningless (§6). Three of these were caught only by an adversarial/security reviewer or by reasoning about the lock-free writer — not by the 170-test suite — which is exactly why they are worth writing down.

## When to Apply

- Any gate/policy whose green result grants an irreversible or trust-bearing capability — verify it cannot be defeated by the actor it gates, and that it fails closed on missing/corrupt config.
- Any value read from persisted bytes to make an enforcement decision — branch explicitly on absent (legitimate default) vs present-but-unreadable (error); never `.ok().unwrap_or_default()`.
- Any sensitive string (argv, identity, path) that reaches more than one machine-visible egress — redact per-token at *each* surface, success paths included.
- Any determining read that feeds a write while a lock-free writer races — push it into the writer's transaction.
- Any numbered-migration head bump — grep the entire test tree for the old head literal; advance at-head fixtures and bump head+1 refusal fixtures.

## Scope boundaries (deferred)

NOT tamper-proof (DB edits re-evaluate green) — Phase 5 hash-chaining (NER-136). The gate binds `(program, args, snapshot_id, exit_code)` only — not environment, cwd, or executable contents; a same-name hollow command or an environment-flip can still pass — Phase 5+. Default mode stays trivially passable by a lone `run -- true` (acknowledged); a repo-default-required-gate is future work. Persisted per-gate history, a typed `OverallVerdict` (vs the `status: String`), a `DecisionKind` enum (vs `decision: &str` + `enforce_check: bool`), `AcceptArgs`/`ProposalScopedArgs` flatten, gate read-back in `show`, and tokenizer/`representative_evidence_id` unit tests are deferred — see the NER-135 code-review triage.

## Related

- Plan: `docs/plans/completed/2026-05-29-009-feat-phase-4-declarative-check-engine-plan.md`
- Code-review triage: `docs/code-reviews/2026-05-29-ner-135-phase-4.md` (the fail-open §3 and the redaction §4 holes were P1 code-review findings; the in-txn §5 decision and the §2 behavior-change framing were doc-review findings — none were pre-merge test failures)
- Requirements: `docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md` (the wedge the gate hardens)
- Substrate this builds on: `docs/solutions/architecture-patterns/crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md` (§2 in-txn determining read — the rule §5 reuses), `docs/solutions/architecture-patterns/schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md` (the migration + "real test graph not a plausible green" lesson §6 extends; the additive-error drift-guard the new `CHECK_NOT_PASSED` followed), `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` (the `rowid` "latest" tiebreak §1 reuses), `docs/solutions/architecture-patterns/write-binding-verification-and-content-backend-isolation-2026-05-29.md` (additive-error-on-every-surface §6 of that doc)
- Implementation: `crates/forge-policy/src/lib.rs` (`evaluate`, `verdict_for`, default mode), `crates/forge-store/src/lib.rs` (`record_check`, `decide` in-txn gate, `evaluate_check_on`, `intent_check_spec` fail-closed, `redact_gate_result`, `check_spec_json_from_requires`), `crates/forge-store/src/error.rs` (`CheckNotPassed`, per-token `unmet` redaction), `crates/forge-store/migrations/003_check_spec.sql`
- Eval: `scripts/e2e-eval.sh` (the `DECLARATIVE CHECK GATES (NER-135)` block drives the shipped binary)
