use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct ShowRecord {
    pub attempt: Option<AttemptRecord>,
    pub latest_snapshot: Option<SnapshotSummary>,
    pub latest_evidence: Option<EvidenceSummary>,
    pub latest_proposal: Option<ProposalSummary>,
    pub latest_check: Option<CheckSummary>,
    pub latest_decision: Option<String>,
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
