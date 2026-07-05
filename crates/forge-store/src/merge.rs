use super::*;

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
        set_context_expected_content_ref(tx, &context, &input.merged_content_ref)?;
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
