use super::*;

#[derive(Debug, Clone)]
pub struct OperationViewInput {
    pub request_id: Option<String>,
    pub command: String,
    pub kind: String,
    pub view_kind: ViewKind,
    pub state: Value,
}

#[derive(Debug, Clone)]
pub struct OperationViewResult {
    pub operation_id: String,
    pub view_id: String,
}

pub(crate) const VISIBILITY_PRIVATE: &str = "private";
pub(crate) const VISIBILITY_TEAM: &str = "team";
pub(crate) const VISIBILITY_PUBLIC: &str = "public";
pub(crate) const VISIBILITY_EMBARGOED: &str = "embargoed";
pub(crate) const CAPABILITY_SEE_STUB: &str = "see_stub";
pub(crate) const CAPABILITY_INSPECT_CONTENT: &str = "inspect_content";
pub(crate) const CAPABILITY_INSPECT_EVIDENCE: &str = "inspect_evidence";
pub(crate) const CAPABILITY_SYNC_MATERIALIZE: &str = "sync_materialize";
pub(crate) const CAPABILITY_PUBLISH_REVEAL: &str = "publish_reveal";
pub(crate) const DEFAULT_EMBARGO_RELEASE_CONTENT_CLASSES: &[&str] =
    &["release_inputs", "sanitized_provenance"];
pub(crate) const EMBARGO_RELEASE_REVOCATION_WARNING: &str =
    "Revocation applies to future releases and does not claw back already delivered bundles.";
pub(crate) const EMBARGO_STATE_ACTIVE: &str = "active";
pub(crate) const EMBARGO_STATE_ACCEPTED_UNDER_EMBARGO: &str = "accepted_under_embargo";
pub(crate) const EMBARGO_STATE_RELEASED_UNDER_EMBARGO: &str = "released_under_embargo";
pub(crate) const EMBARGO_STATE_REVEALED: &str = "revealed";
pub(crate) const EMBARGO_STATE_PUBLISHED: &str = "published";
pub(crate) const EMBARGO_STATE_CLOSED: &str = "closed";
pub(crate) const PUBLIC_PROJECTION_PROVENANCE_ONLY: &str = "provenance_only";
pub(crate) const PUBLIC_PROJECTION_SANITIZED_SOURCE: &str = "sanitized_source";
pub(crate) const PUBLIC_PROJECTION_FULL_SOURCE: &str = "full_source";

pub(crate) fn ensure_work_package_exists(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<()> {
    let exists: bool = match work_package_kind {
        "intent" => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM intents WHERE repo_id = ?1 AND id = ?2)",
            params![repo_id, work_package_id],
            |row| row.get(0),
        )?,
        "attempt" => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE repo_id = ?1 AND id = ?2)",
            params![repo_id, work_package_id],
            |row| row.get(0),
        )?,
        "proposal" => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM proposals WHERE repo_id = ?1 AND id = ?2)",
            params![repo_id, work_package_id],
            |row| row.get(0),
        )?,
        _ => {
            return Err(ForgeError::VisibilityPolicyInvalid {
                reason: format!("unsupported work package kind `{work_package_kind}`"),
            }
            .into())
        }
    };
    if exists {
        Ok(())
    } else {
        Err(ForgeError::VisibilityPolicyUnmet {
            operation: "resolve_work_package".to_string(),
            work_package_kind: work_package_kind.to_string(),
            work_package_id: work_package_id.to_string(),
            capability: "exists".to_string(),
            disclosure: "hidden".to_string(),
        }
        .into())
    }
}

pub fn record_failed_operation(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    code: &str,
    message: &str,
    details: Value,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    // No replay guard: this records the failure of an operation that did not
    // commit a row, so any pre-existing same-`request_id` row belongs to a
    // distinct attempt and the unique index (caught as a non-busy error by the
    // caller's `.ok()`) is the correct backstop. IMMEDIATE + retry only (R3).
    with_immediate_retry(&mut connection, |tx| {
        // A failed op is a third chain-write site (it bypasses insert_operation_view
        // with its own INSERT + CAS). It must carry a content_hash too, or it leaves a
        // NULL-hash op on the spine that `doctor`/the gate would mis-flag as tampered
        // (NER-136). No domain row, so the digest is None.
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command,
                kind: "recoverable_failure",
                created_at_ms: now,
            },
            None,
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'failed', 'recoverable_failure', ?5, ?6, ?7, ?8, ?9)",
            params![
                operation_id,
                context.repo_id,
                request_id,
                command,
                context.current_operation_id,
                view_id,
                // Persist the typed error's `details` alongside code/message so a
                // later `--request-id` replay reconstructs the SAME details the first
                // response carried (FIX C). Old rows lacking `details` fall back to
                // an empty object at replay time.
                json!({ "message": message, "code": code, "details": details }).to_string(),
                content_hash,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'failed', ?4, ?5)",
            params![
                view_id,
                context.repo_id,
                operation_id,
                json!({
                    "lifecycle": "recoverable_failure",
                    "failed_command": command,
                    "message": message
                })
                .to_string(),
                now
            ],
        )?;
        let updated = tx.execute(
            "UPDATE current_state
             SET current_operation_id = ?1, current_view_id = ?2, updated_at_ms = ?3
             WHERE singleton = 1 AND current_operation_id = ?4",
            params![operation_id, view_id, now, context.current_operation_id],
        )?;
        if updated != 1 {
            // Intentionally left UNTYPED (plain anyhow, not the retryable
            // `CurrentStateChanged`): this is the failure-recording path, whose
            // result the CLI already swallows with `.ok()` (command_result's error
            // arm). A CAS loss here just means the failure was not recorded; it must
            // not become a retryable CONFLICT.
            return Err(anyhow!("current operation changed"));
        }
        Ok(())
    })?;
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

/// The stored chain hash of an operation, or the genesis sentinel when the parent
/// is absent (the `init` genesis op) or predates Phase 5 (a legacy NULL hash). It is
/// the `parent_hash` input to the next link, so a chain always anchors on one
/// canonical value (NER-136). Read on the writer's `&tx` so the folded parent and
/// the singleton CAS pointer are the same row.
pub(crate) fn op_content_hash(conn: &Connection, operation_id: Option<&str>) -> Result<String> {
    let Some(operation_id) = operation_id else {
        return Ok(integrity::GENESIS_PARENT_HASH.to_string());
    };
    let stored: Option<String> = conn
        .query_row(
            "SELECT content_hash FROM operations WHERE id = ?1",
            params![operation_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(stored.unwrap_or_else(|| integrity::GENESIS_PARENT_HASH.to_string()))
}

pub(crate) fn insert_operation_view(
    tx: &Transaction<'_>,
    repo_id: &str,
    parent_operation_id: Option<&str>,
    input: OperationViewInput,
) -> Result<OperationViewResult> {
    insert_operation_view_chained(tx, repo_id, parent_operation_id, input, None)
}

/// Append an operation/view, folding `domain_digest` (the evidence/decision row's
/// own `content_hash`, or `None` for ops with no domain row) plus the parent op's
/// hash into `operations.content_hash` — the tamper-evident chain spine (NER-136).
/// Computed inside the writer's IMMEDIATE txn; the parent read is on the same `&tx`.
pub(crate) fn insert_operation_view_chained(
    tx: &Transaction<'_>,
    repo_id: &str,
    parent_operation_id: Option<&str>,
    input: OperationViewInput,
    domain_digest: Option<&str>,
) -> Result<OperationViewResult> {
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let status = format!("{:?}", OperationStatus::Succeeded).to_lowercase();
    let view_kind = format!("{:?}", input.view_kind).to_lowercase();
    let parent_hash = op_content_hash(tx, parent_operation_id)?;
    let content_hash = integrity::operation_link_hash(
        &parent_hash,
        &integrity::OperationDigestInput {
            operation_id: &operation_id,
            command: &input.command,
            kind: &input.kind,
            created_at_ms: now,
        },
        domain_digest,
    );

    tx.execute(
        "INSERT INTO operations (
            id, repo_id, request_id, command, status, kind, parent_operation_id,
            resulting_view_id, error_json, content_hash, created_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)",
        params![
            operation_id,
            repo_id,
            input.request_id,
            input.command,
            status,
            input.kind,
            parent_operation_id,
            view_id,
            content_hash,
            now
        ],
    )?;
    tx.execute(
        "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            view_id,
            repo_id,
            operation_id,
            view_kind,
            input.state.to_string(),
            now
        ],
    )?;
    let expected_operation = parent_operation_id.context("missing parent operation")?;
    let updated = tx.execute(
        "UPDATE current_state
         SET current_operation_id = ?1, current_view_id = ?2, updated_at_ms = ?3
         WHERE singleton = 1 AND current_operation_id = ?4",
        params![operation_id, view_id, now, expected_operation],
    )?;
    if updated != 1 {
        // The optimistic singleton CAS lost the race: another writer advanced
        // `current_state` between this command's determining read and its write.
        // Surface it TYPED so the CLI classifies it `retryable` (code CONFLICT) and
        // does NOT persist it under the `--request-id` — a retry re-executes against
        // fresh state instead of replaying a poisoned failure (NER-133 FIX D / R7).
        // Caveat: re-executing re-runs the command; for `forge run` that re-executes
        // the child process (run records evidence via this fn and is the lock
        // carve-out). See the `notes.retry_side_effects` entry in `forge schema`.
        return Err(ForgeError::CurrentStateChanged.into());
    }
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

/// How long a connection waits on a held write lock before SQLite returns
/// `SQLITE_BUSY`. Generous because contention here is brief (small txns) and the
/// bounded retry in `with_immediate_retry` is only a defensive backstop.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    // `busy_timeout` and `synchronous` are per-connection and must be re-applied
    // on every open; `journal_mode=WAL` is persistent (header byte) but is cheap
    // to re-assert. WAL lets readers run without blocking the single writer, so
    // many `forge` processes can share one `.forge/forge.db` (R2).
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // `journal_mode` returns a row, so `pragma_update` errors with
    // `ExecuteReturnedResults`; `execute_batch` is the correct call.
    connection.execute_batch("PRAGMA journal_mode=WAL;")?;
    // NORMAL is the crash-safe WAL pairing: only the last commit can be lost on
    // power loss, never the database. (FULL at decision points is deferred.)
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

/// Upper bound on `IMMEDIATE`-txn attempts before a transient lock error is
/// surfaced. `busy_timeout` already absorbs ordinary contention at `BEGIN`, so
/// this only matters for `SQLITE_BUSY_SNAPSHOT` (which `busy_timeout` does not
/// retry) and the rare post-timeout `SQLITE_BUSY`.
const WRITE_TXN_MAX_ATTEMPTS: u32 = 6;

/// Run `body` inside a `BEGIN IMMEDIATE` transaction, committing on success, and
/// retry the whole transaction on transient `SQLITE_BUSY` / `SQLITE_BUSY_SNAPSHOT`
/// with bounded, jittered backoff (R3).
///
/// `IMMEDIATE` takes the write lock at `BEGIN`, so a read-then-write body cannot
/// hit the deferred-upgrade `SQLITE_BUSY_SNAPSHOT` race; the 517 catch below is a
/// defensive backstop. Non-busy errors (including [`RequestIdReplay`]) propagate
/// immediately without retry.
///
/// `body` is `FnMut` because it may run once per retry attempt, so it must not
/// move captured values out — writer closures therefore `.clone()` any owned
/// input they consume (e.g. `request_id`, `OperationViewInput`) on each call.
pub(crate) fn with_immediate_retry<T, F>(connection: &mut Connection, mut body: F) -> Result<T>
where
    F: FnMut(&Transaction<'_>) -> Result<T>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match run_immediate_once(connection, &mut body) {
            Ok(value) => return Ok(value),
            Err(error) => {
                if attempt < WRITE_TXN_MAX_ATTEMPTS && is_retryable_busy(&error) {
                    sleep_backoff(attempt);
                    continue;
                }
                return Err(error);
            }
        }
    }
}

/// Single `IMMEDIATE` attempt. Split out so the `&mut Connection` borrow is
/// released between retries (the txn is dropped — and thus rolled back — on any
/// error before commit).
fn run_immediate_once<T, F>(connection: &mut Connection, body: &mut F) -> Result<T>
where
    F: FnMut(&Transaction<'_>) -> Result<T>,
{
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let value = body(&tx)?;
    tx.commit()?;
    Ok(value)
}

/// True if any link in the error's source chain is a `SQLITE_BUSY`-class failure.
/// Matching the primary `DatabaseBusy` code covers every `SQLITE_BUSY_*` extended
/// code, including `SQLITE_BUSY_SNAPSHOT` (517); the explicit 517 check documents
/// that intent for reviewers.
pub(crate) fn is_retryable_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(rusqlite::Error::SqliteFailure(err, _)) =
            cause.downcast_ref::<rusqlite::Error>()
        {
            matches!(
                err.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) || err.extended_code == 517
        } else {
            false
        }
    })
}

/// Short, jittered backoff between busy retries. Jitter mixes the process id
/// (distinct per concurrent process) with the wall-clock nanosecond (distinct
/// per attempt) over a 0–24 ms window, so concurrent processes desynchronize
/// rather than retrying in lockstep even when their clocks read the same coarse
/// nanosecond. No `rand` dependency is pulled in.
fn sleep_backoff(attempt: u32) {
    let base_ms = (1u64 << attempt.min(5)).min(50); // 2, 4, 8, 16, 32, 50…
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter_ms = u64::from(std::process::id()).wrapping_add(u64::from(nanos)) % 25;
    std::thread::sleep(Duration::from_millis(base_ms + jitter_ms));
}

/// Signals that a writer observed an already-recorded operation for the same
/// `(repo_id, request_id)` inside its `IMMEDIATE` transaction (U5). The writer
/// rolls back without inserting domain rows; the CLI replays the carried
/// operation's original result instead of treating this as a fresh write.
#[derive(Debug, Clone)]
pub struct RequestIdReplay {
    pub operation: RequestIdOperation,
}

impl std::fmt::Display for RequestIdReplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "request id already recorded for command {}",
            self.operation.command
        )
    }
}

impl std::error::Error for RequestIdReplay {}

/// Re-check, as the first statement of an `IMMEDIATE` write transaction, whether
/// this `(repo_id, request_id)` already produced a committed operation row. If so
/// (a concurrent retry won the race), abort with [`RequestIdReplay`] carrying the
/// existing row so the caller can replay rather than collide at commit (U5,
/// option a). The same `created_at_ms DESC, rowid DESC` ordering as the CLI
/// pre-flight read keeps replay deterministic.
pub(crate) fn replay_guard(
    tx: &Transaction<'_>,
    repo_id: &str,
    request_id: Option<&str>,
) -> Result<()> {
    let Some(request_id) = request_id else {
        return Ok(());
    };
    let existing = tx
        .query_row(
            "SELECT o.id, o.command, o.status, o.error_json, o.kind, o.resulting_view_id, v.state_json
             FROM operations o
             LEFT JOIN views v ON v.id = o.resulting_view_id
             WHERE o.repo_id = ?1 AND o.request_id = ?2
             ORDER BY o.created_at_ms DESC, o.rowid DESC LIMIT 1",
            params![repo_id, request_id],
            |row| {
                let error_json: Option<String> = row.get(3)?;
                let state_json: Option<String> = row.get(6)?;
                Ok(RequestIdOperation {
                    operation_id: row.get(0)?,
                    command: row.get(1)?,
                    status: row.get(2)?,
                    error_json: error_json.and_then(|json| serde_json::from_str(&json).ok()),
                    kind: row.get(4)?,
                    view_id: row.get(5)?,
                    state: state_json.and_then(|json| serde_json::from_str(&json).ok()),
                })
            },
        )
        .optional()?;
    match existing {
        Some(operation) => Err(RequestIdReplay { operation }.into()),
        None => Ok(()),
    }
}
