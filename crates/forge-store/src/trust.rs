use serde::Serialize;

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct TrustPolicy {
    pub min_accept_trust: String,
    pub min_export_trust: String,
    pub supported_trust_levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostedRunnerAttestation {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub trust_level: String,
    pub issuer: String,
    pub key_fingerprint: String,
    pub public_key: String,
    pub subject_count: i64,
    pub signature_count: i64,
}

pub type ThirdPartyAttestation = HostedRunnerAttestation;

#[derive(Clone, Copy)]
struct ExternalAttestationKind<'a> {
    trust_level: &'a str,
    trust_origin: &'a str,
    action: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalKeyStatus {
    pub key_fingerprint: String,
    pub public_key: String,
    pub key_path: String,
    pub exists_before_command: bool,
    pub signature_count: i64,
    pub local_key_count: i64,
    pub peer_key_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalKeyRotation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_key_backup_path: Option<String>,
    pub key_fingerprint: String,
    pub public_key: String,
    pub key_path: String,
    pub signature_count: i64,
    pub local_key_count: i64,
    pub peer_key_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrgStatus {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    pub policy_revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap_actor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap_key_fingerprint: Option<String>,
    pub recovery_status: String,
    pub principal_count: i64,
    pub key_binding_count: i64,
    pub role_binding_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrgBootstrap {
    pub operation_id: String,
    pub enabled: bool,
    pub org_id: String,
    pub policy_revision: i64,
    pub owner_actor_id: String,
    pub owner_alias: String,
    pub key_fingerprint: String,
    pub public_key: String,
    pub role: String,
    pub audit_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustPolicyAction {
    Accept,
    Export,
}

impl TrustPolicyAction {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustPolicyAction::Accept => "accept",
            TrustPolicyAction::Export => "export",
        }
    }
}

const TRUST_SELF_REPORTED: &str = "self_reported";
const TRUST_LOCALLY_OBSERVED: &str = "locally_observed";
pub(crate) const TRUST_LOCALLY_SIGNED: &str = "locally_signed";
const TRUST_HOSTED_RUNNER_OBSERVED: &str = "hosted_runner_observed";
const TRUST_HOSTED_RUNNER_SIGNED: &str = "hosted_runner_signed";
const TRUST_THIRD_PARTY_ATTESTED: &str = "third_party_attested";

fn supported_trust_levels() -> Vec<String> {
    [
        TRUST_SELF_REPORTED,
        TRUST_LOCALLY_OBSERVED,
        TRUST_LOCALLY_SIGNED,
        TRUST_HOSTED_RUNNER_OBSERVED,
        TRUST_HOSTED_RUNNER_SIGNED,
        TRUST_THIRD_PARTY_ATTESTED,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub(crate) fn ensure_active_org_principal(
    conn: &Connection,
    repo_id: &str,
    principal_id: &str,
) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM org_principals
            WHERE repo_id = ?1 AND id = ?2 AND state = 'active'
        )",
        params![repo_id, principal_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(ForgeError::PrivateDecryptAuthorityMissing {
            principal_id: principal_id.to_string(),
            reason: "missing_active_principal".to_string(),
        }
        .into())
    }
}

pub(crate) fn ensure_active_org_role(
    conn: &Connection,
    repo_id: &str,
    principal_id: &str,
    roles: &[&str],
) -> Result<()> {
    let active_roles: Vec<String> = {
        let mut statement = conn.prepare(
            "SELECT role FROM org_role_bindings
             WHERE repo_id = ?1 AND principal_id = ?2 AND state = 'active'",
        )?;
        let rows = statement.query_map(params![repo_id, principal_id], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<String>, _>>()?
    };
    if active_roles
        .iter()
        .any(|role| roles.iter().any(|allowed| role == allowed))
    {
        Ok(())
    } else {
        Err(ForgeError::PrivateDecryptAuthorityMissing {
            principal_id: principal_id.to_string(),
            reason: "missing_active_org_role".to_string(),
        }
        .into())
    }
}

pub(crate) fn embargo_authority_on(
    conn: &Connection,
    repo_id: &str,
    actor: &str,
    action: &str,
) -> Result<(String, i64)> {
    let org = org_status_on(conn, repo_id)?;
    if !org.enabled {
        return Ok((actor.to_string(), org.policy_revision));
    }
    let principal_active: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM org_principals
            WHERE repo_id = ?1 AND id = ?2 AND state = 'active'
        )",
        params![repo_id, actor],
        |row| row.get(0),
    )?;
    if !principal_active {
        return Err(ForgeError::OrgAuthorityRequired {
            action: action.to_string(),
            required_role: "owner_or_maintainer".to_string(),
        }
        .into());
    }
    let has_role: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM org_role_bindings
            WHERE repo_id = ?1 AND principal_id = ?2 AND state = 'active'
              AND role IN ('owner', 'maintainer')
        )",
        params![repo_id, actor],
        |row| row.get(0),
    )?;
    if !has_role {
        return Err(ForgeError::OrgAuthorityRequired {
            action: action.to_string(),
            required_role: "owner_or_maintainer".to_string(),
        }
        .into());
    }
    Ok((actor.to_string(), org.policy_revision))
}

pub fn trust_policy(cwd: &Path) -> Result<TrustPolicy> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    trust_policy_on(&connection)
}

pub fn set_trust_policy(
    cwd: &Path,
    min_accept_trust: Option<&str>,
    min_export_trust: Option<&str>,
) -> Result<TrustPolicy> {
    if let Some(level) = min_accept_trust {
        validate_trust_level(level)?;
    }
    if let Some(level) = min_export_trust {
        validate_trust_level(level)?;
    }
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        let current = trust_policy_on(tx)?;
        let accept = min_accept_trust.unwrap_or(&current.min_accept_trust);
        let export = min_export_trust.unwrap_or(&current.min_export_trust);
        tx.execute(
            "UPDATE trust_policy
             SET min_accept_trust = ?1, min_export_trust = ?2, updated_at_ms = ?3
             WHERE singleton = 1",
            params![accept, export, now_ms()],
        )?;
        trust_policy_on(tx)
    })
}

pub fn local_key_status(cwd: &Path) -> Result<LocalKeyStatus> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let key = signing::local_key_status(&context.root_path)?;
    signing::register_local_signing_key(
        &connection,
        &context.repo_id,
        &key.public_key,
        &key.key_fingerprint,
        now_ms(),
    )?;
    let key_summary = signing::signature_key_summary(&connection, &context.repo_id)?;
    Ok(LocalKeyStatus {
        signature_count: signature_count_for_fingerprint(&connection, &key.key_fingerprint)?,
        key_fingerprint: key.key_fingerprint,
        public_key: key.public_key,
        key_path: key.key_path,
        exists_before_command: key.exists_before_command,
        local_key_count: key_summary.local_key_fingerprints.len() as i64,
        peer_key_count: key_summary.peer_key_fingerprints.len() as i64,
    })
}

pub fn rotate_local_key(cwd: &Path) -> Result<LocalKeyRotation> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let rotation = signing::rotate_local_key(&context.root_path)?;
    signing::register_local_signing_key(
        &connection,
        &context.repo_id,
        &rotation.new_key.public_key,
        &rotation.new_key.key_fingerprint,
        now_ms(),
    )?;
    let key_summary = signing::signature_key_summary(&connection, &context.repo_id)?;
    Ok(LocalKeyRotation {
        previous_fingerprint: rotation.previous_fingerprint,
        previous_key_backup_path: rotation.previous_key_backup_path,
        signature_count: signature_count_for_fingerprint(
            &connection,
            &rotation.new_key.key_fingerprint,
        )?,
        key_fingerprint: rotation.new_key.key_fingerprint,
        public_key: rotation.new_key.public_key,
        key_path: rotation.new_key.key_path,
        local_key_count: key_summary.local_key_fingerprints.len() as i64,
        peer_key_count: key_summary.peer_key_fingerprints.len() as i64,
    })
}

pub fn org_status(cwd: &Path) -> Result<OrgStatus> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    org_status_on(&connection, &context.repo_id)
}

pub fn init_org_governance(
    cwd: &Path,
    request_id: Option<String>,
    actor: &str,
    reason: Option<&str>,
) -> Result<OrgBootstrap> {
    let actor = actor.trim();
    if actor.is_empty() {
        return Err(ForgeError::OrgAuthorityRequired {
            action: "org_init".into(),
            required_role: "non_empty_actor".into(),
        }
        .into());
    }

    let context = open_repository(cwd)?;
    let key = signing::local_key_status(&context.root_path)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let profile = org_status_on(tx, &context.repo_id)?;
        if profile.enabled {
            return Err(ForgeError::OrgAlreadyEnabled {
                org_id: profile.org_id.unwrap_or_else(|| "unknown".into()),
            }
            .into());
        }

        let now = now_ms();
        let org_id = new_id("org");
        let owner_actor_id = new_id("actor");
        let alias_id = new_id("org_alias");
        let key_binding_id = new_id("org_key");
        let role_binding_id = new_id("org_role");
        let audit_id = new_id("org_audit");
        let policy_revision = 1;

        signing::register_local_signing_key(
            tx,
            &context.repo_id,
            &key.public_key,
            &key.key_fingerprint,
            now,
        )?;

        tx.execute(
            "INSERT INTO org_principals (
                id, repo_id, kind, state, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, 'human', 'active', ?3, ?3)",
            params![owner_actor_id, context.repo_id, now],
        )?;
        tx.execute(
            "INSERT INTO org_principal_aliases (
                id, repo_id, principal_id, alias_kind, alias_value, visibility, state,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 'actor', ?4, 'private', 'active', ?5, ?5)",
            params![alias_id, context.repo_id, owner_actor_id, actor, now],
        )?;
        tx.execute(
            "INSERT INTO org_key_bindings (
                id, repo_id, principal_id, key_fingerprint, public_key, binding_authority,
                state, valid_from_revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?3, 'active', ?6, ?7, ?7)",
            params![
                key_binding_id,
                context.repo_id,
                owner_actor_id,
                key.key_fingerprint,
                key.public_key,
                policy_revision,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO org_role_bindings (
                id, repo_id, principal_id, role, authority, state, valid_from_revision,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 'owner', ?3, 'active', ?4, ?5, ?5)",
            params![
                role_binding_id,
                context.repo_id,
                owner_actor_id,
                policy_revision,
                now
            ],
        )?;

        let prior_state = json!({"enabled": false, "policy_revision": 0}).to_string();
        let new_state = json!({
            "enabled": true,
            "org_id": org_id.clone(),
            "owner_actor_id": owner_actor_id.clone(),
            "owner_alias": actor,
            "key_fingerprint": key.key_fingerprint.clone(),
            "role": "owner",
            "policy_revision": policy_revision
        })
        .to_string();
        tx.execute(
            "INSERT INTO org_policy_audit (
                id, repo_id, action, actor_id, acting_key_fingerprint, authority,
                prior_state_json, new_state_json, policy_revision, reason, created_at_ms
             ) VALUES (?1, ?2, 'org_init', ?3, ?4, ?3, ?5, ?6, ?7, ?8, ?9)",
            params![
                audit_id,
                context.repo_id,
                owner_actor_id,
                key.key_fingerprint,
                prior_state,
                new_state,
                policy_revision,
                reason,
                now
            ],
        )?;
        tx.execute(
            "UPDATE org_authority_profile
             SET enabled = 1,
                 org_id = ?1,
                 policy_revision = ?2,
                 bootstrap_actor_id = ?3,
                 bootstrap_key_fingerprint = ?4,
                 recovery_status = 'normal',
                 updated_at_ms = ?5
             WHERE singleton = 1",
            params![
                org_id.clone(),
                policy_revision,
                owner_actor_id.clone(),
                key.key_fingerprint.clone(),
                now
            ],
        )?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "org init".to_string(),
                kind: "org_initialized".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": "org_initialized",
                    "org_id": org_id.clone(),
                    "owner_actor_id": owner_actor_id.clone(),
                    "key_fingerprint": key.key_fingerprint.clone(),
                    "replay_data": {
                        "enabled": true,
                        "org_id": org_id.clone(),
                        "policy_revision": policy_revision,
                        "owner_actor_id": owner_actor_id.clone(),
                        "owner_alias": actor,
                        "key_fingerprint": key.key_fingerprint.clone(),
                        "public_key": key.public_key.clone(),
                        "role": "owner",
                        "audit_id": audit_id.clone(),
                    }
                }),
            },
        )?;

        Ok(OrgBootstrap {
            operation_id: op.operation_id,
            enabled: true,
            org_id,
            policy_revision,
            owner_actor_id,
            owner_alias: actor.to_string(),
            key_fingerprint: key.key_fingerprint.clone(),
            public_key: key.public_key.clone(),
            role: "owner".into(),
            audit_id,
        })
    })
}

pub(crate) fn org_status_on(conn: &Connection, repo_id: &str) -> Result<OrgStatus> {
    let (
        enabled,
        org_id,
        policy_revision,
        bootstrap_actor_id,
        bootstrap_key_fingerprint,
        recovery_status,
    ): (
        i64,
        Option<String>,
        i64,
        Option<String>,
        Option<String>,
        String,
    ) = conn.query_row(
        "SELECT enabled, org_id, policy_revision, bootstrap_actor_id,
                bootstrap_key_fingerprint, recovery_status
         FROM org_authority_profile
         WHERE singleton = 1",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;

    Ok(OrgStatus {
        enabled: enabled == 1,
        org_id,
        policy_revision,
        bootstrap_actor_id,
        bootstrap_key_fingerprint,
        recovery_status,
        principal_count: count_org_rows(conn, "org_principals", repo_id)?,
        key_binding_count: count_org_rows(conn, "org_key_bindings", repo_id)?,
        role_binding_count: count_org_rows(conn, "org_role_bindings", repo_id)?,
    })
}

fn count_org_rows(conn: &Connection, table: &str, repo_id: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE repo_id = ?1");
    Ok(conn.query_row(&sql, params![repo_id], |row| row.get(0))?)
}

pub fn attest_hosted_runner(
    cwd: &Path,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
    key_path: &Path,
    issuer: &str,
) -> Result<HostedRunnerAttestation> {
    let context = open_repository(cwd)?;
    attest_external(
        &context,
        attempt_id,
        proposal_id,
        key_path,
        issuer,
        ExternalAttestationKind {
            trust_level: TRUST_HOSTED_RUNNER_SIGNED,
            trust_origin: "hosted_runner",
            action: "attest_hosted_runner",
        },
    )
}

pub fn attest_third_party(
    cwd: &Path,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
    key_path: &Path,
    issuer: &str,
) -> Result<ThirdPartyAttestation> {
    let context = open_repository(cwd)?;
    attest_external(
        &context,
        attempt_id,
        proposal_id,
        key_path,
        issuer,
        ExternalAttestationKind {
            trust_level: TRUST_THIRD_PARTY_ATTESTED,
            trust_origin: "third_party",
            action: "attest_third_party",
        },
    )
}

fn attest_external(
    context: &RepositoryContext,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
    key_path: &Path,
    issuer: &str,
    kind: ExternalAttestationKind<'_>,
) -> Result<HostedRunnerAttestation> {
    let resolved_attempt = resolve_attempt_in_context(context, attempt_id)?.attempt;
    // NER-260: derive the owning attempt from an explicit `--proposal` (the proposal's
    // revision is what gets attested), mirroring the other resolve_proposal callers.
    let resolved = resolve_proposal(context, &resolved_attempt, proposal_id, true)?;
    let proposal = resolved.proposal;
    let signer = signing::ExternalAttestationSigner::load_from_pkcs8(key_path)?;
    let mut connection = open_connection(&context.database_path)?;
    let created = now_ms();
    let mut unsigned = Vec::new();
    let subjects = trust_subjects_for_revision(
        &connection,
        &context.repo_id,
        &proposal.proposal_revision_id,
        true,
        &mut unsigned,
    )?;
    if subjects.is_empty() && unsigned.is_empty() {
        unsigned.push(SignatureFinding {
            kind: SignatureFindingKind::MissingSignature,
            subject_kind: "proposal_revision".to_string(),
            subject_id: proposal.proposal_revision_id.clone(),
            key_fingerprint: None,
        });
    }
    if !unsigned.is_empty() {
        return Err(ForgeError::TrustPolicyUnmet {
            action: kind.action.to_string(),
            required_trust: kind.trust_level.to_string(),
            signature_issues: unsigned,
        }
        .into());
    }
    let subject_count = subjects.len() as i64;
    let signature_count = with_immediate_retry(&mut connection, |tx| {
        let mut inserted = 0_i64;
        for (subject_kind, subject_id, signed_digest) in &subjects {
            if signer.sign_subject(
                tx,
                signing::ExternalSignatureInput {
                    repo_id: &context.repo_id,
                    subject_kind,
                    subject_id,
                    signed_digest,
                    trust_level: kind.trust_level,
                    trust_origin: kind.trust_origin,
                    created_at_ms: created,
                },
            )? {
                inserted += 1;
            }
        }
        Ok(inserted)
    })?;
    Ok(HostedRunnerAttestation {
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        trust_level: kind.trust_level.to_string(),
        issuer: issuer.to_string(),
        key_fingerprint: signer.key_fingerprint().to_string(),
        public_key: signer.public_key().to_string(),
        subject_count,
        signature_count,
    })
}

fn signature_count_for_fingerprint(conn: &Connection, key_fingerprint: &str) -> Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM ledger_signatures WHERE key_fingerprint = ?1",
        params![key_fingerprint],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(crate) fn trust_policy_on(conn: &Connection) -> Result<TrustPolicy> {
    let row = conn
        .query_row(
            "SELECT min_accept_trust, min_export_trust
             FROM trust_policy
             WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let (min_accept_trust, min_export_trust) = row.unwrap_or_else(|| {
        (
            TRUST_SELF_REPORTED.to_string(),
            TRUST_SELF_REPORTED.to_string(),
        )
    });
    Ok(TrustPolicy {
        min_accept_trust,
        min_export_trust,
        supported_trust_levels: supported_trust_levels(),
    })
}

fn validate_trust_level(level: &str) -> Result<()> {
    if trust_rank(level).is_some() {
        Ok(())
    } else {
        Err(ForgeError::UnsupportedTrustLevel {
            level: level.to_string(),
            supported: supported_trust_levels(),
        }
        .into())
    }
}

pub(crate) fn trust_rank(level: &str) -> Option<u8> {
    match level {
        TRUST_SELF_REPORTED => Some(0),
        TRUST_LOCALLY_OBSERVED => Some(1),
        TRUST_LOCALLY_SIGNED => Some(2),
        TRUST_HOSTED_RUNNER_OBSERVED => Some(3),
        TRUST_HOSTED_RUNNER_SIGNED => Some(4),
        TRUST_THIRD_PARTY_ATTESTED => Some(5),
        _ => None,
    }
}

fn requires_local_signatures(level: &str) -> bool {
    trust_rank(level).is_some_and(|rank| rank >= trust_rank(TRUST_LOCALLY_SIGNED).unwrap())
}

fn requires_hosted_runner_signatures(level: &str) -> bool {
    trust_rank(level).is_some_and(|rank| {
        rank >= trust_rank(TRUST_HOSTED_RUNNER_OBSERVED).unwrap()
            && rank < trust_rank(TRUST_THIRD_PARTY_ATTESTED).unwrap()
    })
}

fn requires_third_party_signatures(level: &str) -> bool {
    trust_rank(level).is_some_and(|rank| rank >= trust_rank(TRUST_THIRD_PARTY_ATTESTED).unwrap())
}

pub fn enforce_trust_policy(
    cwd: &Path,
    action: TrustPolicyAction,
    proposal_revision_id: &str,
) -> Result<()> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let policy = trust_policy_on(&connection)?;
    let required_trust = match action {
        TrustPolicyAction::Accept => policy.min_accept_trust,
        TrustPolicyAction::Export => policy.min_export_trust,
    };
    if !requires_local_signatures(&required_trust) {
        return Ok(());
    }

    let mut unsigned = Vec::new();
    let subjects = trust_subjects_for_revision(
        &connection,
        &context.repo_id,
        proposal_revision_id,
        action == TrustPolicyAction::Export,
        &mut unsigned,
    )?;
    if subjects.is_empty() && unsigned.is_empty() {
        unsigned.push(SignatureFinding {
            kind: SignatureFindingKind::MissingSignature,
            subject_kind: "proposal_revision".to_string(),
            subject_id: proposal_revision_id.to_string(),
            key_fingerprint: None,
        });
    }
    let mut signature_issues =
        signing::verify_subject_local_signatures(&connection, subjects.clone())?;
    signature_issues.extend(unsigned);
    let required_rank = trust_rank(&required_trust).unwrap_or(0);
    if requires_hosted_runner_signatures(&required_trust) {
        signature_issues.extend(signing::verify_subject_hosted_runner_signatures(
            &connection,
            subjects.clone(),
        )?);
    }
    if requires_third_party_signatures(&required_trust) {
        signature_issues.extend(signing::verify_subject_third_party_signatures(
            &connection,
            subjects,
        )?);
    }
    let third_party_rank = trust_rank(TRUST_THIRD_PARTY_ATTESTED).unwrap();
    if required_rank > third_party_rank {
        signature_issues.push(SignatureFinding {
            kind: SignatureFindingKind::MissingSignature,
            subject_kind: "attestation".to_string(),
            subject_id: required_trust.clone(),
            key_fingerprint: None,
        });
    }
    signature_issues.sort_by(|left, right| {
        left.subject_kind
            .cmp(&right.subject_kind)
            .then_with(|| left.subject_id.cmp(&right.subject_id))
            .then_with(|| left.kind.as_str().cmp(right.kind.as_str()))
    });
    signature_issues.dedup();
    if signature_issues.is_empty() {
        Ok(())
    } else {
        Err(ForgeError::TrustPolicyUnmet {
            action: action.as_str().to_string(),
            required_trust,
            signature_issues,
        }
        .into())
    }
}

fn trust_subjects_for_revision(
    conn: &Connection,
    repo_id: &str,
    proposal_revision_id: &str,
    include_decision: bool,
    unsigned: &mut Vec<SignatureFinding>,
) -> Result<Vec<(String, String, String)>> {
    let snapshot_id: String = conn.query_row(
        "SELECT pr.snapshot_id
         FROM proposal_revisions pr
         JOIN proposals p ON p.id = pr.proposal_id
         WHERE p.repo_id = ?1 AND pr.id = ?2",
        params![repo_id, proposal_revision_id],
        |row| row.get(0),
    )?;
    let mut subjects = Vec::new();
    let mut evidence = conn.prepare(
        "SELECT id, content_hash
         FROM evidence
         WHERE repo_id = ?1 AND snapshot_id = ?2
         ORDER BY created_at_ms, rowid",
    )?;
    for row in evidence.query_map(params![repo_id, snapshot_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
    })? {
        let (id, content_hash) = row?;
        if let Some(digest) = content_hash {
            subjects.push(("evidence".to_string(), id, digest));
        } else {
            unsigned.push(SignatureFinding {
                kind: SignatureFindingKind::MissingSignature,
                subject_kind: "evidence".to_string(),
                subject_id: id,
                key_fingerprint: None,
            });
        }
    }

    if include_decision {
        let decision = conn
            .query_row(
                "SELECT id, content_hash, commit_id
                 FROM decisions
                 WHERE repo_id = ?1 AND proposal_revision_id = ?2 AND decision = 'accepted'
                 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                params![repo_id, proposal_revision_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((id, content_hash, commit_id)) = decision {
            if let Some(digest) = content_hash {
                subjects.push(("decision".to_string(), id, digest));
            } else {
                unsigned.push(SignatureFinding {
                    kind: SignatureFindingKind::MissingSignature,
                    subject_kind: "decision".to_string(),
                    subject_id: id,
                    key_fingerprint: None,
                });
            }
            if let Some(commit_id) = commit_id {
                subjects.push(("commit".to_string(), commit_id.clone(), commit_id));
            }
        }
    }

    Ok(subjects)
}
