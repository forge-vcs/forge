use serde::Serialize;

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct EmbargoWorkflowRecord {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_projection_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_actor_ref: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbargoWorkflowEventRecord {
    pub event_id: String,
    pub work_package_kind: String,
    pub work_package_id: String,
    pub action: String,
    pub actor: String,
    pub authority: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_state: Option<String>,
    pub policy_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_authorization_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_projection_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_actor_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_classes_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_summary_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_digest: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbargoWorkflowResult {
    pub workflow: EmbargoWorkflowRecord,
    pub event: EmbargoWorkflowEventRecord,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbargoReleasePlan {
    pub release_event_id: String,
    pub recipient: String,
    pub policy_revision: i64,
    pub content_classes: Vec<String>,
    pub generated_at_ms: i64,
    pub revocation_warning: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbargoReleaseRecord {
    pub workflow: EmbargoWorkflowRecord,
    pub event: EmbargoWorkflowEventRecord,
    pub release_authorization_id: String,
    pub recipient: String,
    pub policy_revision: i64,
    pub content_classes: Vec<String>,
    pub generated_at_ms: i64,
    pub revocation_warning: String,
}

fn embargo_release_content_classes(content_classes: &[String]) -> Vec<String> {
    let selected = if content_classes.is_empty() {
        DEFAULT_EMBARGO_RELEASE_CONTENT_CLASSES
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    } else {
        content_classes.to_vec()
    };
    selected
}

fn validate_embargo_release_content_classes(content_classes: &[String]) -> Result<()> {
    for content_class in content_classes {
        if !DEFAULT_EMBARGO_RELEASE_CONTENT_CLASSES.contains(&content_class.as_str()) {
            bail!("unsupported embargo release content class `{content_class}`");
        }
    }
    Ok(())
}

pub(crate) fn embargo_workflow_required(
    operation: &str,
    work_package_kind: &str,
    work_package_id: &str,
) -> ForgeError {
    ForgeError::EmbargoWorkflowRequired {
        operation: operation.to_string(),
        work_package_kind: work_package_kind.to_string(),
        work_package_id: work_package_id.to_string(),
    }
}

fn embargo_state_invalid(action: &str, state: &str, required: &str) -> ForgeError {
    ForgeError::EmbargoStateInvalid {
        action: action.to_string(),
        state: state.to_string(),
        required: required.to_string(),
    }
}

pub fn mark_embargo_workflow(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<EmbargoWorkflowResult> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let (authority, policy_revision) =
            embargo_authority_on(tx, &context.repo_id, actor, "embargo_mark")?;
        let prior = embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?;
        if let Some(record) = prior.as_ref() {
            if record.state != EMBARGO_STATE_ACTIVE {
                return Err(
                    embargo_state_invalid("mark", &record.state, EMBARGO_STATE_ACTIVE).into(),
                );
            }
        }
        let prior_state = prior.as_ref().map(|record| record.state.as_str());
        let now = now_ms();
        upsert_work_package_visibility(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            VISIBILITY_EMBARGOED,
            now,
        )?;
        tx.execute(
            "INSERT INTO embargo_workflows (
                repo_id, work_package_kind, work_package_id, state,
                public_projection_mode, public_actor_ref, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, ?5)
             ON CONFLICT(repo_id, work_package_kind, work_package_id)
             DO UPDATE SET state = excluded.state, updated_at_ms = excluded.updated_at_ms",
            params![
                context.repo_id,
                work_package_kind,
                work_package_id,
                EMBARGO_STATE_ACTIVE,
                now
            ],
        )?;
        insert_visibility_audit(
            tx,
            &context.repo_id,
            Some(work_package_kind),
            Some(work_package_id),
            "set_visibility",
            actor,
            prior_state.map(|_| VISIBILITY_EMBARGOED),
            Some(VISIBILITY_EMBARGOED),
            None,
            None,
            reason,
            now,
        )?;
        let event = insert_embargo_event(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "mark",
            actor,
            &authority,
            prior_state,
            Some(EMBARGO_STATE_ACTIVE),
            policy_revision,
            reason,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            now,
        )?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| anyhow!("embargo workflow missing after mark"))?;
        Ok(EmbargoWorkflowResult { workflow, event })
    })
}

pub fn grant_embargo_capability(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    capability: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<EmbargoWorkflowResult> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_capability(capability)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| {
                    embargo_workflow_required(
                        "grant_capability",
                        work_package_kind,
                        work_package_id,
                    )
                })?;
        if matches!(
            workflow.state.as_str(),
            EMBARGO_STATE_CLOSED | EMBARGO_STATE_PUBLISHED
        ) {
            return Err(embargo_state_invalid(
                "grant",
                &workflow.state,
                "active, accepted_under_embargo, released_under_embargo, or revealed",
            )
            .into());
        }
        let (authority, policy_revision) =
            embargo_authority_on(tx, &context.repo_id, actor, "embargo_grant")?;
        let now = now_ms();
        let existing_id: Option<String> = tx
            .query_row(
                "SELECT id FROM visibility_grants
                 WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3
                   AND recipient = ?4 AND capability = ?5",
                params![
                    context.repo_id,
                    work_package_kind,
                    work_package_id,
                    recipient,
                    capability
                ],
                |row| row.get(0),
            )
            .optional()?;
        let grant_id = existing_id.unwrap_or_else(|| new_id("grant"));
        tx.execute(
            "INSERT INTO visibility_grants (
                id, repo_id, work_package_kind, work_package_id, recipient,
                capability, created_at_ms, revoked_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
             ON CONFLICT(repo_id, work_package_kind, work_package_id, recipient, capability)
             DO UPDATE SET revoked_at_ms = NULL",
            params![
                grant_id,
                context.repo_id,
                work_package_kind,
                work_package_id,
                recipient,
                capability,
                now
            ],
        )?;
        insert_visibility_audit(
            tx,
            &context.repo_id,
            Some(work_package_kind),
            Some(work_package_id),
            "grant_capability",
            actor,
            None,
            None,
            Some(recipient),
            Some(capability),
            reason,
            now,
        )?;
        let event = insert_embargo_event(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "grant",
            actor,
            &authority,
            Some(&workflow.state),
            Some(&workflow.state),
            policy_revision,
            reason,
            Some(recipient),
            Some(capability),
            None,
            None,
            None,
            None,
            None,
            None,
            now,
        )?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| anyhow!("embargo workflow missing after grant"))?;
        Ok(EmbargoWorkflowResult { workflow, event })
    })
}

pub fn revoke_embargo_capability(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    capability: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<EmbargoWorkflowResult> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_capability(capability)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| {
                    embargo_workflow_required(
                        "revoke_capability",
                        work_package_kind,
                        work_package_id,
                    )
                })?;
        let (authority, policy_revision) =
            embargo_authority_on(tx, &context.repo_id, actor, "embargo_revoke")?;
        let grant_id: Option<String> = tx
            .query_row(
                "SELECT id FROM visibility_grants
                 WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3
                   AND recipient = ?4 AND capability = ?5",
                params![
                    context.repo_id,
                    work_package_kind,
                    work_package_id,
                    recipient,
                    capability
                ],
                |row| row.get(0),
            )
            .optional()?;
        let Some(grant_id) = grant_id else {
            return Err(ForgeError::VisibilityPolicyUnmet {
                operation: "embargo_revoke".to_string(),
                work_package_kind: work_package_kind.to_string(),
                work_package_id: work_package_id.to_string(),
                capability: capability.to_string(),
                disclosure: "hidden".to_string(),
            }
            .into());
        };
        let now = now_ms();
        tx.execute(
            "UPDATE visibility_grants
             SET revoked_at_ms = ?1
             WHERE id = ?2 AND repo_id = ?3",
            params![now, grant_id, context.repo_id],
        )?;
        insert_visibility_audit(
            tx,
            &context.repo_id,
            Some(work_package_kind),
            Some(work_package_id),
            "revoke_capability",
            actor,
            None,
            None,
            Some(recipient),
            Some(capability),
            reason,
            now,
        )?;
        let event = insert_embargo_event(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "revoke",
            actor,
            &authority,
            Some(&workflow.state),
            Some(&workflow.state),
            policy_revision,
            reason,
            Some(recipient),
            Some(capability),
            None,
            None,
            None,
            None,
            None,
            None,
            now,
        )?;
        Ok(EmbargoWorkflowResult { workflow, event })
    })
}

pub fn prepare_embargo_release_workflow(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    actor: &str,
    content_classes: &[String],
    _reason: Option<&str>,
) -> Result<EmbargoReleasePlan> {
    validate_work_package_kind(work_package_kind)?;
    let content_classes = embargo_release_content_classes(content_classes);
    validate_embargo_release_content_classes(&content_classes)?;
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    ensure_work_package_exists(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
    )?;
    let workflow = embargo_workflow_on(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
    )?
    .ok_or_else(|| embargo_workflow_required("release", work_package_kind, work_package_id))?;
    if !matches!(
        workflow.state.as_str(),
        EMBARGO_STATE_ACCEPTED_UNDER_EMBARGO | EMBARGO_STATE_RELEASED_UNDER_EMBARGO
    ) {
        return Err(embargo_state_invalid(
            "release",
            &workflow.state,
            "accepted_under_embargo or released_under_embargo",
        )
        .into());
    }
    if !has_active_visibility_grant(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
        recipient,
        CAPABILITY_SYNC_MATERIALIZE,
    )? {
        return Err(ForgeError::VisibilityPolicyUnmet {
            operation: "embargo_release".to_string(),
            work_package_kind: work_package_kind.to_string(),
            work_package_id: work_package_id.to_string(),
            capability: CAPABILITY_SYNC_MATERIALIZE.to_string(),
            disclosure: "hidden".to_string(),
        }
        .into());
    }
    if !has_active_visibility_grant(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
        recipient,
        CAPABILITY_PUBLISH_REVEAL,
    )? {
        return Err(ForgeError::VisibilityPolicyUnmet {
            operation: "embargo_release".to_string(),
            work_package_kind: work_package_kind.to_string(),
            work_package_id: work_package_id.to_string(),
            capability: CAPABILITY_PUBLISH_REVEAL.to_string(),
            disclosure: "hidden".to_string(),
        }
        .into());
    }
    let (_authority, policy_revision) =
        embargo_authority_on(&connection, &context.repo_id, actor, "embargo_release")?;
    let now = now_ms();
    Ok(EmbargoReleasePlan {
        release_event_id: new_id("embargo_event"),
        recipient: recipient.to_string(),
        policy_revision,
        content_classes,
        generated_at_ms: now,
        revocation_warning: EMBARGO_RELEASE_REVOCATION_WARNING.to_string(),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn finish_embargo_release_workflow(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    actor: &str,
    content_classes: &[String],
    release_event_id: &str,
    expected_policy_revision: i64,
    generated_at_ms: i64,
    bundle_digest: &str,
    reason: Option<&str>,
) -> Result<EmbargoReleaseRecord> {
    validate_work_package_kind(work_package_kind)?;
    let content_classes = embargo_release_content_classes(content_classes);
    validate_embargo_release_content_classes(&content_classes)?;
    let content_classes_json = serde_json::to_string(&content_classes)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| {
                    embargo_workflow_required("release", work_package_kind, work_package_id)
                })?;
        if !matches!(
            workflow.state.as_str(),
            EMBARGO_STATE_ACCEPTED_UNDER_EMBARGO | EMBARGO_STATE_RELEASED_UNDER_EMBARGO
        ) {
            return Err(embargo_state_invalid(
                "release",
                &workflow.state,
                "accepted_under_embargo or released_under_embargo",
            )
            .into());
        }
        if !has_active_visibility_grant(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            recipient,
            CAPABILITY_SYNC_MATERIALIZE,
        )? {
            return Err(ForgeError::VisibilityPolicyUnmet {
                operation: "embargo_release".to_string(),
                work_package_kind: work_package_kind.to_string(),
                work_package_id: work_package_id.to_string(),
                capability: CAPABILITY_SYNC_MATERIALIZE.to_string(),
                disclosure: "hidden".to_string(),
            }
            .into());
        }
        if !has_active_visibility_grant(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            recipient,
            CAPABILITY_PUBLISH_REVEAL,
        )? {
            return Err(ForgeError::VisibilityPolicyUnmet {
                operation: "embargo_release".to_string(),
                work_package_kind: work_package_kind.to_string(),
                work_package_id: work_package_id.to_string(),
                capability: CAPABILITY_PUBLISH_REVEAL.to_string(),
                disclosure: "hidden".to_string(),
            }
            .into());
        }
        let (authority, policy_revision) =
            embargo_authority_on(tx, &context.repo_id, actor, "embargo_release")?;
        if policy_revision != expected_policy_revision {
            bail!("embargo release authority changed while building release bundle");
        }
        let release_authorization_id = new_id("embargo_release");
        tx.execute(
            "INSERT INTO embargo_release_authorizations (
                id, repo_id, work_package_kind, work_package_id, recipient, authority,
                policy_revision, content_classes_json, reason, created_at_ms, revoked_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL)
             ON CONFLICT(repo_id, work_package_kind, work_package_id, recipient)
             DO UPDATE SET authority = excluded.authority,
                           policy_revision = excluded.policy_revision,
                           content_classes_json = excluded.content_classes_json,
                           reason = excluded.reason,
                           created_at_ms = excluded.created_at_ms,
                           revoked_at_ms = NULL",
            params![
                release_authorization_id,
                context.repo_id,
                work_package_kind,
                work_package_id,
                recipient,
                authority,
                policy_revision,
                content_classes_json,
                reason,
                generated_at_ms
            ],
        )?;
        let stored_release_authorization_id: String = tx.query_row(
            "SELECT id FROM embargo_release_authorizations
             WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3 AND recipient = ?4",
            params![context.repo_id, work_package_kind, work_package_id, recipient],
            |row| row.get(0),
        )?;
        tx.execute(
            "UPDATE embargo_workflows
             SET state = ?1, updated_at_ms = ?2
             WHERE repo_id = ?3 AND work_package_kind = ?4 AND work_package_id = ?5",
            params![
                EMBARGO_STATE_RELEASED_UNDER_EMBARGO,
                generated_at_ms,
                context.repo_id,
                work_package_kind,
                work_package_id
            ],
        )?;
        let event = insert_embargo_event_with_id(
            tx,
            release_event_id,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "release",
            actor,
            &authority,
            Some(&workflow.state),
            Some(EMBARGO_STATE_RELEASED_UNDER_EMBARGO),
            policy_revision,
            reason,
            Some(recipient),
            Some(CAPABILITY_SYNC_MATERIALIZE),
            Some(&stored_release_authorization_id),
            None,
            None,
            Some(&content_classes_json),
            Some("{}"),
            Some(bundle_digest),
            generated_at_ms,
        )?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| anyhow!("embargo workflow missing after release"))?;
        Ok(EmbargoReleaseRecord {
            workflow,
            event,
            release_authorization_id: stored_release_authorization_id,
            recipient: recipient.to_string(),
            policy_revision,
            content_classes: content_classes.clone(),
            generated_at_ms,
            revocation_warning: EMBARGO_RELEASE_REVOCATION_WARNING.to_string(),
        })
    })
}

pub fn reveal_embargo_workflow(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    actor: &str,
    public_projection_mode: &str,
    public_actor_ref: Option<&str>,
    reason: Option<&str>,
) -> Result<EmbargoWorkflowResult> {
    validate_work_package_kind(work_package_kind)?;
    validate_public_projection_mode(public_projection_mode)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| {
                    embargo_workflow_required("reveal", work_package_kind, work_package_id)
                })?;
        if !matches!(
            workflow.state.as_str(),
            EMBARGO_STATE_ACCEPTED_UNDER_EMBARGO | EMBARGO_STATE_RELEASED_UNDER_EMBARGO
        ) {
            return Err(embargo_state_invalid(
                "reveal",
                &workflow.state,
                "accepted_under_embargo or released_under_embargo",
            )
            .into());
        }
        let (authority, policy_revision) =
            embargo_authority_on(tx, &context.repo_id, actor, "embargo_reveal")?;
        let now = now_ms();
        tx.execute(
            "UPDATE embargo_workflows
             SET state = ?1, public_projection_mode = ?2, public_actor_ref = ?3, updated_at_ms = ?4
             WHERE repo_id = ?5 AND work_package_kind = ?6 AND work_package_id = ?7",
            params![
                EMBARGO_STATE_REVEALED,
                public_projection_mode,
                public_actor_ref,
                now,
                context.repo_id,
                work_package_kind,
                work_package_id
            ],
        )?;
        let event = insert_embargo_event(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "reveal",
            actor,
            &authority,
            Some(&workflow.state),
            Some(EMBARGO_STATE_REVEALED),
            policy_revision,
            reason,
            None,
            None,
            None,
            Some(public_projection_mode),
            public_actor_ref,
            None,
            None,
            None,
            now,
        )?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| anyhow!("embargo workflow missing after reveal"))?;
        Ok(EmbargoWorkflowResult { workflow, event })
    })
}

pub fn publish_embargo_workflow(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<EmbargoWorkflowResult> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| {
                    embargo_workflow_required("publish", work_package_kind, work_package_id)
                })?;
        if workflow.state != EMBARGO_STATE_REVEALED {
            return Err(
                embargo_state_invalid("publish", &workflow.state, EMBARGO_STATE_REVEALED).into(),
            );
        }
        let (authority, policy_revision) =
            embargo_authority_on(tx, &context.repo_id, actor, "embargo_publish")?;
        let now = now_ms();
        tx.execute(
            "UPDATE embargo_workflows
             SET state = ?1, updated_at_ms = ?2
             WHERE repo_id = ?3 AND work_package_kind = ?4 AND work_package_id = ?5",
            params![
                EMBARGO_STATE_PUBLISHED,
                now,
                context.repo_id,
                work_package_kind,
                work_package_id
            ],
        )?;
        let event = insert_embargo_event(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "publish",
            actor,
            &authority,
            Some(&workflow.state),
            Some(EMBARGO_STATE_PUBLISHED),
            policy_revision,
            reason,
            None,
            None,
            None,
            workflow.public_projection_mode.as_deref(),
            workflow.public_actor_ref.as_deref(),
            None,
            None,
            None,
            now,
        )?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| anyhow!("embargo workflow missing after publish"))?;
        Ok(EmbargoWorkflowResult { workflow, event })
    })
}

pub fn close_embargo_workflow(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<EmbargoWorkflowResult> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| {
                    embargo_workflow_required("close", work_package_kind, work_package_id)
                })?;
        if matches!(
            workflow.state.as_str(),
            EMBARGO_STATE_CLOSED | EMBARGO_STATE_PUBLISHED
        ) {
            return Err(embargo_state_invalid(
                "close",
                &workflow.state,
                "active, accepted_under_embargo, released_under_embargo, or revealed",
            )
            .into());
        }
        let (authority, policy_revision) =
            embargo_authority_on(tx, &context.repo_id, actor, "embargo_close")?;
        let now = now_ms();
        tx.execute(
            "UPDATE embargo_workflows
             SET state = ?1, updated_at_ms = ?2
             WHERE repo_id = ?3 AND work_package_kind = ?4 AND work_package_id = ?5",
            params![
                EMBARGO_STATE_CLOSED,
                now,
                context.repo_id,
                work_package_kind,
                work_package_id
            ],
        )?;
        let event = insert_embargo_event(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "close",
            actor,
            &authority,
            Some(&workflow.state),
            Some(EMBARGO_STATE_CLOSED),
            policy_revision,
            reason,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            now,
        )?;
        let workflow =
            embargo_workflow_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .ok_or_else(|| anyhow!("embargo workflow missing after close"))?;
        Ok(EmbargoWorkflowResult { workflow, event })
    })
}

pub fn ensure_embargo_publishable(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<()> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    ensure_work_package_exists(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
    )?;
    let workflow = embargo_workflow_on(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
    )?;
    let visibility = effective_work_package_visibility_on(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
    )?;
    if visibility != VISIBILITY_EMBARGOED && workflow.is_none() {
        return Ok(());
    }
    let state = workflow
        .as_ref()
        .map(|record| record.state.as_str())
        .unwrap_or(VISIBILITY_EMBARGOED);
    if state != EMBARGO_STATE_PUBLISHED {
        return Err(embargo_state_invalid("export_branch", state, EMBARGO_STATE_PUBLISHED).into());
    }
    let mode = workflow
        .as_ref()
        .and_then(|record| record.public_projection_mode.as_deref())
        .unwrap_or("");
    if mode != PUBLIC_PROJECTION_FULL_SOURCE {
        return Err(
            embargo_state_invalid("export_branch", mode, PUBLIC_PROJECTION_FULL_SOURCE).into(),
        );
    }
    Ok(())
}

pub(crate) fn embargo_workflow_on(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<Option<EmbargoWorkflowRecord>> {
    conn.query_row(
        "SELECT work_package_kind, work_package_id, state, public_projection_mode,
                public_actor_ref, created_at_ms, updated_at_ms
         FROM embargo_workflows
         WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3",
        params![repo_id, work_package_kind, work_package_id],
        |row| {
            Ok(EmbargoWorkflowRecord {
                work_package_kind: row.get(0)?,
                work_package_id: row.get(1)?,
                state: row.get(2)?,
                public_projection_mode: row.get(3)?,
                public_actor_ref: row.get(4)?,
                created_at_ms: row.get(5)?,
                updated_at_ms: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn insert_embargo_event(
    tx: &Transaction<'_>,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    action: &str,
    actor: &str,
    authority: &str,
    prior_state: Option<&str>,
    new_state: Option<&str>,
    policy_revision: i64,
    reason: Option<&str>,
    recipient: Option<&str>,
    capability: Option<&str>,
    release_authorization_id: Option<&str>,
    public_projection_mode: Option<&str>,
    public_actor_ref: Option<&str>,
    content_classes_json: Option<&str>,
    check_summary_json: Option<&str>,
    bundle_digest: Option<&str>,
    now: i64,
) -> Result<EmbargoWorkflowEventRecord> {
    let event_id = new_id("embargo_event");
    insert_embargo_event_with_id(
        tx,
        &event_id,
        repo_id,
        work_package_kind,
        work_package_id,
        action,
        actor,
        authority,
        prior_state,
        new_state,
        policy_revision,
        reason,
        recipient,
        capability,
        release_authorization_id,
        public_projection_mode,
        public_actor_ref,
        content_classes_json,
        check_summary_json,
        bundle_digest,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn insert_embargo_event_with_id(
    tx: &Transaction<'_>,
    event_id: &str,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    action: &str,
    actor: &str,
    authority: &str,
    prior_state: Option<&str>,
    new_state: Option<&str>,
    policy_revision: i64,
    reason: Option<&str>,
    recipient: Option<&str>,
    capability: Option<&str>,
    release_authorization_id: Option<&str>,
    public_projection_mode: Option<&str>,
    public_actor_ref: Option<&str>,
    content_classes_json: Option<&str>,
    check_summary_json: Option<&str>,
    bundle_digest: Option<&str>,
    now: i64,
) -> Result<EmbargoWorkflowEventRecord> {
    tx.execute(
        "INSERT INTO embargo_workflow_events (
            id, repo_id, work_package_kind, work_package_id, action, actor, authority,
            prior_state, new_state, policy_revision, reason, recipient, capability,
            release_authorization_id, public_projection_mode, public_actor_ref,
            content_classes_json, check_summary_json, bundle_digest, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            event_id,
            repo_id,
            work_package_kind,
            work_package_id,
            action,
            actor,
            authority,
            prior_state,
            new_state,
            policy_revision,
            reason,
            recipient,
            capability,
            release_authorization_id,
            public_projection_mode,
            public_actor_ref,
            content_classes_json,
            check_summary_json,
            bundle_digest,
            now
        ],
    )?;
    Ok(EmbargoWorkflowEventRecord {
        event_id: event_id.to_string(),
        work_package_kind: work_package_kind.to_string(),
        work_package_id: work_package_id.to_string(),
        action: action.to_string(),
        actor: actor.to_string(),
        authority: authority.to_string(),
        prior_state: prior_state.map(str::to_string),
        new_state: new_state.map(str::to_string),
        policy_revision,
        reason: reason.map(str::to_string),
        recipient: recipient.map(str::to_string),
        capability: capability.map(str::to_string),
        release_authorization_id: release_authorization_id.map(str::to_string),
        public_projection_mode: public_projection_mode.map(str::to_string),
        public_actor_ref: public_actor_ref.map(str::to_string),
        content_classes_json: content_classes_json.map(str::to_string),
        check_summary_json: check_summary_json.map(str::to_string),
        bundle_digest: bundle_digest.map(str::to_string),
        created_at_ms: now,
    })
}

pub(crate) fn record_embargo_accept_on(
    tx: &Transaction<'_>,
    repo_id: &str,
    proposal_id: &str,
    actor: &str,
    created_at_ms: i64,
) -> Result<()> {
    let workflow = embargo_workflow_on(tx, repo_id, "proposal", proposal_id)?;
    let visibility = effective_work_package_visibility_on(tx, repo_id, "proposal", proposal_id)?;
    if workflow.is_none() && visibility != VISIBILITY_EMBARGOED {
        return Ok(());
    }
    let prior_state = workflow
        .as_ref()
        .map(|record| record.state.as_str())
        .unwrap_or(EMBARGO_STATE_ACTIVE);
    if matches!(
        prior_state,
        EMBARGO_STATE_CLOSED | EMBARGO_STATE_REVEALED | EMBARGO_STATE_PUBLISHED
    ) {
        return Err(embargo_state_invalid(
            "accept",
            prior_state,
            "active or accepted_under_embargo",
        )
        .into());
    }
    let (authority, policy_revision) = embargo_authority_on(tx, repo_id, actor, "embargo_accept")?;
    upsert_work_package_visibility(
        tx,
        repo_id,
        "proposal",
        proposal_id,
        VISIBILITY_EMBARGOED,
        created_at_ms,
    )?;
    tx.execute(
        "INSERT INTO embargo_workflows (
            repo_id, work_package_kind, work_package_id, state,
            public_projection_mode, public_actor_ref, created_at_ms, updated_at_ms
         ) VALUES (?1, 'proposal', ?2, ?3, NULL, NULL, ?4, ?4)
         ON CONFLICT(repo_id, work_package_kind, work_package_id)
         DO UPDATE SET state = excluded.state, updated_at_ms = excluded.updated_at_ms",
        params![
            repo_id,
            proposal_id,
            EMBARGO_STATE_ACCEPTED_UNDER_EMBARGO,
            created_at_ms
        ],
    )?;
    insert_embargo_event(
        tx,
        repo_id,
        "proposal",
        proposal_id,
        "accept",
        actor,
        &authority,
        Some(prior_state),
        Some(EMBARGO_STATE_ACCEPTED_UNDER_EMBARGO),
        policy_revision,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        created_at_ms,
    )?;
    Ok(())
}
