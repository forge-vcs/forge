---
title: "fix: Phase 1a substrate — durability, WAL concurrency, collision-safe IDs, idempotent replay"
type: fix
status: active
date: 2026-05-28
origin: docs/ROADMAP.md  # Phase 1a; tracked as Linear NER-131
deepened: 2026-05-28
---

# fix: Phase 1a substrate — durability, WAL concurrency, collision-safe IDs, idempotent replay

## Summary

Phase 1a makes the four cheapest, highest-leverage substrate fixes the competing-attempts wedge depends on, so multiple `forge` processes can operate on one `.forge/forge.db` in parallel without `database is locked`, ID collisions, a silently lost object, or a double-applied retry. The work is: (1) fix the swallowed-fsync correctness in the native object store's write path (full object-before-row durability ordering is Phase 1b); (2) configure the SQLite connection for multi-process use (WAL + `synchronous=NORMAL` + `busy_timeout`) and make every writer use `BEGIN IMMEDIATE` with a bounded busy-retry; (3) replace the collision-prone `{millis}_{nanos}` ID scheme in both generators with UUIDv7 and add a deterministic same-millisecond tiebreak to every "latest" selector; (4) make the `--request-id` replay check serialize against the write so a concurrent retry replays cleanly instead of erroring.

This is the roadmap's "single most important thing to build next" — it depends on nothing and unblocks Phases 3–6. It does **not** attempt the full crash-correctness invariant (store-before-DB ordering, atomic restore, advisory lock, crash-injection harness), which is Phase 1b (NER-132).

---

## Problem Frame

Forge v0 was explicitly scoped to a solo-developer, single-process loop, so the substrate was never hardened for concurrent agents. Today: `open_connection` sets only `PRAGMA foreign_keys=ON` (rollback journal, so a second concurrent writer hard-fails with `SQLITE_BUSY` immediately); IDs are `format!("{prefix}_{millis}_{nanos}")` from two separate generators, which collide when two mints land in the same nanosecond and are not lexicographically sortable; the native object store's parent-directory fsync swallows its error (`let _ = file.sync_all()`) and never fsyncs newly-created ancestor directories; and the `--request-id` replay check runs on a separate read-only connection *before* any write lock, so two racing retries with the same id both pass the check and collide at commit instead of replaying cleanly. Each of these is a trust hole, and trust is the product. The competing-attempts wedge — many agents attempting one intent in parallel — exercises exactly these failure modes.

---

## Requirements

- R1. No object-write durability path swallows an fsync error; newly-created ancestor directories are fsynced on first creation. (Roadmap exit: no `let _ = .*sync` remains on a durability path.)
- R2. The production SQLite connection opens in WAL mode with `synchronous=NORMAL` and an explicit `busy_timeout` on every open.
- R3. Every write transaction uses `BEGIN IMMEDIATE`; transient `SQLITE_BUSY` / `SQLITE_BUSY_SNAPSHOT` is absorbed by a bounded retry rather than surfaced to the user.
- R4. Entity IDs (`forge-core`: Repository/Operation/View) and domain IDs (`forge-store`: intent/attempt/snapshot/evidence/proposal/revision/check/decision) are collision-resistant and time-sortable (UUIDv7), replacing `{millis}_{nanos}` in **both** generators while preserving the human-readable `<prefix>_` form. (Roadmap exit: zero ID collisions across a 10k-mint loop.)
- R5. Every "latest" selector (`ORDER BY created_at_ms DESC LIMIT 1`) breaks same-millisecond ties deterministically.
- R6. A retried mutation with the same `--request-id` creates exactly one set of domain rows under concurrency and returns the original result; reuse of an id for a different command still errors `REQUEST_ID_CONFLICT`. (Roadmap exit: same `--request-id` retried creates exactly one domain row.)
- R7. Any new crate satisfies the ≥4-day minimum-release-age supply-chain gate and the PRD small/audited dependency policy; no custom crypto.
- R8. The `forge.cli.v0` JSON envelope contract and the existing security defaults (secret-risk exclusion, evidence redaction, `EXCERPT_LIMIT`) are preserved unchanged.

---

## Scope Boundaries

- No native-VCS, merge, diff, or remote work — Phases 7–9.
- No declarative check engine, evidence hashing, or compare/rank — Phases 4–6.
- ID change is a **forward-only mint-format change**: existing rows keep their existing IDs (opaque TEXT keys), and there is no schema migration and no backfill.
- No new typed-error enum or `LOCK_TIMEOUT` code — error taxonomy work is Phase 2 (NER-133). Phase 1a's bounded retry should make genuine timeout exhaustion rare; if it surfaces, it remains a generic `COMMAND_FAILED` until Phase 2.

### Deferred to Follow-Up Work

- **Store-before-DB durability ordering** (object durable before the txn that commits its `content_ref`), **crash-atomic worktree restore** (`materialize_tree` `fs::write` → temp+rename, lib.rs:387/399), **repo-level advisory lock** with a typed `LOCK_TIMEOUT`, and the **crash-injection harness**: Phase 1b (NER-132). U1 fixes only the object **write** path's fsync correctness.
- **`PENDING`-before-side-effect idempotency** for commands whose effects escape the SQLite transaction (the `run --` subprocess; loose-object writes): Phase 1b. Phase 1a relies on `BEGIN IMMEDIATE` serialization to make *concurrent* same-id retries replay; full crash-during-side-effect resumption is out of scope.
- **Cross-read atomicity for distinct concurrent operations:** `BEGIN IMMEDIATE` (U4) serializes commits but does not by itself make atomic the "latest" reads that `propose`/`record_check`/`record_evidence` perform on a separate connection before their write txn, nor `accept`'s CLI-layer `STALE_BASE` check. U4 moves the in-DB determining reads into the txn where feasible; the residual (e.g. the git `current_head` read for `STALE_BASE`) plus a repo-level advisory lock are Phase 1b (NER-132).
- **Network/cloud-synced filesystem detection** for `.forge` (WAL is same-host-only): documented as a risk here; detection/refusal is a later hardening item.
- **`synchronous=FULL` for `accept`/`export`** (power-loss durability of the committed row at decision points): deferred; `NORMAL` is the Phase 1a default.

---

## Context & Research

### Relevant Code and Patterns

- `crates/forge-store/src/lib.rs`
  - `open_connection` (~1875-1879) — the single place all connections are configured; today only `foreign_keys=ON`.
  - Writer transactions use `connection.transaction()` (= `BEGIN DEFERRED`). **The authoritative rule is "every `connection.transaction()` that performs an INSERT/UPDATE"** (13 in the current code, not 11 — verify with `grep -c`): `init_repository` (:304), `create_operation_view` (:365), `create_attempt` (:488), `save_snapshot` (:567), `record_restore` (:644), `record_evidence` (:671), `propose` (:744), `record_check` (:822), `decide` (:874), **`attach_attempt` (:1409)**, `record_failed_operation` (:1218), migration txn (:1817). Implement against the rule, not the enumeration, so no writer (e.g. the easily-missed `attach_attempt`) is left on `DEFERRED`.
  - `new_id` (~1788) `format!("{prefix}_{millis}_{nanos}")`; used at :503, :516, :575, :672, :745, :746, :823, :875.
  - **Nine** "latest" selectors to tiebreak: `:426` (operations-for-request — the idempotency replay lookup), `:570` (latest snapshot, parent pointer), `:939` (latest decision for revision), `:1456` (latest snapshot for attempt), `:1479` (latest evidence for attempt), `:1543` (latest proposal_revision, orders on `pr.created_at_ms`), `:1654` (latest check), `:1696` (latest decision), `:1714` (latest publication). (The task summary said "seven"; research confirmed nine.)
  - Idempotency today: unique index `idx_operations_request_id` on `operations(repo_id, request_id)` (migrations/001_init.sql:28-30) gates only the operations table; domain inserts already happen **in the same transaction** as the operations row, so they roll back atomically. The gap is the **pre-flight replay read** in the CLI (`command_result`, `crates/forge-cli/src/main.rs:566-656`, calling `operation_for_request` on a separate connection at :582-584 *before* any write lock).
- `crates/forge-core/src/lib.rs` — `unique_suffix` (:88-95) feeds `RepositoryId`/`OperationId`/`ViewId`; `now_ms` (:81-86) is the `created_at_ms` source. **Fix R4 must change this generator too**, not just `forge-store::new_id`.
- `crates/forge-content-native/src/lib.rs` — `write_object` (:147-164): `create_dir_all` (:156, :157) → `NamedTempFile` → `sync_all` (:160) → `persist` (:161) → `best_effort_sync_dir(parent)` (:162). `best_effort_sync_dir` (:505-509) swallows both the open and the `sync_all` error (`let _ =` at :507).
- `Cargo.toml` — centralized `[workspace.dependencies]`; crates use `dep.workspace = true`. `uuid`/`ulid` absent; `getrandom` already present transitively. `rusqlite 0.32.1` features `["bundled"]` (SQLite 3.46.0). `rusqlite` is a `forge-cli` dev-dependency, used for direct-DB assertions in tests.
- Tests — `crates/forge-cli/tests/common/mod.rs` (`TestRepo::new_git`, `repo.forge()`); `crates/forge-cli/tests/forge_start_save.rs:88-178` has the existing **single-process** request-id replay tests and the direct-DB-count idiom (`SELECT COUNT(*) …`). No concurrency or crash test exists anywhere.

### Institutional Learnings

- `docs/solutions/` contains only a README — **no prior `/ce-compound` solution docs**. The durable prior art is in completed plans:
  - Plan 003 (native content store) already shipped the temp+`sync_all`+`persist`+best-effort-dir-sync recipe and explicitly warned *"do not claim database-grade durability until crash simulation exists"* — so U1 is a correctness fix to an acknowledged best-effort path, not a new guarantee.
  - Plan 002 (hardening) established that idempotency state lives in SQLite, replay is **command-aware and status-aware** (success replays the result; failure replays the failure; different command ⇒ `REQUEST_ID_CONFLICT`), and flagged that "idempotency, CAS, and proposal binding touch the same store flows" — so changing transaction behavior must re-verify all three.
- **After Phase 1a lands, run `/ce-compound`** — SQLite WAL/`busy_timeout`/`IMMEDIATE` rationale and the ID-scheme change are exactly the non-obvious learnings the empty solutions folder should capture.

### External References

- SQLite WAL + concurrency: `journal_mode=WAL` is **persistent** (header bytes, survives reopen); `busy_timeout` and `synchronous` are **per-connection** and must be re-applied every open. `synchronous=NORMAL` is the correct WAL pairing (crash-safe; only risks losing the last commit on power loss). https://www.sqlite.org/wal.html
- The TOCTOU trap: a `DEFERRED` txn that reads then writes can fail the lock upgrade with `SQLITE_BUSY_SNAPSHOT` (517), which `busy_timeout` does **not** retry. `BEGIN IMMEDIATE` takes the write lock up front, eliminating the upgrade race; its `SQLITE_BUSY` *is* retried by `busy_timeout`. Wrap writers in a bounded retry that also catches 517. https://www.sqlite.org/lang_transaction.html , https://www.sqlite.org/rescode.html
  - **Scope limit (confirmed by review):** `BEGIN IMMEDIATE` makes reads-and-writes atomic only *within the writer transaction's own connection*. It does **not** retroactively protect a "latest" read done on a **separate connection before** the txn begins — exactly what `propose` (:741), `record_check` (:806-808), and `record_evidence` (:669) do today (each calls `latest_snapshot_for_attempt`/`latest_evidence_for_attempt`, which open their own connection at ~:1452/:1475). Two such commands can read the same "latest" and both commit. Phase 1a closes the single-connection upgrade race and the request-id replay race (U5); cross-read atomicity for these three writers is handled by moving their determining reads into the `IMMEDIATE` txn (U4), and any residual is deferred to Phase 1b.
- WAL pitfalls (→ risks): same-host filesystem only (NFS/cloud-sync corrupts); `forge.db` + `-wal` + `-shm` are an atomic set; checkpoint starvation under always-on readers. 
- macOS durability: Rust's `File::sync_all()` already issues `F_FULLFSYNC` on Darwin — prefer it over raw `libc::fsync`; there is no portable `F_FULLFSYNC` for a directory fd, so directory durability uses `File::open(dir)?.sync_all()` (maps to `fsync`, the best the OS exposes for directory entries). https://github.com/rust-lang/rust/issues/55920
- ID choice: `uuid = { version = "1.23.1", features = ["v7"] }` — published 2026-04-16 (clears the 4-day gate), pulls only `getrandom` (vs `ulid`'s full `rand` stack), MSRV 1.85 < pinned 1.92, IETF RFC 9562. `Uuid::now_v7()` is intra-process monotonic; cross-process same-ms order is non-deterministic, which is why R5's tiebreak matters.
- rusqlite 0.32 API: `pragma_update(None, "journal_mode", "WAL")` **errors** (`ExecuteReturnedResults`) because the pragma returns a row — use `execute_batch("PRAGMA journal_mode=WAL;")` (or `pragma_update_and_check`). `synchronous`/`foreign_keys` via `pragma_update`. `Connection::busy_timeout(Duration)` helper. `transaction_with_behavior(TransactionBehavior::Immediate)` for writers. https://docs.rs/rusqlite/0.32.1/

---

## Key Technical Decisions

- **`uuid` v7 over `ulid`:** smaller dependency footprint (only `getrandom`, which is already transitively present, vs `ulid`'s `rand` stack), IETF-standard, satisfies the small/audited policy (R7). Both are time-sortable; footprint is the tiebreaker.
- **Preserve the human-readable prefix:** mint as `format!("{prefix}_{}", Uuid::now_v7().simple())` so the `repo_`/`op_`/`view_`/domain prefixes and the `Display`/TEXT-PK contract are unchanged; only the suffix body becomes collision-resistant and sortable. No schema migration.
- **Tiebreak on `rowid DESC`, not `id DESC`:** rowid is monotonic by insertion order and format-independent, so it sorts correctly across the transition where a table holds both old `{millis}_{nanos}` rows and new UUIDv7 rows. (`id DESC` would mis-order mixed formats.) Applied as `ORDER BY created_at_ms DESC, rowid DESC LIMIT 1`.
- **WAL via `execute_batch`, `synchronous=NORMAL`, `busy_timeout` via the typed helper** — set on every `open_connection` (readers included; WAL lets readers not block the writer). `synchronous=FULL` for decision points is deferred.
- **`BEGIN IMMEDIATE` only on write transactions** (every writer `transaction()` site — see Context; the INSERT/UPDATE rule, not a fixed count), not on read-only `query_row` calls. A small bounded-retry helper wraps the immediate-txn body and retries on `SQLITE_BUSY`/`SQLITE_BUSY_SNAPSHOT` with jittered backoff.
- **Idempotency reframe:** the fix is to make the replay check **serialize against the write**, not to add missing domain-row idempotency (which already exists via same-txn atomic rollback). **Commit to one structure (review found the two are not correctness-equivalent):** re-run the `operation_for_request` existence check **inside the `IMMEDIATE` transaction as the first statement, sharing that one txn with all domain inserts**, so a concurrent same-id retry observes the first writer's committed row and takes the clean replay path. The unique-violation-catch variant is a fallback only, used if the control flow makes the in-txn re-check impractical.

---

## Open Questions

### Resolved During Planning

- *Which ID crate?* → `uuid` 1.23.1 with `v7` (see decisions).
- *Does R4 need a migration?* → No. IDs are opaque TEXT primary keys; existing rows are untouched; new mints use the new format. `rowid` tiebreak needs no schema change (tables are not `WITHOUT ROWID`).
- *How many selector sites?* → Nine (enumerated above), not seven.
- *Is domain-row idempotency actually missing?* → No; it is transitively gated by the same-transaction operations-row unique index. The real gap is the unsynchronized pre-flight replay read.

### Deferred to Implementation

- Exact bounded-retry parameters (attempt count, backoff/jitter) — tune against the U6 ≥8-process test; start at ~5 attempts with small jittered backoff and a `busy_timeout` of ~5s.
- The exact placement of option (a)'s in-txn `operation_for_request` re-check within `command_result`'s control flow (the *structure* is pinned to option (a) in Key Technical Decisions; only the code arrangement is deferred).
- Whether `record_restore`/`init_repository` need `IMMEDIATE` for correctness or only for consistency — apply uniformly unless a read-only fast path is clearly safe.

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

```
open_connection(path):
    conn = Connection::open(path)
    conn.busy_timeout(5s)                                   # per-connection (R2)
    conn.execute_batch("PRAGMA journal_mode=WAL;")          # persistent; pragma_update would error
    conn.pragma_update(None, "synchronous", "NORMAL")       # WAL pairing
    conn.pragma_update(None, "foreign_keys", "ON")          # unchanged

with_write_txn(conn, |tx| { ... }):                         # R3 — wraps every writer
    retry up to N with jittered backoff on SQLITE_BUSY | SQLITE_BUSY_SNAPSHOT:
        tx = conn.transaction_with_behavior(Immediate)      # write lock at BEGIN
        result = body(tx)
        tx.commit()

new_id(prefix) -> format!("{prefix}_{}", Uuid::now_v7().simple())   # R4 (forge-store)
unique_suffix() -> Uuid::now_v7().simple().to_string()             # R4 (forge-core)

latest selector -> "... ORDER BY created_at_ms DESC, rowid DESC LIMIT 1"   # R5 (×9)

write_object(...):                                          # R1
    create ancestor dirs, fsync each newly-created ancestor (propagate error)
    temp.write_all; temp.sync_all()?; temp.persist(path)?
    sync_dir(parent)?      # was best-effort `let _ = sync_all`; now propagated
```

---

## Implementation Units

### U1. Crash-safe object-write durability (native store)

**Goal:** Remove the swallowed fsync on the object-write path and fsync newly-created ancestor directories, so a returned `Ok` from `write_object` means the object file *and* its directory entries reached the fsync layer.

**Requirements:** R1, R8

**Dependencies:** None

**Files:**
- Modify: `crates/forge-content-native/src/lib.rs` (`write_object` ~147-164; replace `best_effort_sync_dir` ~505-509 with a `Result`-returning `sync_dir`; fsync ancestors created at :156-157)
- Test: `crates/forge-content-native/src/lib.rs` (unit tests, alongside `loose_object_write_is_idempotent_and_verified` ~556)

**Approach:**
- Replace `best_effort_sync_dir(path)` with `fn sync_dir(path) -> Result<()>` that opens the dir and `sync_all()`s it, `.with_context(...)`-wrapping failures (anyhow style, matching :116/:384). Propagate out of `write_object`.
- When `create_dir_all` creates new ancestor levels (e.g. the first object in a fresh `sha256/<xx>/`), fsync each newly-created ancestor on first creation, not just the immediate parent (R1 correctness requirement). Probing which levels are new (to skip re-syncing already-durable dirs) is a **performance optimization, not a correctness requirement** — fsyncing the immediate parent unconditionally already satisfies R1; add the skip-probe only if write latency proves to be a problem.
- Keep the change away from `is_secret_risk_path`/policy logic (R8). Use `File::sync_all()` (gets macOS `F_FULLFSYNC` for free).
- Scope strictly to the **write** path; the restore path (`materialize_tree` :387/:399) and store-before-DB ordering are Phase 1b.

**Patterns to follow:** existing `with_context` error wrapping in this file; the existing temp+`sync_all`+`persist` sequence (keep it, add durable-dir propagation).

**Test scenarios:**
- Happy path: writing a new object into a fresh shard directory returns `Ok` and the object is readable/verifiable afterward (extends the idempotent-write test).
- Error path: when directory fsync cannot succeed (simulate via an unwritable/again-removed parent or an injected error seam), `write_object` returns `Err` with context rather than silently succeeding.
- Edge case: writing a second object into an already-existing shard dir does not error and does not redundantly fail on ancestor fsync (already-durable dirs handled).
- Static check: `grep` over durability paths shows no `let _ = .*sync` remains (also a U6 / CI-style assertion).

**Verification:** `write_object` has no error-swallowing sync; new objects in fresh shards round-trip; `cargo test -p forge-content-native` passes.

---

### U2. Collision-resistant, sortable IDs (UUIDv7) + deterministic latest-selector tiebreaks

**Goal:** Replace the `{millis}_{nanos}` scheme in both generators with UUIDv7 and make every "latest" selector deterministic under same-millisecond ties.

**Requirements:** R4, R5, R7

**Dependencies:** None

**Files:**
- Modify: `Cargo.toml` (add `uuid = { version = "1.23.1", features = ["v7"] }` to `[workspace.dependencies]`)
- Modify: `crates/forge-core/Cargo.toml` (`uuid.workspace = true`), `crates/forge-core/src/lib.rs` (`unique_suffix` :88-95)
- Modify: `crates/forge-store/Cargo.toml` (`uuid.workspace = true`), `crates/forge-store/src/lib.rs` (`new_id` :1788; the nine selectors at :426, :570, :939, :1456, :1479, :1543, :1654, :1696, :1714)
- Test: `crates/forge-core/src/lib.rs` (unit tests for ID format/uniqueness/sortability); selector behavior covered in U6.

**Approach:**
- `unique_suffix()` → `Uuid::now_v7().simple().to_string()`; keep `RepositoryId`/`OperationId`/`ViewId` prefixes. `forge-store::new_id(prefix)` → `format!("{prefix}_{}", Uuid::now_v7().simple())`.
- Add `, rowid DESC` to each of the nine `ORDER BY created_at_ms DESC LIMIT 1` selectors → `ORDER BY created_at_ms DESC, rowid DESC LIMIT 1`. The ASC list/enumeration selectors (~:1340/:1363/:1572) are **out of scope for Phase 1a** — they are not "latest" selectors and R5 does not cover them; if a tiebreak is wanted there, file a follow-up ticket rather than deciding "if low-cost" at implementation time.
- Confirm the 4-day gate at install time before pinning; `uuid 1.23.1` is 42 days old as of today (R7).
- `now_ms()` stays the `created_at_ms` source (unchanged); only the ID *body* changes.

**Patterns to follow:** workspace-dependency style (`dep.workspace = true`); the existing prefixed-newtype `Display` contract.

**Test scenarios:**
- Happy path: a minted ID matches `^<prefix>_[0-9a-f]{32}$` and two IDs minted back-to-back are distinct and lexically ordered by creation.
- Edge case (the exit criterion): minting 10,000 IDs in a tight loop yields zero duplicates (`HashSet` len == 10k). Cross-process collision-freedom is asserted in U6.
- Edge case: two rows inserted in the same millisecond are returned in deterministic order by the `created_at_ms DESC, rowid DESC` selector (smoke test via the store; full coverage in U6).

**Verification:** no `{millis}_{nanos}` remains in either generator; `cargo test -p forge-core` passes; all nine DESC selectors carry the tiebreak; the chosen `uuid` version (1.23.1, 42 days old today) is re-confirmed to clear the ≥4-day release-age gate at install time before pinning (R7).

---

### U3. SQLite connection hardening (WAL + synchronous=NORMAL + busy_timeout)

**Goal:** Configure every connection for safe multi-process access.

**Requirements:** R2, R8

**Dependencies:** None

**Files:**
- Modify: `crates/forge-store/src/lib.rs` (`open_connection` ~1875-1879)
- Test: `crates/forge-cli/tests/` (assert WAL engaged after `init`); store-level smoke test.

**Approach:**
- In `open_connection`: `conn.busy_timeout(Duration::from_secs(5))?;` then `conn.execute_batch("PRAGMA journal_mode=WAL;")?;` (NOT `pragma_update`, which errors on the row-returning pragma) then `pragma_update(None, "synchronous", "NORMAL")` and keep `foreign_keys=ON`.
- Optionally assert WAL engaged with `pragma_update_and_check`/`pragma_query_value` in a debug assertion.
- Note: the first open of an existing v0 DB converts it to WAL persistently and creates `forge.db-wal`/`forge.db-shm` sidecars (covered by the existing `/.forge/` gitignore).

**Patterns to follow:** the existing single-line `pragma_update` for `foreign_keys`.

**Test scenarios:**
- Happy path: after `forge init`, opening `.forge/forge.db` and querying `PRAGMA journal_mode` returns `wal`.
- Edge case: re-opening an existing pre-WAL DB succeeds and converts it (no error, data intact).
- Integration: a normal single-process lifecycle (`init → start → save → run → propose → check → accept → export`) still passes end-to-end with WAL on (regression guard).

**Verification:** `journal_mode=wal` post-init; full existing integration suite green; `cargo test --workspace` passes.

---

### U4. IMMEDIATE write transactions + bounded busy-retry

**Goal:** Make all writers take the write lock up front and absorb transient contention, eliminating the deferred-upgrade `SQLITE_BUSY_SNAPSHOT` race.

**Requirements:** R3

**Dependencies:** U3

**Files:**
- Modify: `crates/forge-store/src/lib.rs` (the ~11 `connection.transaction()` writer sites listed in Context; add a small `with_immediate_retry` helper)
- Test: `crates/forge-cli/tests/` (concurrency behavior; full exit-criteria test in U6)

**Approach:**
- Replace `connection.transaction()` with `connection.transaction_with_behavior(TransactionBehavior::Immediate)` at the writer sites only (not read-only `query_row` paths). Apply the INSERT/UPDATE rule from Context — do not work a fixed list — so `attach_attempt` (:1409) is not missed.
- Where a writer's *determining* read currently runs on a separate connection before the txn (`propose`/`record_check`/`record_evidence` calling `latest_snapshot_for_attempt`/`latest_evidence_for_attempt`), move that read **inside** the `IMMEDIATE` txn (query on `tx`, as `save_snapshot` already does at :568) so the read-then-write is genuinely atomic. If a determining read cannot be moved into the txn, document it and defer to Phase 1b rather than implying it is covered.
- Introduce a bounded-retry helper wrapping the begin→body→commit on a fresh `IMMEDIATE` txn; retry on `SQLITE_BUSY` and `SQLITE_BUSY_SNAPSHOT` (extended code 517) with jittered backoff, bounded (~5 attempts). Surface exhaustion as the current generic failure (no new error code in Phase 1a — Phase 2 owns typed errors).
- Re-verify the CAS/operation-advance and proposal-binding flows still behave (plan 002 flagged these as coupled).

**Patterns to follow:** existing transaction usage; `anyhow` error propagation.

**Test scenarios:**
- Integration: two processes writing concurrently both succeed (one waits on the lock and retries), neither returns `SQLITE_BUSY` to the user.
- Edge case: a read-then-write command under contention does not surface `SQLITE_BUSY_SNAPSHOT` (the IMMEDIATE lock prevents the stale-snapshot upgrade).
- Regression: the existing `REQUEST_ID_CONFLICT`, failed-replay, and proposal-binding tests still pass.

**Verification:** all writers use IMMEDIATE; concurrent writes serialize cleanly; existing suite green.

---

### U5. Serialize the `--request-id` replay check under concurrency

**Goal:** Make a concurrent same-`request-id` retry replay the original result cleanly instead of colliding at commit.

**Requirements:** R6

**Dependencies:** U3, U4

**Files:**
- Modify: `crates/forge-cli/src/main.rs` (`command_result` ~566-656) and/or `crates/forge-store/src/lib.rs` (`operation_for_request` ~418-440 and the writer entry points) to make the replay check observe a committed row under serialization.
- Test: `crates/forge-cli/tests/` (concurrent same-id behavior; counts in U6)

**Approach:**
- Serialize the replay decision against the write using **option (a)**: re-check `operation_for_request` *inside* the `IMMEDIATE` transaction, as the **first statement**, before any domain insert; if a committed row exists, roll back and take the existing command-aware/status-aware replay path (success→replay result, failure→replay failure, different command→`REQUEST_ID_CONFLICT`). The unique-violation-catch variant (b) is a documented fallback only — it is **not** correctness-equivalent, because it depends on the operations insert being ordered relative to the domain inserts.
- Preserve the exact observable contract from plan 002 (command-aware + status-aware replay) — only the *concurrent* behavior changes.

**Patterns to follow:** the existing `command_result` replay branches (:589 conflict, :609 failed-replay, :616 success-replay); the `idx_operations_request_id` unique index.

**Test scenarios:**
- Integration (the exit criterion): N processes invoke the same mutating command with one shared `--request-id`; exactly one set of domain rows exists afterward and every invocation returns the original result (`idempotent_replay == true` for the losers).
- Error path: same `--request-id` reused for a *different* command still yields `REQUEST_ID_CONFLICT` under concurrency.
- Regression: the single-process replay tests (`forge_start_save.rs:88-178`) still pass unchanged.

**Verification:** concurrent same-id retries produce exactly one domain row and replay the original; conflict semantics preserved.

---

### U6. Concurrency + durability exit-criteria integration tests

**Goal:** Prove the roadmap exit criteria end to end with a multi-process harness.

**Requirements:** R1, R2, R3, R4, R5, R6

**Dependencies:** U1, U2, U3, U4, U5

**Files:**
- Create: `crates/forge-cli/tests/forge_concurrency.rs`
- Test: same file (uses `mod common;`, `TestRepo::new_git`, `assert_cmd`, and `rusqlite` direct-DB asserts)

**Approach:**
- Spawn ≥8 concurrent child `forge` **OS processes** — each via `std::process::Command`, launched from `std::thread::spawn` threads (the threads are only the launch harness; the unit of concurrency must be a separate **process**, since an in-process multi-threaded test sharing one connection would not exercise WAL multi-process semantics) — running the compete loop against one shared `TestRepo`; assert no child returned a `SQLITE_BUSY`-class error in its JSON `errors[]`, and that `SELECT COUNT(*) = COUNT(DISTINCT id)` across the minted-ID tables (zero collisions). Tune the U4 retry parameters against this test **before** asserting zero-`SQLITE_BUSY`, and bound induced contention so the retry budget provably absorbs it — otherwise restate the assertion as "no *unretried* `SQLITE_BUSY` within the retry budget" so the gate is deterministic rather than flaky.
- A 10,000-mint loop (in-process or across processes) asserting zero ID collisions (R4 exit).
- N concurrent processes with one shared `--request-id` asserting `SELECT COUNT(*)` of the produced domain rows (snapshot/evidence/proposal) is exactly 1 (R6 exit), reusing the `forge_start_save.rs:143-151` count idiom.
- A static assertion (test or CI grep) that no `let _ = .*sync` remains on a durability path (R1 exit).

**Patterns to follow:** `crates/forge-cli/tests/common/mod.rs`; the direct-DB-count assertions in `forge_start_save.rs`.

**Test scenarios:**
- Integration: ≥8 concurrent processes complete the compete loop with zero `SQLITE_BUSY` and zero ID collisions.
- Integration: 10k-mint loop → zero duplicate IDs.
- Integration: concurrent same-`request-id` → exactly one domain row.
- Static: no swallowed sync remains on durability paths.

**Verification:** all four exit-criteria assertions pass on Linux and macOS; `cargo test --workspace` green.

---

## System-Wide Impact

- **Interaction graph:** `open_connection` is the chokepoint for every store call (reads and writes) — PRAGMA changes are process-wide; the `IMMEDIATE` change is scoped to writers. `command_result` wraps every mutating CLI command, so U5 touches the single idempotency gate.
- **Error propagation:** new failure surfaces (durability fsync error from U1; busy-retry exhaustion from U4) flow through `anyhow`. U1's error is a real new `Err`; U4's exhaustion stays generic `COMMAND_FAILED` (no new code until Phase 2). `error_code` substring-matching (`main.rs:739-745`) is unchanged.
- **State lifecycle risks:** changing transaction behavior touches the coupled idempotency / CAS-advance / proposal-binding flows (plan 002) — the full integration suite is the guard.
- **API surface parity:** JSON envelope (`forge.cli.v0`) unchanged (R8); no new commands, flags, or error codes.
- **Integration coverage:** concurrency and durability are inherently multi-process / crash-adjacent — unit tests can't prove them, hence the U6 process-level harness.
- **Unchanged invariants:** ID *format* changes but ID *opacity* and the `<prefix>_` convention do not; `created_at_ms` source unchanged; security defaults (secret-risk exclusion, redaction, `EXCERPT_LIMIT`) untouched; `DIRTY_WORKTREE`/`REQUEST_ID_CONFLICT` observable behavior preserved. **`STALE_BASE` caveat:** `accept`'s base-head check is a CLI-layer read-then-act (`main.rs` reads `current_head` via git and compares to `proposal.base_head` *outside* any DB txn), so `BEGIN IMMEDIATE` does **not** make it atomic against a concurrent HEAD move; its single-process behavior is preserved, but concurrent-accept atomicity is deferred to Phase 1b (advisory lock / in-txn CAS).

---

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| `pragma_update` used for WAL → `ExecuteReturnedResults` at runtime | Med | High | Use `execute_batch("PRAGMA journal_mode=WAL;")`; assert mode in a test (U3). |
| `SQLITE_BUSY_SNAPSHOT` (517) not retried by `busy_timeout`, deferred writers deadlock | Med | High | `BEGIN IMMEDIATE` everywhere a write happens (U4) + bounded retry catching 517. Never write inside a `DEFERRED` txn. |
| WAL on a network/cloud-synced `.forge` corrupts the DB | Low | High | Document the same-host constraint (Scope/Deferred); detection is a later item. `.forge` is normally repo-local. |
| `forge.db` copied without `-wal`/`-shm` loses committed data | Low | High | Out of scope to fully solve here; note for init/export/backup (Phase 1b). No Phase 1a code copies the DB. |
| `BEGIN IMMEDIATE` does NOT cover "latest" reads done on a separate connection before the txn (`propose`/`record_check`/`record_evidence`) | Med | Med | Move those determining reads inside the `IMMEDIATE` txn (U4); defer residual cross-read atomicity + `accept` `STALE_BASE` to Phase 1b. Do not claim read-then-write atomicity beyond what is actually moved. |
| WAL `-wal`/`-shm` sidecars hold committed-but-uncheckpointed evidence excerpts; copying `.forge` wholesale can exfiltrate them | Low | Med | `.forge` stays gitignored and excluded from snapshots/exports by `is_ignored_by_policy` in both backends; document checkpoint-before-copy and the inverse-leak direction (operational notes); a checkpoint-before-copy helper is Phase 1b. |
| Ancestor-dir fsync adds latency on every write | Low | Med | fsync ancestors **only on first creation** (probe existence); benchmark in U6 if needed. |
| Changing txn behavior regresses idempotency/CAS/proposal-binding | Med | High | Run full `cargo test --workspace` after U4 and U5; plan 002's tests are the regression net. |
| `uuid` latest release younger than 4 days at install | Low | Med | Pin 1.23.1 (42 days old); verify at install; fall back to an older safe pin if needed (R7). |
| macOS directory fsync is weaker than `F_FULLFSYNC` | Low | Low | Use `File::sync_all()` (best the OS exposes for dir entries); accept as known platform limit; full crash proof is Phase 1b. |
| Mixed old/new ID formats mis-sort if tiebreak used `id DESC` | Low | Med | Tiebreak on `rowid DESC` (format-independent), not `id DESC`. |

---

## Phased Delivery

### PR A — Durability + IDs (U1, U2)
Lands first; both are dependency-free and independently testable (durability assertion; single-process ID uniqueness/sortability + the nine tiebreaks). Low blast radius, no concurrency semantics yet.

### PR B — Concurrency + idempotent replay (U3, U4, U5, U6)
Builds on PR A. U3 (PRAGMAs) → U4 (IMMEDIATE + retry) → U5 (serialized replay) → U6 (the multi-process exit-criteria suite that validates the combined Phase 1a outcome). If PR B's diff is large, U6 may split into its own follow-up, but the exit criteria are only provable once U3–U5 are in.

---

## Documentation / Operational Notes

- After merge: flip this plan's frontmatter to `status: completed`, move to `docs/plans/completed/`, set Linear NER-131 → Done, and run `/ce-compound` to capture the WAL/`busy_timeout`/`IMMEDIATE` rationale and the ID-scheme change in `docs/solutions/` (the folder is currently empty of solution docs).
- **WAL sidecars (security + durability):** `forge.db` now travels with `-wal`/`-shm` sidecars. They are excluded from git by `/.forge/` **and** from snapshots/exports by `is_ignored_by_policy` (native `lib.rs:477`; git backend) — any new export path must preserve **both** layers (add a unit test asserting `is_ignored_by_policy` rejects `.forge/` paths). Before any backup/copy of `.forge`, run `PRAGMA wal_checkpoint(TRUNCATE)` and close connections: copying `forge.db` alone loses committed data, and copying all three files wholesale can carry committed-but-uncheckpointed evidence excerpts. A checkpoint-before-copy helper is Phase 1b.

---

## Sources & References

- **Origin:** `docs/ROADMAP.md` — Phase 1a (and "The single most important thing to build next"); Linear **NER-131** (milestone *M1 — Bulletproof the ledger*).
- Related code: `crates/forge-store/src/lib.rs`, `crates/forge-core/src/lib.rs`, `crates/forge-content-native/src/lib.rs`, `crates/forge-cli/src/main.rs`, `crates/forge-store/migrations/001_init.sql`, `crates/forge-cli/tests/common/mod.rs`, `crates/forge-cli/tests/forge_start_save.rs`, `Cargo.toml`.
- Prior plans: `docs/plans/completed/2026-05-28-002-hardening-forge-v0-local-loop-plan.md` (idempotency semantics), `docs/plans/completed/2026-05-28-003-feat-native-forge-content-store-plan.md` (object-write recipe + durability caveat).
- External: SQLite WAL (`sqlite.org/wal.html`), transaction semantics (`sqlite.org/lang_transaction.html`), result codes (`sqlite.org/rescode.html`), rusqlite 0.32.1 docs (`docs.rs/rusqlite/0.32.1/`), `uuid` crate (RFC 9562 / UUIDv7), Rust `sync_all` + `F_FULLFSYNC` on macOS (rust-lang/rust#55920).
