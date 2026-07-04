use anyhow::{anyhow, bail, Context, Result};
use forge_core::{now_ms, OperationId, OperationStatus, ViewId, ViewKind};
use rusqlite::params;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

use crate::{
    integrity, migrations, repo_lock, signing, with_immediate_retry, MergeConflictInput,
    MergeConflictRecord, OperationViewInput, OperationViewResult,
};

pub(crate) const SYNC_MERGED_OP_KIND_SQL_IN: &str =
    "'sync_fetch_merged', 'sync_pull_merged', 'sync_push_merged'";

pub fn is_sync_merged_op_kind(kind: &str) -> bool {
    matches!(
        kind,
        "sync_fetch_merged" | "sync_pull_merged" | "sync_push_merged"
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncCloneRepository {
    pub repository_id: String,
    pub root_path: String,
    pub forge_dir: String,
    pub database_path: String,
    pub content_backend: String,
}

pub fn prepare_native_sync_clone(cwd: &Path, repository_id: &str) -> Result<SyncCloneRepository> {
    let root = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    if root.join(".forge/forge.db").exists() {
        bail!("refusing to clone into an initialized forge repository");
    }
    let non_empty = fs::read_dir(&root)
        .with_context(|| format!("failed to read {}", root.display()))?
        .next()
        .transpose()?
        .is_some();
    if non_empty {
        bail!("refusing to clone into a non-empty directory");
    }
    {
        let mut ancestor = root.parent();
        while let Some(dir) = ancestor {
            if dir.join(".forge/forge.db").exists() {
                bail!("refusing to clone a forge repo nested inside an existing forge repo");
            }
            ancestor = dir.parent();
        }
    }

    let forge_dir = root.join(".forge");
    fs::create_dir_all(&forge_dir)
        .with_context(|| format!("failed to create {}", forge_dir.display()))?;
    let _init_lock = repo_lock::acquire(&forge_dir)?;
    let database_path = forge_dir.join("forge.db");
    let mut connection = crate::open_connection(&database_path)
        .with_context(|| format!("failed to open {}", database_path.display()))?;
    migrations::apply_pending_migrations(&mut connection)?;
    let existing: i64 =
        connection.query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))?;
    if existing != 0 {
        bail!("refusing to clone into a non-empty forge database");
    }
    connection.execute(
        "INSERT INTO repositories (id, root_path, git_head, content_backend, created_at_ms)
         VALUES (?1, ?2, NULL, 'native', ?3)",
        params![repository_id, root.to_string_lossy(), now_ms()],
    )?;

    Ok(SyncCloneRepository {
        repository_id: repository_id.to_string(),
        root_path: root.to_string_lossy().into_owned(),
        forge_dir: forge_dir.to_string_lossy().into_owned(),
        database_path: database_path.to_string_lossy().into_owned(),
        content_backend: "native".to_string(),
    })
}

pub fn record_projected_sync_clone_initialized(
    database_path: &Path,
    repo_id: &str,
    root_path: &Path,
    commit_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let mut connection = crate::open_connection(database_path)
        .with_context(|| format!("failed to open {}", database_path.display()))?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let state_json = json!({
        "repository_id": repo_id,
        "root_path": root_path,
        "content_backend": "native",
        "lifecycle": "sync_clone_projected",
        "commit_id": commit_id,
        "content_ref": content_ref
    })
    .to_string();
    with_immediate_retry(&mut connection, |tx| {
        let genesis_hash = integrity::operation_link_hash(
            integrity::GENESIS_PARENT_HASH,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command: "sync clone",
                kind: "sync_clone_projected",
                created_at_ms: now,
            },
            None,
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, NULL, 'sync clone', ?3, 'sync_clone_projected', NULL, ?4, NULL, ?5, ?6)",
            params![
                operation_id,
                repo_id,
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
                singleton, repo_id, current_operation_id, current_view_id,
                attached_attempt_id, expected_content_ref, updated_at_ms
            ) VALUES (1, ?1, ?2, ?3, NULL, ?4, ?5)",
            params![repo_id, operation_id, view_id, content_ref, now],
        )?;
        Ok(())
    })?;
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

/// Record a `forge sync import --materialize` worktree update. The bundle import first
/// applies the remote native objects and ledger, then the CLI restores the imported HEAD's
/// tree into the worktree. This local op records that materialization without conflating it
/// with user-driven history navigation (`checkout`).
pub fn record_sync_import_materialized(
    cwd: &Path,
    request_id: Option<String>,
    commit_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = crate::open_repository(cwd)?;
    let mut connection = crate::open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        crate::replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = crate::insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "sync import".to_string(),
                kind: "sync_import_materialized".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": "sync_import_materialized",
                    "commit_id": commit_id,
                    "content_ref": content_ref
                }),
            },
        )?;
        tx.execute(
            "UPDATE current_state
             SET attached_attempt_id = NULL, expected_content_ref = ?1
             WHERE singleton = 1",
            params![content_ref],
        )?;
        Ok(op)
    })?;
    Ok(op)
}

pub struct SyncPullMaterializedInput {
    pub state: Value,
    pub content_ref: String,
}

/// Record a `forge sync pull` worktree update separately from `sync import`, so
/// command-scoped request-id replay returns the pull response instead of a
/// REQUEST_ID_CONFLICT against the lower-level import/materialize operation.
pub fn record_sync_pull_materialized(
    cwd: &Path,
    request_id: Option<String>,
    input: SyncPullMaterializedInput,
) -> Result<OperationViewResult> {
    let context = crate::open_repository(cwd)?;
    let mut connection = crate::open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        crate::replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let mut state = input.state.clone();
        if let Some(object) = state.as_object_mut() {
            object.insert("lifecycle".to_string(), json!("sync_pull_materialized"));
        }
        let op = crate::insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "sync pull".to_string(),
                kind: "sync_pull_materialized".to_string(),
                view_kind: ViewKind::Initialized,
                state,
            },
        )?;
        tx.execute(
            "UPDATE current_state
             SET attached_attempt_id = NULL, expected_content_ref = ?1
             WHERE singleton = 1",
            params![input.content_ref],
        )?;
        Ok(op)
    })?;
    Ok(op)
}

pub struct SyncMergeCommitInput<'a> {
    pub protocol_version: &'a str,
    pub direction: &'a str,
    pub remote_path: &'a Path,
    pub base_native_head: &'a str,
    pub ours_native_head: &'a str,
    pub theirs_native_head: &'a str,
    pub merged_content_ref: &'a str,
    pub materialized: bool,
    pub imported_native_objects: i64,
    pub imported_ledger_rows: i64,
}

pub struct SyncMergeCommitResult {
    pub operation: OperationViewResult,
    pub commit_id: String,
}

/// Record a clean divergent peer sync as a local native merge commit.
///
/// The merge commit object is written before the op-log row that references it. HEAD advances
/// only after that DB row commits, matching accept's HEAD-lags-never-leads crash ordering. Fetch
/// advances native history without claiming the worktree changed; pull updates the expected
/// content ref only after the CLI materializes so an interrupted restore is recoverable by retry.
pub fn record_sync_merge_commit(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    input: SyncMergeCommitInput<'_>,
) -> Result<SyncMergeCommitResult> {
    let context = crate::open_repository(cwd)?;
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let signer = signing::LocalSigner::load_or_create(&context.root_path)?;
    let kind = match input.direction {
        "fetch" | "pull" | "push" => format!("sync_{}_merged", input.direction),
        other => bail!("unsupported sync merge direction: {other}"),
    };
    let tree = input
        .merged_content_ref
        .strip_prefix(forge_content::FORGE_TREE_PREFIX)
        .ok_or_else(|| anyhow!("sync merge produced a non-forge-tree content ref"))?
        .to_string();
    forge_content_native::ObjectId::parse(input.ours_native_head)?;
    forge_content_native::ObjectId::parse(input.theirs_native_head)?;
    let mut connection = crate::open_connection(&context.database_path)?;
    let created = now_ms();
    let (op, commit_id) = with_immediate_retry(&mut connection, |tx| {
        crate::replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let commit = forge_content_native::CommitObject {
            schema_version: forge_content_native::COMMIT_SCHEMA_VERSION,
            tree: tree.clone(),
            parents: vec![
                input.ours_native_head.to_string(),
                input.theirs_native_head.to_string(),
            ],
            intent_id: None,
            proposal_revision_id: None,
            decision_id: None,
            evidence_digest: None,
            actor: Some("forge-sync".to_string()),
            authored_time: Some(created),
        };
        let commit_id = store.write_commit(&commit)?.to_string();
        signer.sign_subject(
            tx,
            &context.repo_id,
            "sync_merge_commit",
            &commit_id,
            &commit_id,
            created,
        )?;
        let remote_path = input.remote_path.display().to_string();
        let sync_merge_lineage_hash =
            integrity::sync_merge_lineage_digest(&integrity::SyncMergeLineageDigestInput {
                protocol_version: input.protocol_version,
                direction: input.direction,
                remote_path: &remote_path,
                base_native_head: input.base_native_head,
                ours_native_head: input.ours_native_head,
                theirs_native_head: input.theirs_native_head,
                merged_content_ref: input.merged_content_ref,
                commit_id: &commit_id,
                materialized: input.materialized,
                imported_native_objects: input.imported_native_objects,
                imported_ledger_rows: input.imported_ledger_rows,
            });
        let op = crate::insert_operation_view_chained(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: command.to_string(),
                kind: kind.clone(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": kind,
                    "protocol_version": input.protocol_version,
                    "direction": input.direction,
                    "remote_path": input.remote_path.display().to_string(),
                    "base_native_head": input.base_native_head,
                    "ours_native_head": input.ours_native_head,
                    "theirs_native_head": input.theirs_native_head,
                    "merged_content_ref": input.merged_content_ref,
                    "commit_id": commit_id,
                    "materialized": input.materialized,
                    "imported_native_objects": input.imported_native_objects,
                    "imported_ledger_rows": input.imported_ledger_rows,
                    "sync_merge_lineage_hash": sync_merge_lineage_hash,
                }),
            },
            Some(&sync_merge_lineage_hash),
        )?;
        Ok((op, commit_id))
    })?;
    let refs = forge_content_native::NativeRefStore::new(&context.root_path);
    refs.set_head(&forge_content_native::ObjectId::parse(&commit_id)?)?;
    Ok(SyncMergeCommitResult {
        operation: op,
        commit_id,
    })
}

/// Claim a local request-id for a local peer sync command.
///
/// Some sync outcomes are natural no-ops or apply their primary side effect in
/// a peer repo. The CLI's idempotency preflight still runs in the initiating
/// repo, so this marker keeps the initiator's request-id namespace honest
/// without pretending the command materialized local content.
pub fn record_sync_request_marker(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    direction: &str,
    remote_path: &Path,
    remote_operation_id: Option<&str>,
    replay_data: Option<Value>,
) -> Result<OperationViewResult> {
    let context = crate::open_repository(cwd)?;
    let mut connection = crate::open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        crate::replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        crate::insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: command.to_string(),
                kind: format!("sync_{direction}"),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": format!("sync_{direction}"),
                    "remote_path": remote_path.display().to_string(),
                    "remote_operation_id": remote_operation_id,
                    "replay_data": replay_data,
                }),
            },
        )
    })
}

pub fn set_sync_clone_expected_content_ref(cwd: &Path, content_ref: &str) -> Result<()> {
    let context = crate::open_repository(cwd)?;
    let mut connection = crate::open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        tx.execute(
            "UPDATE current_state
             SET attached_attempt_id = NULL, expected_content_ref = ?1, updated_at_ms = ?2
             WHERE singleton = 1",
            params![content_ref, now_ms()],
        )?;
        Ok(())
    })
}

pub fn record_sync_merge_conflict(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    input: &MergeConflictInput,
) -> Result<MergeConflictRecord> {
    crate::record_merge_conflict_inner(cwd, request_id, command, input, true)
}
