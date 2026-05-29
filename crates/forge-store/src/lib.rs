use anyhow::{anyhow, bail, Context, Result};
use forge_core::{new_id, now_ms, OperationId, OperationStatus, RepositoryId, ViewId, ViewKind};
use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

mod repo_lock;
pub use repo_lock::{LockTimeout, RepoLock};

const MIGRATION_001: &str = include_str!("../migrations/001_init.sql");

#[derive(Debug, Clone, Serialize)]
pub struct InitRepository {
    pub repository_id: String,
    pub root_path: String,
    pub forge_dir: String,
    pub database_path: String,
    pub git_head: Option<String>,
    pub content_backend: String,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub already_initialized: bool,
}

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

#[derive(Debug, Clone)]
pub struct RequestIdOperation {
    pub operation_id: String,
    pub command: String,
    pub status: String,
    pub error_json: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct RepositoryContext {
    pub repo_id: String,
    pub root_path: PathBuf,
    pub database_path: PathBuf,
    pub content_backend: String,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub attached_attempt_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartAttempt {
    pub intent_id: String,
    pub attempt_id: String,
    pub base_head: String,
    pub attached: bool,
    pub operation_id: String,
    pub current_view_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt_id: String,
    pub intent_id: String,
    pub intent: String,
    pub base_head: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptSummary {
    pub attempt_id: String,
    pub intent_id: String,
    pub intent: String,
    pub base_head: String,
    pub status: String,
    pub attached: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptShowRecord {
    pub attempt: AttemptSummary,
    pub latest_snapshot: Option<SnapshotSummary>,
    pub latest_evidence: Option<EvidenceSummary>,
    pub proposals: Vec<ProposalMetadata>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotRecord {
    pub snapshot_id: String,
    pub attempt_id: String,
    pub parent_snapshot_id: Option<String>,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
    pub operation_id: String,
    pub current_view_id: String,
}

#[derive(Debug, Clone)]
pub struct EvidenceInput {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub exit_code: i32,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub stdout_excerpt: String,
    pub stderr_excerpt: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub sensitivity: String,
    pub visibility: String,
    pub trust: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceRecord {
    pub evidence_id: String,
    pub attempt_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub exit_code: i32,
    pub stdout_excerpt: String,
    pub stderr_excerpt: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub sensitivity: String,
    pub visibility: String,
    pub trust: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposalRecord {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckRecord {
    pub check_result_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub status: String,
    pub reason: String,
    pub evidence_id: Option<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionRecord {
    pub decision_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub decision: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicationRecord {
    pub publication_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub branch_name: String,
    pub commit_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShowRecord {
    pub attempt: Option<AttemptRecord>,
    pub latest_snapshot: Option<SnapshotSummary>,
    pub latest_evidence: Option<EvidenceSummary>,
    pub latest_proposal: Option<ProposalSummary>,
    pub latest_check: Option<CheckSummary>,
    pub latest_decision: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotSummary {
    pub snapshot_id: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceSummary {
    pub evidence_id: String,
    pub snapshot_id: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub exit_code: i64,
    pub sensitivity: String,
    pub trust: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposalSummary {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposalMetadata {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
    pub check_status: Option<String>,
    pub decision_status: Option<String>,
    pub publication_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckSummary {
    pub check_result_id: String,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedAttempt {
    pub attempt: AttemptRecord,
}

#[derive(Debug, Clone)]
pub struct ResolvedProposal {
    pub proposal: ProposalSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub issues: Vec<String>,
    pub schema_version: Option<i64>,
    pub dangling_temp_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GcDryRunReport {
    pub dry_run: bool,
    pub unreachable_snapshots: Vec<String>,
    pub unreachable_evidence: Vec<String>,
    pub unreachable_native_objects: Vec<String>,
    pub deleted: Vec<String>,
}

pub fn init_repository(
    cwd: &Path,
    request_id: Option<String>,
    content_backend: String,
) -> Result<InitRepository> {
    if !matches!(content_backend.as_str(), "git" | "native") {
        bail!("unsupported content backend");
    }
    let root = git_root(cwd)?;
    let forge_dir = root.join(".forge");
    fs::create_dir_all(&forge_dir)
        .with_context(|| format!("failed to create {}", forge_dir.display()))?;

    let database_path = forge_dir.join("forge.db");
    let already_had_db = database_path.exists();
    let mut connection = open_connection(&database_path)
        .with_context(|| format!("failed to open {}", database_path.display()))?;
    apply_migrations(&mut connection)?;

    if let Some(existing) = read_init_repository(&connection, &root, &forge_dir, &database_path)? {
        return Ok(InitRepository {
            already_initialized: true,
            ..existing
        });
    }

    let git_head = git_head(&root);
    let repo_id = RepositoryId::new().to_string();
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let state_json = json!({
        "repository_id": repo_id,
        "root_path": root,
        "git_head": git_head,
        "content_backend": content_backend,
        "lifecycle": "initialized"
    })
    .to_string();
    // No replay guard here: `init` has its own idempotency via the
    // `read_init_repository` short-circuit above; this only adds IMMEDIATE +
    // busy-retry for R3 consistency.
    with_immediate_retry(&mut connection, |tx| {
        tx.execute(
            "INSERT INTO repositories (id, root_path, git_head, content_backend, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo_id, root.to_string_lossy(), git_head, content_backend, now],
        )?;
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, created_at_ms
            ) VALUES (?1, ?2, ?3, 'init', ?4, 'repository_initialized', NULL, ?5, NULL, ?6)",
            params![
                operation_id,
                repo_id,
                request_id,
                format!("{:?}", OperationStatus::Succeeded).to_lowercase(),
                view_id,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'initialized', ?4, ?5)",
            params![view_id, repo_id, operation_id, state_json, now],
        )?;
        tx.execute(
            "INSERT INTO current_state (
                singleton, repo_id, current_operation_id, current_view_id, attached_attempt_id, updated_at_ms
            ) VALUES (1, ?1, ?2, ?3, NULL, ?4)",
            params![repo_id, operation_id, view_id, now],
        )?;
        Ok(())
    })?;

    Ok(InitRepository {
        repository_id: repo_id,
        root_path: root.to_string_lossy().into_owned(),
        forge_dir: forge_dir.to_string_lossy().into_owned(),
        database_path: database_path.to_string_lossy().into_owned(),
        git_head,
        content_backend,
        current_operation_id: operation_id,
        current_view_id: view_id,
        already_initialized: already_had_db,
    })
}

pub fn create_operation_view(
    database_path: &Path,
    expected_current_operation_id: &str,
    input: OperationViewInput,
) -> Result<OperationViewResult> {
    let mut connection = open_connection(database_path)
        .with_context(|| format!("failed to open {}", database_path.display()))?;
    with_immediate_retry(&mut connection, |tx| {
        // CAS read inside the IMMEDIATE txn: the parent-operation check and the
        // write share one connection so the advance is atomic.
        let (repo_id, parent_operation_id): (String, String) = tx.query_row(
            "SELECT repo_id, current_operation_id FROM current_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        if parent_operation_id != expected_current_operation_id {
            return Err(anyhow!("current operation changed"));
        }

        insert_operation_view(tx, &repo_id, Some(&parent_operation_id), input.clone())
    })
}

pub fn open_repository(cwd: &Path) -> Result<RepositoryContext> {
    let root = git_root(cwd)?;
    let database_path = root.join(".forge/forge.db");
    if !database_path.exists() {
        return Err(anyhow!("forge repository is not initialized"));
    }
    let mut connection = open_connection(&database_path)?;
    apply_migrations(&mut connection)?;
    let (repo_id, content_backend, current_operation_id, current_view_id, attached_attempt_id): (
        String,
        String,
        String,
        String,
        Option<String>,
    ) = connection.query_row(
        "SELECT cs.repo_id, r.content_backend, cs.current_operation_id, cs.current_view_id, cs.attached_attempt_id
             FROM current_state cs
             JOIN repositories r ON r.id = cs.repo_id
             WHERE cs.singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    Ok(RepositoryContext {
        repo_id,
        root_path: root,
        database_path,
        content_backend,
        current_operation_id,
        current_view_id,
        attached_attempt_id,
    })
}

pub fn repository_content_backend(cwd: &Path) -> Result<String> {
    Ok(open_repository(cwd)?.content_backend)
}

/// Acquire the repo-level advisory write lock for the repository containing `cwd`
/// (PRD §10.6, NER-132). The CLI holds the returned guard across a mutating
/// command's critical section so its determining reads and write are atomic
/// against other `forge` writers.
///
/// Returns `Ok(None)` when there is no repository to lock — `cwd` is not inside a
/// Git work tree, or `.forge` does not exist yet — so the caller's own logic
/// surfaces the canonical "not initialized" error instead of a lock-file error.
/// A genuine contention timeout surfaces as a [`LockTimeout`] (`Err`).
pub fn acquire_repo_lock(cwd: &Path) -> Result<Option<RepoLock>> {
    let root = match git_root(cwd) {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let forge_dir = root.join(".forge");
    if !forge_dir.exists() {
        return Ok(None);
    }
    repo_lock::acquire(&forge_dir).map(Some)
}

pub fn operation_for_request(cwd: &Path, request_id: &str) -> Result<Option<RequestIdOperation>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT id, command, status, error_json
             FROM operations
             WHERE repo_id = ?1 AND request_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, request_id],
            |row| {
                let error_json: Option<String> = row.get(3)?;
                Ok(RequestIdOperation {
                    operation_id: row.get(0)?,
                    command: row.get(1)?,
                    status: row.get(2)?,
                    error_json: error_json.and_then(|json| serde_json::from_str(&json).ok()),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub fn start_attempt(
    cwd: &Path,
    request_id: Option<String>,
    intent: String,
    base_head: String,
) -> Result<StartAttempt> {
    let context = open_repository(cwd)?;
    create_attempt(
        &context,
        request_id,
        None,
        Some(intent),
        base_head,
        true,
        "start",
    )
}

pub fn start_attempt_for_intent(
    cwd: &Path,
    request_id: Option<String>,
    intent_id: String,
    base_head: String,
) -> Result<StartAttempt> {
    let context = open_repository(cwd)?;
    create_attempt(
        &context,
        request_id,
        Some(intent_id),
        None,
        base_head,
        false,
        "attempt start",
    )
}

fn create_attempt(
    context: &RepositoryContext,
    request_id: Option<String>,
    intent_id: Option<String>,
    intent: Option<String>,
    base_head: String,
    attach: bool,
    command: &str,
) -> Result<StartAttempt> {
    let mut connection = open_connection(&context.database_path)?;
    let (intent_id, attempt_id, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let now = now_ms();
        let intent_id = match intent_id.clone() {
            Some(id) => {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM intents WHERE repo_id = ?1 AND id = ?2)",
                    params![context.repo_id, id],
                    |row| row.get(0),
                )?;
                if !exists {
                    bail!("UNKNOWN_INTENT: unknown intent {id}");
                }
                id
            }
            None => {
                let id = new_id("intent");
                tx.execute(
                    "INSERT INTO intents (id, repo_id, text, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        id,
                        context.repo_id,
                        intent
                            .clone()
                            .unwrap_or_else(|| "local agent attempt".to_string()),
                        now
                    ],
                )?;
                id
            }
        };
        let attempt_id = new_id("attempt");
        tx.execute(
            "INSERT INTO attempts (id, repo_id, intent_id, base_head, status, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5)",
            params![attempt_id, context.repo_id, intent_id, base_head, now],
        )?;

        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: command.to_string(),
                kind: "attempt_started".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": "attempt_active",
                    "attempt_id": attempt_id,
                    "intent_id": intent_id
                }),
            },
        )?;
        if attach {
            tx.execute(
                "UPDATE current_state SET attached_attempt_id = ?1 WHERE singleton = 1",
                params![attempt_id],
            )?;
        }
        Ok((intent_id, attempt_id, op))
    })?;

    Ok(StartAttempt {
        intent_id,
        attempt_id,
        base_head,
        attached: attach,
        operation_id: op.operation_id,
        current_view_id: op.view_id,
    })
}

pub fn save_snapshot(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    content_ref: String,
    changed_paths: Vec<String>,
) -> Result<SnapshotRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let mut connection = open_connection(&context.database_path)?;
    let (snapshot_id, parent_snapshot_id, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let parent_snapshot_id: Option<String> = tx
            .query_row(
                "SELECT id FROM snapshots WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                params![attempt.attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        let snapshot_id = new_id("snapshot");
        tx.execute(
            "INSERT INTO snapshots (
                id, repo_id, attempt_id, parent_snapshot_id, content_ref, changed_paths_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                snapshot_id,
                context.repo_id,
                attempt.attempt_id,
                parent_snapshot_id,
                content_ref,
                serde_json::to_string(&changed_paths)?,
                now_ms()
            ],
        )?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "save".to_string(),
                kind: "snapshot_saved".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": "snapshot_saved",
                    "attempt_id": attempt.attempt_id,
                    "snapshot_id": snapshot_id
                }),
            },
        )?;
        Ok((snapshot_id, parent_snapshot_id, op))
    })?;
    Ok(SnapshotRecord {
        snapshot_id,
        attempt_id: attempt.attempt_id,
        parent_snapshot_id,
        content_ref,
        changed_paths,
        operation_id: op.operation_id,
        current_view_id: op.view_id,
    })
}

pub fn snapshot_content_ref(cwd: &Path, snapshot_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT content_ref FROM snapshots WHERE id = ?1",
            params![snapshot_id],
            |row| row.get(0),
        )
        .with_context(|| format!("unknown snapshot {snapshot_id}"))
}

pub fn latest_snapshot_content_ref(cwd: &Path, attempt_id: Option<&str>) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    Ok(latest_snapshot_for_attempt(&context, &attempt.attempt_id)?
        .map(|snapshot| snapshot.content_ref))
}

pub fn record_restore(
    cwd: &Path,
    request_id: Option<String>,
    snapshot_id: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "restore".to_string(),
                kind: "snapshot_restored".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": "snapshot_restored", "snapshot_id": snapshot_id }),
            },
        )
    })?;
    Ok(op)
}

pub fn record_evidence(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    input: EvidenceInput,
) -> Result<EvidenceRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let mut connection = open_connection(&context.database_path)?;
    let (evidence_id, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        // Determining read inside the IMMEDIATE txn: the snapshot the evidence is
        // attributed to is read on the same connection that writes it (U4).
        let snapshot = latest_snapshot_on(tx, &attempt.attempt_id)?;
        let evidence_id = new_id("evidence");
        tx.execute(
            "INSERT INTO evidence (
                id, repo_id, attempt_id, snapshot_id, command, args_json, cwd, exit_code, started_at_ms, ended_at_ms,
                stdout_excerpt, stderr_excerpt, stdout_truncated, stderr_truncated, timed_out,
                sensitivity, visibility, trust, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                evidence_id,
                context.repo_id,
                attempt.attempt_id,
                snapshot.as_ref().map(|snapshot| snapshot.snapshot_id.clone()),
                input.command,
                serde_json::to_string(&input.args)?,
                input.cwd,
                input.exit_code,
                input.started_at_ms,
                input.ended_at_ms,
                input.stdout_excerpt,
                input.stderr_excerpt,
                input.stdout_truncated as i64,
                input.stderr_truncated as i64,
                input.timed_out as i64,
                input.sensitivity,
                input.visibility,
                input.trust,
                now_ms()
            ],
        )?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "run".to_string(),
                kind: "evidence_captured".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": "evidence_captured", "evidence_id": evidence_id }),
            },
        )?;
        Ok((evidence_id, op))
    })?;
    // The record echoes the inputs that were just written; reading the row back
    // on a fresh connection could observe a concurrently-written newer row, so we
    // return the canonical input values directly.
    Ok(EvidenceRecord {
        evidence_id,
        attempt_id: attempt.attempt_id,
        command: input.command,
        args: input.args,
        exit_code: input.exit_code,
        stdout_excerpt: input.stdout_excerpt,
        stderr_excerpt: input.stderr_excerpt,
        stdout_truncated: input.stdout_truncated,
        stderr_truncated: input.stderr_truncated,
        timed_out: input.timed_out,
        sensitivity: input.sensitivity,
        visibility: input.visibility,
        trust: input.trust,
        operation_id: op.operation_id,
    })
}

pub fn propose(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
) -> Result<ProposalRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let mut connection = open_connection(&context.database_path)?;
    let (proposal_id, revision_id, snapshot, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        // Determining read inside the IMMEDIATE txn: the proposal binds to the
        // latest snapshot observed on the same connection that writes it (U4).
        let snapshot = latest_snapshot_on(tx, &attempt.attempt_id)?
            .context("no snapshot saved for active attempt")?;
        let proposal_id = new_id("proposal");
        let revision_id = new_id("revision");
        tx.execute(
            "INSERT INTO proposals (id, repo_id, attempt_id, snapshot_id, base_head, content_ref, status, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'draft', ?7)",
            params![
                proposal_id,
                context.repo_id,
                attempt.attempt_id,
                snapshot.snapshot_id,
                attempt.base_head,
                snapshot.content_ref,
                now_ms()
            ],
        )?;
        tx.execute(
            "INSERT INTO proposal_revisions (id, proposal_id, snapshot_id, content_ref, changed_paths_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision_id,
                proposal_id,
                snapshot.snapshot_id,
                snapshot.content_ref,
                serde_json::to_string(&snapshot.changed_paths)?,
                now_ms()
            ],
        )?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "propose".to_string(),
                kind: "proposal_created".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": "proposal_draft", "proposal_id": proposal_id }),
            },
        )?;
        Ok((proposal_id, revision_id, snapshot, op))
    })?;
    Ok(ProposalRecord {
        proposal_id,
        proposal_revision_id: revision_id,
        attempt_id: attempt.attempt_id,
        snapshot_id: snapshot.snapshot_id,
        base_head: attempt.base_head,
        content_ref: snapshot.content_ref,
        changed_paths: snapshot.changed_paths,
        operation_id: op.operation_id,
    })
}

pub fn record_check(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
    status: String,
    reason: String,
) -> Result<CheckRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let proposal = resolve_proposal(&context, &attempt.attempt_id, proposal_id, true)?.proposal;
    let mut connection = open_connection(&context.database_path)?;
    let (check_result_id, status, reason, evidence_id, op) = with_immediate_retry(
        &mut connection,
        |tx| {
            replay_guard(tx, &context.repo_id, request_id.as_deref())?;
            // Determining read inside the IMMEDIATE txn: the freshness verdict is
            // derived from the latest evidence observed on the same connection
            // that writes the check result (U4).
            let evidence = latest_evidence_on(tx, &attempt.attempt_id)?;
            let evidence_id = evidence.as_ref().map(|e| e.evidence_id.clone());
            let (status, reason) = match evidence.as_ref().and_then(|e| e.snapshot_id.as_deref()) {
                Some(snapshot_id) if snapshot_id == proposal.snapshot_id => {
                    (status.clone(), reason.clone())
                }
                Some(_) => (
                    "stale".to_string(),
                    "latest evidence does not match proposal revision snapshot".to_string(),
                ),
                None => (
                    "missing".to_string(),
                    "no evidence recorded for proposal revision snapshot".to_string(),
                ),
            };
            let check_result_id = new_id("check");
            tx.execute(
                "INSERT INTO check_results (
                    id, repo_id, proposal_id, proposal_revision_id, status, reason, evidence_id, created_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    check_result_id,
                    context.repo_id,
                    proposal.proposal_id,
                    proposal.proposal_revision_id,
                    status,
                    reason,
                    evidence_id,
                    now_ms()
                ],
            )?;
            let op = insert_operation_view(
                tx,
                &context.repo_id,
                Some(&context.current_operation_id),
                OperationViewInput {
                    request_id: request_id.clone(),
                    command: "check".to_string(),
                    kind: "proposal_checked".to_string(),
                    view_kind: ViewKind::Initialized,
                    state: json!({ "lifecycle": "checked", "check_result_id": check_result_id }),
                },
            )?;
            Ok((check_result_id, status, reason, evidence_id, op))
        },
    )?;
    Ok(CheckRecord {
        check_result_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        status,
        reason,
        evidence_id,
        operation_id: op.operation_id,
    })
}

pub fn decide(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
    decision: &str,
) -> Result<DecisionRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let proposal = resolve_proposal(&context, &attempt.attempt_id, proposal_id, true)?.proposal;
    let mut connection = open_connection(&context.database_path)?;
    let (decision_id, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let decision_id = new_id("decision");
        tx.execute(
            "INSERT INTO decisions (id, repo_id, proposal_id, proposal_revision_id, decision, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                decision_id,
                context.repo_id,
                proposal.proposal_id,
                proposal.proposal_revision_id,
                decision,
                now_ms()
            ],
        )?;
        tx.execute(
            "UPDATE proposals SET status = ?1 WHERE id = ?2",
            params![decision, proposal.proposal_id],
        )?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: decision.to_string(),
                kind: format!("proposal_{decision}"),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": decision, "decision_id": decision_id }),
            },
        )?;
        Ok((decision_id, op))
    })?;
    Ok(DecisionRecord {
        decision_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        decision: decision.to_string(),
        operation_id: op.operation_id,
    })
}

pub fn exportable_proposal(
    cwd: &Path,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
) -> Result<ProposalSummary> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    Ok(resolve_proposal(&context, &attempt.attempt_id, proposal_id, true)?.proposal)
}

pub fn latest_decision(cwd: &Path) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    latest_decision_for_context(&context)
}

pub fn decision_for_proposal_revision(
    cwd: &Path,
    proposal_revision_id: &str,
) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT decision FROM decisions
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, proposal_revision_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

pub fn publication_exists_for_branch(cwd: &Path, branch_name: &str) -> Result<bool> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM publications WHERE repo_id = ?1 AND branch_name = ?2",
        params![context.repo_id, branch_name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn record_publication(
    cwd: &Path,
    request_id: Option<String>,
    proposal_id: &str,
    branch_name: String,
    commit_id: String,
) -> Result<PublicationRecord> {
    let context = open_repository(cwd)?;
    let proposal = proposal_by_id(&context, proposal_id)?.context("no proposal exists")?;
    let mut connection = open_connection(&context.database_path)?;
    // Note: the git branch side-effect happens in the CLI before this call, so
    // the replay guard provides DB-row idempotency only; making the export
    // side-effect itself replay-safe is Phase 1b (PENDING-before-side-effect).
    let (publication_id, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let publication_id = new_id("publication");
        tx.execute(
            "INSERT INTO publications (
                id, repo_id, proposal_id, proposal_revision_id, branch_name, commit_id, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                publication_id,
                context.repo_id,
                proposal.proposal_id,
                proposal.proposal_revision_id,
                branch_name,
                commit_id,
                now_ms()
            ],
        )?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "export branch".to_string(),
                kind: "branch_exported".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": "published_branch", "publication_id": publication_id }),
            },
        )?;
        Ok((publication_id, op))
    })?;
    Ok(PublicationRecord {
        publication_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        branch_name,
        commit_id,
        operation_id: op.operation_id,
    })
}

pub fn show(cwd: &Path, attempt_id: Option<&str>) -> Result<ShowRecord> {
    let context = open_repository(cwd)?;
    let attempt = match resolve_attempt_in_context(&context, attempt_id) {
        Ok(resolved) => Some(resolved.attempt),
        Err(error) if error.to_string().contains("no active attempt") => None,
        Err(error) => return Err(error),
    };
    Ok(ShowRecord {
        latest_snapshot: attempt
            .as_ref()
            .map(|attempt| latest_snapshot_for_attempt(&context, &attempt.attempt_id))
            .transpose()?
            .flatten(),
        latest_evidence: attempt
            .as_ref()
            .map(|attempt| latest_evidence_for_attempt(&context, &attempt.attempt_id))
            .transpose()?
            .flatten(),
        latest_proposal: attempt
            .as_ref()
            .map(|attempt| latest_proposal_for_attempt(&context, &attempt.attempt_id))
            .transpose()?
            .flatten(),
        latest_check: attempt
            .as_ref()
            .map(|attempt| latest_check_for_attempt(&context, &attempt.attempt_id))
            .transpose()?
            .flatten(),
        latest_decision: attempt
            .as_ref()
            .map(|attempt| latest_decision_for_attempt(&context, &attempt.attempt_id))
            .transpose()?
            .flatten(),
        attempt,
    })
}

pub fn doctor(cwd: &Path) -> Result<DoctorReport> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let mut issues = Vec::new();
    let mut foreign_key_statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut foreign_key_rows = foreign_key_statement.query([])?;
    while let Some(row) = foreign_key_rows.next()? {
        let table: String = row.get(0)?;
        let rowid: i64 = row.get(1)?;
        issues.push(format!("foreign key violation in {table} row {rowid}"));
    }
    let schema_version = connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    if schema_version != Some(2) {
        issues.push("schema mismatch".to_string());
    }
    let state_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM current_state cs
         JOIN operations o ON o.id = cs.current_operation_id
         JOIN views v ON v.id = cs.current_view_id
         WHERE cs.singleton = 1
           AND o.repo_id = cs.repo_id
           AND v.repo_id = cs.repo_id
           AND v.operation_id = o.id
           AND o.resulting_view_id = v.id",
        [],
        |row| row.get(0),
    )?;
    if state_count != 1 {
        issues.push("invalid current operation/view".to_string());
    }
    let mut dangling_temp_files = Vec::new();
    let temp_dir = context.root_path.join(".forge/tmp");
    if temp_dir.exists() {
        for entry in fs::read_dir(&temp_dir)? {
            let entry = entry?;
            dangling_temp_files.push(entry.path().to_string_lossy().into_owned());
        }
    }
    if !dangling_temp_files.is_empty() {
        issues.push("dangling temporary files".to_string());
    }
    let native_store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let mut reachable_native_objects = std::collections::BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT 'snapshot ' || id, content_ref FROM snapshots
         UNION ALL
         SELECT 'proposal revision ' || id, content_ref FROM proposal_revisions",
    )?;
    let refs = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for content_ref in refs {
        let (label, content_ref) = content_ref?;
        if let Some(tree) = content_ref.strip_prefix("git-tree:") {
            let output = Command::new("git")
                .args(["cat-file", "-e", &format!("{tree}^{{tree}}")])
                .current_dir(&context.root_path)
                .output()?;
            if !output.status.success() {
                issues.push(format!("missing content ref for {label}"));
            }
        } else if content_ref.starts_with("forge-tree:") {
            match native_store.verify_content_ref(&content_ref) {
                Ok(ids) => reachable_native_objects.extend(ids),
                Err(error) => {
                    issues.push(format!("invalid native content ref for {label}: {error}"))
                }
            }
        }
    }
    Ok(DoctorReport {
        ok: issues.is_empty(),
        issues,
        schema_version,
        dangling_temp_files,
    })
}

pub fn pr_body(cwd: &Path) -> Result<String> {
    pr_body_for(cwd, None, None)
}

pub fn pr_body_for(
    cwd: &Path,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
) -> Result<String> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let proposal = resolve_proposal(&context, &attempt.attempt_id, proposal_id, true)?.proposal;
    let evidence = latest_evidence_for_attempt(&context, &attempt.attempt_id)?;
    let check = latest_check_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let decision = latest_decision_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let mut body = String::new();
    body.push_str("# Forge Proposal\n\n");
    body.push_str(&format!("Intent: {}\n\n", attempt.intent));
    body.push_str("## Changed Paths\n");
    for path in proposal.changed_paths {
        body.push_str(&format!("- {path}\n"));
    }
    body.push('\n');
    if let Some(evidence) = evidence {
        body.push_str("## Evidence\n");
        body.push_str(&format!(
            "- `{}` exited with `{}` ({})\n\n",
            evidence.command, evidence.exit_code, evidence.trust
        ));
    }
    if let Some(check) = check {
        body.push_str("## Check\n");
        body.push_str(&format!("- {}: {}\n\n", check.status, check.reason));
    }
    if let Some(decision) = decision {
        body.push_str("## Decision\n");
        body.push_str(&format!("- {decision}\n"));
    }
    if let Some(publication) =
        latest_publication_for_proposal_revision(&context, &proposal.proposal_revision_id)?
    {
        body.push_str("\n## Publication\n");
        body.push_str(&format!(
            "- `{}` at `{}`\n",
            publication.branch_name, publication.commit_id
        ));
    }
    Ok(body)
}

pub fn gc_dry_run(cwd: &Path) -> Result<GcDryRunReport> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let native_store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let mut reachable = std::collections::BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT content_ref FROM snapshots
         UNION
         SELECT content_ref FROM proposal_revisions",
    )?;
    let refs = statement.query_map([], |row| row.get::<_, String>(0))?;
    for content_ref in refs {
        let content_ref = content_ref?;
        if content_ref.starts_with("forge-tree:") {
            if let Ok(ids) = native_store.verify_content_ref(&content_ref) {
                reachable.extend(ids);
            }
        }
    }
    let all = native_store.all_object_ids()?;
    let unreachable_native_objects = all
        .difference(&reachable)
        .map(ToString::to_string)
        .collect();
    Ok(GcDryRunReport {
        dry_run: true,
        unreachable_snapshots: Vec::new(),
        unreachable_evidence: Vec::new(),
        unreachable_native_objects,
        deleted: Vec::new(),
    })
}

pub fn record_failed_operation(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    message: &str,
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
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'failed', 'recoverable_failure', ?5, ?6, ?7, ?8)",
            params![
                operation_id,
                context.repo_id,
                request_id,
                command,
                context.current_operation_id,
                view_id,
                json!({ "message": message }).to_string(),
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
            return Err(anyhow!("current operation changed"));
        }
        Ok(())
    })?;
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

pub fn resolve_attempt(cwd: &Path, attempt_id: Option<&str>) -> Result<ResolvedAttempt> {
    let context = open_repository(cwd)?;
    resolve_attempt_in_context(&context, attempt_id)
}

fn resolve_attempt_in_context(
    context: &RepositoryContext,
    attempt_id: Option<&str>,
) -> Result<ResolvedAttempt> {
    if let Some(attempt_id) = attempt_id {
        let attempt = attempt_by_id(context, attempt_id)?
            .ok_or_else(|| anyhow!("UNKNOWN_ATTEMPT: unknown attempt {attempt_id}"))?;
        return Ok(ResolvedAttempt { attempt });
    }

    if let Some(attached_attempt_id) = context.attached_attempt_id.as_deref() {
        if let Some(attempt) = attempt_by_id(context, attached_attempt_id)? {
            if attempt.status == "active" {
                return Ok(ResolvedAttempt { attempt });
            }
        }
    }

    let attempts = active_attempts(context)?;
    match attempts.as_slice() {
        [] => bail!("no active attempt"),
        [attempt] => Ok(ResolvedAttempt {
            attempt: attempt.clone(),
        }),
        _ => bail!(
            "AMBIGUOUS_ATTEMPT: {}",
            attempts
                .iter()
                .map(|attempt| attempt.attempt_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

fn attempt_by_id(context: &RepositoryContext, attempt_id: &str) -> Result<Option<AttemptRecord>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT a.id, a.intent_id, i.text, a.base_head, a.status
             FROM attempts a
             JOIN intents i ON i.id = a.intent_id
             WHERE a.repo_id = ?1 AND a.id = ?2",
            params![context.repo_id, attempt_id],
            |row| {
                Ok(AttemptRecord {
                    attempt_id: row.get(0)?,
                    intent_id: row.get(1)?,
                    intent: row.get(2)?,
                    base_head: row.get(3)?,
                    status: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn active_attempts(context: &RepositoryContext) -> Result<Vec<AttemptRecord>> {
    let connection = open_connection(&context.database_path)?;
    let mut statement = connection.prepare(
        "SELECT a.id, a.intent_id, i.text, a.base_head, a.status
         FROM attempts a
         JOIN intents i ON i.id = a.intent_id
         WHERE a.repo_id = ?1 AND a.status = 'active'
         ORDER BY a.created_at_ms ASC",
    )?;
    let rows = statement.query_map(params![context.repo_id], |row| {
        Ok(AttemptRecord {
            attempt_id: row.get(0)?,
            intent_id: row.get(1)?,
            intent: row.get(2)?,
            base_head: row.get(3)?,
            status: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn list_attempts(cwd: &Path) -> Result<Vec<AttemptSummary>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let mut statement = connection.prepare(
        "SELECT a.id, a.intent_id, i.text, a.base_head, a.status
         FROM attempts a
         JOIN intents i ON i.id = a.intent_id
         WHERE a.repo_id = ?1
         ORDER BY a.created_at_ms ASC",
    )?;
    let attached = context.attached_attempt_id.clone();
    let rows = statement.query_map(params![context.repo_id], |row| {
        let attempt_id: String = row.get(0)?;
        Ok(AttemptSummary {
            attached: attached.as_deref() == Some(attempt_id.as_str()),
            attempt_id,
            intent_id: row.get(1)?,
            intent: row.get(2)?,
            base_head: row.get(3)?,
            status: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn show_attempt(cwd: &Path, attempt_id: &str) -> Result<AttemptShowRecord> {
    let context = open_repository(cwd)?;
    let attempt = attempt_by_id(&context, attempt_id)?
        .ok_or_else(|| anyhow!("UNKNOWN_ATTEMPT: unknown attempt {attempt_id}"))?;
    Ok(AttemptShowRecord {
        attempt: AttemptSummary {
            attached: context.attached_attempt_id.as_deref() == Some(attempt.attempt_id.as_str()),
            attempt_id: attempt.attempt_id.clone(),
            intent_id: attempt.intent_id.clone(),
            intent: attempt.intent.clone(),
            base_head: attempt.base_head.clone(),
            status: attempt.status.clone(),
        },
        latest_snapshot: latest_snapshot_for_attempt(&context, &attempt.attempt_id)?,
        latest_evidence: latest_evidence_for_attempt(&context, &attempt.attempt_id)?,
        proposals: proposal_metadata_for_attempt(&context, &attempt.attempt_id)?,
    })
}

pub fn attach_attempt(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let attempt = attempt_by_id(&context, attempt_id)?
        .ok_or_else(|| anyhow!("UNKNOWN_ATTEMPT: unknown attempt {attempt_id}"))?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "attempt attach".to_string(),
                kind: "attempt_attached".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": "attempt_attached",
                    "attempt_id": attempt.attempt_id
                }),
            },
        )?;
        tx.execute(
            "UPDATE current_state SET attached_attempt_id = ?1 WHERE singleton = 1",
            params![attempt.attempt_id],
        )?;
        Ok(op)
    })?;
    Ok(op)
}

pub fn attempt_materialization_ref(cwd: &Path, attempt_id: &str) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let attempt = attempt_by_id(&context, attempt_id)?
        .ok_or_else(|| anyhow!("UNKNOWN_ATTEMPT: unknown attempt {attempt_id}"))?;
    Ok(latest_snapshot_for_attempt(&context, &attempt.attempt_id)?
        .map(|snapshot| snapshot.content_ref))
}

pub fn attempt_base_head(cwd: &Path, attempt_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    Ok(attempt_by_id(&context, attempt_id)?
        .ok_or_else(|| anyhow!("UNKNOWN_ATTEMPT: unknown attempt {attempt_id}"))?
        .base_head)
}

fn latest_snapshot_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<SnapshotSummary>> {
    let connection = open_connection(&context.database_path)?;
    latest_snapshot_on(&connection, attempt_id)
}

/// Determining "latest snapshot" read against a caller-supplied connection. A
/// writer passes its own `IMMEDIATE` transaction (`&tx` deref-coerces to
/// `&Connection`) so the read-then-write is atomic on one connection (U4).
fn latest_snapshot_on(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<SnapshotSummary>> {
    connection
        .query_row(
            "SELECT id, content_ref, changed_paths_json FROM snapshots
             WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![attempt_id],
            |row| {
                let changed_paths_json: String = row.get(2)?;
                Ok(SnapshotSummary {
                    snapshot_id: row.get(0)?,
                    content_ref: row.get(1)?,
                    changed_paths: serde_json::from_str(&changed_paths_json).unwrap_or_default(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn latest_evidence_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<EvidenceSummary>> {
    let connection = open_connection(&context.database_path)?;
    latest_evidence_on(&connection, attempt_id)
}

/// Determining "latest evidence" read against a caller-supplied connection (see
/// [`latest_snapshot_on`]); used inside `record_check`'s `IMMEDIATE` txn (U4).
fn latest_evidence_on(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<EvidenceSummary>> {
    connection
        .query_row(
            "SELECT id, snapshot_id, command, args_json, exit_code, sensitivity, trust FROM evidence
             WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![attempt_id],
            |row| {
                let args_json: String = row.get(3)?;
                Ok(EvidenceSummary {
                    evidence_id: row.get(0)?,
                    snapshot_id: row.get(1)?,
                    command: row.get(2)?,
                    args: serde_json::from_str(&args_json).unwrap_or_default(),
                    exit_code: row.get(4)?,
                    sensitivity: row.get(5)?,
                    trust: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn resolve_proposal(
    context: &RepositoryContext,
    attempt_id: &str,
    proposal_id: Option<&str>,
    allow_single_default: bool,
) -> Result<ResolvedProposal> {
    if let Some(proposal_id) = proposal_id {
        let proposal = proposal_by_id(context, proposal_id)?
            .ok_or_else(|| anyhow!("UNKNOWN_PROPOSAL: unknown proposal {proposal_id}"))?;
        if proposal.attempt_id != attempt_id {
            bail!(
                "UNKNOWN_PROPOSAL: proposal {proposal_id} does not belong to attempt {attempt_id}"
            );
        }
        return Ok(ResolvedProposal { proposal });
    }

    let proposals = proposals_for_attempt(context, attempt_id)?;
    match proposals.as_slice() {
        [] => bail!("no proposal exists"),
        [proposal] if allow_single_default => Ok(ResolvedProposal {
            proposal: proposal.clone(),
        }),
        _ => bail!(
            "AMBIGUOUS_PROPOSAL: {}",
            proposals
                .iter()
                .map(|proposal| proposal.proposal_id.as_str())
                .collect::<Vec<_>>()
                .join(",")
        ),
    }
}

fn proposal_by_id(
    context: &RepositoryContext,
    proposal_id: &str,
) -> Result<Option<ProposalSummary>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT p.id, pr.id, p.attempt_id, p.snapshot_id, p.base_head, pr.content_ref, pr.changed_paths_json
             FROM proposals p
             JOIN proposal_revisions pr ON pr.proposal_id = p.id
             WHERE p.repo_id = ?1 AND p.id = ?2
             ORDER BY pr.created_at_ms DESC, pr.rowid DESC LIMIT 1",
            params![context.repo_id, proposal_id],
            |row| {
                let changed_paths_json: String = row.get(6)?;
                Ok(ProposalSummary {
                    proposal_id: row.get(0)?,
                    proposal_revision_id: row.get(1)?,
                    attempt_id: row.get(2)?,
                    snapshot_id: row.get(3)?,
                    base_head: row.get(4)?,
                    content_ref: row.get(5)?,
                    changed_paths: serde_json::from_str(&changed_paths_json).unwrap_or_default(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn proposals_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Vec<ProposalSummary>> {
    let connection = open_connection(&context.database_path)?;
    let mut statement = connection.prepare(
        "SELECT p.id, pr.id, p.attempt_id, p.snapshot_id, p.base_head, pr.content_ref, pr.changed_paths_json
         FROM proposals p
         JOIN proposal_revisions pr ON pr.proposal_id = p.id
         WHERE p.repo_id = ?1 AND p.attempt_id = ?2
         ORDER BY pr.created_at_ms ASC",
    )?;
    let rows = statement.query_map(params![context.repo_id, attempt_id], |row| {
        let changed_paths_json: String = row.get(6)?;
        Ok(ProposalSummary {
            proposal_id: row.get(0)?,
            proposal_revision_id: row.get(1)?,
            attempt_id: row.get(2)?,
            snapshot_id: row.get(3)?,
            base_head: row.get(4)?,
            content_ref: row.get(5)?,
            changed_paths: serde_json::from_str(&changed_paths_json).unwrap_or_default(),
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn latest_proposal_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<ProposalSummary>> {
    Ok(proposals_for_attempt(context, attempt_id)?.pop())
}

pub fn list_proposals(cwd: &Path, attempt_id: Option<&str>) -> Result<Vec<ProposalMetadata>> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    proposal_metadata_for_attempt(&context, &attempt.attempt_id)
}

fn proposal_metadata_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Vec<ProposalMetadata>> {
    proposals_for_attempt(context, attempt_id)?
        .into_iter()
        .map(|proposal| {
            let check_status =
                latest_check_for_proposal_revision(context, &proposal.proposal_revision_id)?
                    .map(|check| check.status);
            let decision_status =
                latest_decision_for_proposal_revision(context, &proposal.proposal_revision_id)?;
            let publication_status =
                latest_publication_for_proposal_revision(context, &proposal.proposal_revision_id)?
                    .map(|_| "published".to_string());
            Ok(ProposalMetadata {
                proposal_id: proposal.proposal_id,
                proposal_revision_id: proposal.proposal_revision_id,
                attempt_id: proposal.attempt_id,
                snapshot_id: proposal.snapshot_id,
                base_head: proposal.base_head,
                content_ref: proposal.content_ref,
                changed_paths: proposal.changed_paths,
                check_status,
                decision_status,
                publication_status,
            })
        })
        .collect()
}

fn latest_check_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<CheckSummary>> {
    let proposal = match latest_proposal_for_attempt(context, attempt_id)? {
        Some(proposal) => proposal,
        None => return Ok(None),
    };
    latest_check_for_proposal_revision(context, &proposal.proposal_revision_id)
}

fn latest_check_for_proposal_revision(
    context: &RepositoryContext,
    proposal_revision_id: &str,
) -> Result<Option<CheckSummary>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT id, status, reason FROM check_results
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, proposal_revision_id],
            |row| {
                Ok(CheckSummary {
                    check_result_id: row.get(0)?,
                    status: row.get(1)?,
                    reason: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn latest_decision_for_context(context: &RepositoryContext) -> Result<Option<String>> {
    let attempt = match resolve_attempt_in_context(context, None) {
        Ok(attempt) => attempt.attempt,
        Err(_) => return Ok(None),
    };
    latest_decision_for_attempt(context, &attempt.attempt_id)
}

fn latest_decision_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<String>> {
    let proposal = match latest_proposal_for_attempt(context, attempt_id)? {
        Some(proposal) => proposal,
        None => return Ok(None),
    };
    latest_decision_for_proposal_revision(context, &proposal.proposal_revision_id)
}

fn latest_decision_for_proposal_revision(
    context: &RepositoryContext,
    proposal_revision_id: &str,
) -> Result<Option<String>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT decision FROM decisions
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, proposal_revision_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn latest_publication_for_proposal_revision(
    context: &RepositoryContext,
    proposal_revision_id: &str,
) -> Result<Option<PublicationRecord>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT id, proposal_id, proposal_revision_id, branch_name, commit_id
             FROM publications
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, proposal_revision_id],
            |row| {
                Ok(PublicationRecord {
                    publication_id: row.get(0)?,
                    proposal_id: row.get(1)?,
                    proposal_revision_id: row.get(2)?,
                    branch_name: row.get(3)?,
                    commit_id: row.get(4)?,
                    operation_id: String::new(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn insert_operation_view(
    tx: &Transaction<'_>,
    repo_id: &str,
    parent_operation_id: Option<&str>,
    input: OperationViewInput,
) -> Result<OperationViewResult> {
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let status = format!("{:?}", OperationStatus::Succeeded).to_lowercase();
    let view_kind = format!("{:?}", input.view_kind).to_lowercase();

    tx.execute(
        "INSERT INTO operations (
            id, repo_id, request_id, command, status, kind, parent_operation_id,
            resulting_view_id, error_json, created_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9)",
        params![
            operation_id,
            repo_id,
            input.request_id,
            input.command,
            status,
            input.kind,
            parent_operation_id,
            view_id,
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
        return Err(anyhow!("current operation changed"));
    }
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

fn apply_migrations(connection: &mut Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );",
    )?;

    let applied = connection
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;

    if applied.is_none() {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(MIGRATION_001)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (1, '001_init', ?1)",
            params![now_ms()],
        )?;
        tx.commit()?;
    }
    ensure_repository_content_backend_column(connection)?;
    ensure_attached_attempt_column(connection)?;

    let applied_002 = connection
        .query_row(
            "SELECT version FROM schema_migrations WHERE version = 2",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if applied_002.is_none() {
        connection.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (2, '002_attached_attempt', ?1)",
            params![now_ms()],
        )?;
    }

    Ok(())
}

fn ensure_repository_content_backend_column(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(repositories)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "content_backend" {
            return Ok(());
        }
    }
    connection.execute(
        "ALTER TABLE repositories ADD COLUMN content_backend TEXT NOT NULL DEFAULT 'git'",
        [],
    )?;
    Ok(())
}

fn ensure_attached_attempt_column(connection: &Connection) -> Result<()> {
    let mut statement = connection.prepare("PRAGMA table_info(current_state)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == "attached_attempt_id" {
            return Ok(());
        }
    }
    connection.execute(
        "ALTER TABLE current_state ADD COLUMN attached_attempt_id TEXT REFERENCES attempts(id)",
        [],
    )?;
    Ok(())
}

/// How long a connection waits on a held write lock before SQLite returns
/// `SQLITE_BUSY`. Generous because contention here is brief (small txns) and the
/// bounded retry in `with_immediate_retry` is only a defensive backstop.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

fn open_connection(path: &Path) -> Result<Connection> {
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
fn with_immediate_retry<T, F>(connection: &mut Connection, mut body: F) -> Result<T>
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
fn is_retryable_busy(error: &anyhow::Error) -> bool {
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
fn replay_guard(tx: &Transaction<'_>, repo_id: &str, request_id: Option<&str>) -> Result<()> {
    let Some(request_id) = request_id else {
        return Ok(());
    };
    let existing = tx
        .query_row(
            "SELECT id, command, status, error_json
             FROM operations
             WHERE repo_id = ?1 AND request_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![repo_id, request_id],
            |row| {
                let error_json: Option<String> = row.get(3)?;
                Ok(RequestIdOperation {
                    operation_id: row.get(0)?,
                    command: row.get(1)?,
                    status: row.get(2)?,
                    error_json: error_json.and_then(|json| serde_json::from_str(&json).ok()),
                })
            },
        )
        .optional()?;
    match existing {
        Some(operation) => Err(RequestIdReplay { operation }.into()),
        None => Ok(()),
    }
}

fn read_init_repository(
    connection: &Connection,
    root: &Path,
    forge_dir: &Path,
    database_path: &Path,
) -> Result<Option<InitRepository>> {
    let row = connection
        .query_row(
            "SELECT r.id, r.git_head, r.content_backend, cs.current_operation_id, cs.current_view_id
             FROM repositories r
             JOIN current_state cs ON cs.repo_id = r.id
             WHERE r.root_path = ?1",
            params![root.to_string_lossy()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;

    Ok(row.map(
        |(repository_id, git_head, content_backend, current_operation_id, current_view_id)| {
            InitRepository {
                repository_id,
                root_path: root.to_string_lossy().into_owned(),
                forge_dir: forge_dir.to_string_lossy().into_owned(),
                database_path: database_path.to_string_lossy().into_owned(),
                git_head,
                content_backend,
                current_operation_id,
                current_view_id,
                already_initialized: true,
            }
        },
    ))
}

fn git_root(cwd: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output()
        .context("failed to run git")?;

    if !output.status.success() {
        return Err(anyhow!(
            "forge init must run inside an existing Git repository"
        ));
    }

    let root = String::from_utf8(output.stdout)?.trim().to_string();
    if root.is_empty() {
        return Err(anyhow!("git returned an empty repository root"));
    }

    Ok(PathBuf::from(root))
}

fn git_head(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_selector_breaks_same_ms_ties_by_rowid() {
        // The nine "latest" selectors append `, rowid DESC` so rows sharing a
        // created_at_ms are returned in deterministic insertion order (highest rowid =
        // most recently inserted). Proven directly against SQLite, independent of the
        // multi-process coverage deferred to Phase 1b.
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE snapshots (id TEXT PRIMARY KEY, attempt_id TEXT, created_at_ms INTEGER);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO snapshots (id, attempt_id, created_at_ms) VALUES ('first', 'a', 100)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO snapshots (id, attempt_id, created_at_ms) VALUES ('second', 'a', 100)",
                [],
            )
            .unwrap();
        let latest: String = connection
            .query_row(
                "SELECT id FROM snapshots WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                params!["a"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(latest, "second");
    }

    #[test]
    fn busy_classification_retries_only_busy_class_errors() {
        use rusqlite::ffi::Error as FfiError;

        // SQLITE_BUSY (5) and SQLITE_BUSY_SNAPSHOT (517) are the retryable cases.
        let busy = anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(5), None));
        assert!(is_retryable_busy(&busy), "plain SQLITE_BUSY must retry");

        let busy_snapshot =
            anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(517), None));
        assert!(
            is_retryable_busy(&busy_snapshot),
            "SQLITE_BUSY_SNAPSHOT (517) must retry"
        );

        // Detected even when wrapped further up the anyhow source chain.
        let wrapped = anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(517), None))
            .context("while committing writer txn");
        assert!(
            is_retryable_busy(&wrapped),
            "chain walk must find a busy cause"
        );

        // A constraint violation (e.g. the request-id unique index) is NOT retryable.
        let constraint =
            anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(19), None));
        assert!(
            !is_retryable_busy(&constraint),
            "SQLITE_CONSTRAINT must not retry"
        );

        // Non-SQLite errors, including the replay sentinel, are not retryable.
        assert!(!is_retryable_busy(&anyhow!("plain failure")));
        assert!(!is_retryable_busy(
            &RequestIdReplay {
                operation: RequestIdOperation {
                    operation_id: "op_x".to_string(),
                    command: "save".to_string(),
                    status: "succeeded".to_string(),
                    error_json: None,
                },
            }
            .into()
        ));
    }
}
