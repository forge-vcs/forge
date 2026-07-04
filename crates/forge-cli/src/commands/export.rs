use forge_protocol::ResponseEnvelope;
use serde_json::json;

use crate::{
    command_result, current_base, owner_base_content_ref, resolve_actor, secret_export_warnings,
    ExportArgs, ExportCommand, ForgeError,
};

pub(crate) fn export_response(request_id: Option<String>, args: ExportArgs) -> ResponseEnvelope {
    match args.command {
        ExportCommand::VerifyBranch(args) => {
            command_result("export verify-branch", request_id, |cwd, _| {
                // Read-only: recompute the provenance digest from the local ledger and
                // confirm the published trailer matches (fail-closed PROVENANCE_MISMATCH /
                // EVIDENCE_TAMPERED). A PASS is trailer↔current-ledger consistency, not
                // cross-machine authenticity (NER-137 R7; see schema notes.provenance).
                let verification = forge_export_git::verify_publication_trailer(&cwd, &args.name)?;
                Ok((None, serde_json::to_value(verification)?, Vec::new()))
            })
        }
        ExportCommand::PrBody(args) => command_result("export pr-body", request_id, |cwd, _| {
            let (body, excluded) =
                forge_store::pr_body_for(&cwd, args.attempt.as_deref(), args.proposal.as_deref())?;
            Ok((
                None,
                json!({ "body": body }),
                secret_export_warnings(&excluded),
            ))
        }),
        ExportCommand::Branch(args) => {
            command_result("export branch", request_id, |cwd, request_id| {
                let proposal = forge_store::exportable_proposal(
                    &cwd,
                    args.attempt.as_deref(),
                    args.proposal.as_deref(),
                )?;
                match forge_store::decision_for_proposal_revision(
                    &cwd,
                    &proposal.proposal_revision_id,
                )?
                .as_deref()
                {
                    Some("accepted") => {}
                    Some("rejected") => return Err(ForgeError::Rejected.into()),
                    _ => return Err(ForgeError::NotAccepted.into()),
                }
                forge_store::ensure_embargo_publishable(&cwd, "proposal", &proposal.proposal_id)?;
                // Verify the accepted decision's integrity BEFORE creating the git
                // branch (NER-136 R4): a tampered decision row that forged `accepted`
                // is refused here, under the held repo lock, so no branch is created.
                forge_store::verify_decision_integrity(&cwd, &proposal.proposal_revision_id)?;
                forge_store::enforce_trust_policy(
                    &cwd,
                    forge_store::TrustPolicyAction::Export,
                    &proposal.proposal_revision_id,
                )?;
                let current_head = current_base(&cwd)?;
                // CLI-layer stale-base pre-check mirroring `accept`: persist the
                // divergence to `conflict_sets` under the held lock BEFORE bailing
                // (NER-133 U7). NER-138 slice 3: after commit-on-accept the ref-store HEAD
                // advances to the accepted proposal's OWN commit, so the expected current
                // head is that `commit_id` — not the proposal's `base_head`, which the accept
                // progressed past. Falls back to `base_head` for git repos / pre-006 accepts
                // (NULL commit_id), where the forge HEAD never moves. This CLI check is
                // authoritative; `export_branch`'s internal check is fed equal anchors below
                // so it never double-fires on the legitimate post-accept divergence.
                let expected_head = forge_store::accepted_commit_id_for_revision(
                    &cwd,
                    &proposal.proposal_revision_id,
                )?
                .unwrap_or_else(|| proposal.base_head.clone());
                if current_head != expected_head {
                    let base_content_ref = owner_base_content_ref(&cwd, &expected_head)?;
                    let ours_content_ref = owner_base_content_ref(&cwd, &current_head)?;
                    return Err(forge_store::StaleBaseConflict {
                        input: forge_store::StaleBaseConflictInput {
                            context: "stale_base_export".to_string(),
                            expected_head,
                            actual_head: current_head,
                            base_content_ref,
                            ours_content_ref,
                            theirs_content_ref: proposal.content_ref.clone(),
                            changed_paths: proposal.changed_paths.clone(),
                        },
                    }
                    .into());
                }
                // git-export interop adapter (NER-134): branch existence/creation is the
                // git-export *target* (publication), not worktree management — ROADMAP
                // keeps git export as interop, so this `forge_content_git::branch_exists`
                // intentionally remains. All worktree/base materialization now goes
                // through `ContentBackend`; this and the `GitContentBackend` constructors
                // in `selected_backend`/`backend_for_content_ref` are the only
                // `forge_content_git::` references left in core lifecycle code.
                if forge_store::publication_exists_for_branch(&cwd, &args.name)?
                    && forge_content_git::branch_exists(&cwd, &args.name)
                {
                    return Err(ForgeError::BranchExists {
                        name: args.name.clone(),
                    }
                    .into());
                }
                // Assemble the provenance trailer from the local ledger (NER-137):
                // this re-verifies the deciding evidence (R8 — EVIDENCE_TAMPERED fails
                // closed here, before the branch) and folds the deciding evidence
                // content_hashes + decision digest into a content-addressed digest the
                // published commit carries and `verify-branch` recomputes.
                let trailer =
                    forge_store::build_publication_trailer(&cwd, &proposal.proposal_revision_id)?;
                let message = forge_store::render_trailer_message(&trailer);
                // `base_head` is BOTH the synthesis source (the git parent is built from the
                // proposal's base tree) AND, passed again as `current_target`, makes
                // `export_branch`'s internal stale check a confirmed no-op — the CLI check
                // above is authoritative now that commit-on-accept advances HEAD off the base.
                let (commit_id, excluded) = forge_export_git::export_branch(
                    &cwd,
                    &args.name,
                    &proposal.base_head,
                    &proposal.base_head,
                    &proposal.content_ref,
                    &message,
                )?;
                let actor = resolve_actor(args.actor.as_deref());
                let publication = forge_store::record_publication(
                    &cwd,
                    request_id,
                    &proposal.proposal_id,
                    args.name,
                    commit_id,
                    &actor,
                )?;
                Ok((
                    Some(publication.operation_id.clone()),
                    serde_json::to_value(publication)?,
                    secret_export_warnings(&excluded),
                ))
            })
        }
    }
}
