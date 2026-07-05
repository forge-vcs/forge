use serde::Serialize;

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct ProposalRecord {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckRecord {
    pub check_result_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub status: String,
    pub reason: String,
    pub evidence_id: Option<String>,
    /// Per-gate verdicts from the policy engine (NER-135). Emit-only in v0 — not
    /// persisted; Phase 6 (NER-137) adds a column when it consumes per-gate history.
    pub gates: Vec<forge_policy::GateResult>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DecisionRecord {
    pub decision_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub decision: String,
    /// The check status observed in-txn when an `accept` was gated on evidence
    /// (NER-135). Omitted entirely for `reject` (the gate never runs), so the reject
    /// response shape is unchanged. `Some("passed")` for a normal accept;
    /// `Some(<failed|missing|stale>)` only when `--allow-unverified` bypassed the
    /// gate, which the CLI surfaces as a `warnings[]` entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_status: Option<String>,
    /// The native `Commit` written when a proposal is accepted in a native repo (NER-138
    /// Phase 7 slice 3). `None` for git-backend repos and for `reject` (no commit). The
    /// ref-store HEAD is advanced to this id after the decision row commits; it is recorded
    /// in `decisions.commit_id` so a torn HEAD advance is reconcilable from the ledger.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_id: Option<String>,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposalReview {
    pub proposal: ProposalMetadata,
    pub attempt: AttemptRecord,
    pub intent: IntentDetail,
    pub readiness: ReviewReadiness,
    pub lifecycle: ReviewLifecycle,
    pub visibility: ReviewVisibility,
    pub evidence_audit: ReviewEvidenceAudit,
    pub diff: ReviewDiff,
    pub terminal_handoffs: Vec<ReviewTerminalHandoff>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewReadiness {
    pub status: String,
    pub summary: String,
    pub deciding_factors: Vec<ReviewFactor>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewFactor {
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewLifecycle {
    pub check_status: Option<String>,
    pub decision_status: Option<String>,
    pub publication_status: Option<String>,
    pub publication: Option<PublicationRecord>,
    pub sibling_attempts: Vec<ReviewAttemptContext>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewAttemptContext {
    pub attempt_id: String,
    pub status: String,
    pub is_owner: bool,
    pub proposal_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewVisibility {
    pub projection: String,
    pub visibility: String,
    pub disclosure: String,
    pub private_path_label_count: i64,
    pub private_path_detail: String,
    pub embargo: Option<ReviewEmbargo>,
    pub projection_checks: Vec<ProjectionDecision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewEmbargo {
    pub state: String,
    pub public_projection_mode: Option<String>,
    pub release_allowed: bool,
    pub reveal_allowed: bool,
    pub publish_allowed: bool,
    pub export_allowed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewEvidenceAudit {
    pub latest_evidence: Option<EvidenceSummary>,
    pub latest_check: Option<CheckSummary>,
    pub trust_policy: TrustPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewDiff {
    pub content_ref: String,
    pub changed_paths: Vec<ReviewChangedPath>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewChangedPath {
    pub path: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewTerminalHandoff {
    pub label: String,
    pub command: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposalSummary {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposalMetadata {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub attempt_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
    pub check_status: Option<String>,
    pub decision_status: Option<String>,
    pub publication_status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckSummary {
    pub check_result_id: String,
    pub status: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ResolvedProposal {
    pub proposal: ProposalSummary,
    /// The attempt that OWNS this proposal. When an explicit `--proposal` id is
    /// supplied, the proposal is resolved globally by id and this attempt is derived
    /// from `proposal.attempt_id` (NER-260) — it is NOT necessarily the caller's
    /// currently-attached/resolved attempt. Downstream gate evaluation and commit
    /// metadata MUST use this attempt so a cross-attempt `--proposal` is judged and
    /// recorded under its own intent. For the no-`--proposal` default branch this is
    /// the caller-resolved attempt, unchanged.
    pub attempt: AttemptRecord,
}

/// Build the persisted check-spec JSON from repeatable `forge start --require
/// "<cmd>"` values (NER-135). Each value is whitespace-tokenized into
/// `(program, args)` — the same shape `forge run -- <argv>` records as evidence, so
/// a gate's identity matches its evidence. Returns `None` when no gate is declared
/// (the policy engine's default mode). Quoting of whitespace-bearing args is a
/// documented v0 limitation (deferred). Lives in the store (which already depends on
/// `forge-policy`) so the CLI need not name `forge_policy` types directly to build a
/// spec — though it still transitively serializes them via `CheckRecord.gates`.
pub fn check_spec_json_from_requires(
    requires: &[String],
    structured_requires: &[String],
) -> Option<String> {
    let parse_gate = |raw: &str, require_structured_pass: bool| -> Option<forge_policy::Gate> {
        let mut tokens = raw.split_whitespace();
        let program = tokens.next()?.to_string();
        let args = tokens.map(str::to_string).collect();
        Some(forge_policy::Gate {
            program,
            args,
            require_structured_pass,
        })
    };
    let mut gates: Vec<forge_policy::Gate> = requires
        .iter()
        .filter_map(|raw| parse_gate(raw, false))
        .collect();
    gates.extend(
        structured_requires
            .iter()
            .filter_map(|raw| parse_gate(raw, true)),
    );
    if gates.is_empty() {
        return None;
    }
    serde_json::to_string(&forge_policy::CheckSpec { gates }).ok()
}

pub fn propose(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    summary: Option<&str>,
) -> Result<ProposalRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    let summary = summary.map(str::to_string);
    let mut connection = open_connection(&context.database_path)?;
    let (proposal_id, revision_id, snapshot, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        // Determining read inside the IMMEDIATE txn: the proposal binds to the
        // latest snapshot observed on the same connection that writes it (U4).
        let snapshot =
            latest_snapshot_on(tx, &attempt.attempt_id)?.ok_or(ForgeError::NoSnapshot)?;
        let proposal_id = new_id("proposal");
        let revision_id = new_id("revision");
        tx.execute(
            "INSERT INTO proposals (id, repo_id, attempt_id, snapshot_id, base_head, content_ref, status, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'draft', ?7)",
            params![
                proposal_id,
                context.repo_id,
                attempt.attempt_id,
                snapshot.snapshot_id,
                attempt.base_head,
                snapshot.content_ref,
                now_ms()
            ],
        )?;
        tx.execute(
            "INSERT INTO proposal_revisions (id, proposal_id, snapshot_id, content_ref, changed_paths_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                revision_id,
                proposal_id,
                snapshot.snapshot_id,
                snapshot.content_ref,
                serde_json::to_string(&snapshot.changed_paths)?,
                now_ms()
            ],
        )?;
        let attempt_visibility = effective_work_package_visibility_on(
            tx,
            &context.repo_id,
            "attempt",
            &attempt.attempt_id,
        )?;
        insert_work_package_visibility(
            tx,
            &context.repo_id,
            "proposal",
            &proposal_id,
            &attempt_visibility,
            now_ms(),
        )?;
        let mut replay_data = json!({
            "proposal_id": proposal_id,
            "proposal_revision_id": revision_id,
            "attempt_id": attempt.attempt_id,
            "snapshot_id": snapshot.snapshot_id,
            "base_head": attempt.base_head,
            "content_ref": snapshot.content_ref,
            "changed_paths": snapshot.changed_paths,
        });
        if let Some(summary) = summary.as_ref() {
            replay_data["summary"] = json!(summary);
        }
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "propose".to_string(),
                kind: "proposal_created".to_string(),
                view_kind: ViewKind::Initialized,
                // NER-255: persist the success `data` payload for idempotent replay
                // (operation_id is overlaid on replay). The lifecycle/proposal_id keys
                // stay siblings for existing readers; `replay_data` is added alongside.
                state: json!({
                    "lifecycle": "proposal_draft",
                    "proposal_id": proposal_id,
                    "replay_data": replay_data
                }),
            },
        )?;
        Ok((proposal_id, revision_id, snapshot, op))
    })?;
    Ok(ProposalRecord {
        proposal_id,
        proposal_revision_id: revision_id,
        attempt_id: attempt.attempt_id,
        snapshot_id: snapshot.snapshot_id,
        base_head: attempt.base_head,
        content_ref: snapshot.content_ref,
        changed_paths: snapshot.changed_paths,
        summary,
        operation_id: op.operation_id,
    })
}

pub fn record_check(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
) -> Result<CheckRecord> {
    let context = open_repository(cwd)?;
    let resolved_attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    // NER-260: an explicit `--proposal` checks the proposal under its OWNING attempt's
    // intent (evidence is read from that attempt's snapshot), not the caller's attached
    // attempt — so the verdict matches the intent the proposal targets.
    let resolved = resolve_proposal(&context, &resolved_attempt, proposal_id, true)?;
    let proposal = resolved.proposal;
    let attempt = resolved.attempt;
    let mut connection = open_connection(&context.database_path)?;
    let (check_result_id, outcome, evidence_id, op) = with_immediate_retry(
        &mut connection,
        |tx| {
            replay_guard(tx, &context.repo_id, request_id.as_deref())?;
            // Aggregate over the proposed snapshot's FULL evidence set, read on the same
            // connection that writes the check result — so a concurrent (lock-free) `run`
            // committing newer evidence cannot make the verdict disagree with what is
            // persisted (NER-132 U2 TOCTOU closure, preserved by the aggregate read).
            // forge-policy is the single source of pass/fail/missing/stale (NER-135 R4).
            let outcome = evaluate_check_on(tx, &attempt, &proposal)?;
            let evidence_id = representative_evidence_id(&outcome);
            let check_result_id = new_id("check");
            tx.execute(
            "INSERT INTO check_results (
                id, repo_id, proposal_id, proposal_revision_id, status, reason, evidence_id, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                check_result_id,
                context.repo_id,
                proposal.proposal_id,
                proposal.proposal_revision_id,
                outcome.status,
                outcome.reason,
                evidence_id,
                now_ms()
            ],
        )?;
            let op = insert_operation_view(
                tx,
                &context.repo_id,
                Some(&context.current_operation_id),
                OperationViewInput {
                    request_id: request_id.clone(),
                    command: "check".to_string(),
                    kind: "proposal_checked".to_string(),
                    view_kind: ViewKind::Initialized,
                    state: json!({ "lifecycle": "checked", "check_result_id": check_result_id }),
                },
            )?;
            Ok((check_result_id, outcome, evidence_id, op))
        },
    )?;
    // Redact secret-like argv in the per-gate egress (the verdict was already
    // computed in-txn from the raw identities; this only affects what is surfaced).
    let gates = outcome.gates.into_iter().map(redact_gate_result).collect();
    Ok(CheckRecord {
        check_result_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        status: outcome.status,
        reason: outcome.reason,
        evidence_id,
        gates,
        operation_id: op.operation_id,
    })
}

/// Redact secret-like `key=value` argv tokens (e.g. `--token=…`, `PASSWORD=…`) in a
/// gate's program + each arg before the identity reaches a machine-visible egress —
/// the `check` JSON `gates[]` here, and (per-token) `CHECK_NOT_PASSED.unmet` in
/// `error.rs` (NER-135 code-review F2). Redacts PER-ARG: `redact_secret_like_text`
/// keys on the first `=`/`:` of its input, so redacting each arg independently catches
/// a secret in any position, where redacting a space-joined identity would only check
/// its first token. Gate specs are persisted (`intents.check_spec_json`) and surfaced
/// WITHOUT execution, so — unlike captured evidence — they get this egress pass. Full
/// non-`key=value` arg scanning is Phase 5 (see schema `notes.secret_protection`).
pub(crate) fn redact_gate_result(gate: forge_policy::GateResult) -> forge_policy::GateResult {
    let program = forge_content::redact_secret_like_text(&gate.program).0;
    let args = gate
        .args
        .iter()
        .map(|arg| forge_content::redact_secret_like_text(arg).0)
        .collect();
    forge_policy::GateResult {
        program,
        args,
        ..gate
    }
}

/// Pick a single representative evidence id for the `check_results.evidence_id`
/// FK from a multi-gate outcome (best-effort — `CheckRecord.gates` carries the
/// authoritative per-gate detail): the first failing gate's deciding evidence,
/// else the first gate with any deciding evidence, else NULL.
pub(crate) fn representative_evidence_id(outcome: &forge_policy::CheckOutcome) -> Option<String> {
    outcome
        .gates
        .iter()
        .find(|gate| {
            gate.verdict == forge_policy::GateVerdict::Failed && gate.evidence_id.is_some()
        })
        .or_else(|| outcome.gates.iter().find(|gate| gate.evidence_id.is_some()))
        .and_then(|gate| gate.evidence_id.clone())
}

/// Load an intent's declared check spec (NER-135). A NULL `check_spec_json`
/// (un-declared intent, or any intent created before Phase 4) yields an empty
/// spec — the policy engine's default mode.
pub(crate) fn intent_check_spec(
    conn: &Connection,
    intent_id: &str,
) -> Result<forge_policy::CheckSpec> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT check_spec_json FROM intents WHERE id = ?1",
            params![intent_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    // Fail CLOSED, not open: a NULL column is a legitimately un-gated intent
    // (default mode), but a non-NULL value that won't parse is corruption (manual
    // edit, partial write, or a future spec shape this binary can't read). Silently
    // collapsing it to an empty spec would downgrade a declared multi-gate intent to
    // the permissive default mode, letting a gated `accept` slip through (NER-135
    // code-review F1). The schema_version gate does not catch this — migration 003
    // only adds the column, not a value-shape version — so the parse must error.
    match stored {
        None => Ok(forge_policy::CheckSpec::default()),
        Some(json) => serde_json::from_str(&json)
            .with_context(|| format!("corrupt check_spec_json for intent {intent_id}")),
    }
}

/// Project every evidence row for an attempt into [`forge_policy::EvidenceFact`]s,
/// newest-first (matching the `created_at_ms DESC, rowid DESC` "latest" tiebreak).
/// Pass a writer's `&tx` to keep the read inside its IMMEDIATE txn (NER-132 U2).
pub(crate) fn evidence_facts_on(
    conn: &Connection,
    attempt_id: &str,
) -> Result<Vec<forge_policy::EvidenceFact>> {
    let mut statement = conn.prepare(
        "SELECT id, command, args_json, exit_code, snapshot_id, created_at_ms, rowid, structured_json FROM evidence
         WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC",
    )?;
    let rows = statement.query_map(params![attempt_id], |row| {
        let args_json: String = row.get(2)?;
        let structured_json: Option<String> = row.get(7)?;
        // Project the parsed test-failure count for a structured gate (NER-136 §U6).
        let structured_failures = structured_json
            .as_deref()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .and_then(|value| value.get("failed").and_then(serde_json::Value::as_u64));
        Ok(forge_policy::EvidenceFact {
            evidence_id: row.get(0)?,
            program: row.get(1)?,
            args: serde_json::from_str(&args_json).unwrap_or_default(),
            exit_code: row.get(3)?,
            snapshot_id: row.get(4)?,
            created_at_ms: row.get(5)?,
            seq: row.get(6)?,
            structured_failures,
        })
    })?;
    let mut facts = Vec::new();
    for fact in rows {
        facts.push(fact?);
    }
    Ok(facts)
}

/// Evaluate the declarative check for a proposal against the attempt's full
/// evidence set, on a caller-supplied connection. The single source of truth for
/// the verdict (NER-135 R4): `record_check` and the in-txn `accept` gate both call
/// this on their own `&tx`, so the determining read stays inside the writer's
/// transaction (NER-132 U2).
pub(crate) fn evaluate_check_on(
    conn: &Connection,
    attempt: &AttemptRecord,
    proposal: &ProposalSummary,
) -> Result<forge_policy::CheckOutcome> {
    let spec = intent_check_spec(conn, &attempt.intent_id)?;
    let facts = evidence_facts_on(conn, &attempt.attempt_id)?;
    let outcome = forge_policy::evaluate(&spec, &proposal.snapshot_id, &facts);
    // Integrity gate (NER-136 R4), fail-CLOSED: if any evidence row that DECIDES a
    // gate was tampered with (its stored content_hash no longer matches a recompute,
    // or a post-watermark hash is missing), refuse. Runs on `&tx` at BOTH
    // `record_check` and `decide`, and — being raised here, before the enforce_check
    // branch — it refuses even under `accept --allow-unverified` (a policy bypass is
    // never an integrity bypass). The deeper full-chain re-walk lives in `doctor`.
    let marker = evidence_high_water(conn)?;
    for gate in &outcome.gates {
        if let Some(evidence_id) = &gate.evidence_id {
            if let IntegrityStatus::Tampered(kind) =
                verify_evidence_integrity(conn, evidence_id, marker)?
            {
                return Err(ForgeError::EvidenceTampered {
                    id: evidence_id.clone(),
                    kind,
                }
                .into());
            }
        }
    }
    Ok(outcome)
}

/// The verdict of re-verifying a hashed row against its stored `content_hash`.
pub(crate) enum IntegrityStatus {
    /// Recomputed hash matches the stored one.
    Verified,
    /// A NULL hash on a pre-watermark row — predates Phase 5, grandfathered.
    LegacyUnverified,
    /// The row was tampered with.
    Tampered(TamperKind),
}

/// The recorded `evidence` rowid high-water mark from migration 004 — the boundary
/// that distinguishes a legacy NULL hash (rowid ≤ mark) from a deleted one (rowid >
/// mark). Keyed on the immutable rowid, never a per-row timestamp the attacker can
/// backdate (NER-136). A fresh post-Phase-5 repo records 0, so any NULL hash is a
/// deletion.
pub(crate) fn evidence_high_water(conn: &Connection) -> Result<i64> {
    let mark: Option<i64> = conn
        .query_row(
            "SELECT evidence_high_water FROM integrity_marker WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(mark.unwrap_or(0))
}

/// Recompute an evidence row's content hash from its stored columns and compare to
/// the persisted `content_hash` (NER-136). Reads the full row on the caller's `&tx`
/// so the determining read stays inside the writer's transaction. This is the cheap
/// per-row gate-path check; it catches the naive "edit a field, leave the hash stale"
/// tamper (the literal Phase 4 honesty-note hole). Catching a *recomputed* row hash
/// requires the op-log re-walk that `doctor` performs.
pub(crate) fn verify_evidence_integrity(
    conn: &Connection,
    evidence_id: &str,
    marker: i64,
) -> Result<IntegrityStatus> {
    let row = conn
        .query_row(
            "SELECT attempt_id, snapshot_id, command, args_json, cwd, exit_code,
                    started_at_ms, ended_at_ms, timed_out, stdout_excerpt, stderr_excerpt,
                    stdout_truncated, stderr_truncated, sensitivity, actor, structured_json,
                    created_at_ms, content_hash, rowid
             FROM evidence WHERE id = ?1",
            params![evidence_id],
            |row| {
                Ok(StoredEvidence {
                    attempt_id: row.get(0)?,
                    snapshot_id: row.get(1)?,
                    command: row.get(2)?,
                    args_json: row.get(3)?,
                    cwd: row.get(4)?,
                    exit_code: row.get(5)?,
                    started_at_ms: row.get(6)?,
                    ended_at_ms: row.get(7)?,
                    timed_out: row.get::<_, i64>(8)? != 0,
                    stdout_excerpt: row.get(9)?,
                    stderr_excerpt: row.get(10)?,
                    stdout_truncated: row.get::<_, i64>(11)? != 0,
                    stderr_truncated: row.get::<_, i64>(12)? != 0,
                    sensitivity: row.get(13)?,
                    actor: row.get(14)?,
                    structured_json: row.get(15)?,
                    created_at_ms: row.get(16)?,
                    content_hash: row.get(17)?,
                    rowid: row.get(18)?,
                })
            },
        )
        .optional()?;
    // No row to verify (e.g. a default-mode gate with no deciding evidence) is not a
    // tamper signal.
    let Some(row) = row else {
        return Ok(IntegrityStatus::Verified);
    };
    let Some(stored_hash) = row.content_hash else {
        return Ok(if row.rowid <= marker {
            IntegrityStatus::LegacyUnverified
        } else {
            IntegrityStatus::Tampered(TamperKind::MissingHash)
        });
    };
    let args: Vec<String> = serde_json::from_str(&row.args_json).unwrap_or_default();
    let recomputed = integrity::evidence_digest(&integrity::EvidenceDigestInput {
        attempt_id: &row.attempt_id,
        snapshot_id: row.snapshot_id.as_deref(),
        command: &row.command,
        args: &args,
        cwd: &row.cwd,
        exit_code: row.exit_code,
        started_at_ms: row.started_at_ms,
        ended_at_ms: row.ended_at_ms,
        timed_out: row.timed_out,
        stdout_excerpt: &row.stdout_excerpt,
        stderr_excerpt: &row.stderr_excerpt,
        stdout_truncated: row.stdout_truncated,
        stderr_truncated: row.stderr_truncated,
        sensitivity: &row.sensitivity,
        actor: &row.actor,
        structured_json: row.structured_json.as_deref(),
        created_at_ms: row.created_at_ms,
    });
    Ok(if recomputed == stored_hash {
        IntegrityStatus::Verified
    } else {
        IntegrityStatus::Tampered(TamperKind::ContentEdit)
    })
}

/// The full evidence row read back for integrity verification.
struct StoredEvidence {
    attempt_id: String,
    snapshot_id: Option<String>,
    command: String,
    args_json: String,
    cwd: String,
    exit_code: i64,
    started_at_ms: i64,
    ended_at_ms: i64,
    timed_out: bool,
    stdout_excerpt: String,
    stderr_excerpt: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    sensitivity: String,
    actor: String,
    structured_json: Option<String>,
    created_at_ms: i64,
    content_hash: Option<String>,
    rowid: i64,
}

pub fn decide(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
    decision: &str,
    enforce_check: bool,
    actor: &str,
) -> Result<DecisionRecord> {
    let context = open_repository(cwd)?;
    let resolved_attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    // NER-260: with an explicit `--proposal`, the OWNING attempt (and thus the intent
    // whose gate spec is evaluated and whose id is stamped into the native commit) is
    // derived from the proposal, NOT the caller's attached attempt. Rebind `attempt` to
    // the proposal's owning attempt so `evaluate_check_on` and `CommitObject.intent_id`
    // below use the correct intent.
    let resolved = resolve_proposal(&context, &resolved_attempt, proposal_id, true)?;
    let proposal = resolved.proposal;
    let attempt = resolved.attempt;
    let signer = signing::LocalSigner::load_or_create(&context.root_path)?;
    let mut connection = open_connection(&context.database_path)?;

    // NER-138 slice 3: a native repo is detected by routing `base_head` through the canonical
    // ObjectId parser (a git repo's `base_head` is a 40-hex git SHA, which does not parse as
    // an `f1:` commit id). When native + accepted, a justified Commit is written and the
    // ref-store HEAD advanced; git repos and rejects leave `commit_id` NULL and never touch
    // the ref store.
    let native_base = forge_content_native::ObjectId::parse(&proposal.base_head)
        .ok()
        .filter(|id| matches!(id.kind(), Ok(forge_content_native::ObjectKind::Commit)));
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);

    let (decision_id, check_status, op, commit_id) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        // Evidence gate (NER-135 R6): for `accept`, re-evaluate the declarative
        // check IN this IMMEDIATE txn (not on a separate connection), so the verdict
        // that gates the decision is computed from the same facts the decision
        // commits against — closing the TOCTOU against the lock-free `run` writer
        // (NER-132 U2). `evaluate_check_on` ALSO fails closed on a tampered deciding
        // row (NER-136), and — being raised before the enforce_check branch — that
        // refusal holds even under `--allow-unverified`.
        let (check_status, deciding_evidence_id) = if decision == "accepted" {
            let outcome = evaluate_check_on(tx, &attempt, &proposal)?;
            if enforce_check && !outcome.passed() {
                return Err(ForgeError::CheckNotPassed {
                    status: outcome.status.clone(),
                    unmet: outcome.unmet_identities(),
                }
                .into());
            }
            // The deciding gate's evidence id (failed gate first, else any with evidence)
            // — its content_hash becomes the commit's opaque evidence_digest below.
            let deciding_evidence_id = representative_evidence_id(&outcome);
            (Some(outcome.status), deciding_evidence_id)
        } else {
            (None, None)
        };
        let decision_id = new_id("decision");
        // Recomputed per busy-retry: the decision digest covers proposal/revision +
        // decision + actor + timestamp, and is both stored on the row and folded into
        // the op-log spine so editing a decision row is detectable (NER-136 R3/R4).
        let created = now_ms();
        let content_hash = integrity::decision_digest(&integrity::DecisionDigestInput {
            proposal_id: &proposal.proposal_id,
            proposal_revision_id: &proposal.proposal_revision_id,
            decision,
            actor,
            created_at_ms: created,
        });
        // NER-138 slice 3: for a native accept, build + DURABLY write the justified commit
        // BEFORE the decision row that references it (store-before-DB). A busy-retry re-runs
        // this closure, minting a fresh decision_id -> a fresh commit object; the loser is an
        // unreferenced orphan (gc-collectible). `actor` + `authored_time` (= the decision
        // timestamp) are in the HASHED bytes so Phase 9 signs who/when. `evidence_digest` is
        // the deciding gate's evidence content_hash wrapped in `Hex64` (excerpt text is
        // structurally unrepresentable). HEAD is advanced only AFTER this txn commits.
        let commit_id = match (decision, &native_base) {
            ("accepted", Some(parent)) => {
                let tree = proposal
                    .content_ref
                    .strip_prefix(forge_content::FORGE_TREE_PREFIX)
                    .ok_or_else(|| {
                        anyhow!("native accepted proposal has a non-forge-tree content ref")
                    })?
                    .to_string();
                let evidence_digest = match &deciding_evidence_id {
                    Some(evidence_id) => {
                        let hash: Option<String> = tx
                            .query_row(
                                "SELECT content_hash FROM evidence WHERE id = ?1",
                                params![evidence_id],
                                |row| row.get(0),
                            )
                            .optional()?;
                        match hash {
                            Some(hash) => Some(forge_content_native::Hex64::new(hash)?),
                            None => None,
                        }
                    }
                    None => None,
                };
                let parents = if let Some((ours_head, base_head)) =
                    resolved_merge_parents_for_proposal_on(
                        tx,
                        &context.repo_id,
                        &proposal.proposal_id,
                        Some(&proposal.content_ref),
                    )? {
                    forge_content_native::ObjectId::parse(&ours_head)?;
                    forge_content_native::ObjectId::parse(&base_head)?;
                    if ours_head == base_head {
                        vec![ours_head]
                    } else {
                        vec![ours_head, base_head]
                    }
                } else {
                    vec![parent.to_string()]
                };
                let commit = forge_content_native::CommitObject {
                    schema_version: forge_content_native::COMMIT_SCHEMA_VERSION,
                    tree,
                    parents,
                    intent_id: Some(attempt.intent_id.clone()),
                    proposal_revision_id: Some(proposal.proposal_revision_id.clone()),
                    decision_id: Some(decision_id.clone()),
                    evidence_digest,
                    actor: Some(actor.to_string()),
                    authored_time: Some(created),
                };
                Some(store.write_commit(&commit)?.to_string())
            }
            _ => None,
        };
        tx.execute(
            "INSERT INTO decisions (id, repo_id, proposal_id, proposal_revision_id, decision, actor, content_hash, created_at_ms, commit_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                decision_id,
                context.repo_id,
                proposal.proposal_id,
                proposal.proposal_revision_id,
                decision,
                actor,
                content_hash,
                created,
                commit_id
            ],
        )?;
        signer.sign_subject(
            tx,
            &context.repo_id,
            "decision",
            &decision_id,
            &content_hash,
            created,
        )?;
        if let Some(commit_id) = &commit_id {
            signer.sign_subject(
                tx,
                &context.repo_id,
                "commit",
                commit_id,
                commit_id,
                created,
            )?;
        }
        tx.execute(
            "UPDATE proposals SET status = ?1 WHERE id = ?2",
            params![decision, proposal.proposal_id],
        )?;
        if decision == "accepted" {
            record_embargo_accept_on(tx, &context.repo_id, &proposal.proposal_id, actor, created)?;
        }
        let op = insert_operation_view_chained(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: decision.to_string(),
                kind: format!("proposal_{decision}"),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": decision, "decision_id": decision_id }),
            },
            Some(&content_hash),
        )?;
        Ok((decision_id, check_status, op, commit_id))
    })?;

    // Advance the ref-store HEAD AFTER the decision row durably committed (store-before-DB
    // + HEAD-lags-never-leads): a crash here leaves HEAD one commit behind the ledger, which
    // `reconcile_native_head` heals on the next command — never HEAD ahead of an
    // uncommitted decision. `commit_id` is the COMMITTED attempt's value returned from the
    // closure (not a closure-captured outer var), so a busy-retry advances HEAD to the winner.
    if let Some(commit_id) = &commit_id {
        let refs = forge_content_native::NativeRefStore::new(&context.root_path);
        refs.set_head(&forge_content_native::ObjectId::parse(commit_id)?)?;
    }

    Ok(DecisionRecord {
        decision_id,
        proposal_id: proposal.proposal_id,
        proposal_revision_id: proposal.proposal_revision_id,
        decision: decision.to_string(),
        check_status,
        commit_id,
        operation_id: op.operation_id,
    })
}

/// Verify an accepted proposal's decision row integrity before trusting `accepted`
/// at `export branch` (NER-136 R4). There is no in-txn site on the export path (the
/// git branch is created before `record_publication`'s txn opens), so this is a
/// verifying read under the held repo lock, before the branch. A mismatch means the
/// decision row was tampered with → refuse with `EVIDENCE_TAMPERED`.
pub fn verify_decision_integrity(cwd: &Path, proposal_revision_id: &str) -> Result<()> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let row = connection
        .query_row(
            "SELECT id, proposal_id, decision, actor, content_hash, created_at_ms, rowid
             FROM decisions
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, proposal_revision_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .optional()?;
    let Some((id, proposal_id, decision, actor, content_hash, created_at_ms, rowid)) = row else {
        return Ok(());
    };
    let Some(stored_hash) = content_hash else {
        // A decision predating Phase 5 (legacy) is grandfathered; a post-watermark
        // NULL is a deletion.
        let marker = decision_high_water(&connection)?;
        if rowid <= marker {
            return Ok(());
        }
        return Err(ForgeError::EvidenceTampered {
            id,
            kind: TamperKind::MissingHash,
        }
        .into());
    };
    let recomputed = integrity::decision_digest(&integrity::DecisionDigestInput {
        proposal_id: &proposal_id,
        proposal_revision_id,
        decision: &decision,
        actor: &actor,
        created_at_ms,
    });
    if recomputed != stored_hash {
        return Err(ForgeError::EvidenceTampered {
            id,
            kind: TamperKind::ContentEdit,
        }
        .into());
    }
    Ok(())
}

/// The recorded `decisions` rowid high-water mark — the legacy/tampered boundary for
/// decision rows (rowid ≤ mark predates Phase 5; rowid > mark with a NULL hash is a
/// deletion).
pub(crate) fn decision_high_water(conn: &Connection) -> Result<i64> {
    let mark: Option<i64> = conn
        .query_row(
            "SELECT decision_high_water FROM integrity_marker WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(mark.unwrap_or(0))
}

pub fn latest_decision(cwd: &Path) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    latest_decision_for_context(&context)
}

pub fn pr_body(cwd: &Path) -> Result<(String, Vec<String>)> {
    pr_body_for(cwd, None, None)
}

/// Render the PR-body markdown, returning `(body, excluded)`. Secret-risk-named
/// changed paths are dropped from the "Changed Paths" list by the default-deny
/// export policy (NER-133 U6) and returned in `excluded` so the CLI can surface them
/// as `warnings[]`.
pub fn pr_body_for(
    cwd: &Path,
    attempt_id: Option<&str>,
    proposal_id: Option<&str>,
) -> Result<(String, Vec<String>)> {
    let context = open_repository(cwd)?;
    let resolved_attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    // NER-260: render the PR body for the proposal's OWNING attempt (intent text,
    // evidence, competing-attempt comparison) so an explicit cross-attempt `--proposal`
    // describes the right intent rather than the caller's attached one.
    let resolved = resolve_proposal(&context, &resolved_attempt, proposal_id, true)?;
    let proposal = resolved.proposal;
    let attempt = resolved.attempt;
    let evidence = latest_evidence_for_attempt(&context, &attempt.attempt_id)?;
    let check = latest_check_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let decision = latest_decision_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let mut body = String::new();
    body.push_str("# Forge Proposal\n\n");
    body.push_str(&format!("Intent: {}\n\n", attempt.intent));
    let (kept_paths, excluded_paths) = forge_content::filter_secret_risk(&proposal.changed_paths);
    body.push_str("## Changed Paths\n");
    for path in &kept_paths {
        body.push_str(&format!("- {path}\n"));
    }
    body.push('\n');
    // Cite the competing attempts against the declared intent (NER-137 R9): the
    // ranked comparison replaces the single-latest-evidence under-report. Uses the
    // same verify-then-rank engine, so a cheap-check-tampered rival shows as
    // `integrity: tampered` / unranked rather than as a silent passing row.
    let comparison = compare_attempts(
        cwd,
        CompareSelector {
            intent_id: Some(attempt.intent_id.clone()),
            attempt_id: None,
        },
    )?;
    if let Some(group) = comparison.intents.first() {
        body.push_str("## Competing Attempts\n");
        for row in &group.attempts {
            let rank = row
                .rank
                .map(|rank| format!("rank {rank}"))
                .unwrap_or_else(|| "unranked".to_string());
            let check = row.check_status.as_deref().unwrap_or("n/a");
            let marker = if row
                .proposal
                .as_ref()
                .map(|candidate| candidate.proposal_id.as_str())
                == Some(proposal.proposal_id.as_str())
            {
                " ← this proposal"
            } else {
                ""
            };
            body.push_str(&format!(
                "- {rank}: attempt `{}` — check {check}, integrity {}{}\n",
                row.attempt_id, row.integrity, marker
            ));
        }
        body.push('\n');
    }
    if let Some(evidence) = evidence {
        body.push_str("## Evidence\n");
        body.push_str(&format!(
            "- `{}` exited with `{}` ({})\n\n",
            evidence.command, evidence.exit_code, evidence.trust
        ));
    }
    if let Some(check) = check {
        body.push_str("## Check\n");
        body.push_str(&format!("- {}: {}\n\n", check.status, check.reason));
    }
    if let Some(decision) = decision {
        body.push_str("## Decision\n");
        body.push_str(&format!("- {decision}\n"));
    }
    if let Some(publication) =
        latest_publication_for_proposal_revision(&context, &proposal.proposal_revision_id)?
    {
        body.push_str("\n## Publication\n");
        body.push_str(&format!(
            "- `{}` at `{}`\n",
            publication.branch_name, publication.commit_id
        ));
    }
    Ok((body, excluded_paths))
}

pub fn attempt_proposal_content_ref(cwd: &Path, attempt_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    let attempt =
        attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?;
    let proposal = latest_proposal_for_attempt(&context, &attempt.attempt_id)?
        .ok_or(ForgeError::NoProposal)?;
    Ok(proposal.content_ref)
}

pub fn proposal_for_merge(cwd: &Path, proposal_id: &str) -> Result<ProposalSummary> {
    let context = open_repository(cwd)?;
    proposal_by_id(&context, proposal_id)?.ok_or_else(|| {
        ForgeError::UnknownProposal {
            selector: proposal_id.to_string(),
        }
        .into()
    })
}

pub fn resolved_merge_ours_head(
    cwd: &Path,
    proposal_id: &str,
    content_ref: &str,
) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    resolved_merge_parents_for_proposal_on(
        &connection,
        &context.repo_id,
        proposal_id,
        Some(content_ref),
    )
    .map(|parents| parents.map(|(ours, _base)| ours))
}

pub(crate) fn latest_evidence_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<EvidenceSummary>> {
    let connection = open_connection(&context.database_path)?;
    latest_evidence_on(&connection, attempt_id)
}

/// Determining "latest evidence" read against a caller-supplied connection (see
/// [`latest_snapshot_on`]); used inside `record_check`'s `IMMEDIATE` txn (U4).
pub(crate) fn latest_evidence_on(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<EvidenceSummary>> {
    connection
        .query_row(
            "SELECT id, snapshot_id, command, args_json, exit_code, sensitivity, trust FROM evidence
             WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![attempt_id],
            |row| {
                let args_json: String = row.get(3)?;
                Ok(EvidenceSummary {
                    evidence_id: row.get(0)?,
                    snapshot_id: row.get(1)?,
                    command: row.get(2)?,
                    args: serde_json::from_str(&args_json).unwrap_or_default(),
                    exit_code: row.get(4)?,
                    sensitivity: row.get(5)?,
                    trust: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

/// Resolve a proposal under a caller-resolved `attempt`.
///
/// NER-260: when an explicit `proposal_id` is supplied, the proposal is resolved
/// GLOBALLY by id (across every attempt/intent in the repo) and the OWNING attempt is
/// derived from `proposal.attempt_id`, exactly mirroring how `proposal_by_id` already
/// works for the export paths. The previous hard cross-check (`proposal.attempt_id !=
/// attempt.attempt_id => UnknownProposal`) is removed so `accept --proposal <id>`
/// succeeds even when a DIFFERENT attempt is attached. A genuinely non-existent id
/// still yields `UnknownProposal`. The returned `attempt` is the owning one — callers
/// MUST use it for gate evaluation and commit metadata so a cross-attempt `--proposal`
/// is judged and recorded under its own intent.
///
/// The no-`--proposal` default branch (single-default / ambiguity) is byte-for-byte
/// unchanged and reuses the passed `attempt` directly, so `--attempt A` and the no-arg
/// "current/attached attempt" behavior are preserved (and no extra DB connection is
/// opened on that path).
pub(crate) fn resolve_proposal(
    context: &RepositoryContext,
    attempt: &AttemptRecord,
    proposal_id: Option<&str>,
    allow_single_default: bool,
) -> Result<ResolvedProposal> {
    if let Some(proposal_id) = proposal_id {
        let proposal =
            proposal_by_id(context, proposal_id)?.ok_or_else(|| ForgeError::UnknownProposal {
                selector: proposal_id.to_string(),
            })?;
        // Derive the OWNING attempt from the globally-resolved proposal rather than
        // cross-checking against the caller's attempt (NER-260). Reuse the passed
        // record when it already is the owner to avoid an extra DB open.
        let owning_attempt = if proposal.attempt_id == attempt.attempt_id {
            attempt.clone()
        } else {
            attempt_by_id(context, &proposal.attempt_id)?.ok_or_else(|| {
                // The proposal references an attempt that no longer exists; treat the
                // selector as unresolvable rather than panicking.
                ForgeError::UnknownProposal {
                    selector: proposal_id.to_string(),
                }
            })?
        };
        return Ok(ResolvedProposal {
            proposal,
            attempt: owning_attempt,
        });
    }

    let proposals = proposals_for_attempt(context, &attempt.attempt_id)?;
    match proposals.as_slice() {
        [] => Err(ForgeError::NoProposal.into()),
        [proposal] if allow_single_default => Ok(ResolvedProposal {
            proposal: proposal.clone(),
            attempt: attempt.clone(),
        }),
        _ => Err(ForgeError::AmbiguousProposal {
            candidate_ids: proposals
                .iter()
                .map(|proposal| proposal.proposal_id.clone())
                .collect(),
        }
        .into()),
    }
}

pub(crate) fn proposal_by_id(
    context: &RepositoryContext,
    proposal_id: &str,
) -> Result<Option<ProposalSummary>> {
    let connection = open_connection(&context.database_path)?;
    proposal_by_id_on(&connection, context, proposal_id)
}

pub(crate) fn proposal_by_id_on(
    connection: &Connection,
    context: &RepositoryContext,
    proposal_id: &str,
) -> Result<Option<ProposalSummary>> {
    connection
        .query_row(
            "SELECT p.id, pr.id, p.attempt_id, p.snapshot_id, p.base_head, pr.content_ref, pr.changed_paths_json
             FROM proposals p
             JOIN proposal_revisions pr ON pr.proposal_id = p.id
             WHERE p.repo_id = ?1 AND p.id = ?2
             ORDER BY pr.created_at_ms DESC, pr.rowid DESC LIMIT 1",
            params![context.repo_id, proposal_id],
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
        .optional()
        .map_err(Into::into)
}

pub(crate) fn resolved_merge_parents_for_proposal_on(
    connection: &Connection,
    repo_id: &str,
    proposal_id: &str,
    content_ref: Option<&str>,
) -> Result<Option<(String, String)>> {
    let mut merge_statement = connection.prepare(
        "SELECT v.state_json
         FROM views v
         JOIN operations o ON o.id = v.operation_id
         WHERE v.repo_id = ?1 AND v.kind = 'merge_clean' AND o.status = 'success'
         ORDER BY v.created_at_ms DESC, v.rowid DESC",
    )?;
    let merge_rows = merge_statement.query_map(params![repo_id], |row| row.get::<_, String>(0))?;
    for row in merge_rows {
        let state_json = row?;
        let Ok(value) = serde_json::from_str::<Value>(&state_json) else {
            continue;
        };
        if value.get("proposal_id").and_then(Value::as_str) != Some(proposal_id) {
            continue;
        }
        if let Some(content_ref) = content_ref {
            if value.get("merged_content_ref").and_then(Value::as_str) != Some(content_ref) {
                continue;
            }
        }
        let Some(ours_head) = value
            .get("ours_head")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(base_head) = value
            .get("base_head")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        return Ok(Some((ours_head, base_head)));
    }

    let mut statement = connection.prepare(
        "SELECT id, paths_json
         FROM conflict_sets
         WHERE repo_id = ?1 AND resolver_backend = 'native_merge' AND status = 'resolved'
         ORDER BY created_at_ms DESC, rowid DESC",
    )?;
    let rows = statement.query_map(params![repo_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (conflict_set_id, paths_json) = row?;
        let Ok(value) = serde_json::from_str::<Value>(&paths_json) else {
            continue;
        };
        if value.get("proposal_id").and_then(Value::as_str) != Some(proposal_id) {
            continue;
        }
        if let Some(content_ref) = content_ref {
            let matching_resolutions: i64 = connection.query_row(
                "SELECT COUNT(*)
                 FROM path_conflicts
                 WHERE conflict_set_id = ?1
                   AND resolution_ref = ?2",
                params![conflict_set_id, content_ref],
                |row| row.get(0),
            )?;
            if matching_resolutions == 0 {
                continue;
            }
        }
        let Some(ours_head) = value
            .get("ours_head")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(base_head) = value
            .get("base_head")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        return Ok(Some((ours_head, base_head)));
    }
    Ok(None)
}

pub(crate) fn proposals_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Vec<ProposalSummary>> {
    let connection = open_connection(&context.database_path)?;
    let mut statement = connection.prepare(
        "SELECT p.id, pr.id, p.attempt_id, p.snapshot_id, p.base_head, pr.content_ref, pr.changed_paths_json
         FROM proposals p
         JOIN proposal_revisions pr ON pr.proposal_id = p.id
         WHERE p.repo_id = ?1 AND p.attempt_id = ?2
           AND NOT EXISTS (
               SELECT 1 FROM proposal_revisions newer
               WHERE newer.proposal_id = pr.proposal_id
                 AND (newer.created_at_ms > pr.created_at_ms
                      OR (newer.created_at_ms = pr.created_at_ms AND newer.rowid > pr.rowid))
           )
         ORDER BY pr.created_at_ms ASC",
    )?;
    let rows = statement.query_map(params![context.repo_id, attempt_id], |row| {
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
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub(crate) fn latest_proposal_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<ProposalSummary>> {
    Ok(proposals_for_attempt(context, attempt_id)?.pop())
}

pub fn list_proposals(cwd: &Path, attempt_id: Option<&str>) -> Result<Vec<ProposalMetadata>> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    proposal_metadata_for_attempt(&context, &attempt.attempt_id)
}

pub fn proposal_review(
    cwd: &Path,
    proposal_id: &str,
    recipient: Option<&str>,
) -> Result<ProposalReview> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let proposal = proposal_by_id_on(&connection, &context, proposal_id)?.ok_or_else(|| {
        ForgeError::UnknownProposal {
            selector: proposal_id.to_string(),
        }
    })?;
    let attempt = attempt_by_id(&context, &proposal.attempt_id)?.ok_or_else(|| {
        ForgeError::UnknownProposal {
            selector: proposal_id.to_string(),
        }
    })?;
    let intent = intent_detail_on(&connection, &context, &attempt.intent_id)?;
    let check = latest_check_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let decision = latest_decision_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let publication =
        latest_publication_for_proposal_revision(&context, &proposal.proposal_revision_id)?;
    let latest_evidence = latest_evidence_for_attempt(&context, &attempt.attempt_id)?;
    let trust_policy = trust_policy_on(&connection)?;
    let private_labels = local_private_path_labels(cwd, "proposal", &proposal.proposal_id)?;
    let private_path_count = private_labels.len() as i64;
    let visibility = review_visibility(
        &connection,
        &context,
        &proposal.proposal_id,
        recipient,
        private_path_count,
    )?;
    let changed_paths = sanitize_review_changed_paths(&proposal.changed_paths, &private_labels);
    let metadata = ProposalMetadata {
        proposal_id: proposal.proposal_id.clone(),
        proposal_revision_id: proposal.proposal_revision_id.clone(),
        attempt_id: proposal.attempt_id.clone(),
        snapshot_id: proposal.snapshot_id.clone(),
        base_head: proposal.base_head.clone(),
        content_ref: proposal.content_ref.clone(),
        changed_paths: changed_paths.iter().map(|path| path.path.clone()).collect(),
        check_status: check.as_ref().map(|check| check.status.clone()),
        decision_status: decision.clone(),
        publication_status: publication.as_ref().map(|_| "published".to_string()),
    };
    let sibling_attempts = attempts_for_intent(&connection, &context.repo_id, &attempt.intent_id)?
        .into_iter()
        .map(|candidate| {
            let proposal_count = proposals_for_attempt(&context, &candidate.attempt_id)
                .map(|proposals| proposals.len())
                .unwrap_or(0);
            ReviewAttemptContext {
                is_owner: candidate.attempt_id == attempt.attempt_id,
                attempt_id: candidate.attempt_id,
                status: candidate.status,
                proposal_count,
            }
        })
        .collect();
    let lifecycle = ReviewLifecycle {
        check_status: metadata.check_status.clone(),
        decision_status: decision.clone(),
        publication_status: metadata.publication_status.clone(),
        publication,
        sibling_attempts,
    };
    let evidence_audit = ReviewEvidenceAudit {
        latest_evidence: latest_evidence.clone(),
        latest_check: check.clone(),
        trust_policy: trust_policy.clone(),
    };
    let (readiness, terminal_handoffs) = review_readiness(
        &proposal.proposal_id,
        check.as_ref(),
        latest_evidence.as_ref(),
        decision.as_deref(),
        metadata.publication_status.as_deref(),
        &trust_policy,
        &visibility,
    );
    let diff = ReviewDiff {
        content_ref: proposal.content_ref,
        changed_paths,
    };
    Ok(ProposalReview {
        proposal: metadata,
        attempt,
        intent,
        readiness,
        lifecycle,
        visibility,
        evidence_audit,
        diff,
        terminal_handoffs,
    })
}

pub(crate) fn sanitize_review_changed_paths(
    changed_paths: &[String],
    private_labels: &[LocalPrivatePathLabel],
) -> Vec<ReviewChangedPath> {
    changed_paths
        .iter()
        .map(|path| {
            if private_labels.iter().any(|label| label.path == *path) {
                ReviewChangedPath {
                    path: "[restricted private path]".to_string(),
                    status: "restricted".to_string(),
                }
            } else {
                ReviewChangedPath {
                    path: path.clone(),
                    status: "changed".to_string(),
                }
            }
        })
        .collect()
}

fn review_visibility(
    connection: &Connection,
    context: &RepositoryContext,
    proposal_id: &str,
    recipient: Option<&str>,
    private_path_label_count: i64,
) -> Result<ReviewVisibility> {
    let visibility = effective_work_package_visibility_on(
        connection,
        &context.repo_id,
        "proposal",
        proposal_id,
    )?;
    let embargo = embargo_workflow_on(connection, &context.repo_id, "proposal", proposal_id)?.map(
        |workflow| ReviewEmbargo {
            release_allowed: workflow.state == EMBARGO_STATE_ACCEPTED_UNDER_EMBARGO
                || workflow.state == EMBARGO_STATE_RELEASED_UNDER_EMBARGO,
            reveal_allowed: workflow.state == EMBARGO_STATE_RELEASED_UNDER_EMBARGO,
            publish_allowed: workflow.state == EMBARGO_STATE_REVEALED,
            export_allowed: workflow.state == EMBARGO_STATE_PUBLISHED
                && workflow.public_projection_mode.as_deref()
                    == Some(PUBLIC_PROJECTION_FULL_SOURCE),
            state: workflow.state,
            public_projection_mode: workflow.public_projection_mode,
        },
    );
    let mut projection_checks = Vec::new();
    let (projection, disclosure) = if let Some(recipient) = recipient {
        for capability in [
            CAPABILITY_SEE_STUB,
            CAPABILITY_INSPECT_CONTENT,
            CAPABILITY_INSPECT_EVIDENCE,
            CAPABILITY_SYNC_MATERIALIZE,
            CAPABILITY_PUBLISH_REVEAL,
        ] {
            projection_checks.push(projection_decision_on(
                connection,
                &context.repo_id,
                "proposal",
                proposal_id,
                recipient,
                capability,
            )?);
        }
        let disclosure = projection_checks
            .iter()
            .find(|decision| decision.capability == CAPABILITY_INSPECT_CONTENT)
            .map(|decision| decision.disclosure.clone())
            .unwrap_or_else(|| "hidden".to_string());
        (format!("recipient:{recipient}"), disclosure)
    } else {
        ("sanitized".to_string(), "redacted".to_string())
    };
    let private_path_detail = if private_path_label_count > 0 {
        "restricted_count_only".to_string()
    } else {
        "none".to_string()
    };
    Ok(ReviewVisibility {
        projection,
        visibility,
        disclosure,
        private_path_label_count,
        private_path_detail,
        embargo,
        projection_checks,
    })
}

fn review_readiness(
    proposal_id: &str,
    check: Option<&CheckSummary>,
    evidence: Option<&EvidenceSummary>,
    decision: Option<&str>,
    publication_status: Option<&str>,
    trust_policy: &TrustPolicy,
    visibility: &ReviewVisibility,
) -> (ReviewReadiness, Vec<ReviewTerminalHandoff>) {
    let mut factors = Vec::new();
    let mut handoffs = Vec::new();
    match check {
        Some(check) if check.status == "passed" => factors.push(review_factor(
            "info",
            "check_passed",
            "latest check passed",
            Some("check"),
        )),
        Some(check) => factors.push(review_factor(
            "blocker",
            "check_not_passed",
            format!("latest check is {}: {}", check.status, check.reason),
            Some("check"),
        )),
        None => factors.push(review_factor(
            "blocker",
            "missing_check",
            "no check result exists for this proposal revision",
            Some("evidence_audit"),
        )),
    }
    if let Some(evidence) = evidence {
        let evidence_rank = trust_rank(&evidence.trust).unwrap_or(0);
        let required_rank = trust_rank(&trust_policy.min_accept_trust).unwrap_or(u8::MAX);
        if evidence_rank < required_rank {
            factors.push(review_factor(
                "blocker",
                "trust_policy_unmet",
                format!(
                    "latest evidence trust `{}` is below accept policy `{}`",
                    evidence.trust, trust_policy.min_accept_trust
                ),
                Some("evidence_audit"),
            ));
        } else {
            factors.push(review_factor(
                "info",
                "trust_policy_met",
                format!(
                    "latest evidence trust `{}` satisfies accept policy `{}`",
                    evidence.trust, trust_policy.min_accept_trust
                ),
                Some("evidence_audit"),
            ));
        }
    } else {
        factors.push(review_factor(
            "blocker",
            "missing_evidence",
            "no command evidence exists for the owning attempt",
            Some("evidence_audit"),
        ));
    }
    if visibility.private_path_label_count > 0 {
        factors.push(review_factor(
            "risk",
            "restricted_content",
            format!(
                "{} private path label(s) are represented by restricted metadata only",
                visibility.private_path_label_count
            ),
            Some("visibility"),
        ));
    }
    if let Some(embargo) = &visibility.embargo {
        if !embargo.export_allowed {
            factors.push(review_factor(
                "risk",
                "embargo_not_public_exportable",
                format!("embargo workflow is `{}`", embargo.state),
                Some("visibility"),
            ));
        }
    }
    match decision {
        Some("rejected") => factors.push(review_factor(
            "blocker",
            "proposal_rejected",
            "proposal has been rejected",
            Some("lifecycle"),
        )),
        Some("accepted") => {
            if publication_status == Some("published") {
                factors.push(review_factor(
                    "info",
                    "published",
                    "accepted proposal has been exported or published",
                    Some("lifecycle"),
                ));
            } else {
                factors.push(review_factor(
                    "risk",
                    "accepted_not_published",
                    "proposal is accepted but not yet published/exported",
                    Some("lifecycle"),
                ));
                handoffs.push(review_handoff(
                    "Export branch",
                    format!("forge export branch --proposal {proposal_id} <branch-name>"),
                    "accepted proposals still need an explicit terminal export",
                ));
            }
        }
        _ => {
            handoffs.push(review_handoff(
                "Accept proposal",
                format!("forge accept --proposal {proposal_id}"),
                "accept remains a terminal-enforced trust-bearing action",
            ));
            handoffs.push(review_handoff(
                "Reject proposal",
                format!("forge reject --proposal {proposal_id}"),
                "reject remains a terminal-enforced decision",
            ));
        }
    }
    if check.is_none() || check.is_some_and(|check| check.status != "passed") {
        handoffs.insert(
            0,
            review_handoff(
                "Run check",
                format!("forge check --proposal {proposal_id}"),
                "a passing check is required before normal acceptance",
            ),
        );
    }
    let has_blocker = factors.iter().any(|factor| factor.severity == "blocker");
    let has_risk = factors.iter().any(|factor| factor.severity == "risk");
    let status = if has_blocker {
        "blocked"
    } else if has_risk {
        "risky"
    } else {
        "ready"
    };
    let summary = match status {
        "ready" => "proposal is ready for the next terminal action",
        "risky" => "proposal can be reviewed, but visibility, embargo, or lifecycle risks remain",
        _ => "proposal is blocked until the listed factors are resolved",
    };
    (
        ReviewReadiness {
            status: status.to_string(),
            summary: summary.to_string(),
            deciding_factors: factors,
        },
        handoffs,
    )
}

fn review_factor(
    severity: &str,
    code: &str,
    message: impl Into<String>,
    source: Option<&str>,
) -> ReviewFactor {
    ReviewFactor {
        severity: severity.to_string(),
        code: code.to_string(),
        message: message.into(),
        source: source.map(str::to_string),
    }
}

fn review_handoff(label: &str, command: impl Into<String>, reason: &str) -> ReviewTerminalHandoff {
    ReviewTerminalHandoff {
        label: label.to_string(),
        command: command.into(),
        reason: reason.to_string(),
    }
}

pub(crate) fn proposal_metadata_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Vec<ProposalMetadata>> {
    proposals_for_attempt(context, attempt_id)?
        .into_iter()
        .map(|proposal| {
            let check_status =
                latest_check_for_proposal_revision(context, &proposal.proposal_revision_id)?
                    .map(|check| check.status);
            let decision_status =
                latest_decision_for_proposal_revision(context, &proposal.proposal_revision_id)?;
            let publication_status =
                latest_publication_for_proposal_revision(context, &proposal.proposal_revision_id)?
                    .map(|_| "published".to_string());
            Ok(ProposalMetadata {
                proposal_id: proposal.proposal_id,
                proposal_revision_id: proposal.proposal_revision_id,
                attempt_id: proposal.attempt_id,
                snapshot_id: proposal.snapshot_id,
                base_head: proposal.base_head,
                content_ref: proposal.content_ref,
                changed_paths: proposal.changed_paths,
                check_status,
                decision_status,
                publication_status,
            })
        })
        .collect()
}

pub(crate) fn latest_check_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<CheckSummary>> {
    let proposal = match latest_proposal_for_attempt(context, attempt_id)? {
        Some(proposal) => proposal,
        None => return Ok(None),
    };
    latest_check_for_proposal_revision(context, &proposal.proposal_revision_id)
}

pub(crate) fn latest_check_for_proposal_revision(
    context: &RepositoryContext,
    proposal_revision_id: &str,
) -> Result<Option<CheckSummary>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT id, status, reason FROM check_results
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, proposal_revision_id],
            |row| {
                Ok(CheckSummary {
                    check_result_id: row.get(0)?,
                    status: row.get(1)?,
                    reason: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn latest_decision_for_context(context: &RepositoryContext) -> Result<Option<String>> {
    let attempt = match resolve_attempt_in_context(context, None) {
        Ok(attempt) => attempt.attempt,
        Err(_) => return Ok(None),
    };
    latest_decision_for_attempt(context, &attempt.attempt_id)
}

pub(crate) fn latest_decision_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<String>> {
    let proposal = match latest_proposal_for_attempt(context, attempt_id)? {
        Some(proposal) => proposal,
        None => return Ok(None),
    };
    latest_decision_for_proposal_revision(context, &proposal.proposal_revision_id)
}

pub(crate) fn latest_decision_for_proposal_revision(
    context: &RepositoryContext,
    proposal_revision_id: &str,
) -> Result<Option<String>> {
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
