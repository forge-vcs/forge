---
title: Make a SQLite-backed CLI safe for many concurrent processes (WAL + IMMEDIATE + idempotent replay)
date: 2026-05-29
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: database
severity: high
applies_when:
  - Multiple short-lived processes (e.g. fanned-out agents) write one local SQLite file in parallel
  - A mutating command must be safely retriable by id without double-applying its effects
  - Writers read a "latest" row and then write based on it (read-then-write)
tags: [sqlite, wal, concurrency, busy-timeout, begin-immediate, idempotency, request-id, rusqlite, uuidv7, fsync]
---

# Make a SQLite-backed CLI safe for many concurrent processes (WAL + IMMEDIATE + idempotent replay)

## Context

Forge records the lifecycle of agent changes in `.forge/forge.db` (SQLite via `rusqlite` "bundled"). v0 was scoped to a single-process loop, so the substrate was never hardened for the competing-attempts wedge — many agents driving one repo in parallel. In that mode the default config fails hard: the rollback journal makes a second concurrent writer hard-fail with `SQLITE_BUSY` immediately, timestamp-only ids collide, and the `--request-id` replay check runs on a separate read-only connection *before* any write lock, so two racing retries both pass it and collide at commit. This doc captures the non-obvious SQLite + idempotency learnings from hardening it (Phase 1a, NER-131, PRs #5/#6) so the next persistence layer that needs multi-process safety does not re-derive them.

## Guidance

### 1. Split connection PRAGMAs by persistence, and set them on *every* open

`journal_mode=WAL` is **persistent** (a database-header byte; survives reopen). `busy_timeout` and `synchronous` are **per-connection** and must be re-applied on every `Connection::open`. WAL is what lets readers run without blocking the single writer — the precondition for multi-process use.

```rust
fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(Duration::from_secs(5))?;        // per-connection
    connection.execute_batch("PRAGMA journal_mode=WAL;")?;   // persistent; see gotcha below
    connection.pragma_update(None, "synchronous", "NORMAL")?; // per-connection; WAL-safe pairing
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}
```

**`rusqlite` gotcha:** `pragma_update(None, "journal_mode", "WAL")` **errors** with `ExecuteReturnedResults`, because `journal_mode` returns a row. Use `execute_batch("PRAGMA journal_mode=WAL;")` (or `pragma_update_and_check`). `synchronous`/`foreign_keys` are fine via `pragma_update`. `synchronous=NORMAL` is the correct WAL pairing: crash-safe, only the last commit can be lost on power loss, never the database.

### 2. `BEGIN IMMEDIATE` on every writer + a bounded busy-retry

A `DEFERRED` transaction (rusqlite's `connection.transaction()` default) that reads then writes can fail the lock upgrade with `SQLITE_BUSY_SNAPSHOT` (extended code **517**) — and **`busy_timeout` does NOT retry 517**. `BEGIN IMMEDIATE` takes the write lock at `BEGIN`, eliminating the upgrade race; its plain `SQLITE_BUSY` *is* absorbed by `busy_timeout`. Apply IMMEDIATE to *every* transaction that does an INSERT/UPDATE — implement against that **rule**, not a hand-maintained list (the easily-missed sites are the ones that only `UPDATE`, plus the migration txn).

```rust
fn with_immediate_retry<T, F>(connection: &mut Connection, mut body: F) -> Result<T>
where F: FnMut(&Transaction<'_>) -> Result<T> {           // FnMut: may run once per attempt
    let mut attempt = 0;
    loop {
        attempt += 1;
        let once = (|| {
            let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let v = body(&tx)?;     // any Err here drops tx => rollback
            tx.commit()?;
            Ok(v)
        })();
        match once {
            Ok(v) => return Ok(v),
            Err(e) if attempt < MAX && is_retryable_busy(&e) => { sleep_backoff(attempt); }
            Err(e) => return Err(e),
        }
    }
}
```

Detect busy by **walking the anyhow source chain** (errors may be `.context()`-wrapped) and matching the *primary* code — `ErrorCode::DatabaseBusy` already covers every `SQLITE_BUSY_*` extended code including 517, so the explicit `== 517` check is documentation, not function:

```rust
fn is_retryable_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|c| matches!(
        c.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(e, _))
            if matches!(e.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) || e.extended_code == 517))
}
```

Because the body is `FnMut` (re-run on retry), it cannot move captured values out — clone owned inputs (`request_id`, etc.) per attempt, and mint ids *inside* the body so the committed attempt's ids are what you return.

### 3. Move the *determining* read inside the IMMEDIATE txn

`BEGIN IMMEDIATE` only makes read-then-write atomic **within one connection's transaction**. A "latest" read done on a *separate connection before* the txn (a common helper-opens-its-own-connection pattern) is not protected — two writers can read the same "latest" and both commit. Move the determining read onto `tx`. Reuse the existing query function without duplicating SQL by giving it a `&Connection` parameter: `rusqlite::Transaction` derefs to `Connection`, so `&tx` deref-coerces to `&Connection`.

```rust
fn latest_snapshot_on(conn: &Connection, attempt_id: &str) -> Result<Option<Snapshot>> { /* query */ }
// outside a txn: latest_snapshot_on(&open_connection(path)?, id)
// inside a writer: latest_snapshot_on(&tx, id)   // &tx: &Transaction -> &Connection
```

### 4. Idempotency under concurrency = re-check inside the write lock, signal replay with a sentinel error

A CLI-layer pre-flight "has this `request_id` run?" read is a good fast path for sequential retries, but it cannot stop the *concurrent* race (both retries pass it before either commits). Close it by re-checking **inside** the IMMEDIATE txn as the **first statement** — under serialized write locks, the loser now observes the winner's committed row:

```rust
fn replay_guard(tx: &Transaction, repo_id: &str, request_id: Option<&str>) -> Result<()> {
    if let Some(rid) = request_id {
        if let Some(op) = existing_operation(tx, repo_id, rid)? {  // query on tx
            return Err(RequestIdReplay { operation: op }.into());  // abort, no domain rows
        }
    }
    Ok(())
}
```

Propagate the "this is a replay, not a failure" decision across the store→CLI boundary with a one-off **sentinel error** carried inside `anyhow::Error` and recovered by `downcast_ref` — the idiomatic anyhow way, and far less invasive than threading a `Committed | Replayed` enum through every writer's return type and every call site:

```rust
match f(cwd, request_id.clone()) {        // CLI command wrapper
    Err(e) if e.downcast_ref::<RequestIdReplay>().is_some() =>
        replay_response(command, request_id, /* the carried operation */),
    /* ...normal Ok / failure handling... */
}
```

Pick the in-txn re-check, **not** "let the unique index throw and catch the violation" — the two are *not* correctness-equivalent (the catch variant depends on insert ordering and surfaces a constraint error instead of a clean replay). Preserve whatever replay contract you already have (here: success→replay result, failure→replay failure, different command→`REQUEST_ID_CONFLICT`).

### 5. Collision-resistant, sortable ids + tiebreak on `rowid`, not `id`

Replace `{millis}_{nanos}` mint schemes with UUIDv7 (`uuid` crate, `v7` feature — pulls only `getrandom`; smaller than `ulid`'s `rand` stack). Keep the human-readable prefix: `format!("{prefix}_{}", Uuid::now_v7().simple())`. Ids stay opaque TEXT keys, so this is a forward-only mint change — no migration. For every `ORDER BY created_at_ms DESC LIMIT 1` "latest" selector, break same-millisecond ties on **`rowid DESC`**, never `id DESC`: `rowid` is insertion-ordered and format-independent, so it sorts correctly across the transition where a table holds both old `{millis}_{nanos}` and new UUIDv7 rows.

### 6. Don't swallow durability fsyncs

On the object-write path, propagate (never `let _ =`) the file *and* parent-directory `sync_all()`, and fsync newly-created ancestor directories on first creation. `File::sync_all()` issues `F_FULLFSYNC` on macOS for free; there is no portable directory `F_FULLFSYNC`, so `File::open(dir)?.sync_all()` is the best the OS exposes for directory entries.

### 7. Test concurrency with real OS processes

An in-process multi-threaded test sharing one `Connection` does **not** exercise WAL multi-process semantics. Spawn real child processes (`std::process::Command`) from threads (threads are just the launch harness). Assert the user-visible contract — *no `SQLITE_BUSY`-class string in any process's output* + *exactly one domain row for a shared `request_id`* + *zero id collisions* — and back it with a direct unit test of `is_retryable_busy` (the integration test can't distinguish "busy never fired" from "busy fired and was retried"). Derive retry backoff jitter from **`process::id()` mixed with the nanosecond** so concurrent processes desync even when their clocks read the same coarse value.

## Why This Matters

Each of these is a trust hole, and for a change-control tool trust *is* the product. Without WAL, the second concurrent agent dies on `database is locked`. Without IMMEDIATE, read-then-write writers deadlock-fail on un-retried 517. Without the in-txn replay guard, a retried `save` silently creates a second snapshot or errors at commit instead of replaying. Without `rowid` tiebreaks, "latest" is nondeterministic across the id-format transition. The fixes are cheap and mostly config, but the *reasons* (which PRAGMA is persistent, why 517 escapes `busy_timeout`, why the pre-flight read can't close the race) are exactly the things that are non-obvious at 2am and expensive to rediscover.

## When to Apply

- Any time more than one process may write one SQLite file (CLIs that fan out, daemons with worker processes, test suites that spawn the binary).
- Any retriable mutation keyed by a client-supplied id (`--request-id`, idempotency key, dedup token).
- Read-then-write transactions whose write depends on the freshest row.

## Examples

**Before (single-process assumptions):** `open_connection` sets only `PRAGMA foreign_keys=ON`; writers use `connection.transaction()` (DEFERRED); the replay check runs on a separate read-only connection before any lock; ids are `{millis}_{nanos}`.

**After:** every connection opens WAL + `busy_timeout(5s)` + `synchronous=NORMAL`; every INSERT/UPDATE txn is `IMMEDIATE` wrapped in a bounded busy/517 retry with determining reads moved inside; the replay check re-runs inside the txn and signals a clean replay via the `RequestIdReplay` sentinel; ids are UUIDv7 with `rowid` tiebreaks. Exit criteria proven by an ≥8-process integration test: zero `SQLITE_BUSY`, zero id collisions, concurrent same-`request-id` → exactly one domain row.

## Scope boundaries (what `BEGIN IMMEDIATE` does *not* buy you)

`IMMEDIATE` serializes *commits*; it does not make atomic a determining read done on a **separate connection** at the CLI layer (e.g. reading an exit code to compute a verdict, or a git `HEAD` for a stale-base check) — that residual cross-read atomicity needs a repo-level advisory lock (deferred here to Phase 1b / NER-132). It also does not reclassify a transient lost-CAS conflict as retryable — if such a conflict is recorded under a `request_id`, status-aware replay will replay the failure; distinguishing transient from deterministic failures belongs with a typed-error taxonomy (Phase 2 / NER-133). Name these boundaries explicitly rather than implying atomicity you didn't deliver.

## Related

- Plan: `docs/plans/completed/2026-05-28-005-fix-substrate-phase-1a-plan.md` (U1–U6)
- Code review triage: `docs/code-reviews/2026-05-29-phase-1a-pr-b-concurrency.md`
- Implementation: `crates/forge-store/src/lib.rs` (`open_connection`, `with_immediate_retry`, `is_retryable_busy`, `replay_guard`), `crates/forge-cli/src/main.rs` (`command_result`, `replay_response`), `crates/forge-cli/tests/forge_concurrency.rs`
- Deferred follow-ups: Linear NER-132 (Phase 1b advisory lock / cross-read atomicity), NER-133 (Phase 2 typed errors)
- External: SQLite WAL (sqlite.org/wal.html), transaction semantics (sqlite.org/lang_transaction.html), result codes (sqlite.org/rescode.html), RFC 9562 (UUIDv7)
