use serde::Serialize;

use super::*;

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
pub struct EvidenceSummary {
    pub evidence_id: String,
    pub snapshot_id: Option<String>,
    pub command: String,
    pub args: Vec<String>,
    pub exit_code: i64,
    pub sensitivity: String,
    pub trust: String,
}

pub fn record_evidence(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    input: EvidenceInput,
) -> Result<EvidenceRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let signer = signing::LocalSigner::load_or_create(&context.root_path)?;
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
        signer.sign_subject(
            tx,
            &context.repo_id,
            "evidence",
            &evidence_id,
            &content_hash,
            created,
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
