---
title: "Evolve a SQLite schema and an agent error contract without bricking old DBs or shipping dead fixes"
date: 2026-05-29
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: migrations-and-error-contract
severity: high
applies_when:
  - A numbered-migration runner replaces an imperative apply-migrations that edited an already-released migration in place
  - A CLI's error codes move from substring-matched message text to a typed error enum
  - A deferred or known-issue fix is wired to a code path and you need to confirm it actually reaches the production caller
  - Migration/upgrade convergence is "proven" by a test that constructs an idealized old-DB shape
tags: [sqlite, migrations, schema-evolution, idempotency, advisory-lock, typed-errors, anyhow, downcast, dead-code, test-false-confidence, ner-133]
---

# Evolve a SQLite schema and an agent error contract without bricking old DBs or shipping dead fixes

## Context

NER-133 (Forge M1 Phase 2) replaced an imperative `apply_migrations` with a numbered `.sql` runner and replaced a substring-matched `error_code()` with a typed `ForgeError` taxonomy. Both are routine-sounding refactors that hid sharp edges only a multi-persona code review (and reconstructing real historical DB states) surfaced. This captures the non-obvious learnings so the next schema bump or contract migration does not re-derive them. Builds on the Phase 1a/1b substrate — see [Related](#related).

## Guidance

### 1. Editing an already-released migration breaks a naive numbered runner — reconcile per-statement, tolerate already-present state

The migration unification reverted `001_init.sql` to a true baseline and added `002` to `ALTER`-in two columns, so fresh-init and upgrade would converge. But `001` had been **edited in place earlier in the project's life**: an older shipped binary (`cd1bb3b`) carried `content_backend` *inline* in `001` and stamped only `schema_migrations` version 1 (version 2 did not exist yet). A DB created by that binary sits at `MAX(version)=1` **with the column already present**. A runner that applies `002` as one `execute_batch` then hits `ALTER TABLE … ADD COLUMN content_backend` → `duplicate column name`, aborts the whole batch (so `002`'s *second*, still-needed `ALTER` never runs), and — because `migrate()` runs on every command — bricks the repo on every invocation.

The fix is to make the runner reconcile rather than assume a clean slate:

- **Apply each migration statement-by-statement, not as one batch**, so an already-satisfied statement doesn't abort the rest.
- **Tolerate `duplicate column name`** on additive `ADD COLUMN`s — an already-present column means the target state is reached; skip and continue.
- **Wrap any *other* DDL failure as a typed `MigrationFailed`** so a genuine defect is diagnosable instead of collapsing to a generic `COMMAND_FAILED`.
- **Grandfather NULL checksums.** Per-migration `sha256` checksums detect tampering, but rows stamped before the checksum column existed carry NULL — verification must skip them, or every pre-existing DB fails on the first upgrade.

```rust
// per pending migration, inside one IMMEDIATE txn:
for statement in sql.split(';') {               // naive split is safe only because our DDL has no ';' in literals
    if let Err(e) = tx.execute_batch(statement.trim()) {
        let msg = e.to_string();
        if msg.contains("duplicate column name") { continue; }   // already-satisfied additive ALTER
        return Err(ForgeError::MigrationFailed { version, message: msg }.into());
    }
}
```

### 2. A convergence test that builds an *idealized* old-DB shape passes green while the real upgrade bricks

The exit criterion was "a schema-diff test proves fresh-init and upgraded-via-ALTER converge." The first implementation's test built its "v1" DB from the *reverted* baseline (neither column inline) — a shape **no shipped binary ever produced**. The test passed by construction while the only real-world v1 shape (`content_backend` inline, from `cd1bb3b`) bricked. **A migration test must reconstruct the actual historical schema that shipped, found from git history (`git show <old-sha>:<migration-file>`), not the schema your current baseline implies.** Add the real shape as an explicit genesis case.

### 3. Run migrations at the command boundary, before the per-command lock — transient self-acquire, never nested

Migrations must run under the repo advisory write lock (Phase 1b), but the lock rule is "acquire exactly once per command, never nested." The seam: `migrate(cwd)` does a cheap version read first — at head it returns taking **no** lock (the common path, including read-only commands); ahead-of-head it refuses read-only **without** writing; only when *behind* does it transiently acquire the lock, apply, and release. It is invoked once at the top of the command funnel **before** the per-command lock, so the two never nest. Crucially, the unknown-future-version refusal must short-circuit **before** any failure-recording write, or the binary writes into a schema it is explicitly refusing.

### 4. Substring error codes → typed enum: carry it in `anyhow` + recover by `downcast`, don't change writer signatures

Replacing a substring `error_code(message)` ladder with a typed `ForgeError` does **not** require threading a `Result<DomainError>` through every writer. Generalize the existing sentinel pattern: construct the variant at the failure site, return it inside `anyhow::Error`, recover it at the CLI boundary via `downcast_ref`, and map to `(code, details, retry)` there. Zero writer signatures change. Preserve every existing code string byte-for-byte (a parity test pins them) and confirm the codes that were raised at the **CLI layer** (not just in the store) are converted too — otherwise deleting the ladder silently regresses them to `COMMAND_FAILED`.

### 5. The sharpest trap: a deferred fix wired to *dead code* while the production path stays unfixed

The taxonomy was meant to resolve a known issue — a transient `current_state` CAS conflict poisoning a `--request-id`. The implementation typed the CAS in `create_operation_view` as a retryable `CurrentStateChanged`… but `create_operation_view` had **zero production callers**. Every real mutating command went through `insert_operation_view`, whose CAS guard still raised a plain `anyhow!("current operation changed")` → recorded → still poisoned the id. A unit test on the dead function passed, so the fix *looked* done. **When closing a deferred/known issue, verify the fix lands on the path the production callers actually take — grep the caller graph (`grep -n "fn foo\|foo(" `), don't trust that a typed variant exists and a test is green.** A passing test on an unreached function is worse than no fix: it advertises closure.

### 6. Classify retryability for the whole error set at once, and don't record transient failures under the request-id

When you finally make `retry.retryable` meaningful, classify *every* code in one pass rather than flipping one in isolation (which fragments the taxonomy). Transient errors (lock timeout, the CAS conflict) must be classified retryable **and not persisted** under the `--request-id`, so a retry re-executes instead of replaying a sticky failure. Note the side effect honestly: re-executing a command that has non-DB effects (e.g. a child-process runner) re-runs those effects on retry — document it in the published contract rather than discovering it later.

## Why This Matters

Migrations and the error contract are the two surfaces where "it works on a fresh DB / it compiles and a test passes" most easily masks a defect that only bites a real user: an old DB created months ago by a since-edited migration, or an agent depending on a retry hint that the production code path never sets. Three of these (the brick, the false-confidence test, the dead-code fix) were invisible to 147 passing tests and were caught only by reconstructing real historical state and grepping the caller graph during code review. The cost of missing them is a bricked repo or a silently-unresolved known issue shipped as "done."

## When to Apply

- Any numbered-migration system layered onto a project that previously edited migrations in place (almost every project that adopted migrations late).
- Any migration "convergence" or "upgrade" test — sanity-check that its old-DB fixture matches a schema a real binary actually wrote.
- Any move from string/substring error signaling to a typed taxonomy in a CLI/daemon with a machine-readable contract.
- Any time you implement a *deferred* or *known-issue* fix: confirm it reaches the production caller, not just a plausible-looking function.

## Scope boundaries (deferred)

Folding the `LockTimeout` sentinel into `ForgeError` as a variant, and replacing the `command_result` 3-tuple return with a named struct, were deferred as maintainability cleanups (drift risk is already closed by a shared backoff constant + a registry drift-guard test). Re-opening the migrate connection after acquiring the lock and making `bootstrap_schema_migrations` DDL atomic were deferred as benign/self-healing hardening. See the NER-133 Linear comments.

## Related

- Plan: `docs/plans/completed/2026-05-29-007-feat-phase-2-migrations-typed-errors-plan.md`
- Code-review triage: `docs/code-reviews/2026-05-29-ner-133-phase-2.md` (the brick and the dead-code fix were review findings, not pre-merge test failures)
- Substrate this builds on: `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` (the `RequestIdReplay` sentinel pattern `ForgeError` generalizes; the "transient vs deterministic" hook), `docs/solutions/architecture-patterns/crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md` (acquire-once-never-nested; the `INSERT OR IGNORE` shim the runner absorbed)
- Implementation: `crates/forge-store/src/migrations.rs` (`apply_pending_migrations`, `migrate`), `crates/forge-store/src/error.rs` (`ForgeError`), `crates/forge-store/src/lib.rs` (`insert_operation_view` CAS guard), `crates/forge-cli/src/main.rs` (`error_to_object`, `command_result`)
- Eval: `scripts/e2e-eval.sh` (drives the shipped binary; wired into CI)
