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

mod error;
mod integrity;
mod migrations;
mod repo_lock;
pub use error::{error_registry, ErrorCodeSpec, ForgeError, NativeHistoryCorruptKind, TamperKind};
pub use repo_lock::{LockTimeout, RepoLock};

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
    /// Who ran the command (NER-136 actor model): `--actor`, else `FORGE_ACTOR`,
    /// else `"unknown"`. Folded into the evidence digest so attribution is itself
    /// tamper-evident.
    pub actor: String,
    /// Machine-readable outcome parsed from the full captured output (NER-136 §U5),
    /// e.g. `{"tool":"cargo-test","passed":12,"failed":0}`. `None` when no parser
    /// matched. Persisted alongside the excerpt and folded into the digest.
    pub structured_json: Option<String>,
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
    /// The trust-ladder rung. In Phase 5 this is a *verifiable* claim: it is emitted
    /// alongside `hash_alg` + `content_hash`, so a reviewer can recompute the digest
    /// rather than taking a bare string on faith (replacing the historic hardcoded
    /// `locally_observed` literal that asserted nothing). Higher rungs (signed,
    /// attested) are Phase 9.
    pub trust: String,
    /// The hash algorithm backing the trust claim (`sha256`).
    pub hash_alg: String,
    /// The evidence row's tamper-evident content hash (NER-136).
    pub content_hash: String,
    /// Who ran the command (attribution, not auth).
    pub actor: String,
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
    /// Per-gate verdicts from the policy engine (NER-135). Emit-only in v0 — not
    /// persisted; Phase 6 (NER-137) adds a column when it consumes per-gate history.
    pub gates: Vec<forge_policy::GateResult>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionRecord {
    pub decision_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub decision: String,
    /// The check status observed in-txn when an `accept` was gated on evidence
    /// (NER-135). Omitted entirely for `reject` (the gate never runs), so the reject
    /// response shape is unchanged. `Some("passed")` for a normal accept;
    /// `Some(<failed|missing|stale>)` only when `--allow-unverified` bypassed the
    /// gate, which the CLI surfaces as a `warnings[]` entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_status: Option<String>,
    /// The native `Commit` written when a proposal is accepted in a native repo (NER-138
    /// Phase 7 slice 3). `None` for git-backend repos and for `reject` (no commit). The
    /// ref-store HEAD is advanced to this id after the decision row commits; it is recorded
    /// in `decisions.commit_id` so a torn HEAD advance is reconcilable from the ledger.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_id: Option<String>,
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

#[derive(Debug, Clone)]
pub struct StaleBaseConflictInput {
    pub context: String,
    pub expected_head: String,
    pub actual_head: String,
    pub base_content_ref: String,
    pub ours_content_ref: String,
    pub theirs_content_ref: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StaleBaseConflict {
    pub input: StaleBaseConflictInput,
}

#[derive(Debug, Clone)]
pub struct MergeConflictInput {
    pub context: String,
    pub proposal_id: Option<String>,
    pub base_head: Option<String>,
    pub ours_head: Option<String>,
    pub base_content_ref: String,
    pub ours_content_ref: String,
    pub theirs_content_ref: String,
    pub conflicts: Vec<forge_content_native::NativeMergeConflict>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeConflictRecord {
    pub conflict_set_id: String,
    pub operation_id: String,
    pub view_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeSuccessRecord {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub snapshot_id: String,
    pub base_content_ref: String,
    pub ours_content_ref: String,
    pub theirs_content_ref: String,
    pub merged_content_ref: String,
    pub operation_id: String,
    pub view_id: String,
}

#[derive(Debug, Clone)]
pub struct MergeSuccessInput {
    pub base_head: String,
    pub ours_head: String,
    pub base_content_ref: String,
    pub ours_content_ref: String,
    pub theirs_content_ref: String,
    pub merged_content_ref: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictResolutionRecord {
    pub conflict_set_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub snapshot_id: String,
    pub evidence_id: String,
    pub resolution_ref: String,
    pub operation_id: String,
    pub view_id: String,
}

impl StaleBaseConflict {
    pub fn forge_error(&self) -> ForgeError {
        ForgeError::StaleBase {
            expected_head: self.input.expected_head.clone(),
            actual_head: self.input.actual_head.clone(),
        }
    }
}

impl std::fmt::Display for StaleBaseConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.forge_error().fmt(f)
    }
}

impl std::error::Error for StaleBaseConflict {}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictListRecord {
    pub conflicts: Vec<ConflictSetSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictShowRecord {
    pub conflict: ConflictSetSummary,
    pub path_conflicts: Vec<PathConflictSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictSetSummary {
    pub conflict_set_id: String,
    pub context: String,
    pub base_content_ref: Option<String>,
    pub ours_content_ref: Option<String>,
    pub theirs_content_ref: Option<String>,
    pub generated_by_operation_id: Option<String>,
    pub resolver_backend: Option<String>,
    pub status: String,
    pub path_conflict_count: i64,
    pub redacted_count: i64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathConflictSummary {
    pub path_conflict_id: String,
    pub path_fingerprint: String,
    pub kind: String,
    pub base_ref: Option<String>,
    pub ours_ref: Option<String>,
    pub theirs_ref: Option<String>,
    pub base_status: Option<String>,
    pub ours_status: Option<String>,
    pub theirs_status: Option<String>,
    pub base_mode: Option<String>,
    pub ours_mode: Option<String>,
    pub theirs_mode: Option<String>,
    pub resolution_ref: Option<String>,
    pub status: String,
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
    /// `content_ref` rows whose referenced object is missing or fails
    /// verification — the failure mode the store-before-DB ordering (NER-132)
    /// makes impossible; this is the safety-net assertion. Empty in a healthy repo.
    pub dangling_content_refs: Vec<String>,
    /// Worktree paths holding a leftover crash-atomic-restore temp
    /// (`.forge-restore-*`), the signature of a restore killed mid-flight. Empty
    /// in a healthy repo.
    pub half_applied_worktrees: Vec<String>,
    /// Rows whose tamper-evident hash failed verification (NER-136): an evidence or
    /// decision row whose content no longer matches its stored hash, an operation
    /// whose chain link is broken (a deletion/reorder), or a post-watermark missing
    /// hash. Empty in a healthy repo. A head-truncated chain (a lost latest op) is
    /// NOT a tamper — it verifies as a legitimately-shorter chain.
    pub tampered_rows: Vec<TamperedRow>,
    /// Native commit-DAG integrity breaks (NER-138 Phase 7 slice 3): a parent cycle, a
    /// dangling parent/tree object, or a `decisions.commit_id` whose commit object is absent.
    /// Empty in a healthy repo. This is the "DAG has no cycles/dangling parents (doctor
    /// verifies)" whole-phase exit criterion.
    pub native_history_issues: Vec<NativeHistoryFinding>,
}

/// One row that failed integrity verification in `doctor`'s chain pass. Carries only
/// an opaque id, the table, and a closed-enum break kind — never an excerpt or
/// command string (this is a machine-visible egress). `kind` serializes as snake_case
/// (`content_edit`/`broken_link`/`missing_hash`).
#[derive(Debug, Clone, Serialize)]
pub struct TamperedRow {
    pub id: String,
    pub table: String,
    pub kind: TamperKind,
}

/// One native-history integrity break found by `doctor`'s commit-DAG walk (NER-138 Phase 7
/// slice 3). Carries only the closed-enum `kind` and opaque `f1:` commit ids — never a path or
/// excerpt. `kind` serializes the SAME way as [`ForgeError::NativeHistoryCorrupt`]'s `details`
/// (the shared [`NativeHistoryCorruptKind`]), so the error payload and the doctor report can
/// never disagree on the break-kind string. Empty in a healthy repo.
#[derive(Debug, Clone, Serialize)]
pub struct NativeHistoryFinding {
    pub kind: NativeHistoryCorruptKind,
    pub commit_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_id: Option<String>,
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
    // A NATIVE repo earns real git independence: it does not require the git binary at init —
    // its root is `cwd` (canonicalized). A GIT-backed repo still anchors on the git toplevel
    // (Forge layers on an existing git repo). This is what lets the full native lifecycle —
    // init included — run with git removed from PATH (NER-138 Phase 7 exit criterion).
    let root = if content_backend == "native" {
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
    } else {
        git_root(cwd)?
    };
    // NER-143 R9: refuse to initialize a forge repo nested inside an existing forge repo.
    // `forge_root`'s nearest-ancestor walk routes a subtree's commands to whichever `.forge`
    // is closer up-tree, and the nested repo's objects look unreachable to the outer repo's
    // gc (a Phase-8 deletion hazard). The check is BACKEND-AGNOSTIC because `forge_root` is:
    // a native inner repo anchors at `cwd` (so it can nest below anything), and a git inner
    // repo whose own toplevel sits below an outer repo can nest too — both are shadowing
    // hazards regardless of backend, so the guard must not be gated on `content_backend`
    // (the code-review adversarial pass flagged the native-only gating as an escape). Message
    // is path-free (S1). This checks ANCESTORS only (`root.parent()` upward), so re-init of
    // the same root never trips it and stays the already_initialized path below. (A
    // deliberately-independent nested repo is not a v0 use case; an --allow-nested opt-out is
    // future work. A narrow cross-repo-init TOCTOU window — two inits racing in
    // ancestor/descendant dirs before either's lock — is an accepted v0 limitation.)
    {
        let mut ancestor = root.parent();
        while let Some(dir) = ancestor {
            if dir.join(".forge/forge.db").exists() {
                bail!("refusing to initialize a forge repo nested inside an existing forge repo");
            }
            ancestor = dir.parent();
        }
    }
    let forge_dir = root.join(".forge");
    fs::create_dir_all(&forge_dir)
        .with_context(|| format!("failed to create {}", forge_dir.display()))?;

    // Serialize concurrent first-inits of the same repo (NER-132 U5): hold the repo
    // write lock across migration + the repository INSERT, so a racing init observes
    // the winner's committed row via read_init_repository below and returns
    // already_initialized rather than colliding on the repositories.root_path UNIQUE
    // constraint. The lock file lives in the .forge dir just created. init does not
    // route through the CLI command_result lock, so it acquires here, never nested.
    let _init_lock = repo_lock::acquire(&forge_dir)?;

    let database_path = forge_dir.join("forge.db");
    let already_had_db = database_path.exists();
    let mut connection = open_connection(&database_path)
        .with_context(|| format!("failed to open {}", database_path.display()))?;
    migrations::apply_pending_migrations(&mut connection)?;

    if let Some(existing) = read_init_repository(&connection, &root, &forge_dir, &database_path)? {
        return Ok(InitRepository {
            already_initialized: true,
            ..existing
        });
    }

    // A native repo records no git_head (it has its own ref store and never shells git);
    // recording it would reintroduce a git dependency at init.
    let git_head = if content_backend == "native" {
        None
    } else {
        git_head(&root)
    };
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
        // The genesis link: parent is the documented genesis sentinel, no domain
        // digest. Stored so `doctor`'s re-walk starts from a verifiable anchor and a
        // fresh repo is never mis-flagged as a NULL-hash (tampered) op (NER-136).
        let genesis_hash = integrity::operation_link_hash(
            integrity::GENESIS_PARENT_HASH,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command: "init",
                kind: "repository_initialized",
                created_at_ms: now,
            },
            None,
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, 'init', ?4, 'repository_initialized', NULL, ?5, NULL, ?6, ?7)",
            params![
                operation_id,
                repo_id,
                request_id,
                format!("{:?}", OperationStatus::Succeeded).to_lowercase(),
                view_id,
                genesis_hash,
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

pub fn open_repository(cwd: &Path) -> Result<RepositoryContext> {
    // Git-free root resolution (slice 3): walk up for `.forge/forge.db` rather than shelling
    // `git rev-parse`, so every post-init command works with git removed from PATH.
    let root = forge_root(cwd)?;
    let database_path = root.join(".forge/forge.db");
    if !database_path.exists() {
        return Err(ForgeError::NotInitialized.into());
    }
    // `open_repository` is a pure open+query: schema migrations are applied by the
    // transient `migrate()` entrypoint at the top of `command_result` (and by
    // `init_repository` under its own lock) — never here, where no lock is held.
    let connection = open_connection(&database_path)?;
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
    // Git-free root resolution (slice 3): a `.forge`-walk, so locking works without git.
    let root = match forge_root(cwd) {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let forge_dir = root.join(".forge");
    if !forge_dir.exists() {
        return Ok(None);
    }
    repo_lock::acquire(&forge_dir).map(Some)
}

/// Bring the repository's schema up to this binary's head, acquiring the repo
/// write lock **only when a migration is actually pending** (NER-133 U4).
///
/// This is the transient, self-acquiring entrypoint the CLI runs at the top of
/// `command_result`, **before** any per-command lock — so it never nests inside an
/// already-held lock. It mirrors [`acquire_repo_lock`]'s resolution so it no-ops
/// when there is nothing to migrate, letting the command's own logic surface the
/// canonical error:
/// - `cwd` is not inside a Git work tree ⇒ `Ok(())` (the command surfaces
///   not-a-git-repo / `NOT_INITIALIZED`).
/// - `.forge/forge.db` does not exist ⇒ `Ok(())` (uninitialized — the command
///   surfaces `NOT_INITIALIZED`).
/// - DB version `== HEAD` ⇒ `Ok(())` on the cheap read, **no lock taken** (the
///   common path, including every read-only command and `run`).
/// - DB version `> HEAD` ⇒ `Err(ForgeError::UnknownSchemaVersion)`: the DB was
///   written by a newer Forge; refuse without acquiring the lock. The CLI maps
///   this to `SCHEMA_VERSION_UNSUPPORTED` and short-circuits before any write.
/// - DB version `< HEAD` ⇒ acquire the repo lock **transiently**, apply the
///   pending migrations (idempotent + version-gated, so a concurrent migrator that
///   won the race is handled), then release the lock (Drop) before returning.
pub fn migrate(cwd: &Path) -> Result<()> {
    // Git-free root resolution (slice 3): a `.forge`-walk, so migration works without git.
    let root = match forge_root(cwd) {
        Ok(root) => root,
        Err(_) => return Ok(()),
    };
    let forge_dir = root.join(".forge");
    let database_path = forge_dir.join("forge.db");
    if !database_path.exists() {
        return Ok(());
    }

    let mut connection = open_connection(&database_path)?;
    let db_version = migrations::current_schema_version(&connection)?;
    let head = migrations::schema_head();

    if db_version == head {
        // Common fast path: nothing to do, take no lock.
        return Ok(());
    }
    if db_version > head {
        // A forward-versioned DB: refuse to write without taking the lock.
        return Err(ForgeError::UnknownSchemaVersion {
            db_version,
            supported_head: head,
        }
        .into());
    }

    // Pending (`db_version < head`): acquire the repo lock transiently, apply, and
    // release on Drop before returning. `apply_pending_migrations` re-reads the
    // applied versions under the lock and is idempotent, so a concurrent migrator
    // that won the race is a no-op here. Acquired exactly once, before the
    // per-command lock — never nested.
    let _lock = repo_lock::acquire(&forge_dir)?;
    migrations::apply_pending_migrations(&mut connection)?;
    Ok(())
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

/// Build the persisted check-spec JSON from repeatable `forge start --require
/// "<cmd>"` values (NER-135). Each value is whitespace-tokenized into
/// `(program, args)` — the same shape `forge run -- <argv>` records as evidence, so
/// a gate's identity matches its evidence. Returns `None` when no gate is declared
/// (the policy engine's default mode). Quoting of whitespace-bearing args is a
/// documented v0 limitation (deferred). Lives in the store (which already depends on
/// `forge-policy`) so the CLI need not name `forge_policy` types directly to build a
/// spec — though it still transitively serializes them via `CheckRecord.gates`.
pub fn check_spec_json_from_requires(
    requires: &[String],
    structured_requires: &[String],
) -> Option<String> {
    let parse_gate = |raw: &str, require_structured_pass: bool| -> Option<forge_policy::Gate> {
        let mut tokens = raw.split_whitespace();
        let program = tokens.next()?.to_string();
        let args = tokens.map(str::to_string).collect();
        Some(forge_policy::Gate {
            program,
            args,
            require_structured_pass,
        })
    };
    let mut gates: Vec<forge_policy::Gate> = requires
        .iter()
        .filter_map(|raw| parse_gate(raw, false))
        .collect();
    gates.extend(
        structured_requires
            .iter()
            .filter_map(|raw| parse_gate(raw, true)),
    );
    if gates.is_empty() {
        return None;
    }
    serde_json::to_string(&forge_policy::CheckSpec { gates }).ok()
}

pub fn start_attempt(
    cwd: &Path,
    request_id: Option<String>,
    intent: String,
    base_head: String,
    check_spec_json: Option<String>,
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
        check_spec_json,
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
        // `attempt start` references an existing intent and inherits its gates; it
        // never mints an intent, so it carries no check spec of its own (NER-135).
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_attempt(
    context: &RepositoryContext,
    request_id: Option<String>,
    intent_id: Option<String>,
    intent: Option<String>,
    base_head: String,
    attach: bool,
    command: &str,
    check_spec_json: Option<String>,
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
                    return Err(ForgeError::UnknownIntent {
                        selector: id.to_string(),
                    }
                    .into());
                }
                id
            }
            None => {
                let id = new_id("intent");
                tx.execute(
                    "INSERT INTO intents (id, repo_id, text, check_spec_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        id,
                        context.repo_id,
                        intent
                            .clone()
                            .unwrap_or_else(|| "local agent attempt".to_string()),
                        check_spec_json,
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
    // Write-binding verification (NER-134): the authoritative, non-bypassable guard
    // on the production write path. Refuse to record the worktree's content under an
    // attempt other than the one the worktree is materialized for.
    verify_worktree_binding(&context, &attempt.attempt_id)?;
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
        // NER-143 R1: the worktree now holds exactly this snapshot's tree (save captured it),
        // so it becomes the expected dirty-check baseline. Atomic with the op-log advance.
        set_expected_content_ref(tx, &content_ref)?;
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

/// Write-binding verification (NER-134): refuse to act on the worktree under
/// `target_attempt_id` when the worktree is currently materialized for a
/// *different* attempt (`current_state.attached_attempt_id`).
///
/// The raw `attached_attempt_id` is consulted regardless of the attached attempt's
/// active/abandoned status: if the worktree holds attempt `W`'s content, recording
/// it under `X != W` is cross-attempt contamination even if `W` was later abandoned.
/// `None` (nothing materialized — the single-attempt v0 flow, or after a forced
/// detach) is always allowed: contamination requires a *different* attempt to have
/// been materialized, and materialization always sets `attached_attempt_id`. The
/// check runs only after attempt resolution succeeds, so an unqualified ambiguous
/// selector still surfaces `AmbiguousAttempt` first.
fn verify_worktree_binding(context: &RepositoryContext, target_attempt_id: &str) -> Result<()> {
    if let Some(attached) = context.attached_attempt_id.as_deref() {
        if attached != target_attempt_id {
            return Err(ForgeError::AttemptWorktreeMismatch {
                requested_attempt: target_attempt_id.to_string(),
                attached_attempt: attached.to_string(),
            }
            .into());
        }
    }
    Ok(())
}

/// Resolve the `save` target attempt and verify the worktree binding *before* the
/// CLI snapshots the worktree (NER-134), so a mismatch fails fast without writing
/// orphan content objects. Returns the resolved attempt id; the CLI passes it back
/// as an explicit selector to [`save_snapshot`], whose own [`verify_worktree_binding`]
/// call is the authoritative guard. Pure read — takes no advisory lock.
pub fn verify_save_target(cwd: &Path, attempt_id: Option<&str>) -> Result<String> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    verify_worktree_binding(&context, &attempt.attempt_id)?;
    Ok(attempt.attempt_id)
}

/// The attempt that owns `snapshot_id` (NER-134 Piece 1b), so `restore` can refuse
/// to materialize a snapshot belonging to an attempt other than the bound one.
pub fn snapshot_owner_attempt_id(cwd: &Path, snapshot_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT attempt_id FROM snapshots WHERE repo_id = ?1 AND id = ?2",
            params![context.repo_id, snapshot_id],
            |row| row.get(0),
        )
        .optional()?
        // Defensive: `restore` resolves `snapshot_content_ref` first, so a missing
        // snapshot already errors before this is reached.
        .ok_or_else(|| ForgeError::NoSnapshot.into())
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

/// The content ref the worktree is EXPECTED to hold — the tree the last materializing op
/// (save/restore/checkout/undo) put there, tracked in `current_state.expected_content_ref`
/// (migration 007, NER-143 R1). `None` for a pre-007 repo or a fresh repo before its first
/// materialize; the dirty-check then falls back to the latest-snapshot baseline. This is the
/// crash-safe baseline: a non-save op materializes a different tree than the latest *saved*
/// snapshot, so comparing the worktree against "latest saved" spuriously fails chained
/// navigation (undo twice) — comparing against "expected" does not.
pub fn expected_content_ref(cwd: &Path) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    Ok(connection
        .query_row(
            "SELECT expected_content_ref FROM current_state WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Set `current_state.expected_content_ref` to the tree a materializing op just put in the
/// worktree (NER-143 R1). Called inside the recorder's `IMMEDIATE` txn — atomic with the
/// op-log advance, so a `CurrentStateChanged` CAS-loss rolls BOTH back together. DR-F2: this
/// is a DEDICATED UPDATE in each of the four materializing recorders, never folded into the
/// shared `insert_operation_view` CAS (which every op hits — folding it there would clobber
/// the expected ref on a non-materializing `accept`/`run`/`propose`/`check`).
fn set_expected_content_ref(tx: &Connection, content_ref: &str) -> Result<()> {
    tx.execute(
        "UPDATE current_state SET expected_content_ref = ?1 WHERE singleton = 1",
        params![content_ref],
    )?;
    Ok(())
}

pub fn record_restore(
    cwd: &Path,
    request_id: Option<String>,
    snapshot_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = insert_operation_view(
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
        )?;
        // NER-143 R1: restore just materialized this snapshot's tree into the worktree.
        set_expected_content_ref(tx, content_ref)?;
        Ok(op)
    })?;
    Ok(op)
}

/// Resolve a historical commit id to the `forge-tree:` content ref of its tree, for
/// `forge checkout` (NER-138 Phase 7 slice 3). Fail-closed BEFORE any materialization:
/// - a non-parseable / non-commit id, or a never-written id the ledger does not reference,
///   is a USER error → a path-free `anyhow` ("unknown commit", mapped to COMMAND_FAILED),
///   NOT corruption (so a typo never inflates the perceived corruption rate);
/// - a commit/tree the ledger references but whose object is missing is genuine corruption
///   → typed `NativeHistoryCorrupt` (DanglingCommitId / DanglingTree).
pub fn checkout_target_content_ref(cwd: &Path, commit_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let id = match forge_content_native::ObjectId::parse(commit_id) {
        Ok(id) if matches!(id.kind(), Ok(forge_content_native::ObjectKind::Commit)) => id,
        _ => bail!("unknown commit: not a native commit id in this repository"),
    };
    let connection = open_connection(&context.database_path)?;
    let commit = match store.read_commit(&id) {
        Ok(commit) => commit,
        Err(_) => {
            let referenced: bool = connection
                .query_row(
                    "SELECT 1 FROM decisions WHERE repo_id = ?1 AND commit_id = ?2 LIMIT 1",
                    params![context.repo_id, commit_id],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if referenced {
                return Err(ForgeError::NativeHistoryCorrupt {
                    kind: NativeHistoryCorruptKind::DanglingCommitId,
                    commit_id: commit_id.to_string(),
                    related_id: None,
                }
                .into());
            }
            bail!("unknown commit: not in this repository's native history");
        }
    };
    let content_ref = format!("{}{}", forge_content::FORGE_TREE_PREFIX, commit.tree);
    // The tree (and everything it reaches) must exist before we clobber the worktree.
    store
        .verify_content_ref(&content_ref)
        .map_err(|_| ForgeError::NativeHistoryCorrupt {
            kind: NativeHistoryCorruptKind::DanglingTree,
            commit_id: commit_id.to_string(),
            related_id: Some(commit.tree.clone()),
        })?;
    Ok(content_ref)
}

/// Record a `forge checkout` in the op-log (NER-138 Phase 7 slice 3) so `undo` can reverse
/// it and gc treats the materialized commit as a reachability root. The target `commit_id`
/// is in the view `state_json`. Does NOT advance the base anchor (checkout is materialize-only
/// — a `save` afterward still diffs against the unchanged base HEAD; see the slice-3 plan).
pub fn record_checkout(
    cwd: &Path,
    request_id: Option<String>,
    commit_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "checkout".to_string(),
                kind: "commit_checked_out".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": "commit_checked_out", "commit_id": commit_id }),
            },
        )?;
        // NER-143 R1: checkout just materialized this commit's tree into the worktree.
        set_expected_content_ref(tx, content_ref)?;
        Ok(op)
    })?;
    Ok(op)
}

/// What a `forge undo` will restore (NER-138 Phase 7 slice 3 / NER-143 R3+R4).
///
/// **Semantics (deliberate v0 cut):** undo reverses the **last save of the attached attempt** by
/// restoring that save's parent snapshot (the `snapshots.parent_snapshot_id` chain — robust, no
/// cross-table timestamp comparison). It is NOT the op-log `current_state` rewind: after a
/// non-save head op (accept/checkout/run) undo still reverses the last *save*, not that head op.
/// The full op-log-rewind model is future work.
///
/// `undone_operation_id` (NER-143 R4) is the `save` operation that produced the snapshot being
/// reversed (the attempt's latest), resolved from that save's view — NOT the op-log head, which
/// after a non-save head op would mislabel the audit field.
#[derive(Debug, Clone, Serialize)]
pub struct UndoTarget {
    pub undone_operation_id: String,
    pub content_ref: String,
    pub restored_snapshot_id: String,
}

/// Resolve what `forge undo` will restore: the parent of the **attached attempt's** latest
/// snapshot. Read-only and fail-closed with a clear, path-free "nothing to undo" when there is no
/// snapshot, or the latest snapshot is the first (no earlier state to restore). Restoring past the
/// first snapshot (to the base) is future work.
pub fn undo_target(cwd: &Path) -> Result<UndoTarget> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    // NER-143 R3: bind to the attached attempt. The repo-wide latest snapshot could belong to a
    // DIFFERENT attempt than the one the worktree is bound to, so undo could otherwise restore
    // attempt X's content into attempt Y's worktree (the dirty-check resolves the attached
    // attempt's latest, so the two would also disagree). The `parent_snapshot_id` chain stays
    // within an attempt (`save_snapshot` chains per-attempt), so binding the latest-snapshot
    // selection to the attached attempt makes "undo the last save" mean "this attempt's last
    // save" and never crosses attempts.
    let attempt = resolve_attempt_in_context(&context, None)?.attempt;
    let latest: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT id, parent_snapshot_id FROM snapshots \
             WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![attempt.attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((latest_id, parent_snapshot_id)) = latest else {
        bail!("nothing to undo: this repository has no snapshots");
    };
    let Some(parent_id) = parent_snapshot_id else {
        bail!("nothing to undo: already at the first saved snapshot");
    };
    let content_ref: String = connection.query_row(
        "SELECT content_ref FROM snapshots WHERE id = ?1",
        params![parent_id],
        |row| row.get(0),
    )?;
    // NER-143 R4: the undone operation is the SAVE that produced the latest snapshot (the one
    // being reversed) — found via that save view's `snapshot_id` — not the op-log head. Scoped
    // to `snapshot_saved` views: a later `restore`/checkout of the same snapshot ALSO carries
    // `$.snapshot_id`, so without the lifecycle filter the ORDER BY would pick that restore op
    // and mislabel the audit field (code-review finding). Falls back to the op-log head only if
    // no save view is found (defensive; a save always records one). `json_extract` is exact (no
    // LIKE false-matches); SQLite's JSON1 is bundled.
    let undone_operation_id: String = connection
        .query_row(
            "SELECT operation_id FROM views \
             WHERE repo_id = ?1 AND json_extract(state_json, '$.snapshot_id') = ?2 \
             AND json_extract(state_json, '$.lifecycle') = 'snapshot_saved' \
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, latest_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_else(|| context.current_operation_id.clone());
    Ok(UndoTarget {
        undone_operation_id,
        content_ref,
        restored_snapshot_id: parent_id,
    })
}

/// Record a `forge undo` in the op-log (NER-138 Phase 7 slice 3). Append-only: undo is a
/// FORWARD operation that restores prior content — it NEVER deletes a `decisions` row (so an
/// undone accept's `commit_id` stays a permanent gc reachability root) or any op-log row. The
/// undone operation + restored snapshot are in the view `state_json` for auditability.
pub fn record_undo(
    cwd: &Path,
    request_id: Option<String>,
    undone_operation_id: &str,
    restored_snapshot_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "undo".to_string(),
                kind: "operation_undone".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": "undone",
                    "undone_operation_id": undone_operation_id,
                    "restored_snapshot_id": restored_snapshot_id,
                }),
            },
        )?;
        // NER-143 R1: undo just materialized the restored snapshot's tree into the worktree.
        set_expected_content_ref(tx, content_ref)?;
        Ok(op)
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
    let (evidence_id, content_hash, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        // Determining read inside the IMMEDIATE txn: the snapshot the evidence is
        // attributed to is read on the same connection that writes it (U4).
        let snapshot = latest_snapshot_on(tx, &attempt.attempt_id)?;
        let snapshot_id = snapshot
            .as_ref()
            .map(|snapshot| snapshot.snapshot_id.clone());
        let evidence_id = new_id("evidence");
        // Recomputed per busy-retry (the body is FnMut): `created` is captured here so
        // the digest is over exactly the bytes the INSERT below persists (NER-136).
        let created = now_ms();
        let content_hash = integrity::evidence_digest(&integrity::EvidenceDigestInput {
            attempt_id: &attempt.attempt_id,
            snapshot_id: snapshot_id.as_deref(),
            command: &input.command,
            args: &input.args,
            cwd: &input.cwd,
            exit_code: input.exit_code as i64,
            started_at_ms: input.started_at_ms,
            ended_at_ms: input.ended_at_ms,
            timed_out: input.timed_out,
            stdout_excerpt: &input.stdout_excerpt,
            stderr_excerpt: &input.stderr_excerpt,
            stdout_truncated: input.stdout_truncated,
            stderr_truncated: input.stderr_truncated,
            sensitivity: &input.sensitivity,
            actor: &input.actor,
            structured_json: input.structured_json.as_deref(),
            created_at_ms: created,
        });
        tx.execute(
            "INSERT INTO evidence (
                id, repo_id, attempt_id, snapshot_id, command, args_json, cwd, exit_code, started_at_ms, ended_at_ms,
                stdout_excerpt, stderr_excerpt, stdout_truncated, stderr_truncated, timed_out,
                sensitivity, visibility, trust, actor, structured_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                evidence_id,
                context.repo_id,
                attempt.attempt_id,
                snapshot_id,
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
                input.actor,
                input.structured_json,
                content_hash,
                created
            ],
        )?;
        // Fold the evidence digest into the op-log spine so a later swap of
        // evidence.content_hash (to cover a tamper) is caught by doctor's re-walk.
        let op = insert_operation_view_chained(
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
            Some(&content_hash),
        )?;
        Ok((evidence_id, content_hash, op))
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
        hash_alg: "sha256".to_string(),
        content_hash,
        actor: input.actor,
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
        let snapshot =
            latest_snapshot_on(tx, &attempt.attempt_id)?.ok_or(ForgeError::NoSnapshot)?;
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
) -> Result<CheckRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let proposal = resolve_proposal(&context, &attempt.attempt_id, proposal_id, true)?.proposal;
    let mut connection = open_connection(&context.database_path)?;
    let (check_result_id, outcome, evidence_id, op) = with_immediate_retry(
        &mut connection,
        |tx| {
            replay_guard(tx, &context.repo_id, request_id.as_deref())?;
            // Aggregate over the proposed snapshot's FULL evidence set, read on the same
            // connection that writes the check result — so a concurrent (lock-free) `run`
            // committing newer evidence cannot make the verdict disagree with what is
            // persisted (NER-132 U2 TOCTOU closure, preserved by the aggregate read).
            // forge-policy is the single source of pass/fail/missing/stale (NER-135 R4).
            let outcome = evaluate_check_on(tx, &attempt, &proposal)?;
            let evidence_id = representative_evidence_id(&outcome);
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
                outcome.status,
                outcome.reason,
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
            Ok((check_result_id, outcome, evidence_id, op))
        },
    )?;
    // Redact secret-like argv in the per-gate egress (the verdict was already
    // computed in-txn from the raw identities; this only affects what is surfaced).
    let gates = outcome.gates.into_iter().map(redact_gate_result).collect();
    Ok(CheckRecord {
        check_result_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        status: outcome.status,
        reason: outcome.reason,
        evidence_id,
        gates,
        operation_id: op.operation_id,
    })
}

/// Redact secret-like `key=value` argv tokens (e.g. `--token=…`, `PASSWORD=…`) in a
/// gate's program + each arg before the identity reaches a machine-visible egress —
/// the `check` JSON `gates[]` here, and (per-token) `CHECK_NOT_PASSED.unmet` in
/// `error.rs` (NER-135 code-review F2). Redacts PER-ARG: `redact_secret_like_text`
/// keys on the first `=`/`:` of its input, so redacting each arg independently catches
/// a secret in any position, where redacting a space-joined identity would only check
/// its first token. Gate specs are persisted (`intents.check_spec_json`) and surfaced
/// WITHOUT execution, so — unlike captured evidence — they get this egress pass. Full
/// non-`key=value` arg scanning is Phase 5 (see schema `notes.secret_protection`).
fn redact_gate_result(gate: forge_policy::GateResult) -> forge_policy::GateResult {
    let program = forge_content::redact_secret_like_text(&gate.program).0;
    let args = gate
        .args
        .iter()
        .map(|arg| forge_content::redact_secret_like_text(arg).0)
        .collect();
    forge_policy::GateResult {
        program,
        args,
        ..gate
    }
}

/// Pick a single representative evidence id for the `check_results.evidence_id`
/// FK from a multi-gate outcome (best-effort — `CheckRecord.gates` carries the
/// authoritative per-gate detail): the first failing gate's deciding evidence,
/// else the first gate with any deciding evidence, else NULL.
fn representative_evidence_id(outcome: &forge_policy::CheckOutcome) -> Option<String> {
    outcome
        .gates
        .iter()
        .find(|gate| {
            gate.verdict == forge_policy::GateVerdict::Failed && gate.evidence_id.is_some()
        })
        .or_else(|| outcome.gates.iter().find(|gate| gate.evidence_id.is_some()))
        .and_then(|gate| gate.evidence_id.clone())
}

/// Load an intent's declared check spec (NER-135). A NULL `check_spec_json`
/// (un-declared intent, or any intent created before Phase 4) yields an empty
/// spec — the policy engine's default mode.
fn intent_check_spec(conn: &Connection, intent_id: &str) -> Result<forge_policy::CheckSpec> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT check_spec_json FROM intents WHERE id = ?1",
            params![intent_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    // Fail CLOSED, not open: a NULL column is a legitimately un-gated intent
    // (default mode), but a non-NULL value that won't parse is corruption (manual
    // edit, partial write, or a future spec shape this binary can't read). Silently
    // collapsing it to an empty spec would downgrade a declared multi-gate intent to
    // the permissive default mode, letting a gated `accept` slip through (NER-135
    // code-review F1). The schema_version gate does not catch this — migration 003
    // only adds the column, not a value-shape version — so the parse must error.
    match stored {
        None => Ok(forge_policy::CheckSpec::default()),
        Some(json) => serde_json::from_str(&json)
            .with_context(|| format!("corrupt check_spec_json for intent {intent_id}")),
    }
}

/// Project every evidence row for an attempt into [`forge_policy::EvidenceFact`]s,
/// newest-first (matching the `created_at_ms DESC, rowid DESC` "latest" tiebreak).
/// Pass a writer's `&tx` to keep the read inside its IMMEDIATE txn (NER-132 U2).
fn evidence_facts_on(
    conn: &Connection,
    attempt_id: &str,
) -> Result<Vec<forge_policy::EvidenceFact>> {
    let mut statement = conn.prepare(
        "SELECT id, command, args_json, exit_code, snapshot_id, created_at_ms, rowid, structured_json FROM evidence
         WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC",
    )?;
    let rows = statement.query_map(params![attempt_id], |row| {
        let args_json: String = row.get(2)?;
        let structured_json: Option<String> = row.get(7)?;
        // Project the parsed test-failure count for a structured gate (NER-136 §U6).
        let structured_failures = structured_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .and_then(|value| value.get("failed").and_then(serde_json::Value::as_u64));
        Ok(forge_policy::EvidenceFact {
            evidence_id: row.get(0)?,
            program: row.get(1)?,
            args: serde_json::from_str(&args_json).unwrap_or_default(),
            exit_code: row.get(3)?,
            snapshot_id: row.get(4)?,
            created_at_ms: row.get(5)?,
            seq: row.get(6)?,
            structured_failures,
        })
    })?;
    let mut facts = Vec::new();
    for fact in rows {
        facts.push(fact?);
    }
    Ok(facts)
}

/// Evaluate the declarative check for a proposal against the attempt's full
/// evidence set, on a caller-supplied connection. The single source of truth for
/// the verdict (NER-135 R4): `record_check` and the in-txn `accept` gate both call
/// this on their own `&tx`, so the determining read stays inside the writer's
/// transaction (NER-132 U2).
fn evaluate_check_on(
    conn: &Connection,
    attempt: &AttemptRecord,
    proposal: &ProposalSummary,
) -> Result<forge_policy::CheckOutcome> {
    let spec = intent_check_spec(conn, &attempt.intent_id)?;
    let facts = evidence_facts_on(conn, &attempt.attempt_id)?;
    let outcome = forge_policy::evaluate(&spec, &proposal.snapshot_id, &facts);
    // Integrity gate (NER-136 R4), fail-CLOSED: if any evidence row that DECIDES a
    // gate was tampered with (its stored content_hash no longer matches a recompute,
    // or a post-watermark hash is missing), refuse. Runs on `&tx` at BOTH
    // `record_check` and `decide`, and — being raised here, before the enforce_check
    // branch — it refuses even under `accept --allow-unverified` (a policy bypass is
    // never an integrity bypass). The deeper full-chain re-walk lives in `doctor`.
    let marker = evidence_high_water(conn)?;
    for gate in &outcome.gates {
        if let Some(evidence_id) = &gate.evidence_id {
            if let IntegrityStatus::Tampered(kind) =
                verify_evidence_integrity(conn, evidence_id, marker)?
            {
                return Err(ForgeError::EvidenceTampered {
                    id: evidence_id.clone(),
                    kind,
                }
                .into());
            }
        }
    }
    Ok(outcome)
}

/// The verdict of re-verifying a hashed row against its stored `content_hash`.
enum IntegrityStatus {
    /// Recomputed hash matches the stored one.
    Verified,
    /// A NULL hash on a pre-watermark row — predates Phase 5, grandfathered.
    LegacyUnverified,
    /// The row was tampered with.
    Tampered(TamperKind),
}

/// The recorded `evidence` rowid high-water mark from migration 004 — the boundary
/// that distinguishes a legacy NULL hash (rowid ≤ mark) from a deleted one (rowid >
/// mark). Keyed on the immutable rowid, never a per-row timestamp the attacker can
/// backdate (NER-136). A fresh post-Phase-5 repo records 0, so any NULL hash is a
/// deletion.
fn evidence_high_water(conn: &Connection) -> Result<i64> {
    let mark: Option<i64> = conn
        .query_row(
            "SELECT evidence_high_water FROM integrity_marker WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(mark.unwrap_or(0))
}

/// Recompute an evidence row's content hash from its stored columns and compare to
/// the persisted `content_hash` (NER-136). Reads the full row on the caller's `&tx`
/// so the determining read stays inside the writer's transaction. This is the cheap
/// per-row gate-path check; it catches the naive "edit a field, leave the hash stale"
/// tamper (the literal Phase 4 honesty-note hole). Catching a *recomputed* row hash
/// requires the op-log re-walk that `doctor` performs.
fn verify_evidence_integrity(
    conn: &Connection,
    evidence_id: &str,
    marker: i64,
) -> Result<IntegrityStatus> {
    let row = conn
        .query_row(
            "SELECT attempt_id, snapshot_id, command, args_json, cwd, exit_code,
                    started_at_ms, ended_at_ms, timed_out, stdout_excerpt, stderr_excerpt,
                    stdout_truncated, stderr_truncated, sensitivity, actor, structured_json,
                    created_at_ms, content_hash, rowid
             FROM evidence WHERE id = ?1",
            params![evidence_id],
            |row| {
                Ok(StoredEvidence {
                    attempt_id: row.get(0)?,
                    snapshot_id: row.get(1)?,
                    command: row.get(2)?,
                    args_json: row.get(3)?,
                    cwd: row.get(4)?,
                    exit_code: row.get(5)?,
                    started_at_ms: row.get(6)?,
                    ended_at_ms: row.get(7)?,
                    timed_out: row.get::<_, i64>(8)? != 0,
                    stdout_excerpt: row.get(9)?,
                    stderr_excerpt: row.get(10)?,
                    stdout_truncated: row.get::<_, i64>(11)? != 0,
                    stderr_truncated: row.get::<_, i64>(12)? != 0,
                    sensitivity: row.get(13)?,
                    actor: row.get(14)?,
                    structured_json: row.get(15)?,
                    created_at_ms: row.get(16)?,
                    content_hash: row.get(17)?,
                    rowid: row.get(18)?,
                })
            },
        )
        .optional()?;
    // No row to verify (e.g. a default-mode gate with no deciding evidence) is not a
    // tamper signal.
    let Some(row) = row else {
        return Ok(IntegrityStatus::Verified);
    };
    let Some(stored_hash) = row.content_hash else {
        return Ok(if row.rowid <= marker {
            IntegrityStatus::LegacyUnverified
        } else {
            IntegrityStatus::Tampered(TamperKind::MissingHash)
        });
    };
    let args: Vec<String> = serde_json::from_str(&row.args_json).unwrap_or_default();
    let recomputed = integrity::evidence_digest(&integrity::EvidenceDigestInput {
        attempt_id: &row.attempt_id,
        snapshot_id: row.snapshot_id.as_deref(),
        command: &row.command,
        args: &args,
        cwd: &row.cwd,
        exit_code: row.exit_code,
        started_at_ms: row.started_at_ms,
        ended_at_ms: row.ended_at_ms,
        timed_out: row.timed_out,
        stdout_excerpt: &row.stdout_excerpt,
        stderr_excerpt: &row.stderr_excerpt,
        stdout_truncated: row.stdout_truncated,
        stderr_truncated: row.stderr_truncated,
        sensitivity: &row.sensitivity,
        actor: &row.actor,
        structured_json: row.structured_json.as_deref(),
        created_at_ms: row.created_at_ms,
    });
    Ok(if recomputed == stored_hash {
        IntegrityStatus::Verified
    } else {
        IntegrityStatus::Tampered(TamperKind::ContentEdit)
    })
}

/// The full evidence row read back for integrity verification.
struct StoredEvidence {
    attempt_id: String,
    snapshot_id: Option<String>,
    command: String,
    args_json: String,
    cwd: String,
    exit_code: i64,
    started_at_ms: i64,
    ended_at_ms: i64,
    timed_out: bool,
    stdout_excerpt: String,
    stderr_excerpt: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    sensitivity: String,
    actor: String,
    structured_json: Option<String>,
    created_at_ms: i64,
    content_hash: Option<String>,
    rowid: i64,
}

pub fn decide(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
    decision: &str,
    enforce_check: bool,
    actor: &str,
) -> Result<DecisionRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let proposal = resolve_proposal(&context, &attempt.attempt_id, proposal_id, true)?.proposal;
    let mut connection = open_connection(&context.database_path)?;

    // NER-138 slice 3: a native repo is detected by routing `base_head` through the canonical
    // ObjectId parser (a git repo's `base_head` is a 40-hex git SHA, which does not parse as
    // an `f1:` commit id). When native + accepted, a justified Commit is written and the
    // ref-store HEAD advanced; git repos and rejects leave `commit_id` NULL and never touch
    // the ref store.
    let native_base = forge_content_native::ObjectId::parse(&proposal.base_head)
        .ok()
        .filter(|id| matches!(id.kind(), Ok(forge_content_native::ObjectKind::Commit)));
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);

    let (decision_id, check_status, op, commit_id) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        // Evidence gate (NER-135 R6): for `accept`, re-evaluate the declarative
        // check IN this IMMEDIATE txn (not on a separate connection), so the verdict
        // that gates the decision is computed from the same facts the decision
        // commits against — closing the TOCTOU against the lock-free `run` writer
        // (NER-132 U2). `evaluate_check_on` ALSO fails closed on a tampered deciding
        // row (NER-136), and — being raised before the enforce_check branch — that
        // refusal holds even under `--allow-unverified`.
        let (check_status, deciding_evidence_id) = if decision == "accepted" {
            let outcome = evaluate_check_on(tx, &attempt, &proposal)?;
            if enforce_check && !outcome.passed() {
                return Err(ForgeError::CheckNotPassed {
                    status: outcome.status.clone(),
                    unmet: outcome.unmet_identities(),
                }
                .into());
            }
            // The deciding gate's evidence id (failed gate first, else any with evidence)
            // — its content_hash becomes the commit's opaque evidence_digest below.
            let deciding_evidence_id = representative_evidence_id(&outcome);
            (Some(outcome.status), deciding_evidence_id)
        } else {
            (None, None)
        };
        let decision_id = new_id("decision");
        // Recomputed per busy-retry: the decision digest covers proposal/revision +
        // decision + actor + timestamp, and is both stored on the row and folded into
        // the op-log spine so editing a decision row is detectable (NER-136 R3/R4).
        let created = now_ms();
        let content_hash = integrity::decision_digest(&integrity::DecisionDigestInput {
            proposal_id: &proposal.proposal_id,
            proposal_revision_id: &proposal.proposal_revision_id,
            decision,
            actor,
            created_at_ms: created,
        });
        // NER-138 slice 3: for a native accept, build + DURABLY write the justified commit
        // BEFORE the decision row that references it (store-before-DB). A busy-retry re-runs
        // this closure, minting a fresh decision_id -> a fresh commit object; the loser is an
        // unreferenced orphan (gc-collectible). `actor` + `authored_time` (= the decision
        // timestamp) are in the HASHED bytes so Phase 9 signs who/when. `evidence_digest` is
        // the deciding gate's evidence content_hash wrapped in `Hex64` (excerpt text is
        // structurally unrepresentable). HEAD is advanced only AFTER this txn commits.
        let commit_id = match (decision, &native_base) {
            ("accepted", Some(parent)) => {
                let tree = proposal
                    .content_ref
                    .strip_prefix(forge_content::FORGE_TREE_PREFIX)
                    .ok_or_else(|| {
                        anyhow!("native accepted proposal has a non-forge-tree content ref")
                    })?
                    .to_string();
                let evidence_digest = match &deciding_evidence_id {
                    Some(evidence_id) => {
                        let hash: Option<String> = tx
                            .query_row(
                                "SELECT content_hash FROM evidence WHERE id = ?1",
                                params![evidence_id],
                                |row| row.get(0),
                            )
                            .optional()?;
                        match hash {
                            Some(hash) => Some(forge_content_native::Hex64::new(hash)?),
                            None => None,
                        }
                    }
                    None => None,
                };
                let parents = if let Some((ours_head, base_head)) =
                    resolved_merge_parents_for_proposal_on(
                        tx,
                        &context.repo_id,
                        &proposal.proposal_id,
                        Some(&proposal.content_ref),
                    )? {
                    forge_content_native::ObjectId::parse(&ours_head)?;
                    forge_content_native::ObjectId::parse(&base_head)?;
                    if ours_head == base_head {
                        vec![ours_head]
                    } else {
                        vec![ours_head, base_head]
                    }
                } else {
                    vec![parent.to_string()]
                };
                let commit = forge_content_native::CommitObject {
                    schema_version: forge_content_native::COMMIT_SCHEMA_VERSION,
                    tree,
                    parents,
                    intent_id: Some(attempt.intent_id.clone()),
                    proposal_revision_id: Some(proposal.proposal_revision_id.clone()),
                    decision_id: Some(decision_id.clone()),
                    evidence_digest,
                    actor: Some(actor.to_string()),
                    authored_time: Some(created),
                };
                Some(store.write_commit(&commit)?.to_string())
            }
            _ => None,
        };
        tx.execute(
            "INSERT INTO decisions (id, repo_id, proposal_id, proposal_revision_id, decision, actor, content_hash, created_at_ms, commit_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                decision_id,
                context.repo_id,
                proposal.proposal_id,
                proposal.proposal_revision_id,
                decision,
                actor,
                content_hash,
                created,
                commit_id
            ],
        )?;
        tx.execute(
            "UPDATE proposals SET status = ?1 WHERE id = ?2",
            params![decision, proposal.proposal_id],
        )?;
        let op = insert_operation_view_chained(
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
            Some(&content_hash),
        )?;
        Ok((decision_id, check_status, op, commit_id))
    })?;

    // Advance the ref-store HEAD AFTER the decision row durably committed (store-before-DB
    // + HEAD-lags-never-leads): a crash here leaves HEAD one commit behind the ledger, which
    // `reconcile_native_head` heals on the next command — never HEAD ahead of an
    // uncommitted decision. `commit_id` is the COMMITTED attempt's value returned from the
    // closure (not a closure-captured outer var), so a busy-retry advances HEAD to the winner.
    if let Some(commit_id) = &commit_id {
        let refs = forge_content_native::NativeRefStore::new(&context.root_path);
        refs.set_head(&forge_content_native::ObjectId::parse(commit_id)?)?;
    }

    Ok(DecisionRecord {
        decision_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        decision: decision.to_string(),
        check_status,
        commit_id,
        operation_id: op.operation_id,
    })
}

/// Heal a torn commit-on-accept (NER-138 Phase 7 slice 3): a crash after the decision row
/// committed but before the ref-store HEAD advanced leaves HEAD one or more commits behind
/// the ledger. The SQLite ledger is authoritative and HEAD is a reconcilable cache, so this
/// advances HEAD to the latest accepted `decisions.commit_id` when it lags. Runs at the
/// command boundary under the held advisory lock (serialized, cannot race), BEFORE the base
/// anchor is read and before the preflight-replay short-circuit (so a same-`request_id`
/// replay of a torn accept still heals HEAD). A no-op on git repos (no native `commit_id`,
/// no ref store) and before any justified commit. HEAD only ever moves FORWARD: the ledger
/// tip must descend from the current HEAD (else the store is corrupt → `NativeHistoryCorrupt`),
/// and a missing tip/parent object (the store-before-DB violation) is surfaced typed.
pub fn reconcile_native_head(cwd: &Path) -> Result<()> {
    let context = match open_repository(cwd) {
        Ok(context) => context,
        // Not a forge repo / not initialized: nothing to reconcile. The command's own
        // `open_repository` surfaces the real NOT_INITIALIZED — reconcile is best-effort.
        Err(_) => return Ok(()),
    };
    let refs = forge_content_native::NativeRefStore::new(&context.root_path);
    let current_head = refs.read_head()?;
    let connection = open_connection(&context.database_path)?;
    let Some(tip) = native_tip(&context, &connection)? else {
        return Ok(()); // genesis-only / git repo — nothing to reconcile
    };
    if current_head.as_ref() == Some(&tip) {
        return Ok(()); // HEAD already current
    }
    // Walk the tip's ancestry: every object must exist (a miss is the store-before-DB
    // violation), there is no cycle, and the current HEAD must be an ancestor of the tip
    // (HEAD lags, never forks). Merge history may reach the same shared ancestor more
    // than once; repeated visited commits are normal diamond ancestry, not corruption.
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let commits = walk_native_commits(&store, &tip)?;
    let head_reached = current_head
        .as_ref()
        .map(|head| commits.iter().any(|(cid, _)| cid == head))
        .unwrap_or(true);
    if !head_reached {
        // The ledger tip does not descend from the current HEAD — a fork, which lock-
        // serialized accepts cannot produce: the store is corrupt.
        return Err(ForgeError::NativeHistoryCorrupt {
            kind: NativeHistoryCorruptKind::DanglingParent,
            commit_id: tip.to_string(),
            related_id: current_head.map(|head| head.to_string()),
        }
        .into());
    }
    refs.set_head(&tip)?;
    Ok(())
}

/// The authoritative native history tip: the latest accepted `decisions.commit_id` (by
/// op-log chain order, `rowid DESC` tiebreak) if any justified commit exists, else the
/// ref-store HEAD (the genesis), else `None`. Shared by `reconcile_native_head` (the advance
/// target) and `native_log` (the walk origin) so the two can never disagree on the tip —
/// `log` is read-only and never writes HEAD, so it tolerates a not-yet-reconciled HEAD by
/// resolving the tip from the ledger directly.
fn native_tip(
    context: &RepositoryContext,
    connection: &Connection,
) -> Result<Option<forge_content_native::ObjectId>> {
    let latest: Option<String> = connection
        .query_row(
            "SELECT commit_id FROM decisions \
             WHERE repo_id = ?1 AND commit_id IS NOT NULL \
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id],
            |row| row.get(0),
        )
        .optional()?;
    match latest {
        Some(commit) => Ok(Some(forge_content_native::ObjectId::parse(&commit)?)),
        None => forge_content_native::NativeRefStore::new(&context.root_path).read_head(),
    }
}

/// One commit in the native history, as surfaced by `forge log` through the JSON contract
/// ("show every change under this intent and the evidence that justified it"). Optional
/// justification fields are omitted when absent (genesis), matching the on-object shape.
#[derive(Debug, Clone, Serialize)]
pub struct CommitView {
    pub commit_id: String,
    pub tree: String,
    pub parents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_revision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authored_time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_digest: Option<String>,
}

/// Walk the native commit DAG from the authoritative tip (NER-138 Phase 7 slice 3),
/// tip→genesis, returning each commit's justification. Read-only (no lock, no HEAD write).
/// When `intent` is `Some`, only commits whose `intent_id` matches are returned — the literal
/// "show every change under this intent" query. A missing tip/parent object surfaces typed
/// `NativeHistoryCorrupt` (DanglingCommitId for the tip, DanglingParent deeper); a parent
/// cycle is `Cycle`.
pub fn native_log(cwd: &Path, intent: Option<&str>) -> Result<Vec<CommitView>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let tip = native_tip(&context, &connection)?;
    let mut out = Vec::new();
    let Some(tip) = tip else {
        return Ok(out);
    };
    for (cid, commit) in walk_native_commits(&store, &tip)? {
        let matches = intent
            .map(|want| commit.intent_id.as_deref() == Some(want))
            .unwrap_or(true);
        if matches {
            out.push(CommitView {
                commit_id: cid.to_string(),
                tree: commit.tree.clone(),
                parents: commit.parents.clone(),
                intent_id: commit.intent_id.clone(),
                proposal_revision_id: commit.proposal_revision_id.clone(),
                decision_id: commit.decision_id.clone(),
                actor: commit.actor.clone(),
                authored_time: commit.authored_time,
                evidence_digest: commit
                    .evidence_digest
                    .as_ref()
                    .map(|h| h.as_str().to_string()),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeVisitState {
    Visiting,
    Visited,
}

fn walk_native_commits(
    store: &forge_content_native::NativeObjectStore,
    tip: &forge_content_native::ObjectId,
) -> Result<
    Vec<(
        forge_content_native::ObjectId,
        forge_content_native::CommitObject,
    )>,
> {
    let mut out = Vec::new();
    let mut states = std::collections::BTreeMap::new();
    let mut stack = vec![(tip.clone(), false)];
    while let Some((cid, expanded)) = stack.pop() {
        if expanded {
            states.insert(cid, NativeVisitState::Visited);
            continue;
        }
        match states.get(&cid).copied() {
            Some(NativeVisitState::Visited) => continue,
            Some(NativeVisitState::Visiting) => {
                return Err(ForgeError::NativeHistoryCorrupt {
                    kind: NativeHistoryCorruptKind::Cycle,
                    commit_id: cid.to_string(),
                    related_id: None,
                }
                .into());
            }
            None => {}
        }
        states.insert(cid.clone(), NativeVisitState::Visiting);
        let commit = store.read_commit(&cid).map_err(|_| {
            let kind = if &cid == tip {
                NativeHistoryCorruptKind::DanglingCommitId
            } else {
                NativeHistoryCorruptKind::DanglingParent
            };
            anyhow::Error::from(ForgeError::NativeHistoryCorrupt {
                kind,
                commit_id: cid.to_string(),
                related_id: None,
            })
        })?;
        out.push((cid.clone(), commit.clone()));
        stack.push((cid, true));
        for parent in commit.parents.iter().rev() {
            stack.push((forge_content_native::ObjectId::parse(parent)?, false));
        }
    }
    Ok(out)
}

/// Verify an accepted proposal's decision row integrity before trusting `accepted`
/// at `export branch` (NER-136 R4). There is no in-txn site on the export path (the
/// git branch is created before `record_publication`'s txn opens), so this is a
/// verifying read under the held repo lock, before the branch. A mismatch means the
/// decision row was tampered with → refuse with `EVIDENCE_TAMPERED`.
pub fn verify_decision_integrity(cwd: &Path, proposal_revision_id: &str) -> Result<()> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let row = connection
        .query_row(
            "SELECT id, proposal_id, decision, actor, content_hash, created_at_ms, rowid
             FROM decisions
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, proposal_revision_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((id, proposal_id, decision, actor, content_hash, created_at_ms, rowid)) = row else {
        return Ok(());
    };
    let Some(stored_hash) = content_hash else {
        // A decision predating Phase 5 (legacy) is grandfathered; a post-watermark
        // NULL is a deletion.
        let marker = decision_high_water(&connection)?;
        if rowid <= marker {
            return Ok(());
        }
        return Err(ForgeError::EvidenceTampered {
            id,
            kind: TamperKind::MissingHash,
        }
        .into());
    };
    let recomputed = integrity::decision_digest(&integrity::DecisionDigestInput {
        proposal_id: &proposal_id,
        proposal_revision_id,
        decision: &decision,
        actor: &actor,
        created_at_ms,
    });
    if recomputed != stored_hash {
        return Err(ForgeError::EvidenceTampered {
            id,
            kind: TamperKind::ContentEdit,
        }
        .into());
    }
    Ok(())
}

/// The recorded `decisions` rowid high-water mark — the legacy/tampered boundary for
/// decision rows (rowid ≤ mark predates Phase 5; rowid > mark with a NULL hash is a
/// deletion).
fn decision_high_water(conn: &Connection) -> Result<i64> {
    let mark: Option<i64> = conn
        .query_row(
            "SELECT decision_high_water FROM integrity_marker WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(mark.unwrap_or(0))
}

/// A published proposal's provenance trailer (NER-137): the values that go into the
/// `Forge-*` commit trailer lines. `provenance_digest` is the content-addressed digest
/// `verify-branch` recomputes from the local ledger.
#[derive(Debug, Clone)]
pub struct PublicationTrailer {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub intent: String,
    pub provenance_digest: String,
    pub actor: String,
    /// Canonical, secret-redacted `"identity=verdict"` strings, sorted.
    pub gates: Vec<String>,
}

/// Assemble the provenance trailer for an accepted proposal revision, **re-verifying
/// the deciding evidence first** (NER-137 R8 — `evaluate_check_on` raises
/// `EVIDENCE_TAMPERED` on a tampered deciding row, so export fails closed before the
/// branch is created). The "content-addressed evidence digest" folds the deciding
/// gates' Phase 5 `content_hash`es, so it recomputes from the ledger by construction.
pub fn build_publication_trailer(
    cwd: &Path,
    proposal_revision_id: &str,
) -> Result<PublicationTrailer> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let (proposal, attempt) =
        proposal_and_attempt_for_revision(&context, &connection, proposal_revision_id)?;

    // R8: integrity-verifying check — a tampered deciding row fails closed here.
    let outcome = evaluate_check_on(&connection, &attempt, &proposal)?;

    let mut evidence_hashes = Vec::new();
    for gate in &outcome.gates {
        if let Some(evidence_id) = &gate.evidence_id {
            evidence_hashes.push(evidence_content_hash_of(&connection, evidence_id)?);
        }
    }
    let (decision_digest, actor) =
        decision_digest_and_actor(&connection, &context.repo_id, proposal_revision_id)?;

    // Canonical, redacted gate outcomes (the commit is a published egress, so gate
    // identities go through the per-token redactor) — sorted for a stable digest.
    let mut gate_outcomes: Vec<String> = outcome
        .gates
        .iter()
        .map(|gate| {
            let redacted = redact_gate_result(gate.clone());
            format!(
                "{}={}",
                forge_policy::identity_string(&redacted.program, &redacted.args),
                verdict_label(redacted.verdict)
            )
        })
        .collect();
    gate_outcomes.sort();

    let provenance_digest = integrity::publication_digest(&integrity::PublicationDigestInput {
        proposal_id: &proposal.proposal_id,
        proposal_revision_id,
        evidence_hashes: &evidence_hashes,
        decision_digest: &decision_digest,
        gate_outcomes: &gate_outcomes,
    });

    Ok(PublicationTrailer {
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal_revision_id.to_string(),
        intent: attempt.intent,
        provenance_digest,
        actor,
        gates: gate_outcomes,
    })
}

/// Render a publication trailer into a git commit message body: a human first line +
/// the intent, then the machine `Forge-*` trailer lines (one `Forge-Provenance-Digest`,
/// no Evidence/Publication split). Parsed back by `parse_forge_trailers` (NER-137 U6).
pub fn render_trailer_message(trailer: &PublicationTrailer) -> String {
    let mut message = format!("Forge accepted proposal {}\n\n", trailer.proposal_id);
    if !trailer.intent.is_empty() {
        message.push_str(&trailer.intent);
        message.push_str("\n\n");
    }
    message.push_str(&format!("Forge-Proposal-Id: {}\n", trailer.proposal_id));
    message.push_str(&format!(
        "Forge-Proposal-Revision-Id: {}\n",
        trailer.proposal_revision_id
    ));
    message.push_str(&format!(
        "Forge-Provenance-Digest: {}\n",
        trailer.provenance_digest
    ));
    message.push_str(&format!("Forge-Decision-Actor: {}\n", trailer.actor));
    message.push_str(&format!("Forge-Gates: {}\n", trailer.gates.join("; ")));
    message
}

fn verdict_label(verdict: forge_policy::GateVerdict) -> &'static str {
    match verdict {
        forge_policy::GateVerdict::Passed => "passed",
        forge_policy::GateVerdict::Failed => "failed",
        forge_policy::GateVerdict::Missing => "missing",
        forge_policy::GateVerdict::Stale => "stale",
    }
}

/// Resolve the `(ProposalSummary, AttemptRecord)` a proposal-revision id names.
fn proposal_and_attempt_for_revision(
    context: &RepositoryContext,
    conn: &Connection,
    proposal_revision_id: &str,
) -> Result<(ProposalSummary, AttemptRecord)> {
    let proposal = conn
        .query_row(
            "SELECT p.id, pr.id, p.attempt_id, p.snapshot_id, p.base_head, pr.content_ref, pr.changed_paths_json
             FROM proposal_revisions pr
             JOIN proposals p ON p.id = pr.proposal_id
             WHERE p.repo_id = ?1 AND pr.id = ?2",
            params![context.repo_id, proposal_revision_id],
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
        .optional()?
        .ok_or_else(|| ForgeError::UnknownProposal {
            selector: proposal_revision_id.to_string(),
        })?;
    let attempt = attempt_by_id(context, &proposal.attempt_id)?.ok_or_else(|| {
        ForgeError::UnknownAttempt {
            selector: proposal.attempt_id.clone(),
        }
    })?;
    Ok((proposal, attempt))
}

/// The stored `content_hash` of an evidence row (empty string when NULL — a legacy
/// pre-Phase-5 row; the digest still computes deterministically).
fn evidence_content_hash_of(conn: &Connection, evidence_id: &str) -> Result<String> {
    let hash: Option<String> = conn
        .query_row(
            "SELECT content_hash FROM evidence WHERE id = ?1",
            params![evidence_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(hash.unwrap_or_default())
}

/// The latest decision row's `(content_hash, actor)` for a revision — the decision
/// digest folded into the provenance digest, and the deciding actor for the trailer.
///
/// **Fail-closed:** errors `NotAccepted` when the revision has no decision row, or its
/// latest decision is not `accepted` (NER-137 code-review). `export branch` already
/// gates on `accepted` before assembling the trailer, but `verify-branch` calls
/// `build_publication_trailer` independently — without this guard a manufactured commit
/// referencing a never-accepted revision would produce a self-consistent (empty-decision)
/// digest that `verify-branch` would confirm as `verified`.
fn decision_digest_and_actor(
    conn: &Connection,
    repo_id: &str,
    proposal_revision_id: &str,
) -> Result<(String, String)> {
    let row: Option<(Option<String>, String, String)> = conn
        .query_row(
            "SELECT content_hash, actor, decision FROM decisions
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![repo_id, proposal_revision_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((hash, actor, decision)) = row else {
        return Err(ForgeError::NotAccepted.into());
    };
    if decision != "accepted" {
        return Err(ForgeError::NotAccepted.into());
    }
    Ok((hash.unwrap_or_default(), actor))
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

/// The native `Commit` id recorded when a proposal revision was accepted (NER-138 Phase 7
/// slice 3), or `None` for a git-backend repo / a pre-006 accept (NULL `commit_id`) / an
/// unaccepted revision. After commit-on-accept the ref-store HEAD advances to this id, so it
/// is the *expected* current head for the accepted proposal — `export branch` compares the
/// live head against it (not the proposal's `base_head`, which the accept progressed past).
pub fn accepted_commit_id_for_revision(
    cwd: &Path,
    proposal_revision_id: &str,
) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let commit_id: Option<Option<String>> = connection
        .query_row(
            "SELECT commit_id FROM decisions
             WHERE repo_id = ?1 AND proposal_revision_id = ?2 AND decision = 'accepted'
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, proposal_revision_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(commit_id.flatten())
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
    actor: &str,
) -> Result<PublicationRecord> {
    let context = open_repository(cwd)?;
    let proposal = proposal_by_id(&context, proposal_id)?.ok_or(ForgeError::NoProposal)?;
    let mut connection = open_connection(&context.database_path)?;
    // Note: the git branch side-effect happens in the CLI before this call, so
    // the replay guard provides DB-row idempotency only; making the export
    // side-effect itself replay-safe is Phase 1b (PENDING-before-side-effect).
    let (publication_id, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let publication_id = new_id("publication");
        tx.execute(
            "INSERT INTO publications (
                id, repo_id, proposal_id, proposal_revision_id, branch_name, commit_id, actor, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                publication_id,
                context.repo_id,
                proposal.proposal_id,
                proposal.proposal_revision_id,
                branch_name,
                commit_id,
                actor,
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

/// Persist a single `conflict_sets` row recording a stale-base divergence, then
/// return the new conflict-set id (NER-133 U7). This is a pure metadata insert —
/// no merge/diff engine — written before the CLI raises [`ForgeError::StaleBase`]
/// so the divergence survives the bail.
///
/// `context` is `"stale_base_accept"` or `"stale_base_export"`. `paths_json`
/// carries `{expected_head, actual_head, paths, redacted_count}`; any secret-risk
/// path in `paths` is dropped via [`forge_content::filter_secret_risk`] before
/// serialization, so a secret-risk filename never reaches the stored JSON — only
/// its count appears.
///
/// The caller already holds the per-command advisory lock (`accept`/`export
/// branch` are mutating), so this does NOT acquire the lock; it is just a single
/// `IMMEDIATE` DB transaction with no lock nesting.
pub fn record_conflict_set(
    cwd: &Path,
    context: &str,
    expected_head: &str,
    actual_head: &str,
    paths: &[String],
) -> Result<String> {
    let repo = open_repository(cwd)?;
    let (kept, dropped) = forge_content::filter_secret_risk(paths);
    let paths_json = json!({
        "expected_head": expected_head,
        "actual_head": actual_head,
        "paths": kept,
        "redacted_count": dropped.len(),
    })
    .to_string();
    let conflict_set_id = new_id("conflict");
    let mut connection = open_connection(&repo.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        tx.execute(
            "INSERT INTO conflict_sets (id, repo_id, context, paths_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![conflict_set_id, repo.repo_id, context, paths_json, now_ms()],
        )?;
        Ok(())
    })?;
    Ok(conflict_set_id)
}

pub fn record_failed_operation_with_conflict(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    code: &str,
    message: &str,
    details: Value,
    conflict: &StaleBaseConflictInput,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let conflict_set_id = new_id("conflict");
    let now = now_ms();
    with_immediate_retry(&mut connection, |tx| {
        let prepared_conflict =
            prepare_stale_base_conflict(&context, &operation_id, &conflict_set_id, now, conflict)?;
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command,
                kind: "recoverable_failure",
                created_at_ms: now,
            },
            Some(&prepared_conflict.content_hash),
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'failed', 'recoverable_failure', ?5, ?6, ?7, ?8, ?9)",
            params![
                operation_id,
                context.repo_id,
                request_id.clone(),
                command,
                context.current_operation_id,
                view_id,
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
                    "message": message,
                    "conflict_set_id": conflict_set_id,
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
        insert_prepared_conflict(tx, &context, &operation_id, &prepared_conflict)?;
        Ok(())
    })?;
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

pub fn record_merge_conflict(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    input: &MergeConflictInput,
) -> Result<MergeConflictRecord> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let conflict_set_id = new_id("conflict");
    let now = now_ms();
    with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let prepared_conflict =
            prepare_merge_conflict(&context, &operation_id, &conflict_set_id, now, input)?;
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command,
                kind: "merge_conflict",
                created_at_ms: now,
            },
            Some(&prepared_conflict.content_hash),
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'success', 'merge_conflict', ?5, ?6, NULL, ?7, ?8)",
            params![
                operation_id,
                context.repo_id,
                request_id,
                command,
                context.current_operation_id,
                view_id,
                content_hash,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'merge_conflict', ?4, ?5)",
            params![
                view_id,
                context.repo_id,
                operation_id,
                json!({
                    "lifecycle": "merge_conflict",
                    "conflict_set_id": conflict_set_id,
                    "proposal_id": input.proposal_id.clone(),
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
        insert_prepared_conflict(tx, &context, &operation_id, &prepared_conflict)?;
        Ok(())
    })?;
    Ok(MergeConflictRecord {
        conflict_set_id,
        operation_id,
        view_id,
    })
}

pub fn record_merge_success(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    proposal: &ProposalSummary,
    input: &MergeSuccessInput,
) -> Result<MergeSuccessRecord> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let mut out: Option<MergeSuccessRecord> = None;
    with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let parent_snapshot_id =
            latest_snapshot_on(tx, &proposal.attempt_id)?.map(|snapshot| snapshot.snapshot_id);
        let snapshot_id = new_id("snapshot");
        let revision_id = new_id("revision");
        let changed_paths_json = serde_json::to_string(&proposal.changed_paths)?;
        tx.execute(
            "INSERT INTO snapshots (
                id, repo_id, attempt_id, parent_snapshot_id, content_ref, changed_paths_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                snapshot_id,
                context.repo_id,
                proposal.attempt_id,
                parent_snapshot_id,
                input.merged_content_ref,
                changed_paths_json,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO proposal_revisions (id, proposal_id, snapshot_id, content_ref, changed_paths_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision_id,
                proposal.proposal_id,
                snapshot_id,
                input.merged_content_ref,
                changed_paths_json,
                now
            ],
        )?;
        tx.execute(
            "UPDATE proposals SET snapshot_id = ?1, content_ref = ?2, status = 'draft' WHERE id = ?3",
            params![snapshot_id, input.merged_content_ref, proposal.proposal_id],
        )?;
        set_expected_content_ref(tx, &input.merged_content_ref)?;
        let merge_lineage_hash =
            integrity::merge_lineage_digest(&integrity::MergeLineageDigestInput {
                proposal_id: &proposal.proposal_id,
                proposal_revision_id: &revision_id,
                snapshot_id: &snapshot_id,
                base_head: &input.base_head,
                ours_head: &input.ours_head,
                base_content_ref: &input.base_content_ref,
                ours_content_ref: &input.ours_content_ref,
                theirs_content_ref: &input.theirs_content_ref,
                merged_content_ref: &input.merged_content_ref,
            });
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command,
                kind: "merge_clean",
                created_at_ms: now,
            },
            Some(&merge_lineage_hash),
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'success', 'merge_clean', ?5, ?6, NULL, ?7, ?8)",
            params![
                operation_id,
                context.repo_id,
                request_id.clone(),
                command,
                context.current_operation_id,
                view_id,
                content_hash,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'merge_clean', ?4, ?5)",
            params![
                view_id,
                context.repo_id,
                operation_id,
                json!({
                    "lifecycle": "merge_clean",
                    "proposal_id": proposal.proposal_id.clone(),
                    "proposal_revision_id": revision_id,
                    "snapshot_id": snapshot_id,
                    "base_head": input.base_head.clone(),
                    "ours_head": input.ours_head.clone(),
                    "base_content_ref": input.base_content_ref.clone(),
                    "ours_content_ref": input.ours_content_ref.clone(),
                    "theirs_content_ref": input.theirs_content_ref.clone(),
                    "merged_content_ref": input.merged_content_ref.clone(),
                    "merge_lineage_hash": merge_lineage_hash,
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
        out = Some(MergeSuccessRecord {
            proposal_id: proposal.proposal_id.clone(),
            proposal_revision_id: revision_id,
            snapshot_id,
            base_content_ref: input.base_content_ref.clone(),
            ours_content_ref: input.ours_content_ref.clone(),
            theirs_content_ref: input.theirs_content_ref.clone(),
            merged_content_ref: input.merged_content_ref.clone(),
            operation_id: operation_id.clone(),
            view_id: view_id.clone(),
        });
        Ok(())
    })?;
    out.ok_or_else(|| anyhow!("merge did not produce a record"))
}

pub fn resolve_conflict_with_tree(
    cwd: &Path,
    request_id: Option<String>,
    conflict_set_id: &str,
    resolution_ref: &str,
) -> Result<ConflictResolutionRecord> {
    let context = open_repository(cwd)?;
    if resolution_ref.starts_with(forge_content::FORGE_TREE_PREFIX) {
        forge_content_native::NativeObjectStore::new(&context.root_path)
            .verify_content_ref(resolution_ref)?;
    } else {
        return Err(anyhow!(
            "conflict resolution requires a forge-tree content ref"
        ));
    }
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let mut out: Option<ConflictResolutionRecord> = None;
    with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let (paths_json, status): (String, String) = tx
            .query_row(
                "SELECT paths_json, status FROM conflict_sets WHERE id = ?1 AND repo_id = ?2",
                params![conflict_set_id, context.repo_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| ForgeError::ConflictSetNotFound {
                conflict_set_id: conflict_set_id.to_string(),
            })?;
        if status == "resolved" {
            return Err(anyhow!("conflict set is already resolved"));
        }
        let proposal_id = serde_json::from_str::<Value>(&paths_json)
            .ok()
            .and_then(|value| {
                value
                    .get("proposal_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow!("conflict set has no proposal binding"))?;
        let proposal = proposal_by_id_on(tx, &context, &proposal_id)?.ok_or_else(|| {
            ForgeError::UnknownProposal {
                selector: proposal_id.clone(),
            }
        })?;
        let parent_snapshot_id =
            latest_snapshot_on(tx, &proposal.attempt_id)?.map(|snapshot| snapshot.snapshot_id);
        let snapshot_id = new_id("snapshot");
        let revision_id = new_id("revision");
        let changed_paths_json = serde_json::to_string(&proposal.changed_paths)?;
        tx.execute(
            "INSERT INTO snapshots (
                id, repo_id, attempt_id, parent_snapshot_id, content_ref, changed_paths_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                snapshot_id,
                context.repo_id,
                proposal.attempt_id,
                parent_snapshot_id,
                resolution_ref,
                changed_paths_json,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO proposal_revisions (id, proposal_id, snapshot_id, content_ref, changed_paths_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision_id,
                proposal.proposal_id,
                snapshot_id,
                resolution_ref,
                changed_paths_json,
                now
            ],
        )?;
        let evidence_id = new_id("evidence");
        let evidence_args = vec![
            "conflict".to_string(),
            "resolve".to_string(),
            conflict_set_id.to_string(),
            "--tree".to_string(),
            resolution_ref.to_string(),
        ];
        let actor = "unknown".to_string();
        let cwd = ".".to_string();
        let evidence_hash = integrity::evidence_digest(&integrity::EvidenceDigestInput {
            attempt_id: &proposal.attempt_id,
            snapshot_id: None,
            command: "forge",
            args: &evidence_args,
            cwd: &cwd,
            exit_code: 0,
            started_at_ms: now,
            ended_at_ms: now,
            timed_out: false,
            stdout_excerpt: "",
            stderr_excerpt: "",
            stdout_truncated: false,
            stderr_truncated: false,
            sensitivity: "normal",
            actor: &actor,
            structured_json: None,
            created_at_ms: now,
        });
        tx.execute(
            "INSERT INTO evidence (
                id, repo_id, attempt_id, snapshot_id, command, args_json, cwd, exit_code,
                started_at_ms, ended_at_ms, stdout_excerpt, stderr_excerpt,
                stdout_truncated, stderr_truncated, timed_out, sensitivity, visibility,
                trust, actor, structured_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'forge', ?5, ?6, 0, ?7, ?8, '', '', 0, 0, 0,
                      'normal', 'internal', 'local', ?9, NULL, ?10, ?11)",
            params![
                evidence_id,
                context.repo_id,
                proposal.attempt_id,
                Option::<String>::None,
                serde_json::to_string(&evidence_args)?,
                cwd,
                now,
                now,
                actor,
                evidence_hash,
                now
            ],
        )?;
        tx.execute(
            "UPDATE proposals SET snapshot_id = ?1, content_ref = ?2, status = 'draft' WHERE id = ?3",
            params![snapshot_id, resolution_ref, proposal.proposal_id],
        )?;
        tx.execute(
            "UPDATE path_conflicts SET status = 'resolved', resolution_ref = ?1 WHERE conflict_set_id = ?2",
            params![resolution_ref, conflict_set_id],
        )?;
        tx.execute(
            "UPDATE conflict_sets SET status = 'resolved' WHERE id = ?1",
            params![conflict_set_id],
        )?;
        set_expected_content_ref(tx, resolution_ref)?;
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command: "conflict resolve",
                kind: "conflict_resolved",
                created_at_ms: now,
            },
            Some(&evidence_hash),
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, 'conflict resolve', 'success', 'conflict_resolved', ?4, ?5, NULL, ?6, ?7)",
            params![
                operation_id,
                context.repo_id,
                request_id.clone(),
                context.current_operation_id,
                view_id,
                content_hash,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'conflict_resolved', ?4, ?5)",
            params![
                view_id,
                context.repo_id,
                operation_id,
                json!({
                    "lifecycle": "conflict_resolved",
                    "conflict_set_id": conflict_set_id,
                    "proposal_id": proposal.proposal_id,
                    "proposal_revision_id": revision_id,
                    "snapshot_id": snapshot_id,
                    "evidence_id": evidence_id,
                    "resolution_ref": resolution_ref,
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
        out = Some(ConflictResolutionRecord {
            conflict_set_id: conflict_set_id.to_string(),
            proposal_id: proposal.proposal_id,
            proposal_revision_id: revision_id,
            snapshot_id,
            evidence_id,
            resolution_ref: resolution_ref.to_string(),
            operation_id: operation_id.clone(),
            view_id: view_id.clone(),
        });
        Ok(())
    })?;
    out.ok_or_else(|| anyhow!("conflict resolution did not produce a record"))
}

pub fn preflight_conflict_resolution(
    cwd: &Path,
    conflict_set_id: &str,
    resolution_ref: &str,
) -> Result<()> {
    let context = open_repository(cwd)?;
    if resolution_ref.starts_with(forge_content::FORGE_TREE_PREFIX) {
        forge_content_native::NativeObjectStore::new(&context.root_path)
            .verify_content_ref(resolution_ref)?;
    } else {
        return Err(anyhow!(
            "conflict resolution requires a forge-tree content ref"
        ));
    }
    let connection = open_connection(&context.database_path)?;
    let (paths_json, status): (String, String) = connection
        .query_row(
            "SELECT paths_json, status FROM conflict_sets WHERE id = ?1 AND repo_id = ?2",
            params![conflict_set_id, context.repo_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| ForgeError::ConflictSetNotFound {
            conflict_set_id: conflict_set_id.to_string(),
        })?;
    if status == "resolved" {
        return Err(anyhow!("conflict set is already resolved"));
    }
    let proposal_id = serde_json::from_str::<Value>(&paths_json)
        .ok()
        .and_then(|value| {
            value
                .get("proposal_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| anyhow!("conflict set has no proposal binding"))?;
    proposal_by_id_on(&connection, &context, &proposal_id)?.ok_or({
        ForgeError::UnknownProposal {
            selector: proposal_id,
        }
    })?;
    Ok(())
}

fn prepare_stale_base_conflict(
    context: &RepositoryContext,
    operation_id: &str,
    conflict_set_id: &str,
    now: i64,
    input: &StaleBaseConflictInput,
) -> Result<PreparedConflict> {
    let (kept, dropped) = forge_content::filter_secret_risk(&input.changed_paths);
    let paths_json = json!({
        "expected_head": input.expected_head,
        "actual_head": input.actual_head,
        "paths": kept,
        "redacted_count": dropped.len(),
    })
    .to_string();
    let mut path_rows = Vec::with_capacity(kept.len());
    for path in kept {
        path_rows.push(ConflictPathRow {
            id: new_id("path_conflict"),
            path_fingerprint: integrity::path_fingerprint(&path),
            base_path: Some(path.clone()),
            ours_path: Some(path.clone()),
            theirs_path: Some(path.clone()),
            path,
            kind: "content".to_string(),
            base_ref: Some(input.base_content_ref.clone()),
            ours_ref: Some(input.ours_content_ref.clone()),
            theirs_ref: Some(input.theirs_content_ref.clone()),
            base_status: None,
            ours_status: None,
            theirs_status: None,
            base_mode: None,
            ours_mode: None,
            theirs_mode: None,
            resolution_ref: None,
            status: "unresolved".to_string(),
            created_at_ms: now,
        });
    }
    let digest_rows = path_rows
        .iter()
        .map(|row| row.digest_input())
        .collect::<Vec<_>>();
    let content_hash = integrity::conflict_set_digest(&integrity::ConflictSetDigestInput {
        id: conflict_set_id,
        repo_id: &context.repo_id,
        context: &input.context,
        paths_json: &paths_json,
        base_content_ref: Some(&input.base_content_ref),
        ours_content_ref: Some(&input.ours_content_ref),
        theirs_content_ref: Some(&input.theirs_content_ref),
        generated_by_operation_id: Some(operation_id),
        resolver_backend: Some("stale_base"),
        status: "unresolved",
        created_at_ms: now,
        path_conflicts: &digest_rows,
    });
    Ok(PreparedConflict {
        id: conflict_set_id.to_string(),
        context: input.context.clone(),
        paths_json,
        base_content_ref: input.base_content_ref.clone(),
        ours_content_ref: input.ours_content_ref.clone(),
        theirs_content_ref: input.theirs_content_ref.clone(),
        resolver_backend: "stale_base".to_string(),
        status: "unresolved".to_string(),
        content_hash,
        path_rows,
        created_at_ms: now,
        repo_id: context.repo_id.clone(),
    })
}

fn prepare_merge_conflict(
    context: &RepositoryContext,
    operation_id: &str,
    conflict_set_id: &str,
    now: i64,
    input: &MergeConflictInput,
) -> Result<PreparedConflict> {
    let mut redacted_count = 0usize;
    let mut kept_paths = Vec::new();
    let mut path_rows = Vec::new();
    for conflict in &input.conflicts {
        if forge_content::is_secret_risk_path(&conflict.path) {
            redacted_count += 1;
            continue;
        }
        kept_paths.push(conflict.path.clone());
        path_rows.push(ConflictPathRow {
            id: new_id("path_conflict"),
            path_fingerprint: integrity::path_fingerprint(&conflict.path),
            path: conflict.path.clone(),
            base_path: conflict_path_if_present(&conflict.base_status, &conflict.path),
            ours_path: conflict_path_if_present(&conflict.ours_status, &conflict.path),
            theirs_path: conflict_path_if_present(&conflict.theirs_status, &conflict.path),
            kind: native_conflict_kind(conflict.kind).to_string(),
            base_ref: conflict.base_ref.clone(),
            ours_ref: conflict.ours_ref.clone(),
            theirs_ref: conflict.theirs_ref.clone(),
            base_status: conflict.base_status.clone(),
            ours_status: conflict.ours_status.clone(),
            theirs_status: conflict.theirs_status.clone(),
            base_mode: conflict.base_mode.map(|mode| format!("{mode:o}")),
            ours_mode: conflict.ours_mode.map(|mode| format!("{mode:o}")),
            theirs_mode: conflict.theirs_mode.map(|mode| format!("{mode:o}")),
            resolution_ref: None,
            status: "unresolved".to_string(),
            created_at_ms: now,
        });
    }
    let paths_json = json!({
        "proposal_id": input.proposal_id,
        "base_head": input.base_head,
        "ours_head": input.ours_head,
        "paths": kept_paths,
        "redacted_count": redacted_count,
    })
    .to_string();
    let digest_rows = path_rows
        .iter()
        .map(|row| row.digest_input())
        .collect::<Vec<_>>();
    let content_hash = integrity::conflict_set_digest(&integrity::ConflictSetDigestInput {
        id: conflict_set_id,
        repo_id: &context.repo_id,
        context: &input.context,
        paths_json: &paths_json,
        base_content_ref: Some(&input.base_content_ref),
        ours_content_ref: Some(&input.ours_content_ref),
        theirs_content_ref: Some(&input.theirs_content_ref),
        generated_by_operation_id: Some(operation_id),
        resolver_backend: Some("native_merge"),
        status: "unresolved",
        created_at_ms: now,
        path_conflicts: &digest_rows,
    });
    Ok(PreparedConflict {
        id: conflict_set_id.to_string(),
        context: input.context.clone(),
        paths_json,
        base_content_ref: input.base_content_ref.clone(),
        ours_content_ref: input.ours_content_ref.clone(),
        theirs_content_ref: input.theirs_content_ref.clone(),
        resolver_backend: "native_merge".to_string(),
        status: "unresolved".to_string(),
        content_hash,
        path_rows,
        created_at_ms: now,
        repo_id: context.repo_id.clone(),
    })
}

fn conflict_path_if_present(status: &Option<String>, path: &str) -> Option<String> {
    (status.as_deref() == Some("present")).then(|| path.to_string())
}

fn native_conflict_kind(kind: forge_content_native::NativeMergeConflictKind) -> &'static str {
    match kind {
        forge_content_native::NativeMergeConflictKind::Content => "content",
        forge_content_native::NativeMergeConflictKind::Binary => "binary",
        forge_content_native::NativeMergeConflictKind::DeleteModify => "delete_modify",
        forge_content_native::NativeMergeConflictKind::Rename => "rename",
        forge_content_native::NativeMergeConflictKind::DirFile => "dir_file",
        forge_content_native::NativeMergeConflictKind::Mode => "mode",
        forge_content_native::NativeMergeConflictKind::Symlink => "symlink",
    }
}

fn insert_prepared_conflict(
    tx: &Transaction<'_>,
    _context: &RepositoryContext,
    operation_id: &str,
    prepared: &PreparedConflict,
) -> Result<()> {
    tx.execute(
        "INSERT INTO conflict_sets (
            id, repo_id, context, paths_json, created_at_ms, base_content_ref,
            ours_content_ref, theirs_content_ref, generated_by_operation_id,
            resolver_backend, status, content_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            prepared.id,
            prepared.repo_id,
            prepared.context,
            prepared.paths_json,
            prepared.created_at_ms,
            prepared.base_content_ref,
            prepared.ours_content_ref,
            prepared.theirs_content_ref,
            operation_id,
            prepared.resolver_backend,
            prepared.status,
            prepared.content_hash,
        ],
    )?;
    for row in &prepared.path_rows {
        tx.execute(
            "INSERT INTO path_conflicts (
                id, conflict_set_id, path, path_fingerprint, base_path, ours_path, theirs_path,
                kind, base_ref, ours_ref, theirs_ref, base_status, ours_status, theirs_status,
                base_mode, ours_mode, theirs_mode, resolution_ref, status, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                row.id,
                prepared.id,
                row.path,
                row.path_fingerprint,
                row.base_path,
                row.ours_path,
                row.theirs_path,
                row.kind,
                row.base_ref,
                row.ours_ref,
                row.theirs_ref,
                row.base_status,
                row.ours_status,
                row.theirs_status,
                row.base_mode,
                row.ours_mode,
                row.theirs_mode,
                row.resolution_ref,
                row.status,
                row.created_at_ms,
            ],
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PreparedConflict {
    id: String,
    repo_id: String,
    context: String,
    paths_json: String,
    base_content_ref: String,
    ours_content_ref: String,
    theirs_content_ref: String,
    resolver_backend: String,
    status: String,
    content_hash: String,
    path_rows: Vec<ConflictPathRow>,
    created_at_ms: i64,
}

#[derive(Debug, Clone)]
struct ConflictPathRow {
    id: String,
    path: String,
    path_fingerprint: String,
    base_path: Option<String>,
    ours_path: Option<String>,
    theirs_path: Option<String>,
    kind: String,
    base_ref: Option<String>,
    ours_ref: Option<String>,
    theirs_ref: Option<String>,
    base_status: Option<String>,
    ours_status: Option<String>,
    theirs_status: Option<String>,
    base_mode: Option<String>,
    ours_mode: Option<String>,
    theirs_mode: Option<String>,
    resolution_ref: Option<String>,
    status: String,
    created_at_ms: i64,
}

impl ConflictPathRow {
    fn digest_input(&self) -> integrity::PathConflictDigestInput<'_> {
        integrity::PathConflictDigestInput {
            id: &self.id,
            path: &self.path,
            path_fingerprint: &self.path_fingerprint,
            base_path: self.base_path.as_deref(),
            ours_path: self.ours_path.as_deref(),
            theirs_path: self.theirs_path.as_deref(),
            kind: &self.kind,
            base_ref: self.base_ref.as_deref(),
            ours_ref: self.ours_ref.as_deref(),
            theirs_ref: self.theirs_ref.as_deref(),
            base_status: self.base_status.as_deref(),
            ours_status: self.ours_status.as_deref(),
            theirs_status: self.theirs_status.as_deref(),
            base_mode: self.base_mode.as_deref(),
            ours_mode: self.ours_mode.as_deref(),
            theirs_mode: self.theirs_mode.as_deref(),
            resolution_ref: self.resolution_ref.as_deref(),
            status: &self.status,
            created_at_ms: self.created_at_ms,
        }
    }
}

pub fn show(cwd: &Path, attempt_id: Option<&str>) -> Result<ShowRecord> {
    let context = open_repository(cwd)?;
    let attempt = match resolve_attempt_in_context(&context, attempt_id) {
        Ok(resolved) => Some(resolved.attempt),
        Err(error)
            if matches!(
                error.downcast_ref::<ForgeError>(),
                Some(ForgeError::NoActiveAttempt)
            ) =>
        {
            None
        }
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

pub fn conflict_list(cwd: &Path) -> Result<ConflictListRecord> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    Ok(ConflictListRecord {
        conflicts: query_conflict_summaries(&connection, None)?,
    })
}

pub fn conflict_show(cwd: &Path, conflict_set_id: &str) -> Result<ConflictShowRecord> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let mut conflicts = query_conflict_summaries(&connection, Some(conflict_set_id))?;
    let Some(conflict) = conflicts.pop() else {
        return Err(ForgeError::ConflictSetNotFound {
            conflict_set_id: conflict_set_id.to_string(),
        }
        .into());
    };
    let mut statement = connection.prepare(
        "SELECT id, path_fingerprint, kind, base_ref, ours_ref, theirs_ref,
                base_status, ours_status, theirs_status, base_mode, ours_mode,
                theirs_mode, resolution_ref, status
         FROM path_conflicts
         WHERE conflict_set_id = ?1
         ORDER BY rowid",
    )?;
    let path_conflicts = statement
        .query_map(params![conflict_set_id], |row| {
            Ok(PathConflictSummary {
                path_conflict_id: row.get(0)?,
                path_fingerprint: row.get(1)?,
                kind: row.get(2)?,
                base_ref: row.get(3)?,
                ours_ref: row.get(4)?,
                theirs_ref: row.get(5)?,
                base_status: row.get(6)?,
                ours_status: row.get(7)?,
                theirs_status: row.get(8)?,
                base_mode: row.get(9)?,
                ours_mode: row.get(10)?,
                theirs_mode: row.get(11)?,
                resolution_ref: row.get(12)?,
                status: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ConflictShowRecord {
        conflict,
        path_conflicts,
    })
}

fn query_conflict_summaries(
    connection: &Connection,
    conflict_set_id: Option<&str>,
) -> Result<Vec<ConflictSetSummary>> {
    let sql = if conflict_set_id.is_some() {
        "SELECT cs.id, cs.context, cs.paths_json, cs.base_content_ref, cs.ours_content_ref,
                cs.theirs_content_ref, cs.generated_by_operation_id, cs.resolver_backend,
                cs.status, COUNT(pc.id)
         FROM conflict_sets cs
         LEFT JOIN path_conflicts pc ON pc.conflict_set_id = cs.id
         WHERE cs.id = ?1
         GROUP BY cs.id
         ORDER BY cs.created_at_ms, cs.rowid"
    } else {
        "SELECT cs.id, cs.context, cs.paths_json, cs.base_content_ref, cs.ours_content_ref,
                cs.theirs_content_ref, cs.generated_by_operation_id, cs.resolver_backend,
                cs.status, COUNT(pc.id)
         FROM conflict_sets cs
         LEFT JOIN path_conflicts pc ON pc.conflict_set_id = cs.id
         GROUP BY cs.id
         ORDER BY cs.created_at_ms, cs.rowid"
    };
    let mut statement = connection.prepare(sql)?;
    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ConflictSetSummary> {
        let paths_json: String = row.get(2)?;
        let redacted_count = conflict_redacted_count(&paths_json);
        let warnings = if redacted_count > 0 {
            vec![format!(
                "redacted {redacted_count} secret-risk path(s) from conflict metadata"
            )]
        } else {
            Vec::new()
        };
        Ok(ConflictSetSummary {
            conflict_set_id: row.get(0)?,
            context: row.get(1)?,
            base_content_ref: row.get(3)?,
            ours_content_ref: row.get(4)?,
            theirs_content_ref: row.get(5)?,
            generated_by_operation_id: row.get(6)?,
            resolver_backend: row.get(7)?,
            status: row.get(8)?,
            path_conflict_count: row.get(9)?,
            redacted_count,
            warnings,
        })
    };
    let rows = if let Some(id) = conflict_set_id {
        statement
            .query_map(params![id], map_row)?
            .collect::<rusqlite::Result<_>>()?
    } else {
        statement
            .query_map([], map_row)?
            .collect::<rusqlite::Result<_>>()?
    };
    Ok(rows)
}

fn conflict_redacted_count(paths_json: &str) -> i64 {
    serde_json::from_str::<Value>(paths_json)
        .ok()
        .and_then(|value| value.get("redacted_count").and_then(Value::as_i64))
        .unwrap_or(0)
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
    if schema_version != Some(migrations::schema_head()) {
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
    // A committed `content_ref` whose object is missing or fails verification is
    // the exact failure the store-before-DB ordering (NER-132) makes impossible;
    // surface it as its own category so the exit criterion is machine-checkable.
    let mut dangling_content_refs = Vec::new();
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
                dangling_content_refs.push(format!("missing content ref for {label}"));
            }
        } else if content_ref.starts_with("forge-tree:") {
            match native_store.verify_content_ref(&content_ref) {
                Ok(ids) => reachable_native_objects.extend(ids),
                Err(error) => {
                    dangling_content_refs
                        .push(format!("invalid native content ref for {label}: {error}"));
                }
            }
        }
    }
    issues.extend(dangling_content_refs.iter().cloned());

    // A crash-atomic restore (NER-132 U4) killed mid-flight leaves a
    // `.forge-restore-*` temp in a worktree directory; those live in the worktree,
    // not `.forge/tmp`, so scan the work tree (excluding `.git`/`.forge`) for them.
    let half_applied_worktrees = scan_restore_temps(&context.root_path)?;
    if !half_applied_worktrees.is_empty() {
        issues.push("half-applied worktree (leftover restore temp files)".to_string());
    }

    // Tamper-evidence chain pass (NER-136): re-verify every hashed row offline.
    let tampered_rows = verify_integrity_chain(&connection)?;
    if !tampered_rows.is_empty() {
        issues.push(format!("{} tampered row(s) detected", tampered_rows.len()));
    }

    // Native commit-DAG integrity pass (NER-138 Phase 7 slice 3): walk the DAG from the
    // authoritative tip and cross-check the ledger, REPORTING (not raising) cycles / dangling
    // parents / dangling trees / dangling commit_ids. The raising counterpart lives in
    // reconcile/checkout; doctor is the offline health report.
    let native_history_issues = verify_native_history(&context, &connection, &native_store)?;
    if !native_history_issues.is_empty() {
        issues.push(format!(
            "{} native-history integrity break(s) detected",
            native_history_issues.len()
        ));
    }

    Ok(DoctorReport {
        ok: issues.is_empty(),
        issues,
        schema_version,
        dangling_temp_files,
        dangling_content_refs,
        half_applied_worktrees,
        tampered_rows,
        native_history_issues,
    })
}

/// `doctor`'s native commit-DAG integrity pass (NER-138 Phase 7 slice 3): walk the DAG from
/// the authoritative tip detecting cycles (visited set), dangling parents, and dangling trees,
/// then cross-check every `decisions.commit_id` resolves to an existing commit object. Reports
/// findings (does not raise) — fail-closed at the call sites that raise. Findings are deduped
/// by (kind, commit_id, related_id).
fn verify_native_history(
    context: &RepositoryContext,
    connection: &Connection,
    store: &forge_content_native::NativeObjectStore,
) -> Result<Vec<NativeHistoryFinding>> {
    let mut findings: Vec<NativeHistoryFinding> = Vec::new();
    let mut push = |finding: NativeHistoryFinding| {
        if !findings.iter().any(|existing| {
            existing.kind.as_str() == finding.kind.as_str()
                && existing.commit_id == finding.commit_id
                && existing.related_id == finding.related_id
        }) {
            findings.push(finding);
        }
    };

    if let Some(tip) = native_tip(context, connection)? {
        let mut states = std::collections::BTreeMap::new();
        let mut stack = vec![(tip, false)];
        while let Some((commit_id, expanded)) = stack.pop() {
            if expanded {
                states.insert(commit_id, NativeVisitState::Visited);
                continue;
            }
            match states.get(&commit_id).copied() {
                Some(NativeVisitState::Visited) => continue,
                Some(NativeVisitState::Visiting) => {
                    push(NativeHistoryFinding {
                        kind: NativeHistoryCorruptKind::Cycle,
                        commit_id: commit_id.to_string(),
                        related_id: None,
                    });
                    continue;
                }
                None => {}
            }
            states.insert(commit_id.clone(), NativeVisitState::Visiting);
            let commit = match store.read_commit(&commit_id) {
                Ok(commit) => commit,
                Err(_) => {
                    push(NativeHistoryFinding {
                        kind: NativeHistoryCorruptKind::DanglingCommitId,
                        commit_id: commit_id.to_string(),
                        related_id: None,
                    });
                    continue;
                }
            };
            stack.push((commit_id.clone(), true));
            let tree_ref = format!("{}{}", forge_content::FORGE_TREE_PREFIX, commit.tree);
            if store.verify_content_ref(&tree_ref).is_err() {
                push(NativeHistoryFinding {
                    kind: NativeHistoryCorruptKind::DanglingTree,
                    commit_id: commit_id.to_string(),
                    related_id: Some(commit.tree.clone()),
                });
            }
            for parent in &commit.parents {
                match forge_content_native::ObjectId::parse(parent) {
                    Ok(parent_id) if store.read_commit(&parent_id).is_ok() => {
                        stack.push((parent_id, false));
                    }
                    _ => push(NativeHistoryFinding {
                        kind: NativeHistoryCorruptKind::DanglingParent,
                        commit_id: commit_id.to_string(),
                        related_id: Some(parent.clone()),
                    }),
                }
            }
        }
    }

    // Cross-check every accepted decisions.commit_id resolves to a commit object — catches a
    // dangling commit_id even if it is off the tip's ancestry (the store-before-DB violation).
    let mut statement = connection
        .prepare("SELECT commit_id FROM decisions WHERE repo_id = ?1 AND commit_id IS NOT NULL")?;
    let rows = statement.query_map(params![context.repo_id], |row| row.get::<_, String>(0))?;
    for row in rows {
        let commit_id = row?;
        match forge_content_native::ObjectId::parse(&commit_id) {
            Ok(id) if store.read_commit(&id).is_ok() => {}
            _ => push(NativeHistoryFinding {
                kind: NativeHistoryCorruptKind::DanglingCommitId,
                commit_id,
                related_id: None,
            }),
        }
    }

    Ok(findings)
}

/// Re-verify the full tamper-evident chain offline (NER-136 §U8): every evidence and
/// decision row's own content hash, plus every operation's chain link (which folds
/// the domain digest, so a *recomputed* row hash that slipped past the cheap gate
/// check is caught here at the operation that chained the old digest). Reads a
/// consistent ordered snapshot; a head-truncated chain (a lost latest op) is reported
/// as clean, NOT a tamper, because there is no expected-count check.
fn verify_integrity_chain(conn: &Connection) -> Result<Vec<TamperedRow>> {
    let mut tampered = Vec::new();
    let evidence_marker = evidence_high_water(conn)?;
    let op_marker = op_high_water(conn)?;
    let decision_marker = decision_high_water(conn)?;

    // (a) Every evidence row's own digest.
    let mut evidence_ids = conn.prepare("SELECT id FROM evidence ORDER BY rowid")?;
    let ids: Vec<String> = evidence_ids
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in ids {
        if let IntegrityStatus::Tampered(kind) =
            verify_evidence_integrity(conn, &id, evidence_marker)?
        {
            tampered.push(TamperedRow {
                id,
                table: "evidence".to_string(),
                kind,
            });
        }
    }

    // (b) Every decision row's own digest.
    let mut decision_rows = conn.prepare(
        "SELECT id, proposal_id, proposal_revision_id, decision, actor, content_hash, created_at_ms, rowid
         FROM decisions ORDER BY rowid",
    )?;
    let decisions: Vec<StoredDecision> = decision_rows
        .query_map([], |row| {
            Ok(StoredDecision {
                id: row.get(0)?,
                proposal_id: row.get(1)?,
                proposal_revision_id: row.get(2)?,
                decision: row.get(3)?,
                actor: row.get(4)?,
                content_hash: row.get(5)?,
                created_at_ms: row.get(6)?,
                rowid: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for row in decisions {
        match row.content_hash {
            None if row.rowid > decision_marker => tampered.push(TamperedRow {
                id: row.id,
                table: "decisions".to_string(),
                kind: TamperKind::MissingHash,
            }),
            None => {}
            Some(stored) => {
                let recomputed = integrity::decision_digest(&integrity::DecisionDigestInput {
                    proposal_id: &row.proposal_id,
                    proposal_revision_id: &row.proposal_revision_id,
                    decision: &row.decision,
                    actor: &row.actor,
                    created_at_ms: row.created_at_ms,
                });
                if recomputed != stored {
                    tampered.push(TamperedRow {
                        id: row.id,
                        table: "decisions".to_string(),
                        kind: TamperKind::ContentEdit,
                    });
                }
            }
        }
    }

    // (c) Every operation-owned conflict set's own digest.
    let mut conflict_rows = conn.prepare(
        "SELECT id FROM conflict_sets
         WHERE generated_by_operation_id IS NOT NULL
         ORDER BY rowid",
    )?;
    let conflict_ids: Vec<String> = conflict_rows
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in conflict_ids {
        match recompute_conflict_set_hash(conn, &id)? {
            None => tampered.push(TamperedRow {
                id,
                table: "conflict_sets".to_string(),
                kind: TamperKind::MissingHash,
            }),
            Some((stored, recomputed)) if stored != recomputed => tampered.push(TamperedRow {
                id,
                table: "conflict_sets".to_string(),
                kind: TamperKind::ContentEdit,
            }),
            Some(_) => {}
        }
    }

    // (d) Every operation's chain link (folding its domain digest), in chain order.
    let mut op_rows = conn.prepare(
        "SELECT id, parent_operation_id, command, kind, resulting_view_id, content_hash, created_at_ms, rowid
         FROM operations ORDER BY created_at_ms, rowid",
    )?;
    let ops: Vec<StoredOp> = op_rows
        .query_map([], |row| {
            Ok(StoredOp {
                id: row.get(0)?,
                parent_operation_id: row.get(1)?,
                command: row.get(2)?,
                kind: row.get(3)?,
                resulting_view_id: row.get(4)?,
                content_hash: row.get(5)?,
                created_at_ms: row.get(6)?,
                rowid: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for row in ops {
        let Some(stored) = row.content_hash else {
            if row.rowid > op_marker {
                tampered.push(TamperedRow {
                    id: row.id,
                    table: "operations".to_string(),
                    kind: TamperKind::MissingHash,
                });
            }
            continue;
        };
        let parent_hash = op_content_hash(conn, row.parent_operation_id.as_deref())?;
        let domain_digest = op_domain_digest(conn, row.resulting_view_id.as_deref())?;
        let recomputed = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &row.id,
                command: &row.command,
                kind: &row.kind,
                created_at_ms: row.created_at_ms,
            },
            domain_digest.as_deref(),
        );
        if recomputed != stored {
            tampered.push(TamperedRow {
                id: row.id,
                table: "operations".to_string(),
                kind: TamperKind::BrokenLink,
            });
        }
    }

    Ok(tampered)
}

fn recompute_conflict_set_hash(
    conn: &Connection,
    conflict_set_id: &str,
) -> Result<Option<(String, String)>> {
    let conflict: Option<StoredConflictSet> = conn
        .query_row(
            "SELECT id, repo_id, context, paths_json, created_at_ms, base_content_ref,
                    ours_content_ref, theirs_content_ref, generated_by_operation_id,
                    resolver_backend, content_hash
             FROM conflict_sets WHERE id = ?1",
            params![conflict_set_id],
            |row| {
                Ok(StoredConflictSet {
                    id: row.get(0)?,
                    repo_id: row.get(1)?,
                    context: row.get(2)?,
                    paths_json: row.get(3)?,
                    created_at_ms: row.get(4)?,
                    base_content_ref: row.get(5)?,
                    ours_content_ref: row.get(6)?,
                    theirs_content_ref: row.get(7)?,
                    generated_by_operation_id: row.get(8)?,
                    resolver_backend: row.get(9)?,
                    content_hash: row.get(10)?,
                })
            },
        )
        .optional()?;
    let Some(conflict) = conflict else {
        return Ok(None);
    };
    let Some(stored) = conflict.content_hash.clone() else {
        return Ok(None);
    };
    let mut path_stmt = conn.prepare(
        "SELECT id, path, path_fingerprint, base_path, ours_path, theirs_path, kind,
                base_ref, ours_ref, theirs_ref, base_status, ours_status, theirs_status,
                base_mode, ours_mode, theirs_mode, created_at_ms
         FROM path_conflicts WHERE conflict_set_id = ?1 ORDER BY rowid",
    )?;
    let path_rows: Vec<StoredPathConflict> = path_stmt
        .query_map(params![conflict_set_id], |row| {
            Ok(StoredPathConflict {
                id: row.get(0)?,
                path: row.get(1)?,
                path_fingerprint: row.get(2)?,
                base_path: row.get(3)?,
                ours_path: row.get(4)?,
                theirs_path: row.get(5)?,
                kind: row.get(6)?,
                base_ref: row.get(7)?,
                ours_ref: row.get(8)?,
                theirs_ref: row.get(9)?,
                base_status: row.get(10)?,
                ours_status: row.get(11)?,
                theirs_status: row.get(12)?,
                base_mode: row.get(13)?,
                ours_mode: row.get(14)?,
                theirs_mode: row.get(15)?,
                created_at_ms: row.get(16)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let digest_rows = path_rows
        .iter()
        .map(StoredPathConflict::immutable_digest_input)
        .collect::<Vec<_>>();
    let recomputed = integrity::conflict_set_digest(&integrity::ConflictSetDigestInput {
        id: &conflict.id,
        repo_id: &conflict.repo_id,
        context: &conflict.context,
        paths_json: &conflict.paths_json,
        base_content_ref: conflict.base_content_ref.as_deref(),
        ours_content_ref: conflict.ours_content_ref.as_deref(),
        theirs_content_ref: conflict.theirs_content_ref.as_deref(),
        generated_by_operation_id: conflict.generated_by_operation_id.as_deref(),
        resolver_backend: conflict.resolver_backend.as_deref(),
        status: "unresolved",
        created_at_ms: conflict.created_at_ms,
        path_conflicts: &digest_rows,
    });
    Ok(Some((stored, recomputed)))
}

struct StoredConflictSet {
    id: String,
    repo_id: String,
    context: String,
    paths_json: String,
    created_at_ms: i64,
    base_content_ref: Option<String>,
    ours_content_ref: Option<String>,
    theirs_content_ref: Option<String>,
    generated_by_operation_id: Option<String>,
    resolver_backend: Option<String>,
    content_hash: Option<String>,
}

struct StoredPathConflict {
    id: String,
    path: String,
    path_fingerprint: String,
    base_path: Option<String>,
    ours_path: Option<String>,
    theirs_path: Option<String>,
    kind: String,
    base_ref: Option<String>,
    ours_ref: Option<String>,
    theirs_ref: Option<String>,
    base_status: Option<String>,
    ours_status: Option<String>,
    theirs_status: Option<String>,
    base_mode: Option<String>,
    ours_mode: Option<String>,
    theirs_mode: Option<String>,
    created_at_ms: i64,
}

impl StoredPathConflict {
    fn immutable_digest_input(&self) -> integrity::PathConflictDigestInput<'_> {
        integrity::PathConflictDigestInput {
            id: &self.id,
            path: &self.path,
            path_fingerprint: &self.path_fingerprint,
            base_path: self.base_path.as_deref(),
            ours_path: self.ours_path.as_deref(),
            theirs_path: self.theirs_path.as_deref(),
            kind: &self.kind,
            base_ref: self.base_ref.as_deref(),
            ours_ref: self.ours_ref.as_deref(),
            theirs_ref: self.theirs_ref.as_deref(),
            base_status: self.base_status.as_deref(),
            ours_status: self.ours_status.as_deref(),
            theirs_status: self.theirs_status.as_deref(),
            base_mode: self.base_mode.as_deref(),
            ours_mode: self.ours_mode.as_deref(),
            theirs_mode: self.theirs_mode.as_deref(),
            resolution_ref: None,
            status: "unresolved",
            created_at_ms: self.created_at_ms,
        }
    }
}

/// A decision row read back for `doctor`'s chain pass.
struct StoredDecision {
    id: String,
    proposal_id: String,
    proposal_revision_id: String,
    decision: String,
    actor: String,
    content_hash: Option<String>,
    created_at_ms: i64,
    rowid: i64,
}

/// An operation row read back for `doctor`'s chain re-walk.
struct StoredOp {
    id: String,
    parent_operation_id: Option<String>,
    command: String,
    kind: String,
    resulting_view_id: Option<String>,
    content_hash: Option<String>,
    created_at_ms: i64,
    rowid: i64,
}

/// The recorded `operations` rowid high-water mark (the legacy/tampered boundary for
/// operation rows).
fn op_high_water(conn: &Connection) -> Result<i64> {
    let mark: Option<i64> = conn
        .query_row(
            "SELECT op_high_water FROM integrity_marker WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(mark.unwrap_or(0))
}

/// The domain-row digest an operation folded into its chain link, recovered for the
/// `doctor` re-walk by reading the operation's view `state_json` for an `evidence_id`
/// or `decision_id` and returning that row's stored `content_hash`. `None` for
/// operations with no domain row (init, propose, attach, …).
fn op_domain_digest(conn: &Connection, view_id: Option<&str>) -> Result<Option<String>> {
    let Some(view_id) = view_id else {
        return Ok(None);
    };
    let state_json: Option<String> = conn
        .query_row(
            "SELECT state_json FROM views WHERE id = ?1",
            params![view_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(state_json) = state_json else {
        return Ok(None);
    };
    let Ok(state) = serde_json::from_str::<Value>(&state_json) else {
        return Ok(None);
    };
    if let Some(evidence_id) = state.get("evidence_id").and_then(Value::as_str) {
        return conn
            .query_row(
                "SELECT content_hash FROM evidence WHERE id = ?1",
                params![evidence_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(Into::into);
    }
    if let Some(decision_id) = state.get("decision_id").and_then(Value::as_str) {
        return conn
            .query_row(
                "SELECT content_hash FROM decisions WHERE id = ?1",
                params![decision_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(Into::into);
    }
    if let Some(stored) = state.get("merge_lineage_hash").and_then(Value::as_str) {
        let recomputed = integrity::merge_lineage_digest(&integrity::MergeLineageDigestInput {
            proposal_id: state
                .get("proposal_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            proposal_revision_id: state
                .get("proposal_revision_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            snapshot_id: state
                .get("snapshot_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            base_head: state
                .get("base_head")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            ours_head: state
                .get("ours_head")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            base_content_ref: state
                .get("base_content_ref")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            ours_content_ref: state
                .get("ours_content_ref")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            theirs_content_ref: state
                .get("theirs_content_ref")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            merged_content_ref: state
                .get("merged_content_ref")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        });
        return Ok(Some(if recomputed == stored {
            stored.to_string()
        } else {
            recomputed
        }));
    }
    if let Some(conflict_set_id) = state.get("conflict_set_id").and_then(Value::as_str) {
        return conn
            .query_row(
                "SELECT content_hash FROM conflict_sets WHERE id = ?1",
                params![conflict_set_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(Into::into);
    }
    Ok(None)
}

/// Recursively scan a work tree for leftover crash-atomic-restore temp files
/// (`forge_content_native::RESTORE_TEMP_PREFIX`), skipping `.git` and `.forge`.
/// A match is the signature of a restore killed mid-flight (NER-132 U4/U7).
fn scan_restore_temps(root: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue, // unreadable dir is not a half-applied-restore signal
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                // Never descend into git's or forge's own state — at ANY depth, so a
                // submodule's nested .git (a large, unbounded object tree) is skipped
                // too. Restore temps only land in worktree dirs forge materializes
                // into, never inside a git/forge store, so this loses no real signal.
                if name == ".git" || name == ".forge" {
                    continue;
                }
                stack.push(entry.path());
            } else if name.starts_with(forge_content_native::RESTORE_TEMP_PREFIX) {
                found.push(entry.path().to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    Ok(found)
}

pub fn pr_body(cwd: &Path) -> Result<(String, Vec<String>)> {
    pr_body_for(cwd, None, None)
}

/// Render the PR-body markdown, returning `(body, excluded)`. Secret-risk-named
/// changed paths are dropped from the "Changed Paths" list by the default-deny
/// export policy (NER-133 U6) and returned in `excluded` so the CLI can surface them
/// as `warnings[]`.
pub fn pr_body_for(
    cwd: &Path,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
) -> Result<(String, Vec<String>)> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let proposal = resolve_proposal(&context, &attempt.attempt_id, proposal_id, true)?.proposal;
    let evidence = latest_evidence_for_attempt(&context, &attempt.attempt_id)?;
    let check = latest_check_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let decision = latest_decision_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let mut body = String::new();
    body.push_str("# Forge Proposal\n\n");
    body.push_str(&format!("Intent: {}\n\n", attempt.intent));
    let (kept_paths, excluded_paths) = forge_content::filter_secret_risk(&proposal.changed_paths);
    body.push_str("## Changed Paths\n");
    for path in &kept_paths {
        body.push_str(&format!("- {path}\n"));
    }
    body.push('\n');
    // Cite the competing attempts against the declared intent (NER-137 R9): the
    // ranked comparison replaces the single-latest-evidence under-report. Uses the
    // same verify-then-rank engine, so a cheap-check-tampered rival shows as
    // `integrity: tampered` / unranked rather than as a silent passing row.
    let comparison = compare_attempts(
        cwd,
        CompareSelector {
            intent_id: Some(attempt.intent_id.clone()),
            attempt_id: None,
        },
    )?;
    if let Some(group) = comparison.intents.first() {
        body.push_str("## Competing Attempts\n");
        for row in &group.attempts {
            let rank = row
                .rank
                .map(|rank| format!("rank {rank}"))
                .unwrap_or_else(|| "unranked".to_string());
            let check = row.check_status.as_deref().unwrap_or("n/a");
            let marker = if row
                .proposal
                .as_ref()
                .map(|candidate| candidate.proposal_id.as_str())
                == Some(proposal.proposal_id.as_str())
            {
                " ← this proposal"
            } else {
                ""
            };
            body.push_str(&format!(
                "- {rank}: attempt `{}` — check {check}, integrity {}{}\n",
                row.attempt_id, row.integrity, marker
            ));
        }
        body.push('\n');
    }
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
    Ok((body, excluded_paths))
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
    // NER-138 Phase 7 slice 3: seed reachability from the AUTHORITATIVE ledger tip (not only
    // the ref-store HEAD, which a lock-free, never-reconciled gc could read stale), plus every
    // accepted `decisions.commit_id` and every op-log-referenced commit (a `checkout` target
    // writes NO decision row, so its commit is reachable only through the op-log). Each is
    // walked as a DAG root (commit → ancestry → trees). Best-effort: a dangling root is
    // surfaced by `doctor`, not fatal to this dry-run report. No-op for git-backend repos.
    let mut roots: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(tip) = native_tip(&context, &connection)? {
        roots.insert(tip.to_string());
    }
    let mut decision_stmt = connection
        .prepare("SELECT commit_id FROM decisions WHERE repo_id = ?1 AND commit_id IS NOT NULL")?;
    for row in decision_stmt.query_map(params![context.repo_id], |row| row.get::<_, String>(0))? {
        roots.insert(row?);
    }
    let mut view_stmt = connection.prepare("SELECT state_json FROM views WHERE repo_id = ?1")?;
    for row in view_stmt.query_map(params![context.repo_id], |row| row.get::<_, String>(0))? {
        // NER-143 R6: FAIL CLOSED when a ledger row that DETERMINES a root cannot be read.
        // A malformed `views.state_json` means we cannot know whether this op-log entry named
        // a live commit (a `checkout` target is reachable ONLY through the op-log), so silently
        // skipping it under-counts the root set — harmless for this dry-run report, but a live
        // commit marked "unreachable" would be deleted once Phase 8 (NER-139) wires real
        // mark-sweep deletion to this scan. Propagate instead. Path-free (S1): never interpolate
        // the row. Contrast with a *dangling object* (a determined root/ref whose object is
        // absent) below, which stays best-effort — that is `doctor`'s domain and the established
        // gc-tolerance contract (`doctor_reports_corrupt_native_content_and_gc_reports_unreachable_objects`).
        // Message is honest about the remedy: `doctor` does NOT currently parse every
        // `views.state_json`, so it would not pinpoint this row — say "the ledger is damaged"
        // rather than dead-end the user at `forge doctor` (code-review reliability finding).
        let value: serde_json::Value = serde_json::from_str(&row?).map_err(|_| {
            anyhow!("gc cannot read a ledger view row (corrupt views.state_json); the ledger is damaged")
        })?;
        if let Some(commit_id) = value.get("commit_id").and_then(|v| v.as_str()) {
            roots.insert(commit_id.to_string());
        }
    }
    for root in &roots {
        // NER-143 R6: FAIL CLOSED on a corrupt root id string (the ledger names a root we cannot
        // even parse → the root set is untrustworthy). This covers EVERY root source — the
        // native_tip, the `decisions.commit_id` set (inserted raw above), and the view-derived
        // ids — so an unparseable accepted-commit id also fails closed here, not only a corrupt
        // view row. The reachability WALK from a parseable root stays best-effort: a
        // determined-but-dangling object is a `doctor` finding, not a root-enumeration failure
        // (mirrors the `verify_content_ref` snapshot loop above and the existing corrupt-content
        // gc-tolerance contract). Path-free (S1): never interpolate `root`.
        let id = forge_content_native::ObjectId::parse(root).map_err(|_| {
            anyhow!(
                "gc found an unparseable reachability root in the ledger; the ledger is damaged"
            )
        })?;
        if let Ok(ids) = native_store.reachable_from(&id) {
            reachable.extend(ids);
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

pub fn resolve_attempt(cwd: &Path, attempt_id: Option<&str>) -> Result<ResolvedAttempt> {
    let context = open_repository(cwd)?;
    resolve_attempt_in_context(&context, attempt_id)
}

fn resolve_attempt_in_context(
    context: &RepositoryContext,
    attempt_id: Option<&str>,
) -> Result<ResolvedAttempt> {
    if let Some(attempt_id) = attempt_id {
        let attempt =
            attempt_by_id(context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
                selector: attempt_id.to_string(),
            })?;
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
        [] => Err(ForgeError::NoActiveAttempt.into()),
        [attempt] => Ok(ResolvedAttempt {
            attempt: attempt.clone(),
        }),
        _ => Err(ForgeError::AmbiguousAttempt {
            candidate_ids: attempts
                .iter()
                .map(|attempt| attempt.attempt_id.clone())
                .collect(),
        }
        .into()),
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

/// Which attempts to compare (NER-137). With neither field set, `compare_attempts`
/// returns every intent that has ≥1 attempt; `intent_id` filters to one intent;
/// `attempt_id` scopes to that attempt's intent. Unknown selectors raise the existing
/// `UnknownIntent`/`UnknownAttempt` typed errors — multiple intents are *grouped*,
/// not an ambiguity error.
#[derive(Debug, Clone, Default)]
pub struct CompareSelector {
    pub intent_id: Option<String>,
    pub attempt_id: Option<String>,
}

/// The compare/rank result: competing attempts grouped per intent, each group ranked
/// (NER-137 R1/R2). The headline read surface that lets a human or agent select a
/// winner from verified data and chain `compare → accept` headlessly.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptComparison {
    pub intents: Vec<IntentComparison>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentComparison {
    pub intent_id: String,
    pub intent: String,
    pub attempts: Vec<AttemptCompareRow>,
}

/// One competing attempt's compare row. Ranking is **advisory**; the per-gate results,
/// metrics, integrity label, and raw changed paths are **authoritative** and always
/// present. `rank` is `None` for an attempt that is unrankable — its deciding evidence
/// failed the (cheap per-row) integrity check (`integrity == "tampered"`) or it has no
/// proposal yet — so a headless consumer that selects by numeric-minimum rank can
/// never pick a tampered attempt (NER-137 R4).
#[derive(Debug, Clone, Serialize)]
pub struct AttemptCompareRow {
    pub attempt_id: String,
    pub status: String,
    pub proposal: Option<ComparedProposal>,
    /// Secret-redacted file-level diff summary of the proposal vs its base — the paths
    /// the snapshot changed (the per-attempt "diff summary"). The richer pairwise
    /// file/hunk content diff is the CLI's backend-routed `compare --diff` path.
    pub changed_paths: Vec<String>,
    pub changed_count: usize,
    pub gates: Vec<forge_policy::GateResult>,
    pub check_status: Option<String>,
    pub metrics: StructuredMetrics,
    /// `"verified"` (deciding rows pass the cheap per-row check), `"legacy_unverified"`
    /// (a pre-Phase-5 grandfathered row), `"tampered"` (a deciding row failed the cheap
    /// check — recorded here, NOT propagated, so one bad attempt does not blank the
    /// comparison), or `"no_evidence"` (no deciding evidence / no proposal). The deep
    /// recompute-row-hash case is `doctor`'s op-walk, not this cheap label.
    pub integrity: String,
    pub decision_status: Option<String>,
    pub publication_status: Option<String>,
    pub rank: Option<u32>,
    pub rank_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparedProposal {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
}

/// Parsed numeric metrics aggregated over the proposal-snapshot evidence (NER-137).
/// Test counts drive the ranking metric tier; clippy findings are surfaced but do not
/// influence the order in v0 (R3 narrowing — callers re-rank on these raw metrics).
#[derive(Debug, Clone, Serialize, Default)]
pub struct StructuredMetrics {
    pub tests_passed: Option<u64>,
    pub tests_failed: Option<u64>,
    pub tests_ignored: Option<u64>,
    pub clippy_findings: Option<u64>,
}

const INTEGRITY_VERIFIED: &str = "verified";
const INTEGRITY_LEGACY: &str = "legacy_unverified";
const INTEGRITY_TAMPERED: &str = "tampered";
const INTEGRITY_NO_EVIDENCE: &str = "no_evidence";

/// Evidence-based attempt comparison and ranking (NER-137, Phase 6). Read-only — opens
/// a throwaway connection, takes **no** advisory lock (it never writes). Its ranking is
/// a snapshot a concurrent lock-free `run` can invalidate, so it is **advisory**;
/// `accept`/`decide` keep the authoritative in-txn gate. Ranks on Phase 5
/// cheaply-verified evidence: a deciding row that fails the per-row check labels the
/// attempt `tampered` and leaves it unranked (`rank: null`).
pub fn compare_attempts(cwd: &Path, selector: CompareSelector) -> Result<AttemptComparison> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let marker = evidence_high_water(&connection)?;

    let intent_ids = resolve_compare_intents(&context, &connection, &selector)?;
    let mut intents = Vec::new();
    for (intent_id, intent_text) in intent_ids {
        let attempts = attempts_for_intent(&connection, &context.repo_id, &intent_id)?;
        let mut rows = Vec::new();
        for attempt in attempts {
            rows.push(build_compare_row(&context, &connection, &attempt, marker)?);
        }
        rank_compare_rows(&mut rows);
        intents.push(IntentComparison {
            intent_id,
            intent: intent_text,
            attempts: rows,
        });
    }
    Ok(AttemptComparison { intents })
}

/// Resolve the `(intent_id, intent_text)` groups a `CompareSelector` names. An
/// `attempt_id` maps to its intent; an `intent_id` filters to that one; neither
/// returns every intent with ≥1 attempt, ordered by first attempt.
fn resolve_compare_intents(
    context: &RepositoryContext,
    conn: &Connection,
    selector: &CompareSelector,
) -> Result<Vec<(String, String)>> {
    if let Some(attempt_id) = &selector.attempt_id {
        let attempt =
            attempt_by_id(context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
                selector: attempt_id.clone(),
            })?;
        return Ok(vec![(attempt.intent_id, attempt.intent)]);
    }
    if let Some(intent_id) = &selector.intent_id {
        let text: Option<String> = conn
            .query_row(
                "SELECT text FROM intents WHERE id = ?1",
                params![intent_id],
                |row| row.get(0),
            )
            .optional()?;
        let text = text.ok_or_else(|| ForgeError::UnknownIntent {
            selector: intent_id.clone(),
        })?;
        return Ok(vec![(intent_id.clone(), text)]);
    }
    // All intents that have ≥1 attempt, ordered by the intent's first attempt so the
    // grouping is deterministic.
    let mut statement = conn.prepare(
        "SELECT i.id, i.text, MIN(a.created_at_ms) AS first_attempt
         FROM intents i
         JOIN attempts a ON a.intent_id = i.id
         WHERE i.repo_id = ?1
         GROUP BY i.id, i.text
         ORDER BY first_attempt ASC, i.id ASC",
    )?;
    let rows = statement.query_map(params![context.repo_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn attempts_for_intent(
    conn: &Connection,
    repo_id: &str,
    intent_id: &str,
) -> Result<Vec<AttemptRecord>> {
    let mut statement = conn.prepare(
        "SELECT a.id, a.intent_id, i.text, a.base_head, a.status
         FROM attempts a
         JOIN intents i ON i.id = a.intent_id
         WHERE a.repo_id = ?1 AND a.intent_id = ?2
         ORDER BY a.created_at_ms ASC, a.id ASC",
    )?;
    let rows = statement.query_map(params![repo_id, intent_id], |row| {
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

/// Build one attempt's compare row. Runs the Phase 4 policy evaluation for per-gate
/// verdicts and the Phase 5 per-row integrity check, **recording** (not propagating) a
/// tamper as a label. No git is invoked here (the store stays git-free, PRD §23.4); the
/// per-attempt diff summary is the stored, secret-redacted `changed_paths`.
fn build_compare_row(
    context: &RepositoryContext,
    conn: &Connection,
    attempt: &AttemptRecord,
    marker: i64,
) -> Result<AttemptCompareRow> {
    let proposal = latest_proposal_for_attempt(context, &attempt.attempt_id)?;
    let Some(proposal) = proposal else {
        return Ok(AttemptCompareRow {
            attempt_id: attempt.attempt_id.clone(),
            status: attempt.status.clone(),
            proposal: None,
            changed_paths: Vec::new(),
            changed_count: 0,
            gates: Vec::new(),
            check_status: None,
            metrics: StructuredMetrics::default(),
            integrity: INTEGRITY_NO_EVIDENCE.to_string(),
            decision_status: None,
            publication_status: None,
            rank: None,
            rank_reason: "no proposal yet".to_string(),
        });
    };

    let spec = intent_check_spec(conn, &attempt.intent_id)?;
    let facts = evidence_facts_on(conn, &attempt.attempt_id)?;
    let outcome = forge_policy::evaluate(&spec, &proposal.snapshot_id, &facts);
    let integrity = aggregate_integrity(conn, &outcome, marker)?;
    let metrics = compare_structured_metrics(conn, &attempt.attempt_id, &proposal.snapshot_id)?;

    let (kept_paths, _dropped) = forge_content::filter_secret_risk(&proposal.changed_paths);
    let changed_count = kept_paths.len();
    let gates = outcome.gates.into_iter().map(redact_gate_result).collect();
    let decision_status =
        latest_decision_for_proposal_revision(context, &proposal.proposal_revision_id)?;
    let publication_status =
        latest_publication_for_proposal_revision(context, &proposal.proposal_revision_id)?
            .map(|_| "published".to_string());

    Ok(AttemptCompareRow {
        attempt_id: attempt.attempt_id.clone(),
        status: attempt.status.clone(),
        proposal: Some(ComparedProposal {
            proposal_id: proposal.proposal_id,
            proposal_revision_id: proposal.proposal_revision_id,
            snapshot_id: proposal.snapshot_id,
            base_head: proposal.base_head,
            content_ref: proposal.content_ref,
        }),
        changed_paths: kept_paths,
        changed_count,
        gates,
        check_status: Some(outcome.status),
        metrics,
        integrity,
        decision_status,
        publication_status,
        rank: None, // assigned by rank_compare_rows
        rank_reason: String::new(),
    })
}

/// Aggregate the per-row integrity of the gates' deciding evidence into one label,
/// fail-closed toward the strongest signal: any tampered deciding row → `tampered`;
/// else any legacy → `legacy_unverified`; else if at least one gate has deciding
/// evidence → `verified`; else `no_evidence` (e.g. a `missing` gate set).
fn aggregate_integrity(
    conn: &Connection,
    outcome: &forge_policy::CheckOutcome,
    marker: i64,
) -> Result<String> {
    let mut any_deciding = false;
    let mut any_legacy = false;
    for gate in &outcome.gates {
        if let Some(evidence_id) = &gate.evidence_id {
            any_deciding = true;
            match verify_evidence_integrity(conn, evidence_id, marker)? {
                IntegrityStatus::Tampered(_) => return Ok(INTEGRITY_TAMPERED.to_string()),
                IntegrityStatus::LegacyUnverified => any_legacy = true,
                IntegrityStatus::Verified => {}
            }
        }
    }
    Ok(if any_legacy {
        INTEGRITY_LEGACY.to_string()
    } else if any_deciding {
        INTEGRITY_VERIFIED.to_string()
    } else {
        INTEGRITY_NO_EVIDENCE.to_string()
    })
}

/// Aggregate parsed structured metrics over the proposal-snapshot evidence: sum test
/// counts across test binaries/rows and clippy findings across clippy rows.
fn compare_structured_metrics(
    conn: &Connection,
    attempt_id: &str,
    snapshot_id: &str,
) -> Result<StructuredMetrics> {
    let mut statement = conn.prepare(
        "SELECT structured_json FROM evidence
         WHERE attempt_id = ?1 AND snapshot_id = ?2 AND structured_json IS NOT NULL
         ORDER BY created_at_ms ASC, rowid ASC",
    )?;
    let rows = statement.query_map(params![attempt_id, snapshot_id], |row| {
        row.get::<_, String>(0)
    })?;
    let mut metrics = StructuredMetrics::default();
    for json in rows {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&json?) else {
            continue;
        };
        let get = |key: &str| value.get(key).and_then(serde_json::Value::as_u64);
        metrics.tests_passed = add_opt(metrics.tests_passed, get("passed"));
        metrics.tests_failed = add_opt(metrics.tests_failed, get("failed"));
        metrics.tests_ignored = add_opt(metrics.tests_ignored, get("ignored"));
        metrics.clippy_findings = add_opt(metrics.clippy_findings, get("findings"));
    }
    Ok(metrics)
}

fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(x + y),
    }
}

/// Assign a deterministic total-order rank to the rankable rows of one intent group
/// (NER-137 R3/R4). Rankable = has a proposal AND not `tampered`. Order: all-required-
/// gates-passing first; within a tier fewer parsed test failures, then more parsed
/// passing; stable ties keep first-attempt order (input is created-order). Tampered /
/// no-proposal rows get `rank: null` and are placed after the ranked rows.
fn rank_compare_rows(rows: &mut Vec<AttemptCompareRow>) {
    let rankable =
        |row: &AttemptCompareRow| row.proposal.is_some() && row.integrity != INTEGRITY_TAMPERED;
    // Partition while preserving input (created) order for stable ties.
    let mut ranked: Vec<AttemptCompareRow> = Vec::new();
    let mut unranked: Vec<AttemptCompareRow> = Vec::new();
    for row in rows.drain(..) {
        if rankable(&row) {
            ranked.push(row);
        } else {
            unranked.push(row);
        }
    }
    ranked.sort_by_key(|row| {
        let gates_passing = if row.check_status.as_deref() == Some("passed") {
            0
        } else {
            1
        };
        (
            gates_passing,
            row.metrics.tests_failed.unwrap_or(u64::MAX),
            std::cmp::Reverse(row.metrics.tests_passed.unwrap_or(0)),
        )
    });
    for (index, row) in ranked.iter_mut().enumerate() {
        let rank = (index + 1) as u32;
        row.rank = Some(rank);
        let mut reason = if row.check_status.as_deref() == Some("passed") {
            format!(
                "rank {rank}: all required gates passing ({} failing tests, {} passing)",
                row.metrics.tests_failed.unwrap_or(0),
                row.metrics.tests_passed.unwrap_or(0)
            )
        } else {
            format!(
                "rank {rank}: required gates not satisfied (check status: {})",
                row.check_status.as_deref().unwrap_or("unknown")
            )
        };
        // A legacy_unverified attempt is rankable (its deciding evidence predates
        // Phase 5 and was never hash-verified), so a rank-only consumer must still see
        // that caveat in the explanation (NER-137 code-review).
        if row.integrity == INTEGRITY_LEGACY {
            reason.push_str(
                " — NOTE: deciding evidence is legacy_unverified (pre-Phase-5, not hash-verified)",
            );
        }
        row.rank_reason = reason;
    }
    for row in &mut unranked {
        row.rank = None;
        row.rank_reason = if row.proposal.is_none() {
            "unranked: no proposal yet".to_string()
        } else {
            "unranked: deciding evidence failed the integrity check (tampered) — verify with `doctor`".to_string()
        };
    }
    ranked.append(&mut unranked);
    *rows = ranked;
}

/// The latest proposal's `content_ref` for an attempt — the diffable tree the pairwise
/// `compare --diff` path feeds to the CLI diff router. Errors `UnknownAttempt`
/// when the attempt does not exist, `NoProposal` when it has no proposal yet.
pub fn attempt_proposal_content_ref(cwd: &Path, attempt_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    let attempt =
        attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?;
    let proposal = latest_proposal_for_attempt(&context, &attempt.attempt_id)?
        .ok_or(ForgeError::NoProposal)?;
    Ok(proposal.content_ref)
}

pub fn proposal_for_merge(cwd: &Path, proposal_id: &str) -> Result<ProposalSummary> {
    let context = open_repository(cwd)?;
    proposal_by_id(&context, proposal_id)?.ok_or_else(|| {
        ForgeError::UnknownProposal {
            selector: proposal_id.to_string(),
        }
        .into()
    })
}

pub fn resolved_merge_ours_head(
    cwd: &Path,
    proposal_id: &str,
    content_ref: &str,
) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    resolved_merge_parents_for_proposal_on(
        &connection,
        &context.repo_id,
        proposal_id,
        Some(content_ref),
    )
    .map(|parents| parents.map(|(ours, _base)| ours))
}

pub fn show_attempt(cwd: &Path, attempt_id: &str) -> Result<AttemptShowRecord> {
    let context = open_repository(cwd)?;
    let attempt =
        attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?;
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
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let attempt =
        attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?;
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
        // NER-143 R1: `attempt attach` is the FIFTH materializing op — the CLI restores the
        // attached attempt's tree into the worktree before calling this. Set the expected
        // baseline atomically with the attach (code-review P1: without this, an attach-then-nav
        // spuriously fails DIRTY_WORKTREE because `expected` still points at the prior attempt's
        // tree). Same dedicated-UPDATE discipline as the other recorders (DR-F2).
        set_expected_content_ref(tx, content_ref)?;
        Ok(op)
    })?;
    Ok(op)
}

pub fn attempt_materialization_ref(cwd: &Path, attempt_id: &str) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let attempt =
        attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?;
    Ok(latest_snapshot_for_attempt(&context, &attempt.attempt_id)?
        .map(|snapshot| snapshot.content_ref))
}

pub fn attempt_base_head(cwd: &Path, attempt_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    Ok(attempt_by_id(&context, attempt_id)?
        .ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?
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
        let proposal =
            proposal_by_id(context, proposal_id)?.ok_or_else(|| ForgeError::UnknownProposal {
                selector: proposal_id.to_string(),
            })?;
        if proposal.attempt_id != attempt_id {
            return Err(ForgeError::UnknownProposal {
                selector: proposal_id.to_string(),
            }
            .into());
        }
        return Ok(ResolvedProposal { proposal });
    }

    let proposals = proposals_for_attempt(context, attempt_id)?;
    match proposals.as_slice() {
        [] => Err(ForgeError::NoProposal.into()),
        [proposal] if allow_single_default => Ok(ResolvedProposal {
            proposal: proposal.clone(),
        }),
        _ => Err(ForgeError::AmbiguousProposal {
            candidate_ids: proposals
                .iter()
                .map(|proposal| proposal.proposal_id.clone())
                .collect(),
        }
        .into()),
    }
}

fn proposal_by_id(
    context: &RepositoryContext,
    proposal_id: &str,
) -> Result<Option<ProposalSummary>> {
    let connection = open_connection(&context.database_path)?;
    proposal_by_id_on(&connection, context, proposal_id)
}

fn proposal_by_id_on(
    connection: &Connection,
    context: &RepositoryContext,
    proposal_id: &str,
) -> Result<Option<ProposalSummary>> {
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

fn resolved_merge_parents_for_proposal_on(
    connection: &Connection,
    repo_id: &str,
    proposal_id: &str,
    content_ref: Option<&str>,
) -> Result<Option<(String, String)>> {
    let mut merge_statement = connection.prepare(
        "SELECT v.state_json
         FROM views v
         JOIN operations o ON o.id = v.operation_id
         WHERE v.repo_id = ?1 AND v.kind = 'merge_clean' AND o.status = 'success'
         ORDER BY v.created_at_ms DESC, v.rowid DESC",
    )?;
    let merge_rows = merge_statement.query_map(params![repo_id], |row| row.get::<_, String>(0))?;
    for row in merge_rows {
        let state_json = row?;
        let Ok(value) = serde_json::from_str::<Value>(&state_json) else {
            continue;
        };
        if value.get("proposal_id").and_then(Value::as_str) != Some(proposal_id) {
            continue;
        }
        if let Some(content_ref) = content_ref {
            if value.get("merged_content_ref").and_then(Value::as_str) != Some(content_ref) {
                continue;
            }
        }
        let Some(ours_head) = value
            .get("ours_head")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(base_head) = value
            .get("base_head")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        return Ok(Some((ours_head, base_head)));
    }

    let mut statement = connection.prepare(
        "SELECT id, paths_json
         FROM conflict_sets
         WHERE repo_id = ?1 AND resolver_backend = 'native_merge' AND status = 'resolved'
         ORDER BY created_at_ms DESC, rowid DESC",
    )?;
    let rows = statement.query_map(params![repo_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (conflict_set_id, paths_json) = row?;
        let Ok(value) = serde_json::from_str::<Value>(&paths_json) else {
            continue;
        };
        if value.get("proposal_id").and_then(Value::as_str) != Some(proposal_id) {
            continue;
        }
        if let Some(content_ref) = content_ref {
            let matching_resolutions: i64 = connection.query_row(
                "SELECT COUNT(*)
                 FROM path_conflicts
                 WHERE conflict_set_id = ?1
                   AND resolution_ref = ?2",
                params![conflict_set_id, content_ref],
                |row| row.get(0),
            )?;
            if matching_resolutions == 0 {
                continue;
            }
        }
        let Some(ours_head) = value
            .get("ours_head")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(base_head) = value
            .get("base_head")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        return Ok(Some((ours_head, base_head)));
    }
    Ok(None)
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
           AND NOT EXISTS (
               SELECT 1 FROM proposal_revisions newer
               WHERE newer.proposal_id = pr.proposal_id
                 AND (newer.created_at_ms > pr.created_at_ms
                      OR (newer.created_at_ms = pr.created_at_ms AND newer.rowid > pr.rowid))
           )
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

/// The stored chain hash of an operation, or the genesis sentinel when the parent
/// is absent (the `init` genesis op) or predates Phase 5 (a legacy NULL hash). It is
/// the `parent_hash` input to the next link, so a chain always anchors on one
/// canonical value (NER-136). Read on the writer's `&tx` so the folded parent and
/// the singleton CAS pointer are the same row.
fn op_content_hash(conn: &Connection, operation_id: Option<&str>) -> Result<String> {
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

fn insert_operation_view(
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
fn insert_operation_view_chained(
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

/// Find the Forge repository root by walking up from `cwd` for the nearest ancestor that
/// contains the `.forge/forge.db` repo marker. GIT-FREE (NER-138 Phase 7 slice 3): post-`init`
/// commands resolve the root without the git binary, so the native lifecycle
/// (start→save→…→accept→restore→log→checkout→undo) runs with git removed from PATH. `init`
/// still anchors a *git-backed* repo on the git toplevel (Forge layers on an existing git
/// repo); a *native* repo's root is established at init without git. Returns
/// `NotInitialized` when no `.forge/forge.db` is found up the tree.
fn forge_root(cwd: &Path) -> Result<PathBuf> {
    let start = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut current: &Path = &start;
    loop {
        if current.join(".forge/forge.db").exists() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return Err(ForgeError::NotInitialized.into()),
        }
    }
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

    fn compare_row(
        attempt_id: &str,
        has_proposal: bool,
        integrity: &str,
        check_status: Option<&str>,
        tests_failed: Option<u64>,
        tests_passed: Option<u64>,
    ) -> AttemptCompareRow {
        AttemptCompareRow {
            attempt_id: attempt_id.to_string(),
            status: "active".to_string(),
            proposal: has_proposal.then(|| ComparedProposal {
                proposal_id: format!("prop_{attempt_id}"),
                proposal_revision_id: format!("rev_{attempt_id}"),
                snapshot_id: format!("snap_{attempt_id}"),
                base_head: "base".to_string(),
                content_ref: "git-tree:deadbeef".to_string(),
            }),
            changed_paths: Vec::new(),
            changed_count: 0,
            gates: Vec::new(),
            check_status: check_status.map(str::to_string),
            metrics: StructuredMetrics {
                tests_passed,
                tests_failed,
                tests_ignored: None,
                clippy_findings: None,
            },
            integrity: integrity.to_string(),
            decision_status: None,
            publication_status: None,
            rank: None,
            rank_reason: String::new(),
        }
    }

    #[test]
    fn rank_passing_attempt_above_failing() {
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("failed"),
                Some(2),
                Some(48),
            ),
            compare_row(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(50),
            ),
        ];
        rank_compare_rows(&mut rows);
        // The passing attempt ranks first regardless of input order.
        assert_eq!(rows[0].attempt_id, "b");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].attempt_id, "a");
        assert_eq!(rows[1].rank, Some(2));
    }

    #[test]
    fn rank_fewer_failures_first_within_tier() {
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(48),
            ),
            compare_row(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(50),
            ),
        ];
        rank_compare_rows(&mut rows);
        // Same failures (0), so more passing wins.
        assert_eq!(rows[0].attempt_id, "b");
    }

    #[test]
    fn tampered_attempt_is_unranked_and_placed_last() {
        let mut rows = vec![
            // The would-be winner by exit code, but tampered.
            compare_row(
                "a",
                true,
                INTEGRITY_TAMPERED,
                Some("passed"),
                Some(0),
                Some(99),
            ),
            // An honest but failing attempt.
            compare_row(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("failed"),
                Some(1),
                Some(10),
            ),
        ];
        rank_compare_rows(&mut rows);
        // The honest attempt is the rank-1 winner; the tampered one is unranked & last.
        assert_eq!(rows[0].attempt_id, "b");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].attempt_id, "a");
        assert_eq!(rows[1].rank, None);
        assert!(rows[1].rank_reason.contains("tampered"));
    }

    #[test]
    fn all_tampered_group_yields_no_rank_one() {
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_TAMPERED,
                Some("passed"),
                Some(0),
                Some(1),
            ),
            compare_row(
                "b",
                true,
                INTEGRITY_TAMPERED,
                Some("passed"),
                Some(0),
                Some(2),
            ),
        ];
        rank_compare_rows(&mut rows);
        // A numeric-min consumer cannot select a tampered attempt: no row has rank 1.
        assert!(rows.iter().all(|row| row.rank.is_none()));
    }

    #[test]
    fn no_proposal_attempt_is_unranked() {
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(5),
            ),
            compare_row("b", false, INTEGRITY_NO_EVIDENCE, None, None, None),
        ];
        rank_compare_rows(&mut rows);
        assert_eq!(rows[0].attempt_id, "a");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].rank, None);
        assert!(rows[1].rank_reason.contains("no proposal"));
    }

    #[test]
    fn ranking_is_a_stable_total_order() {
        // Two identical-metric rows keep input (created) order — deterministic.
        let build = || {
            vec![
                compare_row(
                    "a",
                    true,
                    INTEGRITY_VERIFIED,
                    Some("passed"),
                    Some(0),
                    Some(10),
                ),
                compare_row(
                    "b",
                    true,
                    INTEGRITY_VERIFIED,
                    Some("passed"),
                    Some(0),
                    Some(10),
                ),
            ]
        };
        let mut first = build();
        rank_compare_rows(&mut first);
        let mut second = build();
        rank_compare_rows(&mut second);
        let ids_first: Vec<_> = first.iter().map(|r| r.attempt_id.clone()).collect();
        let ids_second: Vec<_> = second.iter().map(|r| r.attempt_id.clone()).collect();
        assert_eq!(ids_first, ids_second);
        assert_eq!(ids_first, vec!["a".to_string(), "b".to_string()]);
    }

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

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    /// FIX D: `insert_operation_view`'s optimistic singleton CAS, when its captured
    /// parent operation no longer matches live `current_state`, raises the typed,
    /// retryable `ForgeError::CurrentStateChanged` (code `CONFLICT`) — NOT a plain
    /// `anyhow!`. This is what the CLI's `is_transient_error` keys on to skip
    /// recording the failure under the `--request-id`. Driven as a focused in-crate
    /// unit test because `insert_operation_view` is private and the production fns
    /// re-read `current_state` per call (so a stale parent can't be pinned through
    /// the public API alone).
    #[test]
    fn insert_operation_view_stale_parent_raises_current_state_changed() {
        let mut connection = Connection::open_in_memory().expect("open in-memory db");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("enable fks");
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .expect("baseline schema");
        // Phase 5 adds operations.content_hash, which insert_operation_view now writes.
        connection
            .execute_batch(include_str!("../migrations/004_integrity_and_actor.sql"))
            .expect("phase 5 integrity columns");

        // Seed: a repo, a genesis operation+view that `current_state` points at, and
        // a SECOND operation row (`op_stale`) that does NOT match current_state.
        connection
            .execute_batch(
                "INSERT INTO repositories (id, root_path, created_at_ms)
                     VALUES ('repo_1', '/tmp/repo', 0);
                 INSERT INTO operations (id, repo_id, command, status, kind, created_at_ms)
                     VALUES ('op_genesis', 'repo_1', 'init', 'succeeded', 'init', 0);
                 INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
                     VALUES ('view_genesis', 'repo_1', 'op_genesis', 'initialized', '{}', 0);
                 INSERT INTO current_state
                     (singleton, repo_id, current_operation_id, current_view_id, updated_at_ms)
                     VALUES (1, 'repo_1', 'op_genesis', 'view_genesis', 0);
                 INSERT INTO operations (id, repo_id, command, status, kind, created_at_ms)
                     VALUES ('op_stale', 'repo_1', 'save', 'succeeded', 'save', 0);",
            )
            .expect("seed rows");

        let error = with_immediate_retry(&mut connection, |tx| {
            // Pass `op_stale` as the parent: it exists (satisfies the FK) but does
            // NOT equal current_state.current_operation_id (`op_genesis`), so the
            // CAS `WHERE current_operation_id = 'op_stale'` updates zero rows.
            insert_operation_view(
                tx,
                "repo_1",
                Some("op_stale"),
                OperationViewInput {
                    request_id: None,
                    command: "save".to_string(),
                    kind: "snapshot_saved".to_string(),
                    view_kind: ViewKind::Initialized,
                    state: json!({ "lifecycle": "test" }),
                },
            )
        })
        .expect_err("a stale parent must lose the CAS");

        let forge_error = error
            .downcast_ref::<ForgeError>()
            .expect("the CAS failure is a typed ForgeError");
        assert_eq!(*forge_error, ForgeError::CurrentStateChanged);
        assert_eq!(forge_error.code(), "CONFLICT");
        assert!(forge_error.retryable());
        assert_eq!(forge_error.after_ms(), Some(50));
    }

    #[test]
    fn record_conflict_set_redacts_secret_paths() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "forge@example.test"]);
        run_git(root, &["config", "user.name", "Forge Test"]);
        fs::write(root.join("README.md"), "hello\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(root, &["commit", "-m", "initial"]);

        init_repository(root, None, "git".to_string()).expect("init repository");

        let paths = vec![
            "src/main.rs".to_string(),
            ".env".to_string(),
            "k.pem".to_string(),
        ];
        let id = record_conflict_set(root, "stale_base_accept", "HEAD0", "HEAD1", &paths)
            .expect("record conflict set");
        assert!(id.starts_with("conflict_"), "unexpected id: {id}");

        let database_path = root.join(".forge/forge.db");
        let connection = Connection::open(&database_path).expect("open db");
        let (context, paths_json): (String, String) = connection
            .query_row(
                "SELECT context, paths_json FROM conflict_sets WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query conflict row");

        assert_eq!(context, "stale_base_accept");
        let value: Value = serde_json::from_str(&paths_json).expect("parse paths_json");
        assert_eq!(value["expected_head"], "HEAD0");
        assert_eq!(value["actual_head"], "HEAD1");
        assert_eq!(value["redacted_count"], 2);
        assert!(
            paths_json.contains("src/main.rs"),
            "non-secret path must be kept: {paths_json}"
        );
        assert!(
            !paths_json.contains(".env"),
            "secret-risk path leaked: {paths_json}"
        );
        assert!(
            !paths_json.contains("k.pem"),
            "secret-risk path leaked: {paths_json}"
        );
    }
}
