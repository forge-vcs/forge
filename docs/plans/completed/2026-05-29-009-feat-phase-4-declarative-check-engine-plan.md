---
title: "feat: Phase 4 — Declarative multi-gate check engine (NER-135)"
type: feat
status: completed
date: 2026-05-29
deepened: 2026-05-29
origin: docs/ROADMAP.md (Phase 4) + Linear NER-135
---

# feat: Phase 4 — Declarative multi-gate check engine (NER-135)

## Summary

Replace the 24-line `forge_policy::evaluate(latest_exit_code)` with a declarative, content-bound, multi-gate engine that aggregates over the **proposed snapshot's full evidence set**. A green check becomes "the required, *named* verifications passed on THIS exact tree." Gates are declared per-intent at `forge start --require "<cmd>"`, persisted on the intent, and inherited by every competing attempt; staleness moves wholly into `forge-policy`; `forge check` emits per-gate verdicts; and `forge accept` requires a passing check by default (escape hatch: `--allow-unverified`), enforced **inside the accept transaction** so the gate cannot be raced.

> **Doc-review (2026-05-29):** five personas (coherence, feasibility, scope-guardian, security-lens, adversarial) reviewed this plan; their 75+-confidence findings are folded in. The most consequential: migration-bump test-pin enumeration (U2/U6), moving the accept gate in-txn (U3/U5), dropping `gates_json` persistence as Phase-6 creep, redacting the error `unmet` list, and honest residual statements about what the gate does *not* bind (environment, cwd, executable contents). See **Open Questions → Resolved During Planning** for the full disposition, including one rejected finding.

---

## Problem Frame

Today's "check engine" is a single `exit==0` comparison against the **single latest** evidence row (`forge_policy::evaluate` at `crates/forge-policy/src/lib.rs:9-24`, fed by `latest_evidence_on` at `crates/forge-store/src/lib.rs:1804`). Two trust-destroying footguns follow:

1. **`run -- true` satisfies any intent.** No gate names *which* command must pass, so any zero-exit command flips the check green.
2. **`run -- echo ok` after a failing `cargo test` flips a failing gate green.** Because the verdict reads only the latest evidence row, a newer trivial success masks an older real failure on the *same* snapshot.

For the competing-attempts wedge (an agent self-selecting a winning attempt — see origin: `docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md`), a green check is the thing that authorizes selection. If the gate is bypassable by the agent itself, the wedge is untrustworthy. Phase 4 hardens the gate to be **content-bound and un-bypassable-by-trivial-command** — explicitly *not* tamper-proof, and explicitly *not* binding the execution environment (see Scope Boundaries / Honesty Note).

> *Note: the ROADMAP Phase 4 bullet names this function `latest_evidence_for_attempt lib.rs:1471-1496`; that name/line is stale — the live single-row read consumed by `record_check` is `latest_evidence_on` at `~1804` (line 1471-1496 is now `gc_dry_run`). This plan's references are authoritative.*

---

## Requirements

- R1. A declarative check spec, persisted **per-intent**, lists the command gates that must pass (e.g. `cargo test` AND `cargo clippy`), replacing `forge_policy::evaluate`.
- R2. The check aggregates over the **proposed snapshot's full evidence set**, not the single latest row — closing the `run -- echo ok` footgun.
- R3. Each gate binds to a specific **command identity** (program + args), so a gate names *which* command must pass — defeating the `run -- true` bypass.
- R4. **Snapshot-staleness lives in `forge-policy`** (the single source of truth); `record_check` no longer computes stale itself.
- R5. `forge check` JSON emits **per-gate verdicts** (`passed`/`failed`/`missing`/`stale`).
- R6. `forge accept` **requires a passing check by default**, configurable via `--allow-unverified` (which emits a `warnings[]` entry). The gate is evaluated **inside the accept IMMEDIATE transaction** (preserving the NER-132 U2 determining-read TOCTOU closure).
- R7. The contract stays additive: any new error code lands as a typed `ForgeError` variant on **every** registry surface; `schema_version` stays `forge.cli.v0`.
- R8. All M1 (Phase 1a/1b) and M2 (Phase 2/3) invariants are preserved — no regression of WAL/IMMEDIATE/busy-retry, advisory-lock acquire-once-never-nested, store-before-DB, the in-txn determining-read TOCTOU closure, migrate-at-command-boundary, typed-error-via-anyhow-downcast, secret-risk exclusion.
- R9. **Default mode (no declared gates) is an intentional behavior change**, not pure back-compat: instead of single-latest-evidence, it aggregates over the proposed snapshot's distinct command identities (latest-per-identity must all pass, ≥1 required). This closes the `echo ok` footgun for *undeclared* intents while reproducing the three existing `forge_propose_check.rs` outcomes (passed / missing / stale). A lone `run -- true` still passes the default — the acknowledged trivial case (Honesty Note).

**Origin actors:** A1 (human developer), A2 (local coding agent — must drive via explicit IDs + named gates), A3 (Forge CLI).
**Origin flows:** F1 (competing attempts under one intent — gates must be identical across attempts for fair comparison), F4 (human compares candidates — per-gate verdicts feed the comparison surface).

---

## Scope Boundaries

- **NOT tamper-proof.** Evidence stays plain mutable SQLite; editing `exit_code 7→0` re-evaluates green. Anti-gaming hash-chaining is Phase 5 (NER-136); signing is Phase 9.
- **The gate binds (program, args, snapshot_id, exit_code) only — NOT the execution environment, cwd, or executable contents** (Honesty Note, from adversarial review):
  - A same-identity re-run that passes due to an *environment* change (env vars, out-of-tree config, a flaky external dependency resolving) flips the gate green even though the snapshotted tree is unchanged. "Latest-matching-wins" is what enables legitimate same-tree re-runs and is the accepted v0 behavior.
  - String-identity binding can be satisfied by a same-name command that does **not** do the work (a `cargo` shim earlier on PATH that exits 0; `cargo test` with an env that neuters it). This is reachable **without** DB tampering, so it is a known residual of the v0 trust model — distinct from the Phase-5 tamper concern.
  - Executable-hash / environment binding is future work (Phase 5+). The v0 claim is *content-bound and un-bypassable-by-trivial-command*, not "un-bypassable-by-same-name-non-trivial-command."
- **No structured result parsers.** Gates evaluate on `exit_code` only — no "0 failing tests" parsing of stdout. Phase 5.
- **No actor/identity model** on the accept decision — Phase 5.
- **No check-spec mini-language.** The spec is a flat, ANDed list of `{program, args}` command-gates plus snapshot-freshness — deliberately NOT Turing-complete (no conditionals, globs, OR-trees, env-matching). ANDing all declared gates is the only combinator.
- **Per-gate verdicts are emit-only, not persisted, in v0.** They are computed and returned in the `check` JSON response; they are NOT written to a DB column. Phase 6 (NER-137) adds a `gates_json` column when it actually consumes per-gate history (additive ALTER, same cheap pattern) — persisting now would be a forward-compat trap for an unbuilt consumer.

### Deferred to Follow-Up Work

- **Mutating an intent's gates after creation** (e.g. `forge check spec set`): v0 declares gates only at `forge start`; to change them, start a new intent. Follow-up.
- **Gate args containing whitespace** (e.g. `--require "sh -c 'a b'"`): v0 `--require` whitespace-tokenizes the value into program+args; quoted-arg parsing is deferred. Documented limitation; gate-declaration may reject obviously-malformed values but does not shell-parse.
- **Persisted per-gate results consumed by a compare/rank surface**: Phase 6 (NER-137) adds + reads `check_results.gates_json`. This plan only emits them in the response.
- **Environment / executable-hash gate binding**: Phase 5+ (the residuals named in the Honesty Note above).

---

## Context & Research

### Relevant Code and Patterns

- `crates/forge-policy/src/lib.rs` — `evaluate` (the 24-line function being replaced) + `CheckEvaluation` (removed; grep all `forge_policy::evaluate` / `CheckEvaluation` call sites — currently only `record_check`).
- `crates/forge-store/src/lib.rs`:
  - `record_check` (~957) — current in-txn single-latest-evidence verdict; the rewrite target. Resolves attempt+proposal *before* the txn, reads evidence *on `tx`* — the rewrite keeps exactly this shape (aggregate read still on `&tx`), preserving the NER-132 U2 TOCTOU closure.
  - `decide` (~1036) — shared by accept/reject; the in-txn accept evidence-gate is added here (gated by a new param, off for reject).
  - `latest_evidence_on` (~1804) — single-row read; kept for `record_evidence` snapshot-attribution and `show`'s display, but **not** used by the new `record_check`.
  - `resolve_proposal` (~1830), `proposal_by_id` — proposal resolution; `snapshot_id`/`proposal_revision_id` already available.
  - `record_evidence` (~812) — shows how evidence binds `snapshot_id` (latest snapshot at run time) + stores `command`/`args_json`/`exit_code`. Command identity = (program=argv[0], args=argv[1..]) per `forge-evidence` `capture_with_timeout`.
  - `create_attempt`/`start_attempt`/`start_attempt_for_intent` (~525-645) — the intent INSERT to thread `check_spec_json` into (only when minting a new intent; `attempt start --intent` references an existing intent and inherits its gates). `AttemptRecord.intent_id` is selected by `attempt_by_id` and is in-scope inside `record_check`/`decide`.
- `crates/forge-store/src/migrations.rs` — `MIGRATIONS` const, `schema_head` (= `MAX(version)`, auto-becomes 3), statement-by-statement runner that tolerates `duplicate column name`. **Inline `#[cfg(test)]` tests pin version 2** (`schema_head_is_max_version`, `fresh_apply_reaches_head_with_checksums`, `unknown_future_version_refuses` — the last self-adjusts via `schema_head()+1`).
- `crates/forge-store/tests/migrate.rs` — integration migration tests; **pin `max_version==2`** at ~131/143/149 and stamp a `(3,"future")` "ahead" fixture (`head_plus_one_is_refused`) that must move to version 4.
- `crates/forge-store/migrations/002_columns.sql` — additive-ALTER precedent for `003_check_spec.sql`.
- `crates/forge-store/src/error.rs` — `ForgeError` enum + `code`/`details`/`retryable`/`after_ms`/`Display`/`error_registry` + drift-guard tests (`registry_covers_every_variant` exhaustive match + `all` array; `codes_match_the_pre_change_registry`).
- `crates/forge-content/src/lib.rs` — `redact_secret_like_text` (key=value/key:value redaction), `is_secret_risk_path`, `is_ignored_by_policy` (excludes `.forge/` from snapshot/export — the at-rest mitigation for the new column).
- `crates/forge-cli/src/schema.rs` + `crates/forge-cli/tests/forge_schema.rs` (`FORGE_ERROR_CODES`) — published registry + drift guard + `notes.secret_protection`.
- `crates/forge-cli/tests/` — every `forge accept` caller that must be updated (the new evidence gate blocks accept without a passing check): `forge_propose_check.rs`, `forge_accept_export.rs`, `forge_attempts.rs` (~270), `forge_secret_export.rs`, `forge_conflict_set.rs` (~91/124, `prepare_proposal` runs no `run`), `forge_pr_body.rs` (~26), `forge_errors.rs`, `forge_concurrency.rs`, and `scripts/e2e-eval.sh`.

### Institutional Learnings

- **schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md** — (§1) additive ALTER statement-by-statement, tolerate `duplicate column name`, grandfather NULL; (§2) a convergence test must reconstruct the *real* historical shape (here: a v2 DB ALTER'd to v3 vs fresh-init-v3) — **and the migration-bump must update every hard-coded version-2 pin**, the exact "green test on a plausible function vs the real caller/test graph" lesson; (§5) verify the fix lands on the real CLI path.
- **write-binding-verification-and-content-backend-isolation-2026-05-29.md** — (§6) a new error code is additive only when it lands on *every* contract surface in one change; the drift-guard tests exist to fail when one is missed.
- **crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md** — (§1) advisory lock acquire-once-never-nested — the new in-txn reads run inside already-locked mutating commands; (§2) the in-txn determining-read closes the TOCTOU against the lock-free `run` writer. **Both `record_check` and the new accept gate must read facts on `&tx`, not a separate connection.**
- **sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md** — `ORDER BY created_at_ms DESC, rowid DESC` tiebreak carries into the aggregate fact ordering used to pick "latest matching per gate."

### External References

None — self-contained Rust/SQLite change with strong local patterns (5 prior phase plans). No external research dispatched.

---

## Key Technical Decisions

- **Gate identity = (program, args) exact string equality.** Evidence stores `command`=argv[0], `args`=argv[1..]; a declared gate `cargo test` → `{program:"cargo", args:["test"]}`. Environment/cwd/executable-hash binding deferred (Honesty Note). Minimal, defeats `run -- true`, no tamper-evidence claim.
- **Spec persisted per-intent (`intents.check_spec_json`), declared at `forge start --require`.** Competing attempts under one intent inherit identical gates → fair Phase 6 compare/rank (origin F1). Chosen by the user over ad-hoc `check --require` and a `.forge/checks.toml` config file.
- **Unifying verdict rule: latest-matching-evidence-per-gate-on-the-proposed-snapshot.** For each gate, consider evidence whose identity matches; the **latest** such row *on the proposed snapshot* (by `created_at_ms`,`rowid`) decides `passed`/`failed`. Handles legitimate same-tree re-runs (latest wins) while `echo ok` (a different identity) cannot interfere with the `cargo test` gate.
- **Per-gate verdict taxonomy:** `passed` (latest matching on proposed snapshot exit 0) · `failed` (latest matching on proposed snapshot nonzero) · `stale` (matching evidence exists but only on a *different* snapshot) · `missing` (no matching evidence anywhere). **Overall precedence:** `failed` > `missing` > `stale` > `passed` for the **declared-gate** case (the rollup); per-gate detail is authoritative. The **default-mode zero-declared-gate** case is separate: synthesized gates are all `stale` when evidence exists only off-snapshot (→ overall `stale`), and there are no gates to be `missing`, so "no evidence at all → `missing`" is reached directly, not via the rollup. The HLD sketch annotates this distinction explicitly so the two precedence signals don't read as a contradiction.
- **Default mode (no declared gates) = aggregate over observed identities** (R9). Synthesize one implicit gate per distinct command-identity observed on the proposed snapshot; pass iff ≥1 exists and all pass. Evidence only on a prior snapshot → `stale` (reason string contains `"does not match proposal revision"`, preserved for the existing test); none at all → `missing`.
- **Staleness computed entirely in `forge-policy`** (R4). `record_check` passes the proposed `snapshot_id` + all facts to `forge_policy::evaluate`.
- **Accept evaluates the gate IN-TXN, inside `decide`'s IMMEDIATE transaction** (revised per review). A shared store helper `evaluate_check_on(conn, …)` reads facts on the *same* connection that commits the decision, so a concurrent lock-free `run` cannot make the gate disagree with the committed decision — the same NER-132 U2 closure `record_check` uses. (The earlier "own-connection pre-flight read" is rejected: it left a TOCTOU window violating R8.) Accept re-evaluates rather than trusting a stored `check_results` row; re-evaluation is **authoritative by design** — if it disagrees with the last `forge check` row (newer evidence since), the accept verdict wins, and `CHECK_NOT_PASSED.details` / the bypass warning makes the divergence legible.
- **Per-gate verdicts are emit-only** (revised per review): returned in the `check` response (`CheckRecord.gates`), not persisted. Migration 003 adds only `intents.check_spec_json`.
- **Gate identities surfaced in machine output are redacted** (security-lens): the `CHECK_NOT_PASSED` `unmet` list runs each entry through `forge_content::redact_secret_like_text` before populating `details`, mirroring the evidence stdout/stderr redaction — closing the new "gate-spec declared-but-never-executed" leak surface that the evidence path (which requires execution + already redacts) does not cover.
- **New error code `CHECK_NOT_PASSED`** for a blocked accept; `--allow-unverified` is the configurable bypass and emits a `warnings[]` entry carrying the observed status.

---

## Open Questions

### Resolved During Planning

- *Where do gates live / how declared?* → Per-intent, at `forge start --require` (user-selected).
- *Persist per-gate results?* → **No (revised).** Emit-only in v0; Phase 6 adds the column when it consumes it (drops a forward-compat trap; scope-guardian P1).
- *Accept gate: own-connection re-eval or in-txn?* → **In-txn (revised).** A separate-connection read left a TOCTOU window against the lock-free `run` writer, violating R8's named NER-132 U2 invariant (scope-guardian/feasibility/adversarial). The gate now runs inside `decide`'s IMMEDIATE txn via a `&Connection`-taking helper shared with `record_check`.
- *Default mode behavior?* → Intentional behavior change to aggregate-over-snapshot (R9), documented; reproduces the three existing outcomes. **Rejected** scope-guardian's "fall back to single-latest-evidence for NULL spec": that re-opens the `failing-test-then-echo-ok` footgun for undeclared intents, violating an exit criterion. The aggregate default is strictly better for the criterion.
- *Secret leak via gate identities?* → Redact the `unmet` list via the existing helper; `.forge/forge.db` exclusion (`is_ignored_by_policy`) is the at-rest mitigation for `check_spec_json`; update `schema.notes.secret_protection` to name the argv/gate-spec surface (security-lens P1/P3).
- *Migration-bump blast radius?* → Enumerated: every version-2 pin (`migrations.rs` inline tests, `tests/migrate.rs`, `e2e-eval.sh`) + the `head_plus_one` "future" fixtures bump to 4 (feasibility P1/100 ×2). In U2/U6.

### Deferred to Implementation

- Exact `EvidenceFact` field set + whether `forge-policy` sorts internally or the store pre-sorts. Favor: store `SELECT … ORDER BY created_at_ms DESC, rowid DESC` (alias `rowid` as the struct's tiebreak field), engine takes first-match-per-identity. Keep the field name consistent between U1's struct and U3's SQL alias (a mismatch is a compile error, not a silent bug).
- Whether `--allow-unverified` lives on a dedicated `AcceptArgs` struct (favored — keeps the flag off `reject`/`check`) vs extending `ProposalScopedArgs`.
- The representative `check_results.evidence_id` for a multi-gate check (a single FK can't represent N gates) — favor: latest failing gate's evidence_id, else latest passing-on-snapshot, else NULL; documented as best-effort, with `CheckRecord.gates` authoritative.

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

**Per-gate verdict decision (the heart of `forge-policy`):**

```
DECLARED-GATE MODE (spec.gates nonempty):
  for each gate G:
      matching         = facts where (fact.program, fact.args) == (G.program, G.args)
      matching_on_snap = matching where fact.snapshot_id == proposed_snapshot_id
      if matching_on_snap nonempty:
          latest = max(matching_on_snap by (created_at_ms, seq))
          verdict = Passed if latest.exit_code == 0 else Failed   # carry latest.evidence_id, exit_code
      elif matching nonempty:
          verdict = Stale          # ran, but only on another tree
      else:
          verdict = Missing
  overall = Failed  if any gate Failed
          else Missing if any gate Missing
          else Stale  if any gate Stale
          else Passed  # all gates Passed (>=1 gate by construction)

DEFAULT MODE (spec.gates empty) — separate, does NOT use the rollup above:
  facts_on_snap = facts where snapshot_id == proposed_snapshot_id
  if facts_on_snap empty:
      overall = Stale if (any facts at all) else Missing   # preserves "does not match proposal revision" reason
      gates   = []   (or observed off-snapshot identities marked Stale)
  else:
      synthesize one gate per distinct identity in facts_on_snap; verdict = latest-on-snap pass/fail
      overall = Passed if all synthesized gates Passed else Failed
```

**Lifecycle flow:**

```mermaid
sequenceDiagram
    participant CLI as forge CLI
    participant Store as forge-store
    participant Policy as forge-policy
    CLI->>Store: start --require "cargo test" --require "cargo clippy"
    Store->>Store: persist gates on intents.check_spec_json (redacted)
    CLI->>Store: run -- cargo test  (evidence bound to latest snapshot)
    CLI->>Store: propose  (binds proposal -> snapshot_id)
    CLI->>Store: check
    Store->>Store: IMMEDIATE txn: load intent spec + ALL evidence facts on &tx
    Store->>Policy: evaluate(spec, proposal.snapshot_id, facts)
    Policy-->>Store: CheckOutcome{status, reason, gates[]}
    Store->>Store: persist check_results(status, reason, evidence_id)  // gates emit-only
    Store-->>CLI: CheckRecord{..., gates[]}
    CLI->>Store: accept  (decide: same in-txn evaluate; require passed unless --allow-unverified)
    Store-->>CLI: CHECK_NOT_PASSED (unmet redacted) if not passed
```

---

## Implementation Units

### U1. Redesign the `forge-policy` engine into a declarative multi-gate evaluator

**Goal:** Replace `evaluate(Option<i64>)` with a pure, fully-unit-tested engine that takes a spec + proposed snapshot id + all evidence facts and returns an overall status plus per-gate verdicts. Zero store/IO dependencies — the heart of the phase.

**Requirements:** R1, R2, R3, R4, R5, R9.

**Dependencies:** None.

**Files:**
- Modify: `crates/forge-policy/src/lib.rs`
- Test: `crates/forge-policy/src/lib.rs` (`#[cfg(test)] mod tests`) — pure unit tests.

**Approach:**
- Public types: `Gate { program: String, args: Vec<String> }`, `CheckSpec { gates: Vec<Gate> }` (empty = default mode), `EvidenceFact { evidence_id, program, args, exit_code: i64, snapshot_id: Option<String>, created_at_ms: i64, seq: i64 }` (`seq` = rowid tiebreak — name it identically to the field U3's SQL maps `rowid` into), `GateVerdict` (`Passed`/`Failed`/`Missing`/`Stale`, serde snake_case), `GateResult { program, args, verdict, evidence_id: Option<String>, exit_code: Option<i64> }`, `CheckOutcome { status: String, reason: String, gates: Vec<GateResult> }`.
- `pub fn evaluate(spec: &CheckSpec, proposed_snapshot_id: &str, facts: &[EvidenceFact]) -> CheckOutcome` per the HLD sketch — declared-gate rollup and default-mode path are distinct code paths.
- Preserve a stale-default reason containing `"does not match proposal revision"`.
- Remove `CheckEvaluation`; the store is the sole consumer.

**Patterns to follow:** `ORDER BY created_at_ms DESC, rowid DESC` "latest" tiebreak (sqlite-concurrency solution doc §5) — mirror in `max by (created_at_ms, seq)`.

**Test scenarios:**
- Happy path — single declared gate `cargo test` with a passing fact on the proposed snapshot → overall `passed`, gate `passed`.
- Edge — declared gate's only matching fact is on a different snapshot → gate `stale`, overall `stale`.
- Edge — declared gate with no matching fact at all → gate `missing`, overall `missing`.
- Error path (`run -- true` defeat) — declared gate `cargo test`, evidence only for `true` (exit 0) on the proposed snapshot → `cargo test` gate `missing` → overall `missing`.
- Error path (footgun) — declared gate `cargo test` with `cargo test`→7 then `echo ok`→0 on the same snapshot → gate `failed` (latest matching `cargo test` is 7) → overall `failed`.
- Edge — two declared gates, one passes one fails → overall `failed`; both gate verdicts present.
- Edge — legitimate same-tree re-run: `cargo test`→7 then `cargo test`→0 on the same snapshot → gate `passed` (latest matching wins).
- Default mode — single `true`→0 on proposed snapshot, no declared gates → overall `passed` (trivial case).
- Default mode (footgun) — `sh -c "exit 7"`→7 and `echo ok`→0 on proposed snapshot, no declared gates → overall `failed`.
- Default mode — evidence only on a prior snapshot → `stale` with the `"does not match proposal revision"` reason; none at all → `missing`.
- Precedence — declared gates yielding {failed, missing} → overall `failed`; {missing, stale} → overall `missing`.

**Verification:** `cargo test -p forge-policy` passes; no `forge-store`/IO dependency; all four verdicts reachable.

---

### U2. Migration 003 — `intents.check_spec_json` + the full schema-version-bump test sweep

**Goal:** Add the one additive nullable column, register migration 3, and update **every** hard-coded version-2 pin + the "future version" fixtures so the bump does not break CI. (Per-gate verdicts are emit-only — no `gates_json` column.)

**Requirements:** R1, R7.

**Dependencies:** None.

**Files:**
- Create: `crates/forge-store/migrations/003_check_spec.sql`
- Modify: `crates/forge-store/src/migrations.rs` (append `(3, "003_check_spec", include_str!(...))`; update inline tests `schema_head_is_max_version` → 3 and `fresh_apply_reaches_head_with_checksums` → len 3 / version[2]==3; `unknown_future_version_refuses` self-adjusts — confirm, don't edit)
- Modify: `crates/forge-store/tests/migrate.rs` (update `max_version==2` assertions at ~131/143/149 → 3; bump the `head_plus_one_is_refused` fixture to stamp version **4** and assert db_version==4 / supported_head==3)
- Modify: `scripts/e2e-eval.sh` (`:61` doctor `schema_version=2`→`3`; `:76` versions `1,2`→`1,2,3`; `:81` "ahead" fixture INSERT version `3`→`4`)
- Test: `crates/forge-store/tests/migrate.rs` + the inline `src/migrations.rs` tests.

**Approach:**
- `003_check_spec.sql`: `ALTER TABLE intents ADD COLUMN check_spec_json TEXT;` (nullable → NULL = no gates = default mode; grandfathered on existing rows).
- `schema_head()` auto-becomes 3; the statement-by-statement runner tolerates `duplicate column name`.

**Patterns to follow:** `002_columns.sql` (additive ALTER); migration solution doc §1 (per-statement, tolerate duplicate column, grandfather NULL) + §2 (convergence; the version-pin sweep is the "real test graph, not a plausible green" lesson).

**Test scenarios:**
- Happy path — fresh init applies 1→3; `intents` has `check_spec_json`.
- Convergence (§2) — a v2 DB (001+002) `migrate()`d to v3 is schema-identical to fresh-init v3; both report head 3.
- Edge — re-running `migrate()` on a v3 DB is a no-op (no `duplicate column` brick).
- Edge — `head_plus_one_is_refused` now stamps version 4 and still refuses with `SCHEMA_VERSION_UNSUPPORTED` (db_version 4, supported_head 3).

**Verification:** `cargo test -p forge-store` (inline + integration) green; `bash scripts/e2e-eval.sh` passes the `schema_version=3` and "ahead refuses" checks; `forge doctor` reports no schema mismatch on a fresh repo.

---

### U3. Store wiring — persist spec on intent, shared in-txn evaluator, rewrite `record_check`

**Goal:** Wire the store to (a) persist (redacted) gates on the intent at start, (b) read all evidence facts in-txn, (c) call `forge_policy::evaluate` via a `&Connection`-taking helper reused by accept, (d) persist status/reason, (e) return per-gate verdicts.

**Requirements:** R1, R2, R3, R4, R5, R8.

**Dependencies:** U1, U2.

**Files:**
- Modify: `crates/forge-store/src/lib.rs`
- Test: covered end-to-end in U6; a focused store test that `record_check` returns `gates` and reproduces default-mode outcomes.

**Approach:**
- **Ownership split (coherence):** U3 adds `check_spec_json: Option<String>` as a parameter on `start_attempt` → `create_attempt` and persists it in the `INSERT INTO intents (…)` (only when minting a new intent). U3's CLI caller passes `None` (a stub); **U5 wires the actual `--require` value.** This keeps U3 buildable/testable without U5.
- `intent_check_spec(conn, intent_id) -> CheckSpec` reading `intents.check_spec_json` (NULL → empty `CheckSpec`).
- `evidence_facts_on(conn, attempt_id) -> Vec<EvidenceFact>` — `SELECT id, command, args_json, exit_code, snapshot_id, created_at_ms, rowid FROM evidence WHERE attempt_id=? ORDER BY created_at_ms DESC, rowid DESC`. **Called on `&tx`** inside the IMMEDIATE txn.
- `evaluate_check_on(conn, repo_id, attempt, proposal) -> CheckOutcome` — loads spec + facts on `conn`, calls `forge_policy::evaluate(&spec, &proposal.snapshot_id, &facts)`. The single source of truth; `record_check` and the accept gate (U5/`decide`) both call it on their `&tx`.
- Rewrite `record_check`: resolve attempt+proposal (unchanged) → inside `with_immediate_retry`, after `replay_guard`: `evaluate_check_on(tx, …)` → INSERT `check_results` (`status`, `reason`, representative `evidence_id`) → keep the op/view insert. **No `gates_json` write.**
- Extend `CheckRecord` with `gates: Vec<forge_policy::GateResult>` (serde). `CheckSummary`/`show` unchanged.

**Patterns to follow:** `record_check`'s existing in-txn determining read (NER-132 U2); `record_evidence`/`propose` IMMEDIATE+replay_guard shape.

**Test scenarios:**
- `record_check` with a NULL `check_spec_json` intent behaves as default mode (back-compat) — passed/missing/stale.
- `record_check` returns populated `gates` and persists `status`/`reason` (no `gates_json`).
- The verdict is computed from facts read on `tx` (no second connection) — guard the NER-132 invariant; do not regress `forge_concurrency.rs`.

**Verification:** `cargo test -p forge-store` passes; `record_check` opens exactly one txn and reads facts on `tx`.

---

### U4. Add `ForgeError::CheckNotPassed` (with redacted `unmet`) across every contract surface

**Goal:** Land `CHECK_NOT_PASSED` additively on all registry surfaces; redact the `unmet` gate-identity list so a secret in a never-executed gate spec doesn't leak through error details.

**Requirements:** R6, R7.

**Dependencies:** U1 (for the gate-identity shape); used by U5.

**Files:**
- Modify: `crates/forge-store/src/error.rs` (enum variant, `code`, `details`, `retryable`, `after_ms`, `Display`, `error_registry` spec, both drift-guard tests: `registry_covers_every_variant`'s `all` array + exhaustive match, and `codes_match_the_pre_change_registry`)
- Modify: `crates/forge-cli/tests/forge_schema.rs` (add `CHECK_NOT_PASSED` to `FORGE_ERROR_CODES`)
- Test: the drift-guard tests + a new `details_redact_secret_like_unmet`.

**Approach:**
- Variant `CheckNotPassed { status: String, unmet: Vec<String> }` — `status` ∈ `failed`/`missing`/`stale`; `unmet` = `"program arg…"` identity strings for non-passed gates.
- `code()` → `"CHECK_NOT_PASSED"`; `retryable()` → false; `after_ms()` → None; `Display` → human message naming status + count.
- `details()` → `json!({ "status": status, "unmet": <each entry passed through forge_content::redact_secret_like_text> })`. **Security (folds the security-lens P1):** unlike evidence (which requires execution and already redacts stdout/stderr), a `--require` gate spec is persisted/surfaced *without execution*, so the engine applies the existing `redact_secret_like_text` to each `unmet` entry. `redact_paths` (filename-level) is the wrong tool here; the key=value redactor catches `--token=…`/`KEY=VAL`.
- Registry spec: `details_keys: &["status", "unmet"]`, retryable false, after_ms None.

**Patterns to follow:** write-binding solution doc §6; the `AttemptWorktreeMismatch` precedent.

**Test scenarios:**
- `registry_covers_every_variant` + `codes_match_the_pre_change_registry` pass with the new variant.
- New `details_redact_secret_like_unmet` — an `unmet` entry `--token=SECRET` (or `GITHUB_TOKEN=abc cargo test`) is redacted in `details["unmet"]`; details keys are exactly `status`+`unmet`.
- `forge_schema.rs` published registry names `CHECK_NOT_PASSED`.

**Verification:** `cargo test -p forge-store` + `cargo test -p forge-cli --test forge_schema` pass; grep confirms the code in `error.rs`, `error_registry`, `FORGE_ERROR_CODES`.

---

### U5. CLI wiring — `start --require`, in-txn accept gate + `--allow-unverified`, check gates, schema notes

**Goal:** Expose the engine: declare (redacted) gates at start, enforce evidence at accept in-txn, surface per-gate verdicts, update published summaries + the secret-protection note.

**Requirements:** R1, R3, R5, R6, R7.

**Dependencies:** U3, U4.

**Files:**
- Modify: `crates/forge-cli/src/main.rs` (`IntentArgs` + `start_response`; new `AcceptArgs` + the accept dispatch/closure; check passthrough via `CheckRecord.gates`; the bypass `warnings[]` entry)
- Modify: `crates/forge-store/src/lib.rs` (thread the accept gate into `decide` — a `require_passing_check: bool` param; evaluate via `evaluate_check_on(tx, …)` inside the existing IMMEDIATE txn; return the observed status so the CLI can warn)
- Modify: `crates/forge-cli/src/schema.rs` (`command_shapes` one-liners for `start`/`check`/`accept`; **extend `notes.secret_protection`** to name the argv/gate-spec surface)
- Test: covered by U6.

**Approach:**
- `IntentArgs` gains `#[arg(long)] require: Vec<String>` (repeatable). `start_response` maps each value: whitespace-tokenize → first token `program`, rest `args` → `Gate`; collect `CheckSpec`; serialize to JSON; pass to `forge_store::start_attempt(...)`. Empty → `None`.
- Accept: dedicated `AcceptArgs { attempt, proposal, #[arg(long)] allow_unverified: bool }`; route `Command::Accept(AcceptArgs)`. The accept closure keeps the existing STALE_BASE pre-check (CLI-layer, under the held lock), then calls `decide(..., require_passing_check = !allow_unverified)`. `decide` evaluates the gate **inside its IMMEDIATE txn** on `tx`; if `status != "passed"` and `require_passing_check` → roll back with `ForgeError::CheckNotPassed { status, unmet }`. When `allow_unverified` and not passed → proceed and the CLI adds a `warnings[]` entry (`"accepted without a passing check (--allow-unverified): status=<status>"`). `reject` keeps `ProposalScopedArgs` (no gate).
- `check` JSON carries `data.gates` once `CheckRecord.gates` exists (U3).
- `schema.rs`: `start` ("…accepts repeatable --require <command> gates"), `check` ("…emits per-gate verdicts passed/failed/missing/stale"), `accept` ("…requires a passing check by default; --allow-unverified bypasses with a warning"); `notes.secret_protection` += "…and command argv strings (including --require gate specs and CHECK_NOT_PASSED.unmet) are redacted for known key=value secret patterns but not otherwise scanned until Phase 5".

**Patterns to follow:** `RunArgs`/`ExportBranchArgs` arg structs; `decision_response`'s STALE_BASE pre-check; `secret_export_warnings` warnings idiom; advisory-lock acquire-once (no second lock — the gate read is inside the existing accept txn).

**Test scenarios:** (in U6)
- `start --require "cargo test" --require "cargo clippy"` persists two gates.
- accept without a passing check → `CHECK_NOT_PASSED`; with `--allow-unverified` → success + warning.
- `--require` absent → default mode unchanged.

**Verification:** `forge --json start "x" --require "cargo test"` persists the spec; `forge --json accept` blocks unverified in-txn; `forge --json schema` lists the updated one-liners + note.

---

### U6. Integration + e2e coverage; update every `forge accept` caller

**Goal:** Prove every exit criterion against the compiled binary, update all existing accept callers for the new gate, and extend the e2e eval.

**Requirements:** R1, R2, R3, R5, R6, R8, R9.

**Dependencies:** U3, U4, U5.

**Files:**
- Modify: `crates/forge-cli/tests/forge_propose_check.rs` (multi-gate + footgun + per-gate-verdict + multi-gate **all-pass** cases; keep the three default-mode back-compat cases green)
- Modify: `crates/forge-cli/tests/forge_accept_export.rs` (run a passing gate before accept, or `--allow-unverified` where the bypass is the subject)
- Modify the other accept callers (grep `accept` across `crates/forge-cli/tests/`): `forge_conflict_set.rs` (`prepare_proposal` ~91/124 — insert `run -- sh -c true` after the final save so evidence binds the proposed snapshot, before accept), `forge_attempts.rs` (~270 — same, after the final save; verify the AMBIGUOUS_PROPOSAL assertion still precedes the gate), `forge_secret_export.rs`, `forge_errors.rs`, `forge_pr_body.rs` (~26 — already runs `sh -c true`; confirm), `forge_concurrency.rs` (runs `run -- true`; confirm)
- Modify: `scripts/e2e-eval.sh` (the accept step + the schema-version pins from U2)
- Test: the files above.

**Approach:** Behavior-focused integration tests on the existing `TestRepo`/`assert_cmd` harness (real temp git repos). Prefer running a realistic passing gate before accept over sprinkling `--allow-unverified`, except where a test specifically asserts the bypass.

**Test scenarios:**
- Exit-criterion — declared gate (`sh -c true` / `sh -c "exit 7"` stand-ins), `run -- true` only → check non-passed; the `true` gate cannot satisfy the named gate.
- Exit-criterion — failing gate then `run -- echo ok` on the same snapshot → check stays `failed`.
- Exit-criterion — two declared gates, **both pass** via the binary → overall `passed` AND `data.gates` shows both `passed` (the all-pass half of the biconditional; adversarial P2).
- Exit-criterion — two declared gates, one fails → overall `failed`.
- Exit-criterion — `check --json` `data.gates[]` carries per-gate `verdict` ∈ {passed, failed, missing, stale}.
- Exit-criterion — accept with a non-passing check → `errors[0].code == "CHECK_NOT_PASSED"`; accept `--allow-unverified` → success + `warnings[]`.
- Accept-vs-stored-check divergence (adversarial P2) — `check` passes (row persisted), then a failing `run` on the same snapshot → `accept` re-evaluates and blocks with `CHECK_NOT_PASSED` (re-eval is authoritative).
- Back-compat — the three existing `forge_propose_check.rs` cases pass under default mode; the existing accept→export flow works once a passing gate is run first.
- e2e — `scripts/e2e-eval.sh` drives `start --require … → run (pass) → propose → check (passed, gates emitted) → accept → export`.

**Verification:** `bash scripts/ci.sh` green (fmt + `cargo test --workspace` + clippy `-D warnings` + the e2e eval).

---

## System-Wide Impact

- **Interaction graph:** `forge start` (now persists gates) → `forge run` (evidence) → `forge propose` (binds snapshot) → `forge check` (new aggregate engine, emit-only gates) → `forge accept` (new in-txn evidence gate) → `forge export branch` (transitively gated via "accepted"). `forge-policy` gains the store as its sole caller; `latest_evidence_on` remains for `record_evidence`/`show`.
- **Error propagation:** `CHECK_NOT_PASSED` rides the typed-error-via-anyhow-downcast path; deterministic/non-retryable; follows the normal status-aware replay contract under `--request-id`.
- **State lifecycle risks:** none new — both the check verdict and the accept gate are computed in-txn on `&tx` (no separate-connection TOCTOU); accept holds the repo lock once (no nested lock).
- **API surface parity:** additive (`CheckRecord.gates`, `CHECK_NOT_PASSED`, `--require`/`--allow-unverified`); `schema_version` stays `forge.cli.v0`.
- **Integration coverage:** footgun closures, multi-gate all-pass, accept divergence, and back-compat are proven end-to-end (U6); `forge-policy` unit tests (U1) prove the engine in isolation.
- **Unchanged invariants:** WAL/IMMEDIATE/busy-retry, advisory-lock acquire-once-never-nested, store-before-DB, UUIDv7 + rowid tiebreaks, migrate-at-command-boundary + unknown-future-version refusal, secret-risk path exclusion in both backends, `--request-id` idempotency, the `run` §10.6 lock carve-out (the check/accept TOCTOU closure relies on the in-txn read, not the lock).

---

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Over-designing the spec into a mini-language (the named Phase 4 risk) | Flat ANDed list of `{program, args}`; no conditionals/globs/trees. Scope Boundaries + scope-guardian review. |
| Migration bump breaks unenumerated version-2 pins → CI red | U2 enumerates every pin (`migrations.rs` inline tests, `tests/migrate.rs`, `e2e-eval.sh`) + bumps the `head_plus_one` fixtures to 4. Treated as in-scope bookkeeping. |
| Adding the accept gate breaks existing accept callers | U6 audits every `forge accept` caller (grep) and inserts a passing gate before accept; in-scope. |
| Accept gate re-opens a TOCTOU vs the lock-free `run` writer | Gate evaluated inside `decide`'s IMMEDIATE txn on `&tx` (not a separate connection) — same NER-132 U2 closure as `record_check` (R8). |
| Default-mode change silently alters existing semantics | R9 documents it as intentional; U6 proves the three existing outcomes still hold (incl. the exact stale reason substring). |
| New error code drifts out of the published contract | U4 lands it on every surface; the `error.rs` drift-guard tests + `FORGE_ERROR_CODES` fail if missed (solution doc §6). |
| Secret in a never-executed `--require` gate spec leaks via error details / at-rest | `unmet` runs through `redact_secret_like_text`; `.forge/forge.db` exclusion (`is_ignored_by_policy`) is the at-rest mitigation; `schema.notes.secret_protection` updated. Full arg-level scanning is Phase 5. |
| Gate green despite unchanged tree (environment flip / same-name hollow command) | Named honestly in Scope Boundaries as accepted v0 residuals (binding is (program,args,snapshot,exit) only); not claimed as un-gameable. Phase 5+ for env/exec-hash binding. |

---

## Documentation / Operational Notes

- On merge, flip the plan to `status: completed` and move to `docs/plans/completed/` (CLAUDE.md); the ROADMAP is the source spec and is not edited here.
- `forge schema` self-documents the new flags/code/notes.
- `/ce-compound` after merge: capture the unifying "latest-matching-per-gate-on-proposed-snapshot" rule, the default-mode back-compat-vs-behavior-change technique, the migration-bump version-pin sweep, and the "accept gate must be in-txn, not own-connection" TOCTOU lesson.

---

## Sources & References

- **Origin:** `docs/ROADMAP.md` (Phase 4) + Linear NER-135 (milestone "M2 — Fund the agent-native wedge").
- Requirements context: `docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md`.
- Solution docs: schema-migration-reconciliation-and-typed-error-contract, write-binding-verification-and-content-backend-isolation, crash-correctness-advisory-lock-and-atomic-restore, sqlite-multiprocess-concurrency-and-idempotent-replay (all `docs/solutions/architecture-patterns/…-2026-05-29.md`).
- Related code: `crates/forge-policy/src/lib.rs`, `crates/forge-store/src/lib.rs` (`record_check`, `decide`, `latest_evidence_on`, `create_attempt`), `crates/forge-store/src/error.rs`, `crates/forge-store/src/migrations.rs`, `crates/forge-store/tests/migrate.rs`, `crates/forge-cli/src/main.rs`, `crates/forge-cli/src/schema.rs`, `scripts/e2e-eval.sh`.
- Prior phase plans: `docs/plans/completed/2026-05-29-007-feat-phase-2-migrations-typed-errors-plan.md`, `…-008-feat-phase-3-write-binding-verification-plan.md`.
