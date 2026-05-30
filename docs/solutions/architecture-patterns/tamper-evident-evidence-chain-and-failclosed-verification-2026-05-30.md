---
title: "Hash-chain an append-only ledger and verify fail-closed: every spine write site, an unforgeable legacy boundary, and gate-vs-doctor depth"
date: 2026-05-30
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: tamper-evident-evidence-and-integrity-verification
severity: high
applies_when:
  - A green result authorizes an autonomous actor, so a tampered input must be detected and refused, not silently trusted
  - You are hash-chaining records into an "append-only spine" and assume one helper is the only writer
  - A schema migration adds a hash/integrity column and you must distinguish a legitimately-unhashed legacy row from a maliciously-deleted hash
  - The hash is computed over text that a redactor mutates before persistence
  - A new typed error/break-kind reaches more than one machine-visible egress (an error payload AND a report)
tags: [hash-chain, tamper-evident, append-only-log, fail-closed, sqlite, in-txn-determining-read, migration-high-water-mark, secret-redaction, additive-error-drift-guard, content-binding, ner-136]
---

# Hash-chain an append-only ledger and verify fail-closed: every spine write site, an unforgeable legacy boundary, and gate-vs-doctor depth

## Context

NER-136 (Forge M2 Phase 5) made evidence tamper-**evident**: each `evidence`/`decision` row gets a SHA-256 `content_hash` folded into the append-only `operations` op-log, and the Phase 4 gate path plus `doctor` verify the hash and refuse a forged row (`EVIDENCE_TAMPERED`). It closes the Phase 4 Honesty Note ("anyone with the DB can edit `exit_code 7→0` and the gate re-evaluates green"). The headline features were the easy part. The parts that bite — and that a 200-test green suite hid until doc-review and code-review surfaced them — are *where the spine is actually written*, *what makes the legacy/tampered boundary forgeable*, *which layer catches which attack*, and *what the hash is computed over*. This captures them so the Phase 9 signing work (and the next "chain a ledger" feature) does not re-derive them. Builds on the M1/M2 substrate ([Related](#related)).

The honest scope is itself a learning: this is tamper-EVIDENT, not tamper-PROOF. An actor with whole-DB write access can recompute the entire chain; cryptographic signing is Phase 9. Detection covers naive, partial, and **interior** tampering — the common cases and the literal Phase 4 hole — not a full rewrite-to-head.

## Guidance

### 1. "Chained into the append-only spine" has every INSERT site, not just the common helper

The natural premise — "`insert_operation_view` is the single chain-write site; every mutating command funnels through it" — is **false**, and believing it bricks honest repos. Two other code paths append `operations` rows directly with their own singleton CAS, bypassing the helper:
- **`record_failed_operation`** — fires on *every deterministic non-transient command failure* (e.g. a `forge accept` with no proposal). A repo that ever recorded one failure has a directly-inserted op.
- **The `init` genesis INSERT** — every fresh repo's first op.

If only the helper computes `content_hash`, those two sites leave a NULL-hash op created *after* the migration high-water mark (§2) → `doctor`/the gate classify it as a deleted hash (tampered) → an honest repo is bricked on its first command failure or fresh init. The fix is to compute the chain link at **all three** sites (genesis folds the documented zero-sentinel parent; the failed op folds a `None` domain digest). The reusable rule: when hash-chaining a *spine table*, `grep "INSERT INTO <table>"` and cover every writer — a "funnel" helper is a convention, not a guarantee. This was the doc-review's load-bearing P0 and was independently re-confirmed by the adversarial code-reviewer.

### 2. A legacy-vs-tampered discriminator must key on a recorded position marker, never a per-row mutable timestamp

A migration that adds a nullable `content_hash` leaves every pre-existing row NULL. You must distinguish a *legitimately-unhashed legacy row* (grandfather it) from a *maliciously-deleted hash* (refuse it). The tempting discriminator — "NULL hash on a row whose `created_at_ms` predates the migration is legacy" — is a **downgrade attack**: `created_at_ms` is attacker-mutable, so backdating it launders a tampered row into the grandfathered branch and passes the gate fail-open. The fix: the migration records a **high-water mark** (`MAX(rowid)` of each table at apply time) into a marker row; `rowid ≤ mark` is legacy, `rowid > mark` is tampered. `rowid` is monotonic and not assigned by ordinary INSERTs.

**State the residual honestly (don't over-claim).** Code-review caught that a `TEXT PRIMARY KEY` table's `rowid` is itself `UPDATE`-able, and the marker row is *outside the hash chain*, so raising the marker (or backdating a rowid) still launders a tampered row. This only **raises the bar** versus the timestamp — it is inside the conceded whole-DB-write (Phase 9 signing) boundary, not a hard barrier. The first comment draft said the rowid "cannot be backdated to launder a tampered row"; the corrected comment says it raises the bar and names the boundary. Hardening the marker (a `doctor` consistency assertion, or folding it into the genesis link) is a deferred follow-up.

### 3. Split verification by layer: a cheap in-txn gate check, and a deep offline re-walk that folds the domain digest

Two attacks, two layers:
- **The naive edit** (the literal Phase 4 hole): edit `exit_code 7→0` and leave the stored `content_hash` stale. Caught by the **gate**, cheaply, in-txn: recompute the deciding evidence row's digest from its columns and compare to the stored hash. Fail-closed → `EVIDENCE_TAMPERED`, raised *before* the enforce-check branch so it refuses **even under `--allow-unverified`** (a policy bypass is never an integrity bypass). Wire it where the production gate actually runs — `evaluate_check_on`, called by **both** `record_check` and `decide` — not a plausible helper (the "verify the production caller graph" lesson).
- **The recompute attack**: edit a field *and* recompute the row's own `content_hash` to match. The cheap per-row check passes. Caught by **`doctor`**'s full op-log re-walk — which **folds each op's domain digest** (the evidence/decision row's `content_hash`, recovered via the op's `view.state_json` `evidence_id`/`decision_id`). The operation chained the *old* digest, so the re-walk mismatches at the first un-rewritten downstream op. Only a full rewrite-to-head evades (conceded).

The load-bearing design choice is **folding the domain digest into the operation's link**. Without it, the op chain (parent + op fields only) would never depend on the evidence digest, and the recompute attack would pass both layers. The gate refuses naive/interior edits; `doctor` refuses the recompute; both are needed and the exit criterion pairs them ("detected by `doctor` AND refused on re-evaluation"). For `export`, there is no in-txn site (the git branch is created before `record_publication`'s txn) — verify the decision row under the held repo lock *before* the branch.

### 4. Hash over the persisted bytes, in-txn, recomputed per retry

Three constraints that each break verification if violated:
- **Over persisted bytes, not raw.** The redactor mutates the excerpt before storage, so the digest must be over the *redacted + truncated* bytes that are actually stored — verification recomputes from the stored columns. Hashing the raw output would mismatch on every redacted row. (Corollary: redact *then* truncate, so a secret straddling the 4096 cap is redacted before its prefix is persisted.)
- **In-txn, per-retry.** Compute the digest inside `record_evidence`'s `with_immediate_retry` `FnMut` closure (after `replay_guard`), recomputed each attempt, with `created_at_ms` captured once and used for both the INSERT and the digest. The parent op hash is read on `&tx` via the **captured `context.current_operation_id`** (the same id the CAS guards), not a live `current_state` join — or a busy-retry could fold a parent the CAS will then reject.
- **Know which "retry" re-runs the body.** `SQLITE_BUSY`/517 re-runs the `FnMut`; a `CurrentStateChanged` CAS loss does **not** (it propagates to the CLI for full command re-execution). A test scenario premised on "CurrentStateChanged re-runs the FnMut" tests nothing.

### 5. A redactor's worst false positive is your own output; exempt it and document the resulting false negative

A hardened high-entropy secret detector will flag the tool's *own* identifiers: Forge emits its git SHAs (40-hex) and its own SHA-256 `content_hash` values (64-hex) into command output, which a 64-char captured excerpt then re-redacts into garbage. So the entropy detector must **exempt** 7/8/40/64-char pure-hex runs and UUID shapes. That exemption is itself a documented **false negative** (a real 40-hex secret slips) — surface it in the machine-readable contract (`forge schema` `notes.secret_protection`), don't bury it. The general rule: an entropy redactor needs structural allowlists for the high-entropy-but-benign shapes its own ecosystem emits, and every allowlist is a leak you must name. (And the promissory "not scanned until Phase 5" note *is* now Phase 5 — update the contract field when the phase that it promised ships; code-review caught it still saying "until Phase 5".)

### 6. A numbered-migration runner that splits on `;` is hostile to comments; a new migration breaks partial test fixtures

Two re-confirmed migration gotchas:
- **Semicolons in comments.** The per-statement runner does `sql.split(';')`. A `;` anywhere — including inside an apostrophe-wrapped `';'` in a `-- comment` — splits mid-comment and the continuation is parsed as SQL ("unrecognized token"). Hit twice during implementation. Comments in a `.sql` applied by a naive splitter must contain no semicolon.
- **A migration that ALTERs tables breaks convergence fixtures that built *partial* schemas.** The migration-reconciliation discipline ("reconstruct the real shipped schema") extends to "include (at least as stubs) every table the *new* migration touches" — a `004` that `ALTER`s `evidence`/`decisions`/`publications` and scans them for the high-water mark fails against fixtures that only built `repositories`/`current_state`/`intents`. Plus the standard head-bump fan-out: `schema_head 3→4` ripples to every hardcoded version literal across the *whole* test tree and the shell e2e eval (grep the literal, advance "at-head" fixtures, bump "head+1 refuses" fixtures).

### 7. Land a new break-kind on every machine-visible surface, and make the two surfaces share a type

The additive-error drift-guard discipline held for `EVIDENCE_TAMPERED`: enum variant, `code()`, `details()` (carrying **only** `{id, kind}` — never an excerpt/command), `retryable()`, `after_ms()`, `Display`, the `error_registry()` `ErrorCodeSpec`, **both** `error.rs` drift-guard tests, and the `FORGE_ERROR_CODES` list in `tests/forge_schema.rs` — all in one change. Extension: the break-kind reaches *two* machine-visible egresses (`EvidenceTampered.details` via `as_str()`, and `DoctorReport.tampered_rows` via serde). Store the `TamperKind` **closed enum** directly in the serialized row (not a `String` built by `as_str().to_string()` at N call sites) with `#[serde(rename_all = "snake_case")]`, and pin a serde-vs-`as_str` parity test — so a new variant is a compile-then-test failure on both surfaces, and they can never disagree on the kind string.

## Why This Matters

A green check authorizes an agent to self-select a winning attempt; the gate must be un-gameable *by the agent itself*. The visible features (a hash column, a verify call) are testable and shippable. The holes that bite are invisible to a green suite: a chain that bricks on the first failed command because a second writer was missed (§1), a legacy/tampered boundary that fails open to a backdated timestamp (§2), a verification that catches the naive attack but not the recompute because the op didn't fold the domain digest (§3), a hash over the wrong bytes (§4), a redactor that mangles the tool's own hashes or leaks a 40-hex secret (§5). Five of these were caught by doc-review or code-review reasoning, not by the 200+-test suite — which is exactly why they are worth writing down before the Phase 9 signing work builds on the chain.

## When to Apply

- Any append-only ledger you hash-chain — enumerate *every* INSERT site of the spine table, not just the common helper.
- Any migration adding an integrity/hash column to existing rows — anchor the legacy/tampered boundary to a recorded position marker, not a mutable per-row field, and name the residual.
- Any verification that must fail closed and gate an irreversible/trust-bearing action — put the cheap determining check in the writer's transaction; reserve the deep re-walk for an offline `doctor`; never let a policy bypass become an integrity bypass.
- Any hash computed over text a redactor rewrites — hash the persisted bytes, redact before truncate.
- Any entropy-based secret redactor — exempt the high-entropy shapes your own ecosystem emits, and surface the resulting false negative in the contract.
- Any new typed error/break-kind reaching more than one machine-visible surface — land it on every surface at once and make the surfaces share a typed enum with a parity test.

## Scope boundaries (deferred)

Tamper-EVIDENT, not tamper-PROOF: a full chain rewrite by a whole-DB-write actor evades detection; cryptographic signing, key management, and the *enforced* trust ladder are Phase 9. The `integrity_marker` row is outside the hash chain (raising it, or backdating a TEXT-PK rowid, launders within the conceded boundary) — hardening it (a `doctor` consistency assertion or folding it into the genesis link) is deferred (NER-136 D1). `accept` relies on `doctor` for the recompute attack rather than walking the deciding evidence's op-link itself (D2); `export` verifies the decision row but not the deciding evidence (D3). Structured gates trust the declared command's honesty (same boundary as `exit_code` — a PATH-shimmed/`--no-run` cargo can spoof a `0 failures` summary). Residual test coverage (FORGE_ACTOR env, redaction warnings assertion, multi-process tamper, clippy degrade-to-None) is D4. See the NER-136 code-review triage.

## Related

- Plan: `docs/plans/completed/2026-05-30-010-feat-phase-5-tamper-evident-evidence-plan.md`
- Code-review triage: `docs/code-reviews/2026-05-30-ner-136-phase-5.md` (the three-write-sites, the rowid-marker, and the export-before-branch issues were doc-review findings; the rowid-mutability residual, the stale `forge schema` note, and the migration over-claim were code-review findings — none were pre-merge test failures). Deferred follow-ups D1–D6 are logged on Linear NER-136.
- Closes the honesty note in: `docs/solutions/architecture-patterns/content-bound-gate-engine-and-failclosed-enforcement-2026-05-29.md` (Phase 4 — "NOT tamper-proof (DB edits re-evaluate green) — Phase 5 hash-chaining"; the §1 verdict rule, §3 fail-closed, §4 per-token/every-egress redaction, and §5 in-txn determining read all carry forward).
- Substrate this builds on: `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` (the `FnMut` recompute-per-attempt, in-txn determining read, and `rowid` tiebreak §4 reuses), `docs/solutions/architecture-patterns/crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md` ("a lock only helps when both racers take it" — why §3's verify is in-txn, not behind the lock the lock-free `run` won't take; `abort()` skips `Drop`, so reclamation lives in `doctor`), `docs/solutions/architecture-patterns/schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md` (the numbered-migration, typed-error-via-downcast, drift-guard, and "real test graph, not a plausible green" lessons §6/§7 extend), `docs/solutions/architecture-patterns/write-binding-verification-and-content-backend-isolation-2026-05-29.md` (the additive-error-on-every-surface discipline §7 extends).
- Implementation: `crates/forge-store/src/integrity.rs` (length-prefixed domain-separated digests), `crates/forge-store/src/lib.rs` (`insert_operation_view_chained`, `op_content_hash`, `record_failed_operation`, the `init` genesis link, `evaluate_check_on` + `verify_evidence_integrity`, `verify_decision_integrity`, `doctor`/`verify_integrity_chain`/`op_domain_digest`, the `*_high_water` markers), `crates/forge-store/src/error.rs` (`EvidenceTampered`, `TamperKind`), `crates/forge-store/migrations/004_integrity_and_actor.sql` (the `integrity_marker`), `crates/forge-content/src/lib.rs` (`redact_evidence_excerpt` + entropy/JSON/PEM/credential-URL detectors), `crates/forge-evidence/src/parsers.rs` (structured parsers), `crates/forge-policy/src/lib.rs` (the structured gate).
- Eval & tests: `scripts/e2e-eval.sh` (the tamper block), `crates/forge-cli/tests/forge_tamper.rs` (mutate→detect→refuse end-to-end).
