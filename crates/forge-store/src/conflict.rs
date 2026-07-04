use serde::Serialize;

use super::*;

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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<ConflictResolutionSuggestion>,
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
pub struct ConflictResolutionSuggestion {
    pub suggestion_id: String,
    pub rank: i64,
    pub resolution_ref: String,
    pub strategy: String,
    pub confidence: String,
    pub requires_explicit_resolve: bool,
    pub provenance: ConflictResolutionSuggestionProvenance,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictResolutionSuggestionProvenance {
    pub conflict_set_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_revision_id: Option<String>,
    pub evidence_input_count: i64,
    pub evidence_input_status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence_input_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_input_status: Option<String>,
    pub intent_input_status: String,
    pub path_conflict_ids: Vec<String>,
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct ConflictSuggestionInputs {
    proposal_id: Option<String>,
    proposal_revision_id: Option<String>,
    evidence_input_ids: Vec<String>,
    check_input_status: Option<String>,
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
    record_merge_conflict_inner(cwd, request_id, command, input, false)
}

pub(crate) fn record_merge_conflict_inner(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    input: &MergeConflictInput,
    dedup_unrequested_sync_conflict: bool,
) -> Result<MergeConflictRecord> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let conflict_set_id = new_id("conflict");
    let now = now_ms();
    with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        if request_id.is_none() && dedup_unrequested_sync_conflict {
            if let Some(existing) = existing_native_merge_conflict(tx, &context.repo_id, input)? {
                return Ok(existing);
            }
        }
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
        Ok(MergeConflictRecord {
            conflict_set_id: conflict_set_id.clone(),
            operation_id: operation_id.clone(),
            view_id: view_id.clone(),
        })
    })
}

fn existing_native_merge_conflict(
    tx: &Transaction<'_>,
    repo_id: &str,
    input: &MergeConflictInput,
) -> Result<Option<MergeConflictRecord>> {
    tx.query_row(
        "SELECT cs.id, cs.generated_by_operation_id, o.resulting_view_id
         FROM conflict_sets cs
         JOIN operations o ON o.id = cs.generated_by_operation_id
         WHERE cs.repo_id = ?1
           AND cs.context = ?2
           AND cs.base_content_ref = ?3
           AND cs.ours_content_ref = ?4
           AND cs.theirs_content_ref = ?5
           AND cs.resolver_backend = 'native_merge'
           AND cs.status IN ('unresolved', 'partially_resolved', 'resolved')
         ORDER BY cs.created_at_ms DESC, cs.rowid DESC
         LIMIT 1",
        params![
            repo_id,
            input.context,
            input.base_content_ref,
            input.ours_content_ref,
            input.theirs_content_ref
        ],
        |row| {
            Ok(MergeConflictRecord {
                conflict_set_id: row.get(0)?,
                operation_id: row.get(1)?,
                view_id: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
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
    let signer = signing::LocalSigner::load_or_create(&context.root_path)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let mut out: Option<ConflictResolutionRecord> = None;
    with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let (paths_json, status, resolver_backend): (String, String, Option<String>) = tx
            .query_row(
                "SELECT paths_json, status, resolver_backend FROM conflict_sets WHERE id = ?1 AND repo_id = ?2",
                params![conflict_set_id, context.repo_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| ForgeError::ConflictSetNotFound {
                conflict_set_id: conflict_set_id.to_string(),
            })?;
        if resolver_backend.as_deref() != Some("native_merge") {
            return Err(ForgeError::UnsupportedContentBackend {
                command: "conflict resolve".to_string(),
                required: "native_merge".to_string(),
                actual: resolver_backend.unwrap_or_else(|| "unknown".to_string()),
            }
            .into());
        }
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
        signer.sign_subject(
            tx,
            &context.repo_id,
            "evidence",
            &evidence_id,
            &evidence_hash,
            now,
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
        set_context_expected_content_ref(tx, &context, resolution_ref)?;
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
    let (paths_json, status, resolver_backend): (String, String, Option<String>) = connection
        .query_row(
            "SELECT paths_json, status, resolver_backend FROM conflict_sets WHERE id = ?1 AND repo_id = ?2",
            params![conflict_set_id, context.repo_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| ForgeError::ConflictSetNotFound {
            conflict_set_id: conflict_set_id.to_string(),
        })?;
    if resolver_backend.as_deref() != Some("native_merge") {
        return Err(ForgeError::UnsupportedContentBackend {
            command: "conflict resolve".to_string(),
            required: "native_merge".to_string(),
            actual: resolver_backend.unwrap_or_else(|| "unknown".to_string()),
        }
        .into());
    }
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

pub fn conflict_list(cwd: &Path) -> Result<ConflictListRecord> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    Ok(ConflictListRecord {
        conflicts: query_conflict_summaries(&connection, None)?,
    })
}

pub fn conflict_show(
    cwd: &Path,
    conflict_set_id: &str,
    suggest: bool,
) -> Result<ConflictShowRecord> {
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
        .collect::<rusqlite::Result<Vec<PathConflictSummary>>>()?;
    let suggestions = if suggest {
        let paths_json = conflict_paths_json(&connection, conflict_set_id)?;
        let inputs = conflict_suggestion_inputs(
            &connection,
            &context.repo_id,
            &paths_json,
            conflict.theirs_content_ref.as_deref(),
        )?;
        conflict_resolution_suggestions(&conflict, &path_conflicts, &inputs)
    } else {
        Vec::new()
    };
    Ok(ConflictShowRecord {
        conflict,
        path_conflicts,
        suggestions,
    })
}

fn conflict_resolution_suggestions(
    conflict: &ConflictSetSummary,
    path_conflicts: &[PathConflictSummary],
    inputs: &ConflictSuggestionInputs,
) -> Vec<ConflictResolutionSuggestion> {
    if conflict.status != "unresolved"
        || conflict.resolver_backend.as_deref() != Some("native_merge")
    {
        return Vec::new();
    }

    let path_conflict_ids = path_conflicts
        .iter()
        .map(|path_conflict| path_conflict.path_conflict_id.clone())
        .collect::<Vec<_>>();
    let source_refs = [
        conflict.base_content_ref.as_ref(),
        conflict.ours_content_ref.as_ref(),
        conflict.theirs_content_ref.as_ref(),
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect::<Vec<_>>();
    let evidence_input_count = inputs.evidence_input_ids.len() as i64;
    let evidence_input_status = if evidence_input_count == 0 {
        "empty"
    } else {
        "present"
    };
    let provenance = ConflictResolutionSuggestionProvenance {
        conflict_set_id: conflict.conflict_set_id.clone(),
        proposal_id: inputs.proposal_id.clone(),
        proposal_revision_id: inputs.proposal_revision_id.clone(),
        evidence_input_count,
        evidence_input_status: evidence_input_status.to_string(),
        evidence_input_ids: inputs.evidence_input_ids.clone(),
        check_input_status: inputs.check_input_status.clone(),
        intent_input_status: "conflict_set_metadata".to_string(),
        path_conflict_ids,
        source_refs,
    };
    let mut suggestions = Vec::new();
    if let Some(resolution_ref) = &conflict.ours_content_ref {
        suggestions.push(ConflictResolutionSuggestion {
            suggestion_id: "suggestion_keep_current_head".to_string(),
            rank: 1,
            resolution_ref: resolution_ref.clone(),
            strategy: "keep_current_head_tree".to_string(),
            confidence: "low".to_string(),
            requires_explicit_resolve: true,
            provenance: provenance.clone(),
        });
    }
    if let Some(resolution_ref) = &conflict.theirs_content_ref {
        let duplicate = conflict.ours_content_ref.as_ref() == Some(resolution_ref);
        if !duplicate {
            suggestions.push(ConflictResolutionSuggestion {
                suggestion_id: "suggestion_use_proposal_tree".to_string(),
                rank: suggestions.len() as i64 + 1,
                resolution_ref: resolution_ref.clone(),
                strategy: "use_proposal_tree".to_string(),
                confidence: "low".to_string(),
                requires_explicit_resolve: true,
                provenance,
            });
        }
    }
    suggestions
}

fn conflict_paths_json(connection: &Connection, conflict_set_id: &str) -> Result<String> {
    connection
        .query_row(
            "SELECT paths_json FROM conflict_sets WHERE id = ?1",
            params![conflict_set_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn conflict_suggestion_inputs(
    connection: &Connection,
    repo_id: &str,
    paths_json: &str,
    theirs_content_ref: Option<&str>,
) -> Result<ConflictSuggestionInputs> {
    let proposal_id = serde_json::from_str::<Value>(paths_json)
        .ok()
        .and_then(|value| {
            value
                .get("proposal_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(proposal_id) = proposal_id else {
        return Ok(ConflictSuggestionInputs::default());
    };
    let Some(theirs_content_ref) = theirs_content_ref else {
        return Ok(ConflictSuggestionInputs {
            proposal_id: Some(proposal_id),
            ..ConflictSuggestionInputs::default()
        });
    };
    let proposal: Option<(String, String)> = connection
        .query_row(
            "SELECT p.attempt_id, pr.id
             FROM proposals p
             JOIN proposal_revisions pr ON pr.proposal_id = p.id
             WHERE p.repo_id = ?1 AND p.id = ?2 AND pr.content_ref = ?3
             ORDER BY pr.created_at_ms DESC, pr.rowid DESC LIMIT 1",
            params![repo_id, proposal_id, theirs_content_ref],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((attempt_id, proposal_revision_id)) = proposal else {
        return Ok(ConflictSuggestionInputs {
            proposal_id: Some(proposal_id),
            ..ConflictSuggestionInputs::default()
        });
    };
    let mut evidence_statement = connection.prepare(
        "SELECT id FROM evidence
         WHERE repo_id = ?1 AND attempt_id = ?2
         ORDER BY created_at_ms DESC, rowid DESC",
    )?;
    let evidence_input_ids = evidence_statement
        .query_map(params![repo_id, attempt_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let check_input_status = connection
        .query_row(
            "SELECT status FROM check_results
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![repo_id, proposal_revision_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(ConflictSuggestionInputs {
        proposal_id: Some(proposal_id),
        proposal_revision_id: Some(proposal_revision_id),
        evidence_input_ids,
        check_input_status,
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
