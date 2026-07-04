use serde::Serialize;

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct VisibilityPolicy {
    pub default_work_package_visibility: String,
    pub supported_visibility_labels: Vec<String>,
    pub supported_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkPackageVisibilityRecord {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub visibility: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VisibilityAuditRecord {
    pub audit_id: String,
    pub work_package_kind: Option<String>,
    pub work_package_id: Option<String>,
    pub action: String,
    pub actor: String,
    pub prior_visibility: Option<String>,
    pub new_visibility: Option<String>,
    pub recipient: Option<String>,
    pub capability: Option<String>,
    pub reason: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct VisibilityGrantRecord {
    pub grant_id: String,
    pub work_package_kind: String,
    pub work_package_id: String,
    pub recipient: String,
    pub capability: String,
    pub created_at_ms: i64,
    pub revoked_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectionDecision {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub visibility: String,
    pub recipient: String,
    pub capability: String,
    pub allowed: bool,
    pub disclosure: String,
}

fn supported_visibility_labels() -> Vec<String> {
    [
        VISIBILITY_PRIVATE,
        VISIBILITY_TEAM,
        VISIBILITY_PUBLIC,
        VISIBILITY_EMBARGOED,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn supported_visibility_capabilities() -> Vec<String> {
    [
        CAPABILITY_SEE_STUB,
        CAPABILITY_INSPECT_CONTENT,
        CAPABILITY_INSPECT_EVIDENCE,
        CAPABILITY_SYNC_MATERIALIZE,
        CAPABILITY_PUBLISH_REVEAL,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub(crate) fn validate_visibility_label(label: &str) -> Result<()> {
    if matches!(
        label,
        VISIBILITY_PRIVATE | VISIBILITY_TEAM | VISIBILITY_PUBLIC | VISIBILITY_EMBARGOED
    ) {
        Ok(())
    } else {
        Err(ForgeError::VisibilityPolicyInvalid {
            reason: format!("unsupported visibility label `{label}`"),
        }
        .into())
    }
}

pub(crate) fn validate_visibility_capability(capability: &str) -> Result<()> {
    if matches!(
        capability,
        CAPABILITY_SEE_STUB
            | CAPABILITY_INSPECT_CONTENT
            | CAPABILITY_INSPECT_EVIDENCE
            | CAPABILITY_SYNC_MATERIALIZE
            | CAPABILITY_PUBLISH_REVEAL
    ) {
        Ok(())
    } else {
        Err(ForgeError::VisibilityPolicyInvalid {
            reason: format!("unsupported visibility capability `{capability}`"),
        }
        .into())
    }
}

pub(crate) fn validate_work_package_kind(kind: &str) -> Result<()> {
    if matches!(kind, "intent" | "attempt" | "proposal") {
        Ok(())
    } else {
        Err(ForgeError::VisibilityPolicyInvalid {
            reason: format!("unsupported work package kind `{kind}`"),
        }
        .into())
    }
}

pub(crate) fn validate_public_projection_mode(mode: &str) -> Result<()> {
    if matches!(
        mode,
        PUBLIC_PROJECTION_PROVENANCE_ONLY
            | PUBLIC_PROJECTION_SANITIZED_SOURCE
            | PUBLIC_PROJECTION_FULL_SOURCE
    ) {
        Ok(())
    } else {
        Err(ForgeError::VisibilityPolicyInvalid {
            reason: format!("unsupported public projection mode `{mode}`"),
        }
        .into())
    }
}

pub fn visibility_policy(cwd: &Path) -> Result<VisibilityPolicy> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    visibility_policy_on(&connection)
}

pub fn set_work_package_visibility(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    visibility: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<WorkPackageVisibilityRecord> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_label(visibility)?;
    if visibility == VISIBILITY_EMBARGOED {
        mark_embargo_workflow(cwd, work_package_kind, work_package_id, actor, reason)?;
        let context = open_repository(cwd)?;
        let connection = open_connection(&context.database_path)?;
        return work_package_visibility_on(
            &connection,
            &context.repo_id,
            work_package_kind,
            work_package_id,
        )?
        .ok_or_else(|| anyhow!("work package visibility missing after embargo mark"));
    }
    if visibility == VISIBILITY_PUBLIC {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "public_private_path_label".to_string(),
        }
        .into());
    }
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let now = now_ms();
        let prior =
            work_package_visibility_on(tx, &context.repo_id, work_package_kind, work_package_id)?
                .map(|record| record.visibility);
        ensure_not_embargo_managed(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "set_visibility",
        )?;
        tx.execute(
            "INSERT INTO work_package_visibility (
                repo_id, work_package_kind, work_package_id, visibility, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(repo_id, work_package_kind, work_package_id)
             DO UPDATE SET visibility = excluded.visibility, updated_at_ms = excluded.updated_at_ms",
            params![
                context.repo_id,
                work_package_kind,
                work_package_id,
                visibility,
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
            prior.as_deref(),
            Some(visibility),
            None,
            None,
            reason,
            now,
        )?;
        work_package_visibility_on(tx, &context.repo_id, work_package_kind, work_package_id)?
            .ok_or_else(|| anyhow!("visibility row missing after write"))
    })
}

pub fn grant_visibility_capability(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    capability: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<VisibilityGrantRecord> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_capability(capability)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        ensure_not_embargo_managed(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "grant_capability",
        )?;
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
        visibility_grant_on(tx, &context.repo_id, &grant_id)?
            .ok_or_else(|| anyhow!("visibility grant missing after write"))
    })
}

pub fn revoke_visibility_capability(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    capability: &str,
    actor: &str,
    reason: Option<&str>,
) -> Result<VisibilityGrantRecord> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_capability(capability)?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        ensure_not_embargo_managed(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            "revoke_capability",
        )?;
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
                operation: "revoke_capability".to_string(),
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
        visibility_grant_on(tx, &context.repo_id, &grant_id)?
            .ok_or_else(|| anyhow!("visibility grant missing after revoke"))
    })
}

pub fn projection_decision(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    capability: &str,
) -> Result<ProjectionDecision> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_capability(capability)?;
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    ensure_work_package_exists(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
    )?;
    projection_decision_on(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
        recipient,
        capability,
    )
}

pub(crate) fn visibility_policy_on(conn: &Connection) -> Result<VisibilityPolicy> {
    let default_work_package_visibility: String = conn
        .query_row(
            "SELECT default_work_package_visibility
             FROM visibility_policy
             WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| ForgeError::VisibilityPolicyInvalid {
            reason: "missing visibility policy".to_string(),
        })?;
    validate_visibility_label(&default_work_package_visibility)?;
    Ok(VisibilityPolicy {
        default_work_package_visibility,
        supported_visibility_labels: supported_visibility_labels(),
        supported_capabilities: supported_visibility_capabilities(),
    })
}

pub(crate) fn work_package_visibility_on(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<Option<WorkPackageVisibilityRecord>> {
    conn.query_row(
        "SELECT work_package_kind, work_package_id, visibility, updated_at_ms
         FROM work_package_visibility
         WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3",
        params![repo_id, work_package_kind, work_package_id],
        |row| {
            Ok(WorkPackageVisibilityRecord {
                work_package_kind: row.get(0)?,
                work_package_id: row.get(1)?,
                visibility: row.get(2)?,
                updated_at_ms: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn effective_work_package_visibility_on(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<String> {
    if let Some(record) =
        work_package_visibility_on(conn, repo_id, work_package_kind, work_package_id)?
    {
        validate_visibility_label(&record.visibility)?;
        return Ok(record.visibility);
    }
    Ok(visibility_policy_on(conn)?.default_work_package_visibility)
}

fn ensure_not_embargo_managed(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    operation: &str,
) -> Result<()> {
    let visibility =
        effective_work_package_visibility_on(conn, repo_id, work_package_kind, work_package_id)?;
    let workflow_exists =
        embargo_workflow_on(conn, repo_id, work_package_kind, work_package_id)?.is_some();
    if visibility == VISIBILITY_EMBARGOED || workflow_exists {
        Err(embargo_workflow_required(operation, work_package_kind, work_package_id).into())
    } else {
        Ok(())
    }
}

pub(crate) fn insert_work_package_visibility(
    tx: &Transaction<'_>,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    visibility: &str,
    now: i64,
) -> Result<()> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_label(visibility)?;
    tx.execute(
        "INSERT INTO work_package_visibility (
            repo_id, work_package_kind, work_package_id, visibility, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(repo_id, work_package_kind, work_package_id) DO NOTHING",
        params![repo_id, work_package_kind, work_package_id, visibility, now],
    )?;
    Ok(())
}

pub(crate) fn upsert_work_package_visibility(
    tx: &Transaction<'_>,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    visibility: &str,
    now: i64,
) -> Result<()> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_label(visibility)?;
    tx.execute(
        "INSERT INTO work_package_visibility (
            repo_id, work_package_kind, work_package_id, visibility, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(repo_id, work_package_kind, work_package_id)
         DO UPDATE SET visibility = excluded.visibility, updated_at_ms = excluded.updated_at_ms",
        params![repo_id, work_package_kind, work_package_id, visibility, now],
    )?;
    Ok(())
}

fn visibility_grant_on(
    conn: &Connection,
    repo_id: &str,
    grant_id: &str,
) -> Result<Option<VisibilityGrantRecord>> {
    conn.query_row(
        "SELECT id, work_package_kind, work_package_id, recipient, capability,
                created_at_ms, revoked_at_ms
         FROM visibility_grants
         WHERE repo_id = ?1 AND id = ?2",
        params![repo_id, grant_id],
        |row| {
            Ok(VisibilityGrantRecord {
                grant_id: row.get(0)?,
                work_package_kind: row.get(1)?,
                work_package_id: row.get(2)?,
                recipient: row.get(3)?,
                capability: row.get(4)?,
                created_at_ms: row.get(5)?,
                revoked_at_ms: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn insert_visibility_audit(
    tx: &Transaction<'_>,
    repo_id: &str,
    work_package_kind: Option<&str>,
    work_package_id: Option<&str>,
    action: &str,
    actor: &str,
    prior_visibility: Option<&str>,
    new_visibility: Option<&str>,
    recipient: Option<&str>,
    capability: Option<&str>,
    reason: Option<&str>,
    now: i64,
) -> Result<VisibilityAuditRecord> {
    let audit_id = new_id("visibility_audit");
    tx.execute(
        "INSERT INTO visibility_audit (
            id, repo_id, work_package_kind, work_package_id, action, actor,
            prior_visibility, new_visibility, recipient, capability, reason, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            audit_id,
            repo_id,
            work_package_kind,
            work_package_id,
            action,
            actor,
            prior_visibility,
            new_visibility,
            recipient,
            capability,
            reason,
            now
        ],
    )?;
    Ok(VisibilityAuditRecord {
        audit_id,
        work_package_kind: work_package_kind.map(str::to_string),
        work_package_id: work_package_id.map(str::to_string),
        action: action.to_string(),
        actor: actor.to_string(),
        prior_visibility: prior_visibility.map(str::to_string),
        new_visibility: new_visibility.map(str::to_string),
        recipient: recipient.map(str::to_string),
        capability: capability.map(str::to_string),
        reason: reason.map(str::to_string),
        created_at_ms: now,
    })
}

pub(crate) fn has_active_visibility_grant(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    capability: &str,
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM visibility_grants
            WHERE repo_id = ?1
              AND work_package_kind = ?2
              AND work_package_id = ?3
              AND recipient = ?4
              AND capability = ?5
              AND revoked_at_ms IS NULL
        )",
        params![
            repo_id,
            work_package_kind,
            work_package_id,
            recipient,
            capability
        ],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(crate) fn projection_decision_on(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    recipient: &str,
    capability: &str,
) -> Result<ProjectionDecision> {
    let visibility =
        effective_work_package_visibility_on(conn, repo_id, work_package_kind, work_package_id)?;
    let granted = has_active_visibility_grant(
        conn,
        repo_id,
        work_package_kind,
        work_package_id,
        recipient,
        capability,
    )?;
    let allowed = match visibility.as_str() {
        VISIBILITY_PUBLIC => true,
        VISIBILITY_TEAM | VISIBILITY_PRIVATE => granted,
        VISIBILITY_EMBARGOED => granted,
        _ => false,
    };
    let disclosure = if allowed {
        "full"
    } else if visibility == VISIBILITY_PRIVATE
        && has_active_visibility_grant(
            conn,
            repo_id,
            work_package_kind,
            work_package_id,
            recipient,
            CAPABILITY_SEE_STUB,
        )?
    {
        "stub"
    } else {
        "hidden"
    };
    Ok(ProjectionDecision {
        work_package_kind: work_package_kind.to_string(),
        work_package_id: work_package_id.to_string(),
        visibility,
        recipient: recipient.to_string(),
        capability: capability.to_string(),
        allowed,
        disclosure: disclosure.to_string(),
    })
}
