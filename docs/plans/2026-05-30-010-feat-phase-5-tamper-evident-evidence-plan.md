---
title: "feat: Phase 5 — Tamper-evident, structured evidence + actor/identity model"
type: feat
status: active
date: 2026-05-30
deepened: 2026-05-30
origin: docs/ROADMAP.md  # Phase 5 section; ticket NER-136. No dedicated *-requirements.md — the ROADMAP Phase 5 entry + the competing-attempts brainstorm (the wedge) are the source.
---

# feat: Phase 5 — Tamper-evident, structured evidence + actor/identity model

> **Doc-review (2026-05-30):** integrated coherence/feasibility/scope/security/adversarial findings. Load-bearing corrections folded in: (1) the op-log chain has **three** write sites, not one (`insert_operation_view`, `record_failed_operation`, the `init` genesis INSERT); (2) the legacy-vs-tampered discriminator is anchored to a recorded **op-log/rowid high-water mark**, not the attacker-mutable `created_at_ms`; (3) the gate/doctor guarantee is honestly scoped — a full chain rewrite up to the head is the conceded tamper-evident-not-proof boundary; (4) the export decision-integrity check is a verifying read under the repo lock **before** the git branch is created (no in-txn site exists on that path); (5) the digest covers every mutable behavioral flag.

## Summary

Make Forge's evidence **trustworthy, tamper-evident, and provenance-attributable** by hash-chaining every domain write into the append-only operations spine, parsing tool output into machine-readable outcomes that an additive check gate can evaluate, attaching an actor to attempts/decisions/publications, and hardening the redactor. The load-bearing piece: the Phase 4 gate path and `doctor` **verify the hash before trusting a row**, so editing `exit_code 7→0` (the documented Phase 4 honesty-note hole) is detected and the gate **refuses** rather than silently re-evaluating green. Everything is additive to the M1/M2 substrate and stays inside the established WAL + `BEGIN IMMEDIATE` + advisory-lock + typed-error invariants. This is tamper-**evident** (detects naive, partial, and interior edits), not tamper-**proof** — a full chain rewrite by an actor with whole-DB write access, cryptographic signing, key management, and the enforced trust ladder are explicitly Phase 9.

---

## Problem Frame

NER-135 (Phase 4) shipped a declarative, content-bound, multi-gate check engine, but its own Honesty Note states the gate is **not tamper-proof**: "anyone with the DB can edit `exit_code 7→0` and the gate re-evaluates green." Evidence rows are plain, mutable SQLite with a hardcoded `trust='locally_observed'` string that asserts nothing verifiable; the gate reasons over a 4096-byte excerpt rather than structured outcomes; there is no actor column anywhere, so "an agent proposed, a human accepted" is unrepresentable; and the redactor only catches line-oriented `key=value` secrets. A green check is what authorizes an agent to self-select a winning attempt (the v0 wedge, see `docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md`) — so the gate must be un-gameable *by the agent itself*, and that requires the integrity model this phase builds. (Phase 6's compare/rank, NER-137, will consume this evidence — but no Phase-6 output-format work is in scope here.)

---

## Requirements

- **R1.** Every evidence row carries a verifiable content hash (SHA-256 over the row's identity- and outcome-bearing fields: command, args, cwd, exit_code, started/ended timing, `timed_out`, stdout/stderr excerpts, the two truncation flags, `sensitivity`, snapshot_id, actor, structured_json, created_at_ms). The hardcoded `trust='locally_observed'` literal is replaced with a self-describing claim `(trust_level, hash_alg, content_hash)`, emitted only when a hash was actually computed.
- **R2.** The per-row content hash is chained into the append-only operations spine: each `operations` row gains a `content_hash` folding its parent operation's hash plus the domain row's digest. It is computed **inside the writer's `BEGIN IMMEDIATE` transaction** at **every** op-append site (`insert_operation_view`, `record_failed_operation`, and the `init` genesis INSERT) and recomputed per busy-retry attempt — preserving the CAS / idempotent-replay / WAL concurrency model (no fork, no double-insert, no NULL-hash op on the spine).
- **R3.** Editing any evidence **or decision** row is detected by `doctor`: a chain re-walk classifies the break (content-edit vs broken-link/deletion/reorder vs missing-hash) and names the first offending row; the walk covers evidence and decision domain digests, not just `operations`.
- **R4.** Editing an evidence row that decides a gate causes check re-evaluation to **refuse** for the detectable cases (naive edit, interior-row edit, partial rewrite) — fail-closed, typed `EVIDENCE_TAMPERED`, not silently pass — at both `check` and `accept`. `accept --allow-unverified` bypasses policy verdicts (failed/missing/stale) but **never** bypasses tamper. `export branch` verifies the decision row's integrity before trusting `status=accepted` **and** before the git branch is created.
- **R5.** Tool-specific structured parsers (cargo test + clippy first) extract machine-readable **numeric** outcomes from the **full** captured output alongside the bounded 4096-byte excerpt; when no parser matches or parsing fails, degrade gracefully to the raw excerpt.
- **R6.** An additive structured gate ("0 failing tests") passes/fails on **parsed counts**, composed conjunctively with the existing `exit_code` gates in the Phase 4 engine: the gate is green **iff `exit_code == 0` AND `parsed_failures == 0`** (strict zero in v0 — no configurable threshold).
- **R7.** An actor/author/decider identity (a representable, auditable attribution string — **not** auth) is recorded on attempts, decisions, publications, and evidence, and is included in the tamper-evident digest so attribution is itself tamper-evident.
- **R8.** The hardened redactor detects bare high-entropy tokens, JSON-embedded secrets, PEM private-key bodies, and `user:pass@host` credential URLs before persistence; each redaction surfaces as a `warnings[]` entry; false positives are bounded (UUID / git-SHA / SHA-256-hex shapes exempted), and the residual false-negatives this creates (a bare 40-hex token shaped like a SHA-1) are documented in `notes.secret_protection`.
- **R9.** The new `EVIDENCE_TAMPERED` code lands on **every** contract surface in one change (enum, `code()`, `details()`, `retryable()`, `after_ms()`, `Display`, `error_registry()` `ErrorCodeSpec`, both `error.rs` drift-guard tests, and `FORGE_ERROR_CODES` in `crates/forge-cli/tests/forge_schema.rs`); `details()` carries only opaque ids + a closed-enum break-kind (never an excerpt/command); envelope `schema_version` stays `forge.cli.v0`.
- **R10.** Migration `004`'s head bump (`schema_head()` → 4) is reconciled across the **entire** test tree and `scripts/e2e-eval.sh` (every head-`3` literal and every `head+1`/future-version stamp); the eval is extended with a tamper-detection check (mutate a row → `doctor` flags it AND `check`/`accept` refuses).
- **R11 (carry-over invariants — additive only).** Preserve WAL + `BEGIN IMMEDIATE` + busy/517 retry, advisory-lock acquire-once-never-nested + the `run` lock carve-out, store-before-DB ordering, the in-txn determining read, the additive-error drift-guard discipline, and the security defaults (4096-byte excerpt cap; `forge_content::is_ignored_by_policy` shared by both backends).
- **R12 (scope).** Tamper-**evident**, not tamper-proof: a full chain rewrite (whole-DB write access), cryptographic signing, key management, and the *enforced* trust ladder are Phase 9, not here.

**Origin actors (from the wedge brainstorm):** A1 Human developer (the "human accepted" in the actor model), A2 Local coding agent (the "agent proposed"; self-selects on a *trustworthy* green), A3 Forge CLI.
**Origin flows:** the lifecycle `init → start → save → run → propose → check → accept → export`; Phase 5 hardens `run` (capture/hash), `check`/`accept` (integrity gate), `export` (decision integrity), and `doctor` (chain verification).

---

## Scope Boundaries

- **Not tamper-proof by signing.** No cryptographic signatures, no key management, no identity/key binding. An actor with full DB write access can recompute the entire chain (including the migration high-water marker) and rewrite the chain head; this phase detects naive/partial/interior edits only. (Phase 9.)
- **No enforced trust ladder.** The trust claim becomes verifiable, but no policy *requires* a minimum tier to accept/publish. (Phase 9.)
- **No universal output parser.** Parsers are tool-specific; v0 ships `cargo test` (libtest summary) and `cargo clippy` only. Unknown tools degrade to the raw excerpt.
- **Structured gates trust the declared command's honesty — the same trust boundary as `exit_code`.** Parser selection is by `(program, args)` identity, which does **not** authenticate that the named tool actually ran; a PATH-shimmed `cargo` or `cargo test --no-run` can emit a `0 failures`-looking summary. Defending against a hostile command wrapper is out of scope (it is the same surface as the agent lying about `exit_code`).
- **Actor is attribution, not authentication.** No permissions, no multi-user coordination, no identity verification. The actor string is caller-declared; its *integrity* (not its *authenticity*) is protected.
- **No env-secret storage.** Redaction here is "detect/redact secrets in captured evidence before persistence." Encrypted env-secret storage (NER-141) is a separate track.
- **No Merkle tree / inclusion proofs, and no persisted `{count, head_hash}` chain summary.** O(n) re-walk over local SQLite is sufficient at v0 scale; tail-truncation/head-rewrite are weakly detected (a dropped latest-evidence row reads as `missing`/`stale`, which already fails closed). Strong head/truncation protection is deferred (see below).

### Deferred to Follow-Up Work

- **Real `gc` must preserve op-log chain integrity.** v0 `gc` is dry-run-only (`gc_dry_run`), so it cannot break the chain. Phase 8's mark-sweep `gc` must tombstone/repair the chain when deleting evidence/operation rows — flag this in the Phase 8 plan.
- **Persisted per-chain `{count, head_hash}` summary** for strong truncation/head-rewrite detection — a `doctor`-hardening follow-up (the v0 gate/doctor cannot detect a full rewrite up to the chain head without it; see Key Technical Decisions).
- **Structured parsers beyond cargo** (pytest, jest, go test, coverage) and string-bearing structured fields (test names, file paths — which would need to route through the redactor) — additive later.

---

## Context & Research

### Relevant Code and Patterns

- **Op-log spine (the chain substrate):** `crates/forge-store/src/lib.rs` `insert_operation_view` writes an `operations` row (`parent_operation_id` = current head) + a `views` row, then advances `current_state` via an **optimistic singleton CAS** (`UPDATE … WHERE current_operation_id = ?parent`). **Two other sites also append `operations` rows directly:** `record_failed_operation` (writes a failed op + its own CAS on every deterministic non-transient failure of a mutating command — incl. the lock-free `run`) and the genesis INSERT in `init_repository` (`parent_operation_id = NULL`). All three must compute `content_hash`. Table DDL in `migrations/001_init.sql`.
- **Evidence capture:** `crates/forge-evidence/src/lib.rs` `capture_with_timeout` builds `CapturedCommand` (hardcoded `trust: "locally_observed"`). `excerpt_file` currently reads only `EXCERPT_LIMIT + 1` bytes then truncates-then-redacts — so full-output parsing and the redact-vs-truncate ordering both need attention. `temp_dir` (holding the captured stdout/stderr files) is owned until `capture_with_timeout` returns, so a parser can read the full output before the `Ok(...)`.
- **Evidence write:** `record_evidence` runs its INSERT inside `with_immediate_retry(&mut connection, |tx| …)` after `replay_guard`, reading the determining snapshot via `latest_snapshot_on(tx, …)` — the template for an in-txn parent-hash read. The `with_immediate_retry` body re-runs only on `SQLITE_BUSY`/517; a `CurrentStateChanged` (CAS loss) is **not** retryable-busy and propagates to the CLI for full command re-execution. The CAS guards on `context.current_operation_id`, captured once at `open_repository` and fixed across busy-retries.
- **Gate path:** `record_check` → `evaluate_check_on(&tx, …)` → `evidence_facts_on(&tx, …)` → `forge_policy::evaluate`. `decide` re-evaluates the gate **in its own IMMEDIATE txn** and gates `accept` on `outcome.passed()`. `EvidenceFact` (`crates/forge-policy/src/lib.rs`) carries `exit_code` only.
- **Export path:** `exportable_proposal` / `decision_for_proposal_revision` read the decision on a **throwaway connection outside any txn**; the CLI's `export branch` creates the git branch *before* `record_publication` opens its own txn — so there is **no** "same `&tx`" site to hook decision verification; it must be a verifying read under the held repo lock, before the git branch.
- **Redactor:** `crates/forge-content/src/lib.rs` `redact_secret_like_text` (line-oriented, keys on first `=`/`:`), `is_secret_like_key`, `is_secret_risk_path`, `is_ignored_by_policy`, `filter_secret_risk` (the `(kept, dropped)` → warnings precedent).
- **Typed-error contract:** `crates/forge-store/src/error.rs` — `ForgeError`, `code()/details()/retryable()/after_ms()/Display`, `error_registry()` + `ErrorCodeSpec`, drift-guard tests `registry_covers_every_variant` (the `all` array **and** the exhaustive match), `codes_match_the_pre_change_registry`, `attempt_worktree_mismatch_details_carry_only_ids`. Mirror surface in `crates/forge-cli/tests/forge_schema.rs` `FORGE_ERROR_CODES`; CLI registry in `crates/forge-cli/src/schema.rs` (`notes.secret_protection` "until Phase 5" promissory note).
- **Migrations:** `crates/forge-store/src/migrations.rs` — numbered `.sql` runner, per-statement apply, tolerate `duplicate column name`, NULL-checksum grandfathering, `schema_head()`. `sha2` in-tree (`checksum_of`). Head-`3` literals exist in: `migrations.rs` (`schema_head_is_max_version`, `cd1bb3b_…converges`), `forge_init.rs` (three sites ~170/203/229), `migrate.rs` (`head_plus_one_is_refused` stamps `(4,'future')`), `forge_migration_upgrade.rs` (`stamp_future_version` hard-codes `VALUES (4,…)`), and `scripts/e2e-eval.sh` (`schema_version=3`).
- **`doctor`:** `crates/forge-store/src/lib.rs` `doctor` — FK check, schema-version check, content-ref verification, `scan_restore_temps`; `DoctorReport` struct (`issues: Vec<String>`).
- **ID/time helpers:** `crates/forge-core/src/lib.rs` `new_id` (UUIDv7), `now_ms`. `regex` is already transitively in `Cargo.lock` (1.12.x) via `assert_cmd`/`predicates`.

### Institutional Learnings

- **`content-bound-gate-engine-and-failclosed-enforcement-2026-05-29.md`** — §3 fail-closed (NULL hash = legitimate-default, present-but-mismatch = corruption → error; never `.ok().unwrap_or(pass)`); §4 per-token redaction on **every** egress (persist, `check`/`show` success envelopes, error `details`, `doctor` output); §5 in-txn determining read; §6 migration-head-bump test fan-out; §7 emit-don't-persist (the structured *outcome* IS consumed by the gate this phase, so persisting it is justified; per-gate *verdicts* stay emit-only). This doc's scope-boundary names NER-136 as the closure of its honesty note.
- **`schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md`** — §1 grandfather NULL; §4 typed error via `anyhow` + `downcast` with no writer signature changes, converted at the CLI layer too; §5 "verify the production caller graph, not a green test on a plausible function" (the reason `record_failed_operation` and `init` must be enumerated as chain-write sites, and the verify must hook `record_check`/`decide`/`export`, not a helper the gate never calls).
- **`write-binding-verification-and-content-backend-isolation-2026-05-29.md`** — §6 the additive-error checklist + `details_carry_only_ids`; §1 "the soundness anchor is the binding record, not content equality"; §4 confine a new concern behind a boundary (keep tool-specific parsing in `parsers.rs`).
- **`sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`** — §2 FnMut recompute-per-attempt; §3 determining read on `&tx` (parent-hash read must be in-txn or two writers fork); §4 replay-guard ordering; §5 rowid tiebreak; §7 test with real OS processes.
- **`crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md`** — §2 a lock only helps when both racers take it (verify in-txn, not behind the advisory lock the lock-free `run` won't take); §1 acquire-once-never-nested; §4 `abort()` skips `Drop`.

### External References

- **Secret detection (detect-secrets / gitleaks / trufflehog):** base64 entropy ≥ ~4.5, hex ≥ ~3.0 bits/char, **min length ~20** gated *before* entropy; exempt UUID and 7/8/40/64-char pure-hex runs — but a bare 40-hex token shaped like a SHA-1 is a known **false-negative** of that exemption (document it). JSON: regex over `"key":"value"` pairs (no recursive parse of a truncated excerpt). PEM: anchor on `BEGIN…END`, non-greedy, truncation-aware. Credential URLs: parse-first; regex fallback **anchored on `scheme://`** to exclude scp-style `git@host:path`, redact only the password group.
- **Hash-chaining (NIST SP 800-185 TupleHash principle):** length-prefix every field (injective; no delimiter ambiguity); domain-separation tag per record type; documented fixed genesis sentinel; include sequence + all metadata in the digest. Tamper-evident-not-tamper-proof boundary stated explicitly. `sha2` only.
- **Crate posture (4-day min-release-age gate):** hand-roll Shannon entropy (~10 lines; add a golden-value test); promote `regex` to a direct workspace dep (already transitively in `Cargo.lock`; linear-time / ReDoS-safe). Compile patterns once via `std::sync::OnceLock`.

---

## Key Technical Decisions

- **Chain into the op-log spine, at all three write sites.** The `operations` table is already a linear append-only chain (parent pointer + singleton CAS). Add `operations.content_hash` = `H(domain_tag ‖ len-prefix(parent_op.content_hash, operation_id, command, kind, created_at_ms, domain_row_digest))`. **Every** site that appends an op must compute it: `insert_operation_view` (success path), `record_failed_operation` (folds parent + a failure-op sentinel digest), and the `init` genesis INSERT (folds the 32-zero genesis sentinel as parent + an init sentinel digest, so the genesis row is itself a verifiable link rather than a NULL-hash anchor). Missing any site leaves a NULL-hash op created after the migration high-water mark, which `doctor`/the gate would mis-classify as tampered — bricking an honest repo on its first command failure or fresh init. Evidence and decision rows **additionally** persist their own `content_hash` for cheap per-row verification.
- **Compute the hash in-txn, recompute per busy-retry.** Inside `record_evidence`'s `with_immediate_retry` body, after `replay_guard`, read the parent op hash by the **captured `context.current_operation_id`** (the exact id the CAS guards on — not a live `current_state` join, so the folded parent and the CAS pointer are always the same row). The body re-runs only on `SQLITE_BUSY`/517; on those the captured parent id is unchanged. A `CurrentStateChanged` CAS loss is **not** a body re-run — it propagates to the CLI for full command re-execution (fresh `open_repository`, fresh parent id).
- **Hash over the persisted (redacted + truncated) bytes.** Verification recomputes from stored columns. Redaction must run before/consistent-with truncation so a secret straddling the 4096 boundary is not half-persisted; `excerpt_file` redacts on a boundary-safe slice, then bounds to `EXCERPT_LIMIT`.
- **Canonical encoding = domain-tag + length-prefixed fields.** No delimiter joining. `Option`/NULL fields carry a presence tag distinct from empty-string. Genesis prev-hash = documented 32-zero-byte sentinel.
- **Legacy vs tampered is anchored to a recorded op-log/rowid high-water mark, NOT a mutable timestamp.** Migration 004 persists a marker — the maximum `evidence.rowid` (and `operations.rowid`) present when 004 applies (a small recorded value). A NULL-`content_hash` row whose `rowid ≤ marker` → `legacy_unverified` (grandfather; gate trusts `exit_code`; `warnings[]`); a NULL hash on a row with `rowid > marker`, or any non-NULL mismatch → `tampered` → refuse. Keying the discriminator on `evidence.created_at_ms` (the first draft) is a **downgrade attack**: `created_at_ms` is attacker-mutable, so backdating it would launder a tampered row into the grandfathered branch and pass the gate. `created_at_ms` *is* now in the digest (so editing it breaks the hash), but the legacy/tampered split must not depend on a field the attacker sets. `rowid` is monotonic and not set by normal INSERTs. (Residual: the marker lives in the same mutable DB — a full-DB-write attacker can rewrite it; that is the conceded Phase 9 boundary, not a v0 gap.)
- **Two-level verification, honestly scoped.** *Gate path* (in-txn, cheap): recompute the deciding evidence row's digest vs stored `evidence.content_hash` (catches the literal `exit_code 7→0` Phase-4 hole), **plus** recompute its operation's link vs the stored parent hash as **defense-in-depth** (catches an editor who fixes the row digest but not the op link). *`doctor`* (offline, deep): full genesis-to-head re-walk recomputing every link, catching interior edits, deletion, and reorder. **Honest boundary (R4/R12):** an attacker who recomputes a row's digest **and** every op link up to and including the current chain **head** produces a chain both the gate and `doctor` accept — there is no successor link to contradict a rewritten head and no external anchor. So the guarantee is: naive, partial, and **interior** tampering is detected; a full rewrite-to-head is not (it needs the deferred `{count, head_hash}` summary or Phase 9 signing). The Key-Decision wording is "raises the bar on the recompute attack," not "defeats it."
- **`--allow-unverified` is a policy bypass, never an integrity bypass.** Tamper is fail-closed unconditionally and is its own `EVIDENCE_TAMPERED` code, not folded into `CHECK_NOT_PASSED`. Verification runs at **`record_check`, `decide`, and `export`** — every place a stored verdict/decision is produced or trusted.
- **Structured outcome is numeric, additive, and conjunctive.** `StructuredOutcome` carries counts only (`{passed, failed, ignored}` for cargo test; `{findings}` for clippy) — no `tool` field (the caller knows the identity) and no string fields (test names/paths could carry secrets; deferred behind the redactor). A structured gate is green **iff `exit_code == 0` AND `parsed_failures == 0`** (strict; no `max_failures`). Disagreement → `failed` (stronger wins). Unparseable output for a *declared* structured gate → `missing`; in *default* mode, degrade to the exit_code verdict with a `warnings[]` note. Persist `structured_json` on evidence (the in-txn gate reads facts from the DB; counts live past the 4096 excerpt) and include it in the digest.
- **Actor source = `--actor` flag, else `FORGE_ACTOR` env, else `"unknown"`.** Never NULL, never refuse. `NOT NULL DEFAULT 'unknown'` columns. Actor is in the digest.
- **Reuse `sha2`; promote `regex`; hand-roll entropy.** No new crypto; no new heavy dependency under the supply-chain-age gate.

---

## Open Questions

### Resolved During Planning

- **Chain anchor — per-attempt or op-log-global?** → Op-log spine (linear, append-only), at all three write sites.
- **Where is the hash computed under concurrency?** → In-txn, recomputed per busy-retry, parent read by the captured `current_operation_id`. `CurrentStateChanged` ⇒ CLI re-execution, not a body re-run.
- **Hash over raw or persisted bytes?** → Persisted (redacted+truncated).
- **Does `--allow-unverified` bypass tamper?** → No, never. Distinct `EVIDENCE_TAMPERED`, enforced at `check`/`accept`/`export`.
- **Legacy NULL-hash rows after migration?** → Discriminate by a recorded `rowid` high-water marker, not `created_at_ms`. Pre-marker NULL = legacy; post-marker NULL or any non-NULL mismatch = tampered.
- **Structured gate vs exit_code disagreement?** → Conjunctive, strict `==0`; stronger wins → `failed`. Truth table in HLD/U6.
- **Do decisions/publications need integrity, not just actor?** → Yes. Decisions persist a `content_hash`; `export` verifies it (under the repo lock, before the git branch); the op-log chain + `doctor` cover publications. Actor on all three.
- **Exact digest field set?** → **Resolved now** (it is a security commitment, not an implementation detail): every mutable identity/outcome/behavioral field listed in R1, including `timed_out`, the truncation flags, `sensitivity`, and `created_at_ms`. Pinned by a golden-vector test in `integrity.rs`.

### Deferred to Implementation

- **Exact regexes for `cargo test`/`clippy` summaries** across libtest variants (plain vs `--format json`, panics, compile-fail-before-tests, 0 tests) — pin against real output in U5; unknown shape → degrade to `None`.
- **`check_spec_json` shape for a structured gate** — whether the Phase-4 deserializer needs a version/kind tag to distinguish exit-code-only gates from structured gates on an upgraded DB; settle in U6 (fail-closed on an unparseable shape, per the Phase-4 `intent_check_spec` rule).
- **Whether the gate verifies one op-link or N for the deciding row** — start with one-link (stored parent) defense-in-depth; `doctor` always does the full walk; extend only if a test shows under-detection within the cheap budget.

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

**Chain write (per `run`), in-txn:**

```
record_evidence(cwd, request_id, attempt, input):
  with_immediate_retry(conn, |tx|:                      # body re-runs only on SQLITE_BUSY/517
    replay_guard(tx, repo, request_id)                  # replay aborts BEFORE any chain mutation
    snapshot   = latest_snapshot_on(tx, attempt)        # existing determining read
    parent_op  = op_hash_of(tx, context.current_operation_id)   # the id the CAS guards on (not a live join)
    digest     = evidence_digest(domain_tag.evidence,   # canonical, length-prefixed; covers ALL mutable fields
                   attempt, snapshot, command, args, cwd, exit_code, started, ended,
                   timed_out, stdout_excerpt, stderr_excerpt, stdout_trunc, stderr_trunc,
                   sensitivity, actor, structured_json, created_at_ms)
    INSERT evidence(..., content_hash=digest, trust=(locally_observed, sha256))
    insert_operation_view(tx, ..., domain_digest = Some(digest))   # folds digest + parent into operations.content_hash

# the SAME content_hash discipline also applies to:
#   record_failed_operation(tx, ...)  -> domain_digest = failure-op sentinel
#   init_repository genesis INSERT     -> parent = genesis sentinel, domain_digest = init sentinel
```

**Structured "0 failing tests" gate (U6) — verdict composition (tamper handled separately, below):**

| exit_code | parsed_failures | parse_status | structured-gate verdict |
|-----------|-----------------|--------------|--------------------------|
| 0         | 0               | ok           | **passed**               |
| 0         | > 0             | ok           | **failed** (parsed dominates) |
| ≠ 0       | 0               | ok           | **failed** (exit dominates) |
| ≠ 0       | —               | unparseable  | **failed** (exit dominates) |
| 0         | —               | unparseable  | **missing** (declared count absent) |

**Integrity gate (orthogonal to the verdict table; runs first, in `record_check`/`decide`'s IMMEDIATE txn):**

```
verify deciding facts on &tx:
  for f in facts deciding a gate:
    integrity = verify_row(f, tx)               # recompute digest vs stored; recompute op link vs stored parent
    if integrity == Tampered:        return Err(EVIDENCE_TAMPERED)   # fail-closed, even under --allow-unverified
    if integrity == LegacyUnverified: warn; trust exit_code for f
  evaluate(spec, snapshot, facts)               # existing Phase 4 engine, unchanged
# export branch: a verifying read of decisions.content_hash under the repo lock, BEFORE the git branch is created.
```

---

## Implementation Units

Grouped into four phases. U-IDs are stable.

### U1. Canonical hashing primitives (digest module)

**Goal:** A pure, heavily-unit-tested module producing the injective, domain-separated SHA-256 digests for evidence rows, decision rows, and operation chain links (including the genesis link). Zero IO.

**Requirements:** R1, R2.

**Dependencies:** None.

**Files:**
- Create: `crates/forge-store/src/integrity.rs` (digest functions, domain tags, genesis sentinel, hex helpers).
- Modify: `crates/forge-store/src/lib.rs` (`mod integrity;`).
- Test: unit tests in `crates/forge-store/src/integrity.rs`.

**Approach:**
- `evidence_digest(fields…) -> String`, `decision_digest(fields…) -> String`, `operation_link_hash(parent_hash, op_fields, domain_digest) -> String`, all hex SHA-256.
- Encoding: `SHA256(domain_tag ‖ for each field: u64-LE length ‖ bytes)`. Distinct tags (`b"forge.evidence.v0\0"`, `b"forge.op.v0\0"`, `b"forge.decision.v0\0"`). `Option<&str>` encodes a presence byte (`None` ≠ `Some("")`). Genesis parent hash = documented all-zero hex sentinel; the genesis op's own `content_hash` = `operation_link_hash(genesis_sentinel, init_op_fields, init_sentinel)` so it verifies.
- Reuse the `Sha256::digest` + hex pattern from `migrations.rs` `checksum_of`.

**Patterns to follow:** `migrations.rs` `checksum_of`; pure-function + exhaustive-unit-test discipline from `forge-policy`.

**Test scenarios:**
- Happy path: a fixed field tuple → a known-stable hex digest (golden vector pinned).
- Edge case: `("ab","c")` vs `("a","bc")` → **different** digests (length-prefix injectivity).
- Edge case: `None` vs `Some("")` for `snapshot_id` → different digests.
- Edge case: flipping `timed_out` true↔false (with all other fields equal) → different digest (proves the behavioral flag is covered).
- Edge case: empty fields (`run -- true`) hash deterministically; non-ASCII / lossy-UTF-8 excerpt bytes hash deterministically.
- Happy path: the genesis link (`operation_link_hash(genesis_sentinel, …)`) is stable and differs from a non-genesis-parent link.

**Verification:** Digests are deterministic, injective, cover every mutable field, and the genesis link has one canonical encoding.

---

### U2. Migration 004 — integrity + actor columns + high-water marker, with full head-bump reconciliation

**Goal:** Add the new columns, persist the legacy/tampered `rowid` high-water marker, bump the schema head to 4, and reconcile **every** hard-coded head literal across the test tree and eval.

**Requirements:** R1, R2, R3, R5, R7, R10.

**Dependencies:** None (DDL only).

**Files:**
- Create: `crates/forge-store/migrations/004_integrity_and_actor.sql`.
- Modify: `crates/forge-store/src/migrations.rs` — `schema_head()` → 4; `schema_head_is_max_version` (→4); `cd1bb3b_v1_with_inline_content_backend_converges` (`versions.last()`/`current_schema_version` 3→4); the `fresh_apply_reaches_head_with_checksums`-style fixture.
- Modify: `crates/forge-cli/tests/forge_init.rs` — **all three** head-`3` literals (~lines 170, 203, 229).
- Modify: `crates/forge-store/tests/migrate.rs` — convergence fixtures include version 4; `head_plus_one_is_refused` stamps `(5,'future')` and asserts `db_version==5`/`supported_head==4` (was `(4,'future')`/`==4`/`==3`).
- Modify: `crates/forge-cli/tests/forge_migration_upgrade.rs` — `stamp_future_version` hard-codes `VALUES (4,…)`; bump to `schema_head()+1` (=5) so it still stamps a *future* version, and update its `db_version` assertions.
- Modify: `scripts/e2e-eval.sh` — `doctor schema_version=3` → 4; keep the "all migration rows carry a checksum" probe.
- Test: the migration runner tests + `migrate.rs` convergence.

**Approach:**
- `004` adds (additive, statement-by-statement, tolerating `duplicate column name`): `evidence.content_hash TEXT`, `evidence.structured_json TEXT`, `evidence.actor TEXT NOT NULL DEFAULT 'unknown'`; `operations.content_hash TEXT`; `decisions.content_hash TEXT`, `decisions.actor TEXT NOT NULL DEFAULT 'unknown'`; `attempts.actor TEXT NOT NULL DEFAULT 'unknown'`; `publications.actor TEXT NOT NULL DEFAULT 'unknown'`. It also records the **high-water marker** (`MAX(rowid)` of `evidence` and of `operations` at apply time) — a small recorded value the legacy/tampered check reads.
- Grep the **whole** test tree + `scripts/e2e-eval.sh` for the literal `3` (head) **and** the literal `4` used as a future-version stamp before declaring the fan-out closed.

**Execution note:** Reconcile the head-bump fixtures first (let the literal failures surface), then add behavior.

**Patterns to follow:** `migrations/002_columns.sql`, `003_check_spec.sql`; the reconciliation discipline in `apply_pending_migrations`.

**Test scenarios:**
- Happy path: fresh init reaches head 4 with a checksum on every migration row, and the high-water marker is recorded.
- Integration: the three genesis cases in `migrate.rs` (fresh-v4, upgraded-via-ALTER, the `cd1bb3b` inline shape) converge to an identical head/schema incl. `004`'s columns.
- Edge case: a DB stamped at 3 upgrades to 4 idempotently; re-running is a no-op; `duplicate column name` tolerated.
- Edge case: DB stamped at `schema_head()+1` (=5) still refuses read-only.

**Verification:** `cargo test --workspace` green at head 4; `forge doctor` reports `schema_version: 4`; the eval's migration block passes; the high-water marker is queryable.

---

### U3. Op-log hash-chain at all three write sites + evidence content hash + actor + verifiable trust claim

**Goal:** Compute and persist the chain at `insert_operation_view`, `record_failed_operation`, and the `init` genesis INSERT, in-txn; attach the actor; replace the hardcoded trust literal with a verifiable claim.

**Requirements:** R1, R2, R7, R11.

**Dependencies:** U1, U2.

**Files:**
- Modify: `crates/forge-store/src/lib.rs` — `record_evidence` (in-txn parent-hash read by captured `current_operation_id`, digest over the full R1 field set, INSERT new columns); `insert_operation_view` (accept `domain_digest: Option<&str>`, fold it + parent hash into `operations.content_hash`); **`record_failed_operation`** (compute `content_hash` folding parent + a failure-op sentinel, in its own IMMEDIATE body); **`init_repository`** genesis INSERT (store the genesis link from U1); add `op_hash_of(conn, op_id)`; thread `actor` through `EvidenceInput`/`EvidenceRecord`; widen other `insert_operation_view` callers to pass a `None`/sentinel digest.
- Modify: `crates/forge-evidence/src/lib.rs` — `CapturedCommand.trust` → a structured claim `(trust_level, hash_alg)`; stop hardcoding the bare literal.
- Modify: `crates/forge-cli/src/main.rs` — a `resolve_actor` helper (`--actor` → `FORGE_ACTOR` → `"unknown"`) in a testable location; `run_response`/`start`/`attempt start` pass actor; add `--actor` to the relevant arg structs.
- Test: `crates/forge-cli/tests/forge_run_evidence.rs`; new `crates/forge-store/tests/integrity_chain.rs`.

**Approach:**
- Parent-hash read + digest live **inside** `with_immediate_retry`, after `replay_guard`, sibling to `latest_snapshot_on(tx, …)`; recompute per busy-retry; read by the **captured** `context.current_operation_id`.
- All three op-append sites compute `content_hash` (the "single site" framing was wrong — `record_failed_operation` and the genesis INSERT bypass `insert_operation_view`). Other ops pass a sentinel digest.
- The trust claim is emitted only when a hash was computed; `EvidenceRecord`/`EvidenceSummary` JSON carries `trust` + `hash_alg` + `content_hash`.

**Execution note:** Add a failing test: `run -- true` then a **deterministic command failure** (which exercises `record_failed_operation`) then another `run`; assert `doctor` verifies the chain clean end-to-end (this would brick under the original single-site design).

**Patterns to follow:** `record_evidence`'s in-txn `latest_snapshot_on(tx, …)`; `replay_guard` ordering; `OperationViewInput` plumbing.

**Test scenarios:**
- Happy path: two sequential `run`s → both evidence rows carry non-NULL `content_hash`; each op's `content_hash` recomputes from its stored parent + the evidence digest.
- Integration: a deterministic command failure (`record_failed_operation`) followed by a success → the chain still verifies clean (no NULL-hash op).
- Integration: a fresh `init` → `doctor` reports the genesis op verifies (no false tamper).
- Integration: a replayed `--request-id` returns the persisted row **verbatim** incl. its stored hash — no recompute, no phantom link.
- Edge case: evidence with `snapshot_id = NULL` hashes/chains correctly.
- Edge case (concurrency, real processes): ≥8 concurrent `run`s **under one attempt** → the op-log chain verifies end-to-end with no fork. Mirror `crates/forge-cli/tests/forge_concurrency.rs`.
- Happy path: `--actor alice` / `FORGE_ACTOR=bob` / neither → recorded actor `alice`/`bob`/`unknown`; actor in the digest.

**Verification:** Every new evidence row has a verifiable hash; the chain verifies across success, failure-op, and genesis; actor round-trips; concurrency leaves no fork; replay is hash-transparent.

---

### U4. Redaction hardening — entropy / JSON / PEM / credential-URL detectors

**Goal:** Catch bare high-entropy tokens, JSON-embedded secrets, PEM bodies, and `user:pass@host` URLs before persistence, surfacing each redaction as a warning, with bounded false positives and a documented residual false-negative.

**Requirements:** R8, R11.

**Dependencies:** None for the detectors; lands **before/with** U3 (the hash is over the redacted bytes; U4's redact-then-truncate reorder must be in place when U3 hashes).

**Files:**
- Modify: `crates/forge-content/src/lib.rs` — add the four detectors as a new redaction **stage**; change the redactor return to carry *what* was redacted (a `Vec<RedactionKind>` / kind-counts) so each maps to one warning; keep the line-oriented `key=value` pass as a stage.
- Modify: `crates/forge-content/Cargo.toml` + root `Cargo.toml` (promote `regex` to a direct `[workspace.dependencies]`).
- Modify: `crates/forge-evidence/src/lib.rs` — `excerpt_file`: redact on a boundary-safe slice **then** bound to `EXCERPT_LIMIT`; thread redaction kinds to the capture result.
- Modify: `crates/forge-cli/src/main.rs` — `run_response` emits one `warnings[]` per redaction.
- Modify: `crates/forge-cli/src/schema.rs` — update `notes.secret_protection` (the "until Phase 5" note is satisfied) and document the 40-hex false-negative.
- Test: `crates/forge-content/src/lib.rs` unit tests (leak corpus) + `crates/forge-cli/tests/forge_run_evidence.rs`.

**Approach:**
- Hand-roll Shannon entropy (golden-value test). Gate by **min length ≥ 20** + charset (hex vs base64) **before** entropy; base64 ≥ 4.5, hex ≥ 3.0. Exempt UUID and 7/8/40/64-char pure-hex runs — **but** narrow the exemption so it does not silently pass a 40-hex secret: prefer exempting only when the hex run sits in a git-SHA-like context (word boundaries / adjacent commit-log shape), else fall through to the key-name heuristic; whatever residual remains is documented in `notes.secret_protection`.
- JSON: regex over `"key"\s*:\s*"value"` (no recursive parse of a truncated excerpt). PEM: anchor `BEGIN … PRIVATE KEY` → `END`, non-greedy; truncated → redact header-to-end-of-buffer. Credential URL: anchored on `scheme://` then `user:pass@`, redact only the password group (excludes scp-style and userinfo-without-password).
- Compile patterns once via `OnceLock`. Degrade gracefully: replace the matched span with `[REDACTED]`, preserve context; never drop the whole excerpt.

**Patterns to follow:** extend `redact_secret_like_text`; `filter_secret_risk`'s `(kept, dropped)` → warnings; `SECRET_RISK_SENSITIVITY` stamping in `capture_with_timeout`.

**Test scenarios (leak corpus):**
- Happy path: a bare 40-char **base64** token (no `key=`) is redacted; a 40-hex git SHA / 64-hex SHA-256 / UUID are **not** (false-positive guard).
- Edge case (documented residual): a bare 40-**hex** token in a non-git context — assert current behavior (whatever the narrowed exemption decides) and that `notes.secret_protection` documents it.
- Happy path: `{"api_key":"ghp_…"}` redacts the value, keeps the key.
- Happy path: a PEM block is fully redacted; a block truncated at 4096 redacts header-to-end.
- Happy path: `postgres://user:s3cr3t@host/db` redacts only `s3cr3t`; `git@github.com:org/repo` and `https://token@host` are **untouched**.
- Edge case: a secret straddling byte 4096 is caught (`4090` filler + `api_key=SECRET…`).
- Edge case: each distinct redaction → exactly one `warnings[]` entry naming the kind; `sensitivity=secret_risk` set when any detector fires.

**Verification:** The corpus is caught with no false positive on Forge's own hashes/SHAs/UUIDs; the 40-hex residual is documented; warnings enumerate each redaction; the persisted (and hashed) excerpt is the redacted one.

---

### U5. Tool-specific structured result parsers (cargo test / clippy)

**Goal:** Extract machine-readable **numeric** outcomes from the **full** captured output via match-dispatched, tool-specific functions, persisted alongside the excerpt; degrade to the raw excerpt when no parser matches.

**Requirements:** R5, R11.

**Dependencies:** U2 (`structured_json` column), U4 (redact-then-truncate reorder lands first; the parser reads the **raw full** output for counts only — counts are numeric and not redacted).

**Files:**
- Create: `crates/forge-evidence/src/parsers.rs` — private functions `parse_cargo_test`, `parse_cargo_clippy`, a `match`-dispatch shim by `(program, args)` identity, and a numeric-only `StructuredOutcome` (`{ passed, failed, ignored }` for test; `{ findings }` for clippy — no `tool` field, no string fields). (A trait/registry is deferred until a third consumer needs runtime polymorphism — two functions behind `parsers.rs` is the boundary.)
- Modify: `crates/forge-evidence/src/lib.rs` — `capture_with_timeout` runs the matching parser on the full captured temp files **before** the `Ok(...)` return; attaches `Option<StructuredOutcome>`; serializes `structured_json`.
- Modify: `crates/forge-store/src/lib.rs` — `EvidenceInput`/`record_evidence` persist `structured_json`; include it in the digest (U1/U3).
- Test: `crates/forge-evidence/src/parsers.rs` unit tests (real cargo output fixtures) + `crates/forge-cli/tests/forge_run_evidence.rs`.

**Approach:**
- Selection by `(program, args)` identity, not content sniffing. Read the full output from the capture temp dir (counts live past 4096). `None` when no parser matches or parsing fails → the gate degrades (U6). Confine tool-specific logic in `parsers.rs`; the gate engine never names a tool. `StructuredOutcome` is numeric-only by design (string fields could carry secrets — deferred behind the redactor).

**Patterns to follow:** `forge-policy` pure-function + fixture-test style; the `parsers.rs` file is the confinement boundary (not a trait object).

**Test scenarios:**
- Happy path: `test result: ok. 12 passed; 0 failed; 1 ignored` → `{passed:12, failed:0, ignored:1}`.
- Happy path: `test result: FAILED. 10 passed; 2 failed` → `{passed:10, failed:2}`.
- Happy path: `cargo clippy` with N warnings → `{findings:N}`.
- Edge case: compile error before tests (exit 101, no summary) → `None` (degrade), not `{failed:0}`.
- Edge case: 0 tests (`0 passed; 0 failed`) → `{passed:0, failed:0}` (distinct from `None`); timeout (`timed_out=true`, partial) → `None`; unknown tool → `None`.
- Integration: a `run -- cargo test` whose summary is past byte 4096 yields correct counts while the stored excerpt is still truncated at 4096.

**Verification:** Counts extracted regardless of excerpt truncation; unknown/failed parses degrade to `None`; `structured_json` is numeric-only, round-trips, and is hashed.

---

### U6. Additive structured "0 failing tests" gate in forge-policy

**Goal:** Let a declared gate assert on parsed counts (strict `==0`, conjunctive with exit_code), composed into the Phase 4 rollup, without regressing exit_code gates.

**Requirements:** R6.

**Dependencies:** U5.

**Files:**
- Modify: `crates/forge-policy/src/lib.rs` — extend `EvidenceFact` with the parsed outcome; add a structured gate kind (a `Gate` variant or a `kind` discriminator — **no** `max_failures`; the predicate is `exit_code==0 && parsed_failures==0`); extend `verdict_for` per the truth table; leave exit_code gates and the four-verdict rollup unchanged.
- Modify: `crates/forge-store/src/lib.rs` — `evidence_facts_on` selects `structured_json`; `intent_check_spec`/`check_spec_json_from_requires` accept the structured gate form (fail-closed on an unparseable shape, per the Phase-4 rule).
- Modify: `crates/forge-cli/src/main.rs` — `IntentArgs` gains a minimal way to declare a structured gate (e.g. `--require-tests-pass "cargo test"`).
- Test: `crates/forge-policy/src/lib.rs` unit tests + `crates/forge-cli/tests/forge_propose_check.rs`.

**Approach:** Additive; the existing four verdicts and `failed > missing > stale > passed` rollup are untouched. Disagreement → `failed`; unparseable for a declared structured gate → `missing`. Emit the parsed counts in `check --json gates[]` (subject to U4 redaction at that egress; counts are numeric so this is a no-op for them, but the program/args still get the egress pass).

**Patterns to follow:** the verdict-rule design + exhaustive unit tests in `forge-policy`; emit-don't-persist for per-gate verdicts (the structured *outcome* is persisted on evidence because the gate needs it).

**Test scenarios:**
- Happy path: `{passed:50, failed:0}` + exit 0 → `passed`.
- Edge case: `{passed:48, failed:2}` + exit 0 → `failed`; `{failed:0}` + exit 101 → `failed`; unparseable declared → `missing`.
- Integration: a structured gate AND an exit_code gate both required → overall `passed` only when both green; otherwise `accept` blocks on `CHECK_NOT_PASSED`.
- Edge case: an intent with no structured gate behaves exactly as Phase 4 (re-run existing `forge_propose_check.rs` assertions).
- Edge case: a deciding evidence row predating Phase 5 (no `structured_json`) under a declared structured gate → `missing` (fail-closed), not a silent exit-code pass.

**Verification:** A "0 failing tests" gate passes/fails on parsed counts; exit_code gates and default mode unchanged; disagreement fails closed.

---

### U7. Integrity verification at check/accept/export + `EVIDENCE_TAMPERED` + decision integrity & actor

**Goal:** The load-bearing integration — the gate refuses a tampered deciding row at **both** `check` and `accept`; `export` refuses a tampered decision before creating the git branch; the new typed error lands on every contract surface; `--allow-unverified` never bypasses tamper.

**Requirements:** R3, R4, R7, R9, R11, R12.

**Dependencies:** U1, U2, U3.

**Files:**
- Modify: `crates/forge-store/src/error.rs` — `ForgeError::EvidenceTampered { id, kind }` where `kind` is a **closed enum** (content_edit/broken_link/missing_hash); `code()="EVIDENCE_TAMPERED"`, `retryable()=false`, `after_ms()=None`, `details()` carries **only** `{id, kind}` (no excerpt/command), `Display`; append the `ErrorCodeSpec`; extend **both** drift tests; add `evidence_tampered_details_carry_only_ids`.
- Modify: `crates/forge-cli/tests/forge_schema.rs` — `FORGE_ERROR_CODES += "EVIDENCE_TAMPERED"`.
- Modify: `crates/forge-store/src/lib.rs` — `evidence_facts_on` selects `content_hash` + the hashed source columns + the deciding op's `content_hash`/parent + `rowid`; a `verify_evidence_integrity(fact, marker, tx)` helper → `Verified | LegacyUnverified | Tampered{kind}` (legacy/tampered by the `rowid` high-water marker, not `created_at_ms`); call it inside **`evaluate_check_on`** (so it runs at **both** `record_check` and `decide` — a tampered deciding row short-circuits to `EvidenceTampered` **before** `evaluate`, even when `enforce_check=false`/`--allow-unverified`); `decide` persists `decisions.content_hash` (decision digest) + actor; `record_publication` persists the publication actor; add a `verify_decision_integrity` used by the export path.
- Modify: `crates/forge-cli/src/main.rs` — `accept`/`reject`/export resolve+pass `actor`; surface `LegacyUnverified` as a warning; **export branch**: replace the plain `decision_for_proposal_revision` accepted-status read with a verifying read (`verify_decision_integrity`) **under the held repo lock, before `export_branch` creates the git branch**; `EvidenceTampered` maps via the existing `error_to_object` downcast (no new wiring).
- Modify: `crates/forge-cli/src/schema.rs` — the `EVIDENCE_TAMPERED` code flows through `error_registry()` automatically; add a `notes.integrity` stating the tamper-evident (not -proof) boundary.
- Test: `error.rs` (drift + details), `crates/forge-cli/tests/forge_propose_check.rs` + `forge_accept_export.rs` + `forge_errors.rs`.

**Approach:**
- Verification is **in-txn** on the same `&tx` at `check`/`accept` (gate-engine §5; lock-correctness §2 — the lock-free `run` makes an own-connection pre-flight a TOCTOU). It is a pure read inside the already-locked command (no re-lock). For **export**, there is no write txn before the git branch, so the decision verify is a verifying read under the held repo lock, before the branch — the "same `&tx`" framing does not apply there; state that explicitly.
- Two branches, fail-closed: pre-marker NULL → `LegacyUnverified` (warn, trust exit_code); post-marker NULL or non-NULL mismatch → `Tampered` → refuse.

**Execution note:** Start with the exit-criterion failing test: record evidence, `propose`, mutate `evidence.exit_code` via raw SQLite, then assert `check` AND `accept` AND `accept --allow-unverified` all return `EVIDENCE_TAMPERED`. Then a decision-tamper test: mutate a `decisions` row → `export branch` refuses **and** no git branch is created.

**Patterns to follow:** `decide`'s in-txn `evaluate_check_on(tx, …)`; `ForgeError::AttemptWorktreeMismatch`'s full additive-surface landing + `details_carry_only_ids`; `error_to_object` downcast.

**Test scenarios:**
- Happy path: untampered evidence → `check`/`accept` behave exactly as Phase 4.
- Error path (exit criterion): mutate `evidence.exit_code 7→0` → `check`, `accept`, and `accept --allow-unverified` all return `EVIDENCE_TAMPERED`.
- Error path: mutate a `decisions` row → `export branch` refuses **before** creating the git branch (assert the branch does not exist).
- Edge case: a pre-marker NULL-hash deciding row → gate passes on exit_code with a `legacy_unverified` warning (no brick).
- Edge case: a NULL-hash row with `rowid > marker` (simulated post-migration deletion of the hash) → `tampered`.
- Edge case: backdating `created_at_ms` below the migration time on a tampered NULL-hash row does **not** launder it (the discriminator is `rowid`, not the timestamp).
- Contract: `forge schema --json` lists `EVIDENCE_TAMPERED`; `details` carries only `{id, kind}`; both drift tests + `FORGE_ERROR_CODES` pass; `schema_version` stays `forge.cli.v0`.

**Verification:** Tampering with a deciding evidence row (at check or accept, incl. `--allow-unverified`) or an accepted decision (at export, before the branch) is refused; legacy rows don't brick; the timestamp-backdate bypass is closed; the contract drift guards pass.

---

### U8. `doctor` chain-verification pass + e2e tamper check + concurrency proof

**Goal:** `doctor` re-walks the chain offline (evidence + decision digests + the op-log), reports tampered/broken rows with the break kind, and correctly distinguishes a legitimately-shorter chain from tampering; the eval proves mutate→detect→refuse; a real-process test proves no fork.

**Requirements:** R3, R10, R11.

**Dependencies:** U1, U2, U3, U7.

**Files:**
- Modify: `crates/forge-store/src/lib.rs` — `doctor`: a new pass reading all `operations` (folding each op's evidence/decision domain digest) in one ordered SELECT, re-walking from the genesis link; classify each break: `content_edit` (row digest ≠ recompute), `broken_link` (op parent hash ≠ predecessor's recomputed hash — deletion/reorder), `missing_hash` (NULL with `rowid > marker`); add `tampered_rows: Vec<TamperedRow>` to `DoctorReport` where `TamperedRow = { id: String, table: String, kind: String }` (closed-enum kind; **no** command/excerpt/actor), plus an issue string when non-empty. **A head-truncated chain (a missing latest op, e.g. from a power-loss last-commit-loss under `synchronous=NORMAL`) is a legitimately-shorter chain — classify it as clean/short, NOT `broken_link`.** Keep this pure-SQLite, independent of the content-ref/backend loop.
- Modify: `scripts/e2e-eval.sh` — a tamper block: init→start→save→run→propose, mutate an evidence row via `sqlite3`, assert `doctor` reports tampered AND `check`/`accept` returns `EVIDENCE_TAMPERED`; gate on `sqlite3` availability.
- Test: `crates/forge-cli/tests/forge_doctor_gc.rs` (doctor flags a mutated evidence row AND a mutated decision row, names them, classifies the kind; a head-truncated chain is NOT a false positive), `crates/forge-cli/tests/forge_concurrency.rs` (after ≥8 concurrent `run`s, `doctor` verifies clean — no fork, no false positive).

**Approach:**
- `doctor` reads a consistent ordered snapshot (single SELECT) to avoid a torn read vs a concurrent lock-free `run`; no advisory lock (read-only). The chain write is a single IMMEDIATE txn, so a crash yields a clean **shorter** chain (no partial link) — `doctor` must report a missing-head as truncation/clean-short, not tamper. A single tampered interior row breaks every link after it; report the **first** break + kind.

**Patterns to follow:** existing `doctor` passes (`scan_restore_temps`, content-ref verification) and `DoctorReport` shape; the e2e-eval `ck`/`ckc` helpers + `sqlite3`-gated DB checks.

**Test scenarios:**
- Happy path: a healthy repo → `doctor.ok == true`, `tampered_rows` empty.
- Error path: mutate `evidence.stdout_excerpt` → `doctor.ok == false`, names that evidence id, kind `content_edit`.
- Error path: mutate a `decisions` row → flagged with kind `content_edit` (R3 covers decisions).
- Error path: delete an interior `operations` row → `broken_link` at the successor.
- Edge case: a pre-marker NULL-hash row is NOT flagged; a post-marker NULL → `missing_hash`.
- Edge case: a head-truncated chain (drop the latest op) → reported as clean/short, **not** a false tamper positive.
- Integration (e2e eval): the tamper block passes end-to-end against the real binary.
- Integration (concurrency): after ≥8 concurrent `run`s, `doctor` verifies the chain clean.

**Verification:** `doctor` detects content edits (evidence + decision), deletions, and post-marker missing hashes with the offending id + kind; head-truncation is not a false positive; the eval proves mutate→detect→refuse; concurrent writes leave a clean chain.

---

## System-Wide Impact

- **Interaction graph:** every mutating command appends an op through one of three sites (`insert_operation_view`, `record_failed_operation`, the `init` genesis) — all now chain-write sites. The gate path (`record_check`/`decide`) and the export path gain integrity reads; `doctor` gains a chain pass; `run` capture gains parsing + hardened redaction.
- **Error propagation:** `EvidenceTampered` rides `anyhow::Error`, recovered by `downcast_ref` at the CLI — no writer signature changes. Fail-closed, non-retryable; deterministic (it does not poison a fresh `--request-id`).
- **State lifecycle risks:** the hash is computed in-txn and recomputed per busy-retry; replay is hash-transparent; a crash mid-write rolls back the whole single txn → a clean shorter chain (no partial link), which `doctor` must not false-positive.
- **API surface parity:** JSON envelope stays `forge.cli.v0` (additive fields only). Both backends keep `is_ignored_by_policy`; the 4096 excerpt cap is preserved (parsing reads the full file separately).
- **Integration coverage:** the multi-process no-fork invariant, the failure-op/genesis chain continuity, and the mutate→detect→refuse story are integration/e2e scenarios unit tests can't prove.
- **Unchanged invariants:** the Phase 4 verdict rule, default mode, per-token/every-egress redaction, in-txn evaluation, and the `CHECK_NOT_PASSED` contract are preserved; structured gates are strictly additive.

---

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Chain computed at only one op-append site → NULL-hash op on a failure/genesis path → `doctor`/gate brick an honest repo | Compute `content_hash` at **all three** sites (`insert_operation_view`, `record_failed_operation`, `init` genesis); test a failure-op + a fresh-init chain verify (U3) |
| Legacy/tampered keyed on attacker-mutable `created_at_ms` → backdate-to-launder downgrade attack | Anchor the discriminator to a recorded `rowid` high-water marker; include `created_at_ms` in the digest; backdate-does-not-launder test (U2/U7) |
| Gate/doctor claim implies tamper-PROOF; a full rewrite-to-head passes both | Scope R4/Key-Decisions to detectable (naive/partial/interior) tampering; name the full-rewrite-to-head boundary; defer the `{count, head_hash}` summary (Scope) |
| Export decision verify has no in-txn site; git branch precedes `record_publication` | A verifying read under the repo lock **before** the branch; assert no branch on refusal (U7) |
| `record_check` not wired → a green `check_results` row persists over tampered evidence | Verify in `evaluate_check_on` so both `record_check` and `decide` refuse (U7) |
| `created_at_ms` / `CurrentStateChanged` misconceptions in the chain write | Read parent by captured `current_operation_id`; body re-runs only on busy/517; `CurrentStateChanged` ⇒ CLI re-execution (U3) |
| Migration 004 head bump leaves a green-but-meaningless literal or an inverted refusal fixture | Enumerate every head-`3` literal + every `head+1`/future stamp (incl. the `(4,'future')` → `(5,'future')` flip); re-grep literal `4` (U2) |
| Redaction false-positive on Forge's own SHA/UUID, or false-negative on a 40-hex secret | Length+charset gate before entropy; narrowed git-SHA-context exemption; document the 40-hex residual in `notes.secret_protection` (U4) |
| Digest omits a mutable behavioral flag (`timed_out`, `sensitivity`, truncation) → editable-without-detection | Digest covers the full R1 field set; golden-vector test on a `timed_out` flip (U1) |
| `doctor`/error `details` leak an excerpt or command | `tampered_rows`/`details` carry only `{id, table, kind}` (closed enum); test the shape (U7/U8) |
| In-txn verify reopens the NER-132 U2 TOCTOU if done on an own connection | Verify on the same `&tx` at check/accept; export verify under the held lock before the branch (U7) |

---

## Documentation / Operational Notes

- Update `crates/forge-cli/src/schema.rs` `notes.secret_protection` (the "until Phase 5" note is satisfied; document the 40-hex residual) and add `notes.integrity` stating the tamper-evident (not -proof) boundary.
- Post-merge: `/ce-compound` a solution doc (the three chain-write sites, the rowid-marker legacy discriminator, the head-rewrite honesty boundary, the redaction false-positive exemptions, the gate-vs-doctor verification division); flip this plan to `status: completed` + move to `docs/plans/completed/`; set NER-136 → Done.
- CI: `bash scripts/ci.sh` is the gate; the eval now includes the tamper block.

---

## Alternative Approaches Considered

- **Per-attempt evidence-only chain (instead of op-log-spine chain).** Rejected: the op-log is already a linear append-only chain; chaining there reuses the existing total order, matches the ticket wording, and uniformly covers evidence *and* decisions. (Per-attempt's gc-friendliness is moot — gc is dry-run-only.)
- **Re-parse the stored 4096 excerpt at gate time (instead of persisting `structured_json`).** Rejected: cargo summaries sit past 4096; re-parsing a redacted/truncated excerpt is fragile. Parse the full output at capture, persist the numeric outcome.
- **Fold tamper into `CHECK_NOT_PASSED`.** Rejected: `--allow-unverified` (a policy bypass) would then also bypass integrity. A distinct `EVIDENCE_TAMPERED` keeps the semantics correct.
- **Legacy/tampered by `created_at_ms` timestamp.** Rejected: attacker-mutable → backdate-to-launder. Use a recorded `rowid` high-water marker.
- **A `StructuredParser` trait + registry.** Deferred: two match-dispatched functions behind `parsers.rs` give the same confinement with no indirection; add the trait when a third runtime-polymorphic consumer exists.
- **A new crypto/hash crate or a Merkle tree.** Rejected: `sha2` is in-tree and sufficient; custom crypto banned; O(n) re-walk is fine at v0; signing/Merkle are deferred.

---

## Phased Delivery

### Phase A — Substrate (U1, U2)
Digest primitives + migration 004 (columns + high-water marker + full head-bump reconciliation). Suite goes green at head 4.

### Phase B — Trustworthy capture (U3, U4, U5)
Evidence gains a verifiable hash chained at all three op sites, an actor, a real trust claim, hardened redaction, and numeric structured outcomes. Chain + redaction land together (the hash is over the redacted bytes).

### Phase C — Structured + integrity gate (U6, U7)
The additive structured gate and the load-bearing integrity verification at check/accept/export + the typed error. Where Phase 5 "bites."

### Phase D — Detection & eval (U8)
`doctor`'s chain pass (evidence + decision + head-truncation handling), the e2e tamper check, and the multi-process no-fork proof.

---

## Sources & References

- **Origin:** `docs/ROADMAP.md` (Phase 5), ticket NER-136, `docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md` (the wedge).
- **Carry-over solution docs:** `docs/solutions/architecture-patterns/content-bound-gate-engine-and-failclosed-enforcement-2026-05-29.md`, `schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md`, `write-binding-verification-and-content-backend-isolation-2026-05-29.md`, `sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`, `crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md`.
- **Phase 4 context:** `docs/plans/completed/2026-05-29-009-feat-phase-4-declarative-check-engine-plan.md`, `docs/code-reviews/2026-05-29-ner-135-phase-4.md`.
- **Related code:** `crates/forge-store/src/{lib.rs,error.rs,migrations.rs}`, `crates/forge-evidence/src/lib.rs`, `crates/forge-policy/src/lib.rs`, `crates/forge-content/src/lib.rs`, `crates/forge-cli/src/{main.rs,schema.rs}`, `crates/forge-cli/tests/forge_schema.rs`, `scripts/e2e-eval.sh`.
- **External:** NIST SP 800-185 (TupleHash length-prefix encoding); detect-secrets / gitleaks / trufflehog entropy thresholds + allowlist heuristics; `rust-lang/regex` (linear-time matching).
