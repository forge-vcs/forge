---
title: "feat: Phase 2 — numbered migration runner, typed ForgeError contract, launch-blocker gates (NER-133)"
type: feat
status: completed
date: 2026-05-29
deepened: 2026-05-29
origin: docs/handoffs/2026-05-29-ner-133-phase-2-kickoff.md
---

# feat: Phase 2 — numbered migration runner, typed ForgeError contract, launch-blocker gates (NER-133)

## Summary

NER-133 is M1's last open track. It makes Forge's schema safely evolvable and its agent-facing contract stable-by-construction, and lands the two cheap PRD §27 launch blockers before any egress path exists. Concretely: replace the imperative `apply_migrations` with a numbered `.sql` runner gated under Phase 1b's advisory write lock (per-migration checksums, unified fresh-init/upgrade paths, read-only refuse on unknown-future versions); introduce a typed `ForgeError` enum that retires the substring-matched `error_code()` **and the CLI-layer `bail!` string contracts** and classifies retryability for the whole error set at once; populate the always-stubbed `errors[].details` / `retry.retryable` / `warnings[]` envelope fields; publish a versioned machine contract via `forge schema` / `--capabilities`; gate evidence/content export on secret-risk by default **at the point where bytes actually flow**; and persist a `conflict_sets` row on every stale-base bail. The `forge.cli.v0` envelope version is **retained** — all changes are additive or fill previously-provisional fields (no code renamed or removed; the first published machine contract is being authored here, so there is no prior contract to break).

> **Doc-review note (2026-05-29):** this plan was strengthened by a 5-persona doc-review pass (coherence, feasibility, security-lens, scope-guardian, adversarial). Their findings are folded in below — most consequentially: the typed-error work must also convert the **CLI-layer** `bail!` sites (not just `forge-store`), or the pinned codes regress to `COMMAND_FAILED`; and the secret-export gate must enforce at the tree-rewrite / `git add` / `pr_body` boundary, not at a path-list filter that the git backend never consults.

---

## Problem Frame

The substrate is now crash-correct (Phase 1a/1b, merged), but two trust holes remain before any publish/egress path is built. First, schema evolution is imperative and divergent: `apply_migrations` hard-codes a single embedded migration plus two unconditional `ALTER`s, so a fresh init and an upgraded DB reach the same columns by *different* genesis paths with no test proving they converge, and there is no mechanism for "add a schema change = drop in a numbered file." Second, the machine contract is decoded from free-text error messages by a substring ladder (`error_code()`) plus CLI `bail!` strings, `errors[].details` is empty, `warnings[]` is never populated, and `retry.retryable` is uniformly `false` for every error — so an agent cannot reliably distinguish a transient `LOCK_TIMEOUT` it should retry from a deterministic domain failure it must not. Two specific defects flow from this (filed as NER-133 Linear comments): `LOCK_TIMEOUT` ships `retryable=false`, and a transient CAS conflict recorded under a `--request-id` poisons that id so a later retry replays the stale failure. The launch blockers (default-deny secret export; conflict-set persistence) are cheap to land now and load-bearing once egress exists.

---

## Requirements

- R1. **Numbered `.sql` migration runner** — each migration discrete, ordered, recorded in `schema_migrations`, with its DDL + version stamp applied in **one `IMMEDIATE` transaction**, gated **under Phase 1b's advisory write lock**, replacing the unconditional `apply_migrations`. (Exit: "adding a schema change needs only a numbered `.sql` file"; "runs crash-atomic & idempotent".)
- R2. **Unify fresh-init and upgrade paths** so the column set converges **by name (type / not-null / default), accepting that the column *ordinal* may differ for DBs created by the merged binary's inline-`001`** (all store SQL is by-name, so ordinal drift is benign); add **per-migration checksums**; an **unknown future version ⇒ read-only refuse**. (Exit: "a schema-diff test proves fresh-init-v2 and upgraded-via-ALTER converge by name" — see U3 for the three genesis cases the test must cover.)
- R3. **Absorb the `INSERT OR IGNORE` `schema_migrations` shim** (NER-132 U5) into the runner, carrying its concurrent-init idempotency invariant forward.
- R4. **Typed `ForgeError` enum** replacing the substring-matched `error_code()` **and every `bail!` string contract the CLI currently decodes — both the `forge-store`/`forge-export-git` sites and the CLI-layer (`main.rs`) sites** — folding the existing `RequestIdReplay` and `LockTimeout` sentinels in as its first members. (Exit: "no error code is string-derived".)
- R5. **Populate `errors[].details`** (candidate ids for ambiguous selectors, expected-vs-actual head for stale base, offending paths for dirty worktree — **with secret-risk paths redacted**, see U1), **meaningful top-level `retry.retryable` / `retry.after_ms`**, and `warnings[]`.
- R6. `LOCK_TIMEOUT` ⇒ top-level `retry.retryable = true` with `retry.after_ms` set, and `LockTimeout.waited_ms` surfaced in `errors[].details` (deferred finding #2).
- R7. Classify the transient CAS conflict (`current operation changed`, the `create_operation_view` singleton CAS only) **and** `LOCK_TIMEOUT` as **transient/retryable**, so a recorded transient failure is re-runnable under its `--request-id` rather than replaying a sticky failure — distinct from a deterministic domain failure (deferred finding #1).
- R8. **`forge schema` / `--capabilities`** emitting `schema_version`, per-command response shapes, and the full error-code registry as a published JSON-Schema contract. (Exit: "`forge schema` emits a versioned contract".)
- R9. **Launch blocker A — secret-export default-deny:** gate evidence/content export on secret-risk by default **at every egress surface** (the exported git tree, the native `synthesize_git_tree` materialization, and `pr_body`), surfacing each dropped path as a `warnings[]` entry. (Exit: "export refuses secret-risk by default with a warning".) Protects **secret-named files only**; secret *values* inside non-secret-named files remain exposed until the Phase 5 content redactor (stated in Scope Boundaries).
- R10. **Launch blocker B — conflict-set metadata persistence:** on `current_head != base_head` at accept/export, write a `conflict_sets` row (PRD §15) — a metadata insert, no merge engine — whose `paths_json` carries at least `{expected_head, actual_head}` (secret-risk paths redacted).
- R11. **Do not regress Phase 1a/1b invariants** (see Institutional Learnings) and **retain `schema_version: "forge.cli.v0"`** (all changes additive / provisional-field population).

**Origin actors:** agent (primary CLI caller), human reviewer (reads `--json` envelope / `forge schema`).
**Origin flows:** `init → start → save → run → propose → check → accept → export branch`; upgrade-on-open; error-and-retry.
**Origin acceptance examples:** carried as exit criteria into U-level Test scenarios below.

---

## Scope Boundaries

- **Not** the full secret redactor — entropy/PEM/URL scanning and content rewriting stay **Phase 5**. Blocker A is a **path-name**-level default-deny gate using the existing `is_secret_risk_path` predicate only. **Residual exposure (acknowledged, not a silent hole):** secret *values* committed inside non-secret-named files (a token in `app.yaml`, a key in `src/config.rs`) egress freely through `export branch` and `export pr-body` until Phase 5. The published `forge schema` contract must not imply content-level secret safety.
- **Not** a merge engine — blocker B is a metadata `INSERT` into the existing `conflict_sets` table. No diff/resolve/auto-rebase.
- **No `AUTOINCREMENT` / rowid-reuse hardening and no real `gc`** — that is **Phase 8**. `gc` stays `--dry-run`-only.
- **No `schema_version` bump** — `forge.cli.v0` is retained (confirmed decision; see Key Technical Decisions).
- **No new third-party crates** for JSON-Schema generation (e.g. `schemars`) — the contract is hand-authored for command shapes and **derived from the `ForgeError` enum** for the code registry. (`sha2` for checksums is already a `[workspace.dependencies]` member — adding `sha2.workspace = true` to `forge-store` is not a new third-party crate; see U3.)
- **No change to the `run` lock carve-out (§10.6)** or the `check`-verdict in-txn close from Phase 1b.

### Deferred to Follow-Up Work

- A `PathConflict` per-path child table (PRD §15 names it): **confirmed deferred** — R10's exit criterion ("a stale-base bail writes a persisted conflict-set row") is satisfied by the existing `conflict_sets` table with `paths_json`; a separate per-path table earns its keep only when a UI or query needs per-path rows. (See Open Questions → Resolved.)
- Performance items reviewed-and-deferred in Phase 1a (`apply_migrations` PRAGMA probes, `open_connection`-per-call, N+1) — the runner rewrite absorbs them *if free*, but they are not re-flagged as new work and are not a gate.

---

## Context & Research

### Relevant Code and Patterns

- **Migration layer** — `crates/forge-store/src/lib.rs`: `apply_migrations(connection: &mut Connection)` (imperative; `CREATE TABLE IF NOT EXISTS schema_migrations` outside any txn; version-1 DDL in one `IMMEDIATE` txn; `ensure_repository_content_backend_column` + `ensure_attached_attempt_column` as unconditional out-of-txn `ALTER`s that **stamp version 2 unconditionally via `INSERT OR IGNORE`**, so a merged-binary DB is already at HEAD=2; `MIGRATION_001 = include_str!("../migrations/001_init.sql")`). Invoked from `init_repository` (under `_init_lock`) and `open_repository` (**no lock held**). `doctor` hard-codes `schema_version != Some(2)`.
- **`schema_migrations`** — `crates/forge-store/migrations/001_init.sql` lines 1-5: `(version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at_ms INTEGER NOT NULL)` — **no `checksum` column**. `content_backend` (line 11, `DEFAULT 'git'`) and `attached_attempt_id` (line 46) are in 001's DDL **and** added by the two `ensure_*_column` ALTERs — the dual-genesis divergence. `conflict_sets` (lines 148-154: `id, repo_id, context, paths_json, created_at_ms`) already exists with **zero writers**.
- **Lock layer** — `crates/forge-store/src/repo_lock.rs`: `RepoLock` (`Drop`-unlock; OS reclaims on death), `acquire(forge_dir)`, `LockTimeout { waited_ms }` ("acquire exactly once, never nested"). `crates/forge-store/src/lib.rs`: `acquire_repo_lock(cwd) -> Result<Option<RepoLock>>`. `crates/forge-cli/src/main.rs`: `command_result` is the **universal funnel for every command except `init`** (verified: read-only commands route through it without taking the lock; `init` self-locks in `init_repository`); the per-command lock is acquired when `requires_repo_lock(command)` (`is_mutating && !run && !init`).
- **Error/contract layer** — `crates/forge-protocol/src/lib.rs`: `SCHEMA_VERSION = "forge.cli.v0"`; `ResponseEnvelope { …, warnings: Vec<String>, errors: Vec<ErrorObject>, retry: RetryMetadata }` — **`retry` is a top-level field on the envelope, NOT a field of `ErrorObject`**; both `success`/`error` hard-code `warnings: Vec::new()` + `RetryMetadata::no()`. `ErrorObject { code, message, details: Value }` with `::new` + `.with_details`; `RetryMetadata { retryable: bool, after_ms: Option<u64> }` (`::no()` only). `crates/forge-cli/src/main.rs`: `error_code(command, message)` substring ladder; **CLI-layer `bail!` string contracts in `main.rs` itself** — `DIRTY_WORKTREE` (attempt-attach ~320, restore ~377), accept-path `STALE_BASE` (`decision_response` ~468), `NOT_ACCEPTED`/`REJECTED` (~541-542), `BRANCH_EXISTS` (~548); `command_result` error flow (`RequestIdReplay` downcast → `record_failed_operation` → `ErrorObject::new(error_code(...))`); `replay_response`; `structured_error`/`parser_error_response` (the existing `details`-population precedent). Sentinels: `RequestIdReplay { operation }` (`forge-store/src/lib.rs`), `LockTimeout` (`repo_lock.rs`). The `"current operation changed"` string is raised at three sites: the genuine singleton CAS in `create_operation_view`, and the parent-operation guard inside `insert_operation_view` and `record_failed_operation`. Duplicated `bail!` contracts in `crates/forge-export-git/src/lib.rs` (`"stale base"`, `"branch already exists"`).
- **Export/secret layer** — `crates/forge-content/src/lib.rs`: `is_secret_risk_path`, `is_restore_temp_path`, `redact_secret_like_text`, `SECRET_RISK_SENSITIVITY`. `is_ignored_by_policy` exists as **two parallel private copies** (`crates/forge-content-git/src/lib.rs` ~189, `crates/forge-content-native/src/lib.rs` ~554) — they drift if only one is touched. Export entrypoints: `export_response` → `ExportCommand::Branch` (`forge_export_git::export_branch`, which receives **`proposal.content_ref` — a git-tree hash, not a path list**; the native path runs `synthesize_git_tree` → `git add -A .`) and `ExportCommand::PrBody` (`forge_store::pr_body_for`, which formats `proposal.changed_paths` directly into markdown). **No secret check at export time today.**
- **Stale-base detection** — `crates/forge-cli/src/main.rs` `decision_response` (accept, under the lock) and `crates/forge-export-git/src/lib.rs` `export_branch`. `base_head` lives on `attempts.base_head` / `proposals.base_head`.
- **Tests** — `crates/forge-cli/tests/` use `assert_cmd` + `tempfile`; `common/mod.rs` `TestRepo::new_git()`/`forge()`; migration/upgrade tests open the DB directly with `rusqlite`. `existing_repository_without_content_backend_column_migrates_on_normal_command` (`forge_init.rs` ~147) drops `content_backend` **without** lowering `MAX(version)`. Code-pinning tests: `forge_accept_export.rs`, `forge_init.rs`. Store unit tests inline + `crates/forge-store/tests/replay_guard.rs`.

### Institutional Learnings

- `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` — **the migration txn is an easily-missed `IMMEDIATE` site**; implement against the rule "every INSERT/UPDATE txn is `IMMEDIATE` + bounded busy/517 retry" (reuse `with_immediate_retry`/`is_retryable_busy`). The `RequestIdReplay` **sentinel-in-`anyhow` + `downcast_ref`** idiom is the precedent `ForgeError` generalizes — **do not** thread a `Committed | Replayed` enum through 12 writer signatures (explicitly rejected). The transient-vs-deterministic distinction (line 146) is the hook R7 implements.
- `docs/solutions/architecture-patterns/crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md` — **acquire the lock exactly once, never nested**; the runner must run under the lock without re-acquiring inside an already-locked section. The `INSERT OR IGNORE` shim (§5) is flagged in-code for this runner to absorb (R3). **Do not set one error's `retry.retryable` in isolation** — classify the whole set (R5/R6/R7). Map `LockTimeout → LOCK_TIMEOUT` and keep `init` codes un-masked (no `NOT_A_GIT_REPOSITORY` catch-all regression). Secret/export exclusion is policy-gated through `is_ignored_by_policy` in **both** backends — implement blocker A in shared `forge-content` and assert symmetric tests in both, mirroring `RESTORE_TEMP_PREFIX`.
- **Reviewed-and-rejected (do not re-litigate):** `RequestIdReplay`/`LockTimeout` single sentinels are accepted and deliberate — `ForgeError` is the *sanctioned* taxonomy exception to CLAUDE.md's "no custom error types". `init` error-code narrowing was an intentional improvement (preserve it). `maybe_crash` is accepted debug-gated scaffolding — leave it; not an error-type violation and not required to change here.

### External References

- Rust std `File::try_lock`/`unlock` (stabilized 1.89) — already in use; no new dep.
- SQLite `ALTER TABLE ADD COLUMN` semantics (a `REFERENCES` column is permitted only with a NULL/constant default — satisfied by the nullable `attached_attempt_id` and `DEFAULT 'git'` `content_backend`); `PRAGMA table_info` for the convergence test (compare name/type/notnull/dflt **sets**, not `cid` ordinals).

---

## Key Technical Decisions

- **Migration runner runs at the command boundary via a transient self-acquiring `migrate(cwd)` entrypoint, layered over a lock-agnostic `apply_pending_migrations(conn)`.** `apply_pending_migrations` assumes the caller has serialized access and never touches the lock; `init_repository` calls it under the existing `_init_lock`. `migrate(cwd)` does a cheap version read first: at head ⇒ return without the lock (the common case, including read-only commands); pending ⇒ acquire the repo lock transiently, apply, release; ahead of head ⇒ return `UnknownSchemaVersion`. `migrate(cwd)` is invoked once at the top of `command_result` **before** the per-command lock, so it never nests. `open_repository` stops applying DDL. *Rationale:* honors "acquire once, never nested" while covering read-only-first-after-upgrade and keeping the `run`/read-only carve-outs intact.
- **`UnknownSchemaVersion` from `migrate()` short-circuits the command BEFORE `record_failed_operation`.** A DB at HEAD+1 was written by a newer Forge; `command_result` must return the `SCHEMA_VERSION_UNSUPPORTED` envelope immediately, never falling through to `record_failed_operation` (which would `INSERT` into a forward-versioned schema — a write the binary is explicitly refusing, and a potential failure on a newer NOT NULL column). *Rationale:* read-only-refuse must actually be read-only.
- **Unify columns by reverting `001_init.sql` to a true v1 baseline and adding `002` with the two `ALTER`s.** Remove `content_backend` (001:11) and `attached_attempt_id` (001:46) from `001`'s DDL; create `002_*.sql` = `ALTER TABLE repositories ADD COLUMN content_backend TEXT NOT NULL DEFAULT 'git'; ALTER TABLE current_state ADD COLUMN attached_attempt_id TEXT REFERENCES attempts(id);`. Three genesis paths must converge **by name**: fresh init (replay 001+002), an old true-v1 DB (replay 002), and a **merged-binary v2 DB** (already at HEAD, skips both — its `content_backend` keeps its inline ordinal). All store SQL is by-name, so the differing ordinal on the third path is benign; the convergence test asserts name/type/notnull/dflt set-equality, not `cid` equality. *Rationale:* the only model that keeps each migration a pure version-gated `.sql` file (the exit criterion). The alternative (keep 001, guard 002 with runtime column-existence checks) is rejected — it requires non-`.sql` conditional logic.
- **Per-migration checksums grandfather NULLs.** Bootstrap adds a nullable `checksum` column to `schema_migrations` (idempotent `ALTER ADD COLUMN`, ignore "duplicate column"); the runner records `sha256(file)` for each migration it applies and verifies the stored checksum on already-applied versions. Pre-existing rows (any version applied before this runner) have `NULL` checksum → verification skipped. *Rationale:* lets us normalize the `001` baseline without bricking existing DBs (no in-the-wild row carries a checksum to mismatch); new applications are verified going forward.
- **`ForgeError` is `anyhow`-carried and recovered by `downcast_ref`**, generalizing the existing sentinel pattern — **not** woven through writer return types. It is defined in `forge-store` and **re-exported so the CLI and `forge-export-git` can construct variants directly**: the CLI-layer `bail!` sites (`DIRTY_WORKTREE`, accept-path `STALE_BASE`, `NOT_ACCEPTED`, `REJECTED`, `BRANCH_EXISTS`) and the `forge-export-git` sites (`StaleBase`, `BranchExists`) all become typed, or the pinned codes regress to `COMMAND_FAILED` when `error_code()` is deleted. Genuinely code-less bails (`gc only supports --dry-run`, `missing command after --`, unsupported-backend) map to `COMMAND_FAILED` explicitly.
- **Retryability is classified for the whole set in one pass.** `LockTimeout` and the **`create_operation_view` singleton CAS** (`CurrentStateChanged`) ⇒ `retryable: true` with `retry.after_ms` set; every deterministic domain error ⇒ `false`. The parent-operation guard failures in `insert_operation_view` / `record_failed_operation` keep their current non-retryable, recorded semantics (they are not the transient-CAS case R7 targets). Transient failures are **not** persisted under the `--request-id` (a retry re-executes); deterministic failures keep the existing status-aware replay contract. *Decision (moved out of Open Questions):* the implementation **skips `record_failed_operation`** for transient errors and introduces **no new `operations.status` value**. `retryable: true` is **advisory** — bounding retries is the client's responsibility (server provides `after_ms` like HTTP `Retry-After`); a persistently-losing CAS will re-execute until the client stops, which is acceptable and documented in the contract.
- **Retain `forge.cli.v0`.** Confirmed with the user. Note the one behavioral change beyond field population: transient failures are no longer recorded under `--request-id`. This refines a provisional behavior (a transient failure was never a value an agent should have depended on replaying) and renames/removes no code, so it stays within the no-bump decision; it is called out here so review sees it explicitly.
- **Secret-export deny enforces where bytes flow, via a shared predicate.** Promote `is_ignored_by_policy` into `forge-content` as a `pub` function and delete both backend-private copies (single compile-time-verified source — the restore-temp learning shows single-backend edits drift). Enforce the secret deny at: (1) the **git tree build** — rewrite the exported tree to drop secret-risk entries before branch creation; (2) **native `synthesize_git_tree`** — delete secret-risk files from the materialized temp worktree before `git add -A`; (3) **`pr_body_for`** — filter `changed_paths` before formatting. Each dropped path → a `warnings[]` entry on the envelope. *Rationale:* filtering only a path list at `export_response` is ineffective because `export_branch` consumes a tree hash and the native path re-materializes everything.
- **Conflict-set persistence reuses the existing `conflict_sets` table** (`context` + `paths_json`), written by a new `record_conflict_set(tx, repo_id, context, paths)` store function **that takes the caller's `&Transaction`** (the accept/export command already holds the per-command lock and runs inside `with_immediate_retry`) — it does **not** acquire a second lock or open a nested `IMMEDIATE` txn. `paths_json` carries `{expected_head, actual_head}` plus affected paths when cheaply available, with secret-risk paths redacted. *Rationale:* table exists; kickoff scopes blocker B as a single metadata insert; no nesting.
- **`details` payloads redact secret-risk paths.** `ForgeError::DirtyWorktree.details()` and `record_conflict_set` filter path lists through `is_secret_risk_path`, replacing matches with a redacted placeholder + count, so secret filenames never reach machine output or the persisted ledger.
- **`forge schema` derives the error-code registry from the `ForgeError` enum** and hand-authors per-command response shapes, emitted as a `serde_json::Value` JSON-Schema document carrying `schema_version`. *Rationale:* no new dependency; the registry stays complete as variants are added.

---

## Open Questions

### Resolved During Planning

- **`schema_version` bump?** No — retain `forge.cli.v0` (user-confirmed; additive/provisional-field changes + one provisional-behavior refinement only).
- **Where does the open-path migration run relative to the command lock?** At the top of `command_result`, before the per-command lock, via the transient `migrate(cwd)` entrypoint (never nested); `UnknownSchemaVersion` short-circuits before any write.
- **`PathConflict` table or `paths_json`?** Reuse `conflict_sets.paths_json`; the per-path table is confirmed-deferred (R10 satisfied by the existing table; no current consumer needs per-path rows).
- **Edit `001` vs guarded `002`?** Edit `001` to a true baseline + pure-`.sql` `002` ALTERs, grandfathering NULL checksums; convergence asserted by-name across three genesis cases.
- **Transient-failure persistence?** Decided (now in Key Technical Decisions): skip `record_failed_operation`, no new `operations.status`, `retryable` advisory with `after_ms`.
- **JSON-Schema dependency?** None — derive the code registry from `ForgeError`, hand-author command shapes.

### Deferred to Implementation

- Exact `ForgeError` variant set and field shapes — finalized while wiring each `bail!`/`anyhow!` call site (store, `forge-export-git`, **and CLI**) to its variant; a parity test must cover every code the deleted `error_code()` produced, including the CLI-originated ones.
- Whether `forge schema` reads from a single embedded contract module or assembles per-command shapes from a small table.
- Exact `details` JSON keys per code (`candidate_ids`, `expected_head`/`actual_head`, `paths`, `waited_ms`) — pinned by the contract test once the registry exists.
- Confirm in implementation that `002`'s `ALTER … ADD COLUMN … REFERENCES attempts(id)` runs cleanly inside the `with_immediate_retry` `IMMEDIATE` txn with `foreign_keys=ON` on a table holding existing rows (NULL default satisfies SQLite's rule; verify in-txn FK enforcement does not reject existing rows).

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

Migration control flow (the lock-nesting-safe seam):

```
command_result(command, …):
    match forge_store::migrate(cwd):          # ← BEFORE any per-command lock
        UpToDate     => proceed               #   common path, no lock taken
        Migrated     => proceed               #   transient lock acquired+released
        Err(UnknownSchemaVersion) =>          #   DB ahead of binary
            return SCHEMA_VERSION_UNSUPPORTED  #   short-circuit; NO record_failed_operation
    _repo_lock = if requires_repo_lock(command) { acquire_repo_lock(cwd)? }  # per-command
    f()   # open_repository (no DDL) + determining reads + writes

migrate(cwd):
    db_v = read MAX(version)        # cheap, no lock
    if db_v == HEAD: UpToDate
    if db_v >  HEAD: Err(UnknownSchemaVersion{db_v, HEAD})
    if db_v <  HEAD: lock=acquire_repo_lock(cwd)?; apply_pending_migrations(conn); drop(lock); Migrated

init_repository(…):
    _init_lock = repo_lock::acquire(forge_dir)?      # existing
    apply_pending_migrations(conn)                   # same runner, lock already held
```

Error mapping (substring ladder + CLI bails → typed; `retry` is TOP-LEVEL on the envelope):

```
store / export-git / CLI bail  ──Err(anyhow carrying ForgeError::X)──▶
command_result: error.downcast_ref::<forge_store::ForgeError>()
    StaleBase{expected, actual}  ─▶ code="STALE_BASE",  details={expected_head, actual_head},        retry=no()
    DirtyWorktree{paths}         ─▶ code="DIRTY_WORKTREE", details={paths: <secret-redacted>},        retry=no()
    LockTimeout{waited_ms}       ─▶ code="LOCK_TIMEOUT", details={waited_ms},  envelope.retry={retryable:true, after_ms}
    CurrentStateChanged (CAS)    ─▶ code="CONFLICT",     envelope.retry={retryable:true, after_ms}; NOT recorded under request-id
    RequestIdReplay              ─▶ replay_response (contract unchanged)
# envelope shape: ResponseEnvelope{ …, errors:[ErrorObject{code,message,details}], retry:RetryMetadata }
# retry lives on the envelope, NOT on errors[]. Assert retry.retryable, errors[0].details.waited_ms.
```

---

## Implementation Units

### U1. Typed `ForgeError` enum in `forge-store` (store, export-git, AND CLI sites)

**Goal:** Introduce a typed error taxonomy that every failure path the CLI currently decodes constructs — store-layer, `forge-export-git`, and the CLI-layer `bail!` sites — folding in `RequestIdReplay` and `LockTimeout`, classifying retryability, and carrying structured (secret-redacted) payloads, without changing writer return signatures.

**Requirements:** R4, R5 (details), R6 (partial), R7 (partial), R11

**Dependencies:** None

**Files:**
- Create: `crates/forge-store/src/error.rs`
- Modify: `crates/forge-store/src/lib.rs` (re-export `ForgeError`; replace store `bail!`/`anyhow!` string contracts with construction), `crates/forge-store/src/repo_lock.rs` (`LockTimeout` feeds a variant), `crates/forge-export-git/src/lib.rs` (raise typed `StaleBase`/`BranchExists`), `crates/forge-export-git/Cargo.toml` (**add path dep on `forge-store`** — acyclic), `crates/forge-cli/src/main.rs` (convert the CLI-layer `bail!` sites — `DIRTY_WORKTREE`, accept-path `STALE_BASE`, `NOT_ACCEPTED`, `REJECTED`, `BRANCH_EXISTS` — to typed `ForgeError`)
- Test: inline `#[cfg(test)] mod tests` in `crates/forge-store/src/error.rs`; parity test in `crates/forge-cli/tests/forge_errors.rs` (new)

**Approach:**
- `enum ForgeError` covering every code the current `error_code()` derives (`StaleBase { expected_head, actual_head }`, `DirtyWorktree { paths }`, `AmbiguousAttempt { candidate_ids }`, `UnknownAttempt`, `AmbiguousProposal { candidate_ids }`, `UnknownProposal`, `UnknownIntent`, `NoActiveAttempt`, `NoSnapshot`, `NoProposal`, `NotAccepted`, `Rejected`, `BranchExists { name }`, `NotInitialized`, `RequestIdConflict`) + folded sentinels (`RequestIdReplay { operation }`, `LockTimeout { waited_ms }`) + new variants (`UnknownSchemaVersion { db_version, supported_head }`, `CurrentStateChanged`, `MigrationFailed { version }`).
- `impl Display + Error`; `fn code(&self) -> &'static str`; `fn retryable(&self) -> bool` (true for `LockTimeout` + `CurrentStateChanged`); `fn after_ms(&self) -> Option<u64>` (backoff hint for the retryable variants); `fn details(&self) -> serde_json::Value` — **`DirtyWorktree.details()` filters paths through `forge_content::is_secret_risk_path`, replacing matches with a redacted placeholder + count.**
- Keep the `anyhow`-carried + `downcast_ref` shape — store fns `return Err(ForgeError::X.into())`; re-export `pub use error::ForgeError` so the CLI and `forge-export-git` construct variants.

**Patterns to follow:** existing `RequestIdReplay`/`LockTimeout` sentinels; the rejected `Committed|Replayed` alternative (do not reintroduce).

**Test scenarios:**
- Happy path: each variant's `.code()` returns the exact string the old `error_code()` produced for the equivalent message — **including the CLI-originated codes** `DIRTY_WORKTREE`, accept-path `STALE_BASE`, `NOT_ACCEPTED`, `REJECTED`, `BRANCH_EXISTS` (parity table). Covers R4.
- Happy path: `LockTimeout.retryable()` and `CurrentStateChanged.retryable()` are `true` with `after_ms` set; a sample of deterministic variants are `false` with `after_ms == None`. Covers R6/R7.
- Edge case: `.details()` for `AmbiguousAttempt`/`StaleBase` carry the expected keys; `DirtyWorktree.details()` **redacts** a `.env`/`*.pem` path (placeholder + count, real name absent). Covers R5.
- Edge case: a `ForgeError` round-trips through `anyhow::Error` and is recovered by `downcast_ref`.

**Verification:** every code string the deleted `error_code()` produced is produced by some `ForgeError::code()`; `forge-store`, `forge-export-git`, and the CLI bail sites compile with no string error contract the CLI must parse.

---

### U2. CLI typed-error mapping + populate `details` / `retry` / `warnings`

**Goal:** Retire `error_code()`; map recovered `ForgeError` to `(code, top-level retry, details)`; populate `ErrorObject.details`, the top-level `retry.retryable`/`after_ms`, and the `warnings[]` plumbing; fix the transient-CAS `--request-id` poisoning by not persisting transient failures.

**Requirements:** R4, R5, R6, R7, R11

**Dependencies:** U1

**Files:**
- Modify: `crates/forge-cli/src/main.rs` (`command_result` error arm: `downcast_ref::<forge_store::ForgeError>()` → `ErrorObject` with `code`/`details` + set the **envelope-level** `RetryMetadata`; delete `error_code()`; transient errors skip `record_failed_operation`; handle `UnknownSchemaVersion` short-circuit per U4), `crates/forge-protocol/src/lib.rs` (`RetryMetadata::retryable(after_ms)` constructor; `ResponseEnvelope::error`/`success` accept `warnings`/`retry` instead of hard-coding empty/`no()` — additive helpers)
- Test: `crates/forge-cli/tests/forge_repo_lock.rs` (extend), `crates/forge-cli/tests/forge_accept_export.rs` (extend), `crates/forge-store/tests/replay_guard.rs` (extend), `crates/forge-cli/tests/forge_errors.rs` (extend)

**Approach:**
- Recover `ForgeError` before `record_failed_operation`: if `retryable()`, **do not** persist under the `request_id` (retry re-runs) and set envelope `retry { retryable: true, after_ms }`; else preserve current status-aware replay recording with `retry = no()`.
- Build `ErrorObject::new(err.code(), message).with_details(err.details())`.
- Preserve `init`'s un-masked behavior (typed `LockTimeout` → `LOCK_TIMEOUT`; genuine not-a-git-repo falls through), and `replay_response`'s contract (code now from the typed error / stored code, not substring).

**Patterns to follow:** existing `RequestIdReplay` downcast arm; `structured_error`/`parser_error_response` (the `details` precedent).

**Test scenarios:**
- Happy path: a `LOCK_TIMEOUT` response has **top-level `retry.retryable == true`** and `errors[0].details.waited_ms` present (the one-line regression assert from the Linear comment — note `retry` is on the envelope, `details` on the error object). Covers R6.
- Integration: a transient CAS conflict under `--request-id X`, then a retry of `X`, **re-executes and succeeds** (no row was recorded). Covers R7 / defer #1.
- Integration: an `insert_operation_view`/`record_failed_operation` parent-guard failure is **still persisted** under its request-id (not reclassified as the retryable CAS). Covers the U1 classification split.
- Happy path: `STALE_BASE` carries `details.expected_head`/`actual_head`; `AMBIGUOUS_ATTEMPT` carries `details.candidate_ids`. Covers R5.
- Edge case: every existing code-pinning test (`forge_accept_export.rs`, `forge_init.rs`) still sees the same `errors[0].code` — including the CLI-originated codes from U1. Covers R4/R11.

**Verification:** `error_code()` is deleted; no test asserts a code that changed; `retry`/`details`/`warnings` are populated from typed data.

---

### U3. Numbered `.sql` migration runner in `forge-store`

**Goal:** Replace `apply_migrations` with a numbered, checksummed, version-gated `.sql` runner that applies each pending migration in one `IMMEDIATE` txn, absorbs the `INSERT OR IGNORE` shim, unifies fresh-init/upgrade by-name, and refuses unknown-future versions.

**Requirements:** R1, R2, R3, R11

**Dependencies:** U1 (`ForgeError::UnknownSchemaVersion` / `MigrationFailed`)

**Files:**
- Create: `crates/forge-store/src/migrations.rs` (embedded ordered list via `include_str!`, `bootstrap_schema_migrations`, `apply_pending_migrations(conn)`, checksum compute/verify), `crates/forge-store/migrations/002_columns.sql`
- Modify: `crates/forge-store/migrations/001_init.sql` (remove `content_backend` line 11 and `attached_attempt_id` line 46), `crates/forge-store/src/lib.rs` (delete `apply_migrations` + `ensure_*_column`; `init_repository` calls `apply_pending_migrations` under `_init_lock`; `doctor` reads dynamic `HEAD` instead of `Some(2)`), `crates/forge-store/Cargo.toml` (**add `sha2.workspace = true`** — `sha2` is already in `[workspace.dependencies]`, used by `forge-content-native`; not a new third-party crate)
- Test: `crates/forge-store/tests/migrations.rs` (new); update `crates/forge-cli/tests/forge_init.rs::existing_repository_without_content_backend_column_migrates_on_normal_command`

**Approach:**
- Embedded migrations: ordered slice `[(1, "001_init", include_str!), (2, "002_columns", include_str!)]`; `HEAD = max version`.
- `bootstrap_schema_migrations(conn)`: `CREATE TABLE IF NOT EXISTS schema_migrations(version, name, applied_at_ms, checksum TEXT)` + idempotent `ALTER ADD COLUMN checksum` (ignore "duplicate column").
- `apply_pending_migrations(conn)`: read applied versions (+ verify non-NULL checksums vs `sha2::Sha256` of the file; NULL → skip). For each embedded version `> max_applied` and `<= HEAD`, run DDL + `INSERT OR IGNORE … (version, name, now, checksum)` in **one `with_immediate_retry` `IMMEDIATE` txn**. If `max_applied > HEAD` → `Err(ForgeError::UnknownSchemaVersion)`.

**Execution note:** write the by-name convergence test (below) **before** reverting `001`, so convergence is green-by-construction.

**Patterns to follow:** `with_immediate_retry`/`is_retryable_busy`; the `INSERT OR IGNORE` shim it replaces; `forge_init.rs`'s DROP-COLUMN-then-reopen upgrade test.

**Test scenarios:**
- Happy path: fresh init reaches `HEAD`; `schema_migrations` has all versions with non-NULL checksums. Covers R1.
- Integration: **by-name convergence across three genesis cases** — (a) fresh init (001+002), (b) an old true-v1 DB ALTER-upgraded via 002, (c) a hand-built merged-binary v2 DB with `content_backend` inline at its old ordinal — all three have equal `PRAGMA table_info` **name/type/notnull/dflt sets** for `repositories` and `current_state` (compare sets, not `cid` ordinals). Covers R2 (exit test).
- Edge case: re-running on a head DB is a no-op; a DB whose `schema_migrations` lacks `checksum` bootstraps the column and grandfathers NULLs. Covers R3.
- Error path: a DB stamped `HEAD+1` ⇒ `ForgeError::UnknownSchemaVersion`. Covers R2.
- Edge case: a tampered already-applied migration (non-NULL checksum mismatch) is refused.
- **Capability-change** (updated existing test): `existing_repository_without_content_backend_column_migrates_on_normal_command` now drops the column **and sets `version=1`** (deleting the version-2 row) to simulate a genuine old schema; structural drift on an *at-HEAD* DB is no longer auto-repaired by migrations (it is surfaced by `doctor`'s schema/FK checks) — assert that behavior explicitly.

**Verification:** `apply_migrations`/`ensure_*_column` are gone; adding a hypothetical `003_*.sql` is the only change needed to add a migration; `doctor` reports healthy at `HEAD`.

---

### U4. Wire `migrate()` at the command boundary; remove DDL from `open_repository`

**Goal:** Run migrations where no lock is held (top of `command_result`) via the transient `migrate(cwd)` entrypoint; short-circuit `UnknownSchemaVersion` before any write; stop `open_repository` from applying DDL.

**Requirements:** R1, R11

**Dependencies:** U3

**Files:**
- Modify: `crates/forge-store/src/lib.rs` (`pub fn migrate(cwd) -> Result<MigrationOutcome>`: cheap version read → up-to-date / refuse / transient-acquire-and-apply; mirror `acquire_repo_lock`'s `Ok(None)`-when-uninitialized so callers still surface `NOT_INITIALIZED`; `open_repository` drops the migration call), `crates/forge-cli/src/main.rs` (`command_result` calls `forge_store::migrate(&cwd)?` before the per-command lock; on `UnknownSchemaVersion`, return `SCHEMA_VERSION_UNSUPPORTED` **before** `record_failed_operation`)
- Test: `crates/forge-cli/tests/forge_migration_upgrade.rs` (new), `crates/forge-cli/tests/forge_concurrency.rs` (extend)

**Approach:**
- `command_result` calls `migrate` first for **all** commands (read + write + `run`); the transient lock releases before per-command locking / child exec, preserving §10.6 and the read-only carve-out. `init` is unaffected (self-locks, calls `apply_pending_migrations` directly).

**Test scenarios:**
- Integration: simulated old DB (drop a column + `version=1`), **first command read-only** (`forge show --json`) ⇒ upgraded, succeeds. Covers R1.
- Integration: simulated old DB, **first command mutating** under the per-command lock ⇒ upgrade applied, no deadlock. Covers R11 (no nesting).
- Integration: a DB at `HEAD+1`, a **mutating** command ⇒ refuses with `SCHEMA_VERSION_UNSUPPORTED` **and writes zero new `operations` rows** (short-circuit before `record_failed_operation`).
- Integration: two concurrent processes against a behind DB both succeed, exactly one set of version rows, no `SQLITE_BUSY`-class output, no deadlock; **and a behind-DB upgrade serializes against an in-flight unrelated mutating command on the same repo** (shared `forge.lock`), not just against another upgrader.

**Verification:** `open_repository` performs no DDL; upgrades apply transparently on first touch under the lock; a forward-versioned DB is never written to; concurrency/crash suites stay green.

---

### U5. `forge schema` / `--capabilities` versioned contract

**Goal:** Add a `forge schema` subcommand emitting `schema_version`, per-command response shapes, and the error-code registry derived from `ForgeError`, as a published JSON-Schema document.

**Requirements:** R8, R11

**Dependencies:** U1, U2

**Files:**
- Modify: `crates/forge-cli/src/main.rs` (`Command` enum + dispatch)
- Create: `crates/forge-cli/src/schema.rs` (assemble the contract `serde_json::Value`) — a separate module because the contract document (per-command shapes + enum-derived registry) is large enough that inlining it into `main.rs`'s dispatch would bury the command flow; if it stays small in implementation, inlining as a private helper is acceptable.
- Test: `crates/forge-cli/tests/forge_schema.rs` (new)

**Approach:**
- Build a `serde_json::Value` JSON-Schema-shaped document: top-level `schema_version: "forge.cli.v0"`, an `errors` registry generated by iterating `ForgeError` codes with `{ retryable, after_ms?, details_keys }`, and hand-authored per-command `data` shapes for the lifecycle commands. The registry must note that `retryable: true` is advisory (client-bounded) and that secret protection is **path-name-level only** (per Scope Boundaries) so the contract does not over-promise content safety. No `schemars` dependency.

**Test scenarios:**
- Happy path: `forge schema --json` emits `schema_version == "forge.cli.v0"` and an `errors` registry containing every `ForgeError` code (asserted against the enum). Covers R8.
- Edge case: each entry has a `retryable` boolean; `LOCK_TIMEOUT.retryable == true`; the document parses as valid JSON and lists the lifecycle commands.

**Verification:** the published contract names every error code, is consistent with the `forge.cli.v0` envelope, and adding a `ForgeError` variant automatically appears in the registry.

---

### U6. Launch blocker A — secret-export default-deny (enforced where bytes flow)

**Goal:** Refuse secret-risk paths at every export egress surface by default, dropping each and surfacing it as a `warnings[]` entry, with one shared policy predicate and symmetric backend coverage.

**Requirements:** R9, R11

**Dependencies:** U2 (warnings plumbing)

**Files:**
- Modify: `crates/forge-content/src/lib.rs` (**promote `is_ignored_by_policy` to `pub`** + add `filter_secret_risk(paths) -> (kept, dropped)`), `crates/forge-content-git/src/lib.rs` and `crates/forge-content-native/src/lib.rs` (**delete the two private `is_ignored_by_policy` copies**, use the shared one), `crates/forge-export-git/src/lib.rs` (rewrite the exported git tree to drop secret-risk entries before branch creation; in `synthesize_git_tree`, delete secret-risk files from the materialized temp worktree **before `git add -A`**), `crates/forge-store/src/lib.rs` (`pr_body_for` filters `changed_paths` through `is_secret_risk_path`), `crates/forge-cli/src/main.rs` (`export_response` collects dropped paths into `warnings[]` for both `Branch` and `PrBody`)
- Test: `crates/forge-cli/tests/forge_secret_export.rs` (new); symmetric inline assertions in both backends

**Approach:**
- Single shared predicate; the deny is self-enforcing at the materialization/tree boundary regardless of what the caller filtered (so the native `git add -A` path cannot leak). The CLI aggregates dropped paths into `warnings`. Default-deny, no opt-out flag (full redactor is Phase 5).

**Test scenarios:**
- Happy path: a proposal containing a `.env`/`*.pem` exports with that path **excluded from the resulting branch tree** and a `warnings[]` entry naming it; non-secret paths exported. Covers R9 (exit test).
- Integration (native backend): the `synthesize_git_tree` path excludes the secret file even though it runs `git add -A` — the file is absent from the committed tree.
- Integration: `export pr-body` omits secret-risk paths from the body and warns.
- Edge case: both backends exclude the identical secret-path set (symmetric assertion); neither backend defines a local `is_ignored_by_policy`. Covers R11 (no drift).

**Verification:** every export egress surface refuses secret-named paths by default with a `warnings[]` entry; one shared predicate; CLAUDE.md security defaults tightened, not weakened.

---

### U7. Launch blocker B — conflict-set metadata persistence

**Goal:** On `current_head != base_head` at accept and export, write a `conflict_sets` row (context + `paths_json`) under the held lock before bailing.

**Requirements:** R10, R11

**Dependencies:** U1 (typed `StaleBase`), U2 (details reuse)

**Files:**
- Modify: `crates/forge-store/src/lib.rs` (`record_conflict_set(tx: &Transaction, repo_id, context, paths) -> Result<String>` using `new_id("conflict")`, writing inside the **caller's** `IMMEDIATE` txn — no new lock, no nested txn; secret-risk paths redacted), `crates/forge-cli/src/main.rs` (`decision_response` accept path and `export_response` stale-base path: write the conflict-set row before raising `StaleBase`, within the existing locked `with_immediate_retry` writer)
- Test: `crates/forge-cli/tests/forge_conflict_set.rs` (new)

**Approach:**
- At both stale-base points, before returning `ForgeError::StaleBase`, call `record_conflict_set` with `context` = `"stale_base_accept"` / `"stale_base_export"` and `paths_json` carrying `{expected_head, actual_head}` (+ affected paths if cheaply available, secret-redacted). The write shares the command's already-held lock and transaction — pure metadata insert, no merge engine.

**Test scenarios:**
- Integration: accept against a moved HEAD ⇒ command fails `STALE_BASE` **and** a `conflict_sets` row exists with `context = "stale_base_accept"` and `paths_json` containing string keys `expected_head` **and** `actual_head`. Covers R10 (exit test).
- Integration: export branch against a moved HEAD ⇒ `STALE_BASE` + a row with `context = "stale_base_export"`.
- Edge case: a non-stale accept/export writes **no** `conflict_sets` row.
- Edge case: a secret-risk path in the divergence is redacted in `paths_json`.

**Verification:** the previously-unused `conflict_sets` table has writers; a stale-base bail persists exactly one row with the head pair; happy paths write none.

---

## System-Wide Impact

- **Interaction graph:** `command_result` gains a pre-lock `migrate()` call affecting **every** command (with an `UnknownSchemaVersion` short-circuit before `record_failed_operation`); `export_response`/`synthesize_git_tree`/`pr_body_for` gain a secret-filter step; `decision_response`/`export_response` gain a conflict-set write; a new `forge schema` command joins the dispatch.
- **Error propagation:** store-, export-git-, **and CLI-layer** failures travel as typed `ForgeError` inside `anyhow`, recovered by `downcast_ref` and mapped to `(code, details, envelope retry)`. Transient errors stop being persisted under the `--request-id`.
- **State lifecycle risks:** migration apply mid-upgrade must be crash-atomic (one `IMMEDIATE` txn per migration) and idempotent (`INSERT OR IGNORE` + version gate + checksum). Editing `001` is the highest-risk change — mitigated by NULL-checksum grandfathering, the three-case by-name convergence test, and the `MAX(version)` gate that makes a merged-binary v2 DB skip both migrations.
- **API surface parity:** the `forge.cli.v0` envelope is unchanged structurally; `forge schema` becomes its canonical description. The duplicated `stale base`/`branch already exists` contracts in `forge-export-git`, the CLI `bail!` sites, and the `error_code()` ladder all move to typed errors in lockstep.
- **Integration coverage:** read-only-first-after-upgrade, mutating-first-after-upgrade, HEAD+1 refusal-without-write, concurrent upgrade + migrating-vs-locked-writer, secret-export symmetry + native `git add -A` enforcement, and stale-base persistence are covered by integration tests.
- **Unchanged invariants:** WAL + per-connection PRAGMAs, `IMMEDIATE` everywhere, 517/busy retry, `RequestIdReplay` replay contract, UUIDv7 + `rowid` tiebreaks, store-before-DB ordering, the `run` lock carve-out (§10.6), acquire-the-lock-once, crash-injection-is-consistency-not-power-loss, and all CLAUDE.md security defaults.

---

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Deleting `error_code()` regresses CLI-raised codes to `COMMAND_FAILED` | Medium | High | U1 converts the CLI `bail!` sites to typed errors; parity test covers every old code incl. CLI-originated ones; existing code-pinning tests left intact (U2). |
| Secret-export gate filters a path list the git backend never consults | Medium | High | U6 enforces at the tree-rewrite / native-`git add` / `pr_body` boundary via a shared self-enforcing predicate; native-backend exclusion test. |
| Editing `001` mis-detects an in-the-wild DB and bricks a repo (ticket-flagged) | Low | High | NULL-checksum grandfathering; strict `MAX(version)` gate + `INSERT OR IGNORE`; three-case by-name convergence test; runner only applies forward from `max_applied`. |
| `migrate()` nests under an already-held lock and deadlocks | Low | High | `migrate()` runs before the per-command lock and only acquires transiently when pending; `init` uses the lock-agnostic runner under `_init_lock`; explicit no-nesting + migrating-vs-locked-writer tests (U4). |
| HEAD+1 refusal writes to a forward-versioned DB | Low | Medium | `UnknownSchemaVersion` short-circuits before `record_failed_operation`; test asserts zero `operations` rows (U4). |
| Over-classifying the parent-op guard as the retryable CAS | Medium | Medium | Only `create_operation_view`'s CAS maps to `CurrentStateChanged`-retryable; guard sites keep recorded semantics; test asserts they still persist (U1/U2). |
| Secret paths leak into `details` / `conflict_sets.paths_json` | Medium | Medium | `DirtyWorktree.details()` and `record_conflict_set` redact via `is_secret_risk_path` (U1/U7). |
| Unbounded client retry on a persistently-losing CAS | Low | Low | `retry.after_ms` set; contract states `retryable` is advisory/client-bounded (U2/U5). |
| Secret-export gate drifts between backends | Low | Medium | Single shared `is_ignored_by_policy` in `forge-content`; both private copies deleted; symmetric tests (U6). |

---

## Phased Delivery

- **Phase A — typed contract foundation:** U1 → U2 → U5. (U5 starts only after **both** U1 and U2 land — it derives the registry from the enum and depends on the CLI mapping.)
- **Phase B — migration framework:** U3 → U4. Depends on U1 (`UnknownSchemaVersion`); may proceed in parallel with U5 once U1+U2 are in.
- **Phase C — launch blockers:** U6 (after U2) and U7 (after U1+U2). Independent of each other.

Each unit is an atomic commit; the verify trio (`cargo fmt --all --check` · `cargo test --workspace` · `cargo clippy --workspace --all-targets -- -D warnings`) must pass per unit.

---

## Documentation / Operational Notes

- On merge: flip this plan to `status: completed`, move to `docs/plans/completed/`, set NER-133 → Done.
- `/ce-compound` candidates (genuinely new ground): the migration-runner-under-transient-lock recipe, the typed-error-taxonomy migration from a substring ladder + CLI bails, the secret-export-enforced-at-the-tree-boundary pattern, and the first published JSON-Schema contract.
- CLAUDE.md "Security defaults" and "Gotchas" gain the export default-deny (path-name-level, residual content exposure until Phase 5) and the read-only-refuse-on-unknown-version behaviors — update the wording if it no longer matches.

---

## Sources & References

- **Origin document:** [docs/handoffs/2026-05-29-ner-133-phase-2-kickoff.md](docs/handoffs/2026-05-29-ner-133-phase-2-kickoff.md)
- Ticket: Linear **NER-133** (Forge project, team SE Engineers) + its two deferred-finding comments; `docs/ROADMAP.md` Phase 2.
- Invariants (must not regress): `docs/solutions/architecture-patterns/crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md`, `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md`.
- Prior reviews (reviewed-and-rejected): `docs/code-reviews/2026-05-29-phase-1a-pr-b-concurrency.md`, `docs/code-reviews/2026-05-29-phase-1b-crash-correctness.md`.
- Key code: `crates/forge-store/src/lib.rs` (`apply_migrations`, `acquire_repo_lock`, `init_repository`, `open_repository`, `doctor`, `create_operation_view`, `pr_body_for`), `crates/forge-store/src/repo_lock.rs` (`LockTimeout`), `crates/forge-store/migrations/001_init.sql`, `crates/forge-cli/src/main.rs` (`command_result`, `error_code`, `replay_response`, `decision_response`, `export_response`, the CLI `bail!` sites, `requires_repo_lock`), `crates/forge-protocol/src/lib.rs` (`ResponseEnvelope`, `ErrorObject`, `RetryMetadata`), `crates/forge-content/src/lib.rs` (`is_secret_risk_path`, `is_ignored_by_policy`), `crates/forge-export-git/src/lib.rs` (`export_branch`, `synthesize_git_tree`).
