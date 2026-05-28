use anyhow::{anyhow, Context, Result};
use forge_core::{now_ms, OperationId, OperationStatus, RepositoryId, ViewId, ViewKind};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const MIGRATION_001: &str = include_str!("../migrations/001_init.sql");

#[derive(Debug, Clone, Serialize)]
pub struct InitRepository {
    pub repository_id: String,
    pub root_path: String,
    pub forge_dir: String,
    pub database_path: String,
    pub git_head: Option<String>,
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
    pub current_operation_id: String,
    pub current_view_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartAttempt {
    pub intent_id: String,
    pub attempt_id: String,
    pub base_head: String,
    pub operation_id: String,
    pub current_view_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt_id: String,
    pub intent_id: String,
    pub intent: String,
    pub base_head: String,
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
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckSummary {
    pub check_result_id: String,
    pub status: String,
    pub reason: String,
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
    pub deleted: Vec<String>,
}

pub fn init_repository(cwd: &Path, request_id: Option<String>) -> Result<InitRepository> {
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
    let tx = connection.transaction()?;

    tx.execute(
        "INSERT INTO repositories (id, root_path, git_head, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
        params![repo_id, root.to_string_lossy(), git_head, now],
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
    let state_json = json!({
        "repository_id": repo_id,
        "root_path": root,
        "git_head": git_head,
        "lifecycle": "initialized"
    })
    .to_string();
    tx.execute(
        "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
         VALUES (?1, ?2, ?3, 'initialized', ?4, ?5)",
        params![view_id, repo_id, operation_id, state_json, now],
    )?;
    tx.execute(
        "INSERT INTO current_state (
            singleton, repo_id, current_operation_id, current_view_id, updated_at_ms
        ) VALUES (1, ?1, ?2, ?3, ?4)",
        params![repo_id, operation_id, view_id, now],
    )?;
    tx.commit()?;

    Ok(InitRepository {
        repository_id: repo_id,
        root_path: root.to_string_lossy().into_owned(),
        forge_dir: forge_dir.to_string_lossy().into_owned(),
        database_path: database_path.to_string_lossy().into_owned(),
        git_head,
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
    let tx = connection.transaction()?;
    let (repo_id, parent_operation_id): (String, String) = tx.query_row(
        "SELECT repo_id, current_operation_id FROM current_state WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    if parent_operation_id != expected_current_operation_id {
        return Err(anyhow!("current operation changed"));
    }

    let result = insert_operation_view(&tx, &repo_id, Some(&parent_operation_id), input)?;
    tx.commit()?;
    Ok(result)
}

pub fn open_repository(cwd: &Path) -> Result<RepositoryContext> {
    let root = git_root(cwd)?;
    let database_path = root.join(".forge/forge.db");
    if !database_path.exists() {
        return Err(anyhow!("forge repository is not initialized"));
    }
    let connection = open_connection(&database_path)?;
    let (repo_id, current_operation_id, current_view_id): (String, String, String) =
        connection.query_row(
            "SELECT repo_id, current_operation_id, current_view_id FROM current_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    Ok(RepositoryContext {
        repo_id,
        root_path: root,
        database_path,
        current_operation_id,
        current_view_id,
    })
}

pub fn operation_for_request(cwd: &Path, request_id: &str) -> Result<Option<RequestIdOperation>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT id, command, status, error_json
             FROM operations
             WHERE repo_id = ?1 AND request_id = ?2
             ORDER BY created_at_ms DESC LIMIT 1",
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
    let mut connection = open_connection(&context.database_path)?;
    let tx = connection.transaction()?;
    let now = now_ms();
    let intent_id = new_id("intent");
    let attempt_id = new_id("attempt");

    tx.execute(
        "INSERT INTO intents (id, repo_id, text, created_at_ms) VALUES (?1, ?2, ?3, ?4)",
        params![intent_id, context.repo_id, intent, now],
    )?;
    tx.execute(
        "INSERT INTO attempts (id, repo_id, intent_id, base_head, status, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, 'active', ?5)",
        params![attempt_id, context.repo_id, intent_id, base_head, now],
    )?;

    let op = insert_operation_view(
        &tx,
        &context.repo_id,
        Some(&context.current_operation_id),
        OperationViewInput {
            request_id,
            command: "start".to_string(),
            kind: "attempt_started".to_string(),
            view_kind: ViewKind::Initialized,
            state: json!({
                "lifecycle": "attempt_active",
                "attempt_id": attempt_id,
                "intent_id": intent_id
            }),
        },
    )?;
    tx.commit()?;

    Ok(StartAttempt {
        intent_id,
        attempt_id,
        base_head,
        operation_id: op.operation_id,
        current_view_id: op.view_id,
    })
}

pub fn save_snapshot(
    cwd: &Path,
    request_id: Option<String>,
    content_ref: String,
    changed_paths: Vec<String>,
) -> Result<SnapshotRecord> {
    let context = open_repository(cwd)?;
    let attempt = active_attempt(&context)?;
    let mut connection = open_connection(&context.database_path)?;
    let tx = connection.transaction()?;
    let parent_snapshot_id: Option<String> = tx
        .query_row(
            "SELECT id FROM snapshots WHERE attempt_id = ?1 ORDER BY created_at_ms DESC LIMIT 1",
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
        &tx,
        &context.repo_id,
        Some(&context.current_operation_id),
        OperationViewInput {
            request_id,
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
    tx.commit()?;
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

pub fn latest_snapshot_content_ref(cwd: &Path) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    Ok(latest_snapshot(&context)?.map(|snapshot| snapshot.content_ref))
}

pub fn record_restore(
    cwd: &Path,
    request_id: Option<String>,
    snapshot_id: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let tx = connection.transaction()?;
    let op = insert_operation_view(
        &tx,
        &context.repo_id,
        Some(&context.current_operation_id),
        OperationViewInput {
            request_id,
            command: "restore".to_string(),
            kind: "snapshot_restored".to_string(),
            view_kind: ViewKind::Initialized,
            state: json!({ "lifecycle": "snapshot_restored", "snapshot_id": snapshot_id }),
        },
    )?;
    tx.commit()?;
    Ok(op)
}

pub fn record_evidence(
    cwd: &Path,
    request_id: Option<String>,
    input: EvidenceInput,
) -> Result<EvidenceRecord> {
    let context = open_repository(cwd)?;
    let attempt = active_attempt(&context)?;
    let snapshot = latest_snapshot(&context)?;
    let mut connection = open_connection(&context.database_path)?;
    let tx = connection.transaction()?;
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
        &tx,
        &context.repo_id,
        Some(&context.current_operation_id),
        OperationViewInput {
            request_id,
            command: "run".to_string(),
            kind: "evidence_captured".to_string(),
            view_kind: ViewKind::Initialized,
            state: json!({ "lifecycle": "evidence_captured", "evidence_id": evidence_id }),
        },
    )?;
    tx.commit()?;
    let latest = latest_evidence(&context)?.context("recorded evidence missing")?;
    Ok(EvidenceRecord {
        evidence_id,
        attempt_id: attempt.attempt_id,
        command: latest.command,
        args: latest.args,
        exit_code: latest.exit_code as i32,
        stdout_excerpt: input.stdout_excerpt,
        stderr_excerpt: input.stderr_excerpt,
        stdout_truncated: input.stdout_truncated,
        stderr_truncated: input.stderr_truncated,
        timed_out: input.timed_out,
        sensitivity: latest.sensitivity,
        visibility: input.visibility,
        trust: latest.trust,
        operation_id: op.operation_id,
    })
}

pub fn propose(cwd: &Path, request_id: Option<String>) -> Result<ProposalRecord> {
    let context = open_repository(cwd)?;
    let attempt = active_attempt(&context)?;
    let snapshot = latest_snapshot(&context)?.context("no snapshot saved for active attempt")?;
    let mut connection = open_connection(&context.database_path)?;
    let tx = connection.transaction()?;
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
        &tx,
        &context.repo_id,
        Some(&context.current_operation_id),
        OperationViewInput {
            request_id,
            command: "propose".to_string(),
            kind: "proposal_created".to_string(),
            view_kind: ViewKind::Initialized,
            state: json!({ "lifecycle": "proposal_draft", "proposal_id": proposal_id }),
        },
    )?;
    tx.commit()?;
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
    status: String,
    reason: String,
) -> Result<CheckRecord> {
    let context = open_repository(cwd)?;
    let proposal = latest_proposal(&context)?.context("no proposal exists")?;
    let evidence = latest_evidence(&context)?;
    let evidence_id = evidence.as_ref().map(|e| e.evidence_id.clone());
    let (status, reason) = match evidence.as_ref().and_then(|e| e.snapshot_id.as_deref()) {
        Some(snapshot_id) if snapshot_id == proposal.snapshot_id => (status, reason),
        Some(_) => (
            "stale".to_string(),
            "latest evidence does not match proposal revision snapshot".to_string(),
        ),
        None => (
            "missing".to_string(),
            "no evidence recorded for proposal revision snapshot".to_string(),
        ),
    };
    let mut connection = open_connection(&context.database_path)?;
    let tx = connection.transaction()?;
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
        &tx,
        &context.repo_id,
        Some(&context.current_operation_id),
        OperationViewInput {
            request_id,
            command: "check".to_string(),
            kind: "proposal_checked".to_string(),
            view_kind: ViewKind::Initialized,
            state: json!({ "lifecycle": "checked", "check_result_id": check_result_id }),
        },
    )?;
    tx.commit()?;
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

pub fn decide(cwd: &Path, request_id: Option<String>, decision: &str) -> Result<DecisionRecord> {
    let context = open_repository(cwd)?;
    let proposal = latest_proposal(&context)?.context("no proposal exists")?;
    let mut connection = open_connection(&context.database_path)?;
    let tx = connection.transaction()?;
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
        &tx,
        &context.repo_id,
        Some(&context.current_operation_id),
        OperationViewInput {
            request_id,
            command: decision.to_string(),
            kind: format!("proposal_{decision}"),
            view_kind: ViewKind::Initialized,
            state: json!({ "lifecycle": decision, "decision_id": decision_id }),
        },
    )?;
    tx.commit()?;
    Ok(DecisionRecord {
        decision_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        decision: decision.to_string(),
        operation_id: op.operation_id,
    })
}

pub fn latest_exportable_proposal(cwd: &Path) -> Result<ProposalSummary> {
    latest_proposal(&open_repository(cwd)?)?.context("no proposal exists")
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
             ORDER BY created_at_ms DESC LIMIT 1",
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
    branch_name: String,
    commit_id: String,
) -> Result<PublicationRecord> {
    let context = open_repository(cwd)?;
    let proposal = latest_proposal(&context)?.context("no proposal exists")?;
    let mut connection = open_connection(&context.database_path)?;
    let tx = connection.transaction()?;
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
        &tx,
        &context.repo_id,
        Some(&context.current_operation_id),
        OperationViewInput {
            request_id,
            command: "export branch".to_string(),
            kind: "branch_exported".to_string(),
            view_kind: ViewKind::Initialized,
            state: json!({ "lifecycle": "published_branch", "publication_id": publication_id }),
        },
    )?;
    tx.commit()?;
    Ok(PublicationRecord {
        publication_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        branch_name,
        commit_id,
        operation_id: op.operation_id,
    })
}

pub fn show(cwd: &Path) -> Result<ShowRecord> {
    let context = open_repository(cwd)?;
    Ok(ShowRecord {
        attempt: active_attempt(&context).ok(),
        latest_snapshot: latest_snapshot(&context)?,
        latest_evidence: latest_evidence(&context)?,
        latest_proposal: latest_proposal(&context)?,
        latest_check: latest_check(&context)?,
        latest_decision: latest_decision_for_context(&context)?,
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
    if schema_version != Some(1) {
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
    let mut statement = connection.prepare("SELECT id, content_ref FROM snapshots")?;
    let refs = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for content_ref in refs {
        let (snapshot_id, content_ref) = content_ref?;
        if let Some(tree) = content_ref.strip_prefix("git-tree:") {
            let output = Command::new("git")
                .args(["cat-file", "-e", &format!("{tree}^{{tree}}")])
                .current_dir(&context.root_path)
                .output()?;
            if !output.status.success() {
                issues.push(format!("missing content ref for snapshot {snapshot_id}"));
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
    let show = show(cwd)?;
    let mut body = String::new();
    body.push_str("# Forge Proposal\n\n");
    if let Some(attempt) = show.attempt {
        body.push_str(&format!("Intent: {}\n\n", attempt.intent));
    }
    if let Some(proposal) = show.latest_proposal {
        body.push_str("## Changed Paths\n");
        for path in proposal.changed_paths {
            body.push_str(&format!("- {path}\n"));
        }
        body.push('\n');
    }
    if let Some(evidence) = show.latest_evidence {
        body.push_str("## Evidence\n");
        body.push_str(&format!(
            "- `{}` exited with `{}` ({})\n\n",
            evidence.command, evidence.exit_code, evidence.trust
        ));
    }
    if let Some(check) = show.latest_check {
        body.push_str("## Check\n");
        body.push_str(&format!("- {}: {}\n\n", check.status, check.reason));
    }
    if let Some(decision) = show.latest_decision {
        body.push_str("## Decision\n");
        body.push_str(&format!("- {decision}\n"));
    }
    if let Some(publication) = latest_publication(&open_repository(cwd)?)? {
        body.push_str("\n## Publication\n");
        body.push_str(&format!(
            "- `{}` at `{}`\n",
            publication.branch_name, publication.commit_id
        ));
    }
    Ok(body)
}

pub fn gc_dry_run(cwd: &Path) -> Result<GcDryRunReport> {
    let _context = open_repository(cwd)?;
    Ok(GcDryRunReport {
        dry_run: true,
        unreachable_snapshots: Vec::new(),
        unreachable_evidence: Vec::new(),
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
    let tx = connection.transaction()?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
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
    tx.commit()?;
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

fn active_attempt(context: &RepositoryContext) -> Result<AttemptRecord> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT a.id, a.intent_id, i.text, a.base_head
             FROM attempts a
             JOIN intents i ON i.id = a.intent_id
             WHERE a.repo_id = ?1 AND a.status = 'active'
             ORDER BY a.created_at_ms DESC LIMIT 1",
            params![context.repo_id],
            |row| {
                Ok(AttemptRecord {
                    attempt_id: row.get(0)?,
                    intent_id: row.get(1)?,
                    intent: row.get(2)?,
                    base_head: row.get(3)?,
                })
            },
        )
        .context("no active attempt")
}

fn latest_snapshot(context: &RepositoryContext) -> Result<Option<SnapshotSummary>> {
    let connection = open_connection(&context.database_path)?;
    let attempt = match active_attempt(context) {
        Ok(attempt) => attempt,
        Err(_) => return Ok(None),
    };
    connection
        .query_row(
            "SELECT id, content_ref, changed_paths_json FROM snapshots
             WHERE attempt_id = ?1 ORDER BY created_at_ms DESC LIMIT 1",
            params![attempt.attempt_id],
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

fn latest_evidence(context: &RepositoryContext) -> Result<Option<EvidenceSummary>> {
    let connection = open_connection(&context.database_path)?;
    let attempt = match active_attempt(context) {
        Ok(attempt) => attempt,
        Err(_) => return Ok(None),
    };
    connection
        .query_row(
            "SELECT id, snapshot_id, command, args_json, exit_code, sensitivity, trust FROM evidence
             WHERE attempt_id = ?1 ORDER BY created_at_ms DESC LIMIT 1",
            params![attempt.attempt_id],
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

fn latest_proposal(context: &RepositoryContext) -> Result<Option<ProposalSummary>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT p.id, pr.id, p.snapshot_id, p.base_head, pr.content_ref, pr.changed_paths_json
             FROM proposals p
             JOIN proposal_revisions pr ON pr.proposal_id = p.id
             WHERE p.repo_id = ?1
             ORDER BY pr.created_at_ms DESC LIMIT 1",
            params![context.repo_id],
            |row| {
                let changed_paths_json: String = row.get(5)?;
                Ok(ProposalSummary {
                    proposal_id: row.get(0)?,
                    proposal_revision_id: row.get(1)?,
                    snapshot_id: row.get(2)?,
                    base_head: row.get(3)?,
                    content_ref: row.get(4)?,
                    changed_paths: serde_json::from_str(&changed_paths_json).unwrap_or_default(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn latest_check(context: &RepositoryContext) -> Result<Option<CheckSummary>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT id, status, reason FROM check_results
             WHERE repo_id = ?1 ORDER BY created_at_ms DESC LIMIT 1",
            params![context.repo_id],
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
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT decision FROM decisions WHERE repo_id = ?1 ORDER BY created_at_ms DESC LIMIT 1",
            params![context.repo_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn latest_publication(context: &RepositoryContext) -> Result<Option<PublicationRecord>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT id, proposal_id, proposal_revision_id, branch_name, commit_id
             FROM publications WHERE repo_id = ?1 ORDER BY created_at_ms DESC LIMIT 1",
            params![context.repo_id],
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

fn new_id(prefix: &str) -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{prefix}_{}_{}",
        duration.as_millis(),
        duration.subsec_nanos()
    )
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
        let tx = connection.transaction()?;
        tx.execute_batch(MIGRATION_001)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (1, '001_init', ?1)",
            params![now_ms()],
        )?;
        tx.commit()?;
    }

    Ok(())
}

fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

fn read_init_repository(
    connection: &Connection,
    root: &Path,
    forge_dir: &Path,
    database_path: &Path,
) -> Result<Option<InitRepository>> {
    let row = connection
        .query_row(
            "SELECT r.id, r.git_head, cs.current_operation_id, cs.current_view_id
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
                ))
            },
        )
        .optional()?;

    Ok(row.map(
        |(repository_id, git_head, current_operation_id, current_view_id)| InitRepository {
            repository_id,
            root_path: root.to_string_lossy().into_owned(),
            forge_dir: forge_dir.to_string_lossy().into_owned(),
            database_path: database_path.to_string_lossy().into_owned(),
            git_head,
            current_operation_id,
            current_view_id,
            already_initialized: true,
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
