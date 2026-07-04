use serde::Serialize;

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct PublicationRecord {
    pub publication_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub branch_name: String,
    pub commit_id: String,
    pub operation_id: String,
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
    pub local_signature_fingerprint: Option<String>,
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
    let (decision_id, decision_digest, actor) =
        decision_digest_and_actor(&connection, &context.repo_id, proposal_revision_id)?;
    let local_signature_fingerprint =
        decision_signature_fingerprint(&connection, &decision_id, &decision_digest)?;

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
        local_signature_fingerprint,
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
    if let Some(fingerprint) = &trailer.local_signature_fingerprint {
        message.push_str(&format!(
            "Forge-Local-Signature-Fingerprint: {fingerprint}\n"
        ));
    }
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
) -> Result<(String, String, String)> {
    let row: Option<(String, Option<String>, String, String)> = conn
        .query_row(
            "SELECT id, content_hash, actor, decision FROM decisions
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![repo_id, proposal_revision_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((decision_id, hash, actor, decision)) = row else {
        return Err(ForgeError::NotAccepted.into());
    };
    if decision != "accepted" {
        return Err(ForgeError::NotAccepted.into());
    }
    Ok((decision_id, hash.unwrap_or_default(), actor))
}

fn decision_signature_fingerprint(
    conn: &Connection,
    decision_id: &str,
    decision_digest: &str,
) -> Result<Option<String>> {
    let (fingerprint, issues) =
        signing::verified_subject_fingerprint(conn, "decision", decision_id, decision_digest)?;
    if issues.is_empty() {
        Ok(fingerprint)
    } else {
        Err(ForgeError::TrustPolicyUnmet {
            action: "export".to_string(),
            required_trust: TRUST_LOCALLY_SIGNED.to_string(),
            signature_issues: issues,
        }
        .into())
    }
}

pub fn exportable_proposal(
    cwd: &Path,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
) -> Result<ProposalSummary> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    // NER-260: resolve globally by id and return the proposal; the owning attempt is
    // derived inside resolve_proposal so a cross-attempt `--proposal` resolves here.
    Ok(resolve_proposal(&context, &attempt, proposal_id, true)?.proposal)
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

pub(crate) fn latest_publication_for_proposal_revision(
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
