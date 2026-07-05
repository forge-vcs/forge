use anyhow::{anyhow, bail, Context, Result};
use forge_core::{new_id, now_ms, OperationId, OperationStatus, RepositoryId, ViewId, ViewKind};
use forge_private::{EncryptedPayload, EncryptionRecipient};
use rusqlite::{
    params, Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod attempts;
mod conflict;
mod embargo;
mod error;
mod evidence;
mod integrity;
mod migrations;
mod private_overlay;
mod proposals;
mod publication;
mod repo_lock;
mod signing;
mod snapshots;
mod storage;
mod sync;
mod trust;
mod visibility;
pub use attempts::{
    attach_attempt, attempt_base_head, attempt_materialization_ref, attempt_workspace_path,
    ensure_attempt_workspace_marker, list_attempts, record_attempt_workspace_materialized,
    resolve_attempt, show_attempt, start_attempt, start_attempt_for_intent, verify_save_target,
    AttemptRecord, AttemptShowRecord, AttemptSummary, ResolvedAttempt, StartAttempt,
};
pub(crate) use attempts::{
    attempt_by_id, resolve_attempt_in_context, verify_worktree_binding, WorkspaceMarker,
};
pub(crate) use conflict::record_merge_conflict_inner;
pub use conflict::{
    conflict_list, conflict_show, preflight_conflict_resolution, record_conflict_set,
    record_failed_operation_with_conflict, record_merge_conflict, resolve_conflict_with_tree,
    ConflictListRecord, ConflictResolutionRecord, ConflictResolutionSuggestion,
    ConflictResolutionSuggestionProvenance, ConflictSetSummary, ConflictShowRecord,
    MergeConflictInput, MergeConflictRecord, PathConflictSummary, StaleBaseConflict,
    StaleBaseConflictInput,
};
pub use embargo::{
    close_embargo_workflow, ensure_embargo_publishable, finish_embargo_release_workflow,
    grant_embargo_capability, mark_embargo_workflow, prepare_embargo_release_workflow,
    publish_embargo_workflow, reveal_embargo_workflow, revoke_embargo_capability,
    EmbargoReleasePlan, EmbargoReleaseRecord, EmbargoWorkflowEventRecord, EmbargoWorkflowRecord,
    EmbargoWorkflowResult,
};
pub(crate) use embargo::{
    embargo_workflow_on, embargo_workflow_required, record_embargo_accept_on,
};
pub use error::{error_registry, ErrorCodeSpec, ForgeError, NativeHistoryCorruptKind, TamperKind};
pub use evidence::{record_evidence, EvidenceInput, EvidenceRecord, EvidenceSummary};
pub use private_overlay::{
    bind_org_encryption_key, capture_local_private_overlays,
    encrypt_private_payload_to_local_store, install_materialized_private_overlays,
    keyed_private_path_hash, local_encryption_recipient, local_private_path_exclusions,
    local_private_path_labels, prepare_materialized_private_overlay, private_decrypt_authority,
    private_overlay_transports_for_snapshots, record_encrypted_private_payload,
    record_private_path_label, scoped_private_path_hash, set_local_private_path_label,
    EncryptedPrivatePayloadInput, EncryptedPrivatePayloadRecord, LocalEncryptedPrivateObject,
    LocalPrivatePathLabel, MaterializedPrivateOverlay, OrgEncryptionKeyBindingRecord,
    PrivateDecryptAuthority, PrivateOverlayMaterializeInput, PrivateOverlayTransportRecord,
    PrivatePathLabelRecord, SaveSnapshotPrivateOverlayInput,
};
pub(crate) use private_overlay::{insert_encrypted_private_payload_on, validate_private_hash};
pub use proposals::{
    attempt_proposal_content_ref, check_spec_json_from_requires, decide, latest_decision,
    list_proposals, pr_body, pr_body_for, proposal_for_merge, proposal_review, propose,
    record_check, resolved_merge_ours_head, verify_decision_integrity, CheckRecord, CheckSummary,
    DecisionRecord, ProposalMetadata, ProposalRecord, ProposalReview, ProposalSummary,
    ResolvedProposal, ReviewAttemptContext, ReviewChangedPath, ReviewDiff, ReviewEmbargo,
    ReviewEvidenceAudit, ReviewFactor, ReviewLifecycle, ReviewReadiness, ReviewTerminalHandoff,
    ReviewVisibility,
};
pub(crate) use proposals::{
    decision_high_water, evaluate_check_on, evidence_facts_on, evidence_high_water,
    intent_check_spec, latest_check_for_attempt, latest_decision_for_attempt,
    latest_decision_for_proposal_revision, latest_evidence_for_attempt,
    latest_proposal_for_attempt, proposal_by_id, proposal_by_id_on, proposal_metadata_for_attempt,
    redact_gate_result, resolve_proposal, verify_evidence_integrity, IntegrityStatus,
};
pub(crate) use publication::latest_publication_for_proposal_revision;
pub use publication::{
    accepted_commit_id_for_revision, build_publication_trailer, decision_for_proposal_revision,
    exportable_proposal, publication_exists_for_branch, record_publication, render_trailer_message,
    PublicationRecord, PublicationTrailer,
};
pub use repo_lock::{LockTimeout, RepoLock};
pub use snapshots::{
    checkout_target_content_ref, expected_content_ref, latest_snapshot_content_ref, native_log,
    reconcile_native_head, record_checkout, record_restore, record_undo, save_snapshot,
    save_snapshot_with_private_overlays, set_materialized_expected_content_ref,
    snapshot_content_ref, snapshot_owner_attempt_id, undo_target, CommitView, SnapshotRecord,
    SnapshotSummary, UndoTarget,
};
pub(crate) use snapshots::{
    latest_snapshot_for_attempt, latest_snapshot_on, native_tip, set_context_expected_content_ref,
    NativeVisitState,
};
pub use storage::{
    storage_accounting, storage_budget_status, StorageAccounting, StorageBudgetStatus,
    StorageCategoryAccounting, StoragePolicy,
};
pub use sync::{
    is_sync_merged_op_kind, prepare_native_sync_clone, record_projected_sync_clone_initialized,
    record_sync_import_materialized, record_sync_merge_commit, record_sync_merge_conflict,
    record_sync_pull_materialized, record_sync_request_marker, set_sync_clone_expected_content_ref,
    SyncCloneRepository, SyncMergeCommitInput, SyncMergeCommitResult, SyncPullMaterializedInput,
};
pub use trust::{
    attest_hosted_runner, attest_third_party, enforce_trust_policy, init_org_governance,
    local_key_status, org_status, rotate_local_key, set_trust_policy, trust_policy,
    HostedRunnerAttestation, LocalKeyRotation, LocalKeyStatus, OrgBootstrap, OrgStatus,
    ThirdPartyAttestation, TrustPolicy, TrustPolicyAction,
};
pub(crate) use trust::{
    embargo_authority_on, ensure_active_org_principal, ensure_active_org_role, org_status_on,
    trust_policy_on, trust_rank, TRUST_LOCALLY_SIGNED,
};
pub(crate) use visibility::{
    effective_work_package_visibility_on, has_active_visibility_grant, insert_visibility_audit,
    insert_work_package_visibility, projection_decision_on, upsert_work_package_visibility,
    validate_public_projection_mode, validate_visibility_capability, validate_visibility_label,
    validate_work_package_kind, visibility_policy_on,
};
pub use visibility::{
    grant_visibility_capability, projection_decision, revoke_visibility_capability,
    set_work_package_visibility, visibility_policy, ProjectionDecision, VisibilityAuditRecord,
    VisibilityGrantRecord, VisibilityPolicy, WorkPackageVisibilityRecord,
};

pub fn signing_key_fingerprint_for_public_key(public_key: &[u8]) -> String {
    signing::key_fingerprint_for_public_key(public_key)
}

#[derive(Debug, Clone, Serialize)]
pub struct InitRepository {
    pub repository_id: String,
    pub root_path: String,
    pub forge_dir: String,
    pub database_path: String,
    pub git_head: Option<String>,
    pub content_backend: String,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub already_initialized: bool,
}

#[derive(Debug, Clone)]
pub struct OperationViewInput {
    pub request_id: Option<String>,
    pub command: String,
    pub kind: String,
    pub view_kind: ViewKind,
    pub state: Value,
}

#[derive(Debug, Clone)]
pub struct OperationViewResult {
    pub operation_id: String,
    pub view_id: String,
}

#[derive(Debug, Clone)]
pub struct RequestIdOperation {
    pub operation_id: String,
    pub command: String,
    pub status: String,
    pub error_json: Option<Value>,
    pub kind: Option<String>,
    pub view_id: Option<String>,
    pub state: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct RepositoryContext {
    pub repo_id: String,
    pub root_path: PathBuf,
    pub worktree_path: PathBuf,
    pub database_path: PathBuf,
    pub content_backend: String,
    pub current_operation_id: String,
    pub current_view_id: String,
    pub attached_attempt_id: Option<String>,
    pub workspace_attempt_id: Option<String>,
}

/// One declared gate as surfaced by `forge intent show`/`list` (NER-257). A serde
/// projection of [`forge_policy::Gate`] that renames `require_structured_pass` to the
/// stable `structured` key. `program`/`args` are run through the same per-arg
/// `key=value` secret redactor [`redact_gate_result`] applies on the check surface, so a
/// secret-like gate token never leaks through this egress (the stored
/// `intents.check_spec_json` is raw).
#[derive(Debug, Clone, Serialize)]
pub struct IntentGate {
    pub program: String,
    pub args: Vec<String>,
    pub structured: bool,
}

/// One intent as surfaced by `forge intent list` (NER-257): id, title/text, a status
/// derived from its linked attempts (no `intents.status` column exists), the declared
/// gate spec, and the linked attempt ids.
#[derive(Debug, Clone, Serialize)]
pub struct IntentSummary {
    pub intent_id: String,
    pub title: String,
    pub status: String,
    pub gates: Vec<IntentGate>,
    pub attempt_ids: Vec<String>,
}

/// One intent's full detail as surfaced by `forge intent show <id>` (NER-257). Same
/// shape as [`IntentSummary`] today; a distinct type leaves room for the detail view to
/// diverge (e.g. per-attempt status) without changing the list contract.
#[derive(Debug, Clone, Serialize)]
pub struct IntentDetail {
    pub intent_id: String,
    pub title: String,
    pub status: String,
    pub gates: Vec<IntentGate>,
    pub attempt_ids: Vec<String>,
}

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

#[derive(Debug, Clone, Serialize)]
pub struct ShowRecord {
    pub attempt: Option<AttemptRecord>,
    pub latest_snapshot: Option<SnapshotSummary>,
    pub latest_evidence: Option<EvidenceSummary>,
    pub latest_proposal: Option<ProposalSummary>,
    pub latest_check: Option<CheckSummary>,
    pub latest_decision: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub issues: Vec<String>,
    /// Non-fatal guidance for configuration that can confuse Forge workflows. Warnings do
    /// not affect `ok` and are also surfaced as top-level CLI `warnings[]`.
    pub warnings: Vec<String>,
    pub schema_version: Option<i64>,
    /// File-byte accounting for `.forge`, grouped by stable storage category. This is
    /// informational only; storage-budget overflow is reported separately and never evicts.
    pub storage: StorageAccounting,
    pub storage_policy: StoragePolicy,
    pub storage_budget: StorageBudgetStatus,
    pub dangling_temp_files: Vec<String>,
    /// `content_ref` rows whose referenced object is missing or fails
    /// verification — the failure mode the store-before-DB ordering (NER-132)
    /// makes impossible; this is the safety-net assertion. Empty in a healthy repo.
    pub dangling_content_refs: Vec<String>,
    /// Worktree paths holding a leftover crash-atomic-restore temp
    /// (`.forge-restore-*`), the signature of a restore killed mid-flight. Empty
    /// in a healthy repo.
    pub half_applied_worktrees: Vec<String>,
    /// Rows whose tamper-evident hash failed verification (NER-136): an evidence or
    /// decision row whose content no longer matches its stored hash, an operation
    /// whose chain link is broken (a deletion/reorder), or a post-watermark missing
    /// hash. Empty in a healthy repo. A head-truncated chain (a lost latest op) is
    /// NOT a tamper — it verifies as a legitimately-shorter chain.
    pub tampered_rows: Vec<TamperedRow>,
    /// Native commit-DAG integrity breaks (NER-138 Phase 7 slice 3): a parent cycle, a
    /// dangling parent/tree object, or a `decisions.commit_id` whose commit object is absent.
    /// Empty in a healthy repo. This is the "DAG has no cycles/dangling parents (doctor
    /// verifies)" whole-phase exit criterion.
    pub native_history_issues: Vec<NativeHistoryFinding>,
    /// Corrupt `views.state_json` rows that make GC's reachability root set
    /// untrustworthy. Empty in a healthy repo.
    pub ledger_view_issues: Vec<LedgerViewFinding>,
    /// Native pack/index entries that fail offset, checksum, decompression, hash, or kind
    /// verification. Empty in a healthy repo.
    pub native_pack_issues: Vec<String>,
    /// Local Phase 9 signing findings: post-signature-migration evidence rows, decision rows,
    /// and native accepted commit ids must carry a valid Ed25519 `locally_signed` attestation.
    /// Empty in a healthy repo. Legacy pre-migration rows are grandfathered by rowid marker.
    pub signature_issues: Vec<SignatureFinding>,
    /// Signing-key origin labels. Local keys are keys minted or used by this repository;
    /// peer keys are valid signing keys imported through sync. A peer key may verify a
    /// signature cryptographically, but it does not satisfy local-only trust policy.
    pub signature_key_summary: SignatureKeySummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SignatureKeySummary {
    pub local_key_fingerprints: Vec<String>,
    pub peer_key_fingerprints: Vec<String>,
    pub hosted_runner_key_fingerprints: Vec<String>,
    pub third_party_key_fingerprints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignatureFinding {
    pub kind: SignatureFindingKind,
    pub subject_kind: String,
    pub subject_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureFindingKind {
    MissingSignature,
    InvalidSignature,
    DigestMismatch,
    SubjectMissing,
    MalformedSignature,
}

impl SignatureFindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SignatureFindingKind::MissingSignature => "missing_signature",
            SignatureFindingKind::InvalidSignature => "invalid_signature",
            SignatureFindingKind::DigestMismatch => "digest_mismatch",
            SignatureFindingKind::SubjectMissing => "subject_missing",
            SignatureFindingKind::MalformedSignature => "malformed_signature",
        }
    }
}

/// One row that failed integrity verification in `doctor`'s chain pass. Carries only
/// an opaque id, the table, and a closed-enum break kind — never an excerpt or
/// command string (this is a machine-visible egress). `kind` serializes as snake_case
/// (`content_edit`/`broken_link`/`missing_hash`).
#[derive(Debug, Clone, Serialize)]
pub struct TamperedRow {
    pub id: String,
    pub table: String,
    pub kind: TamperKind,
}

/// One native-history integrity break found by `doctor`'s commit-DAG walk (NER-138 Phase 7
/// slice 3). Carries only the closed-enum `kind` and opaque `f1:` commit ids — never a path or
/// excerpt. `kind` serializes the SAME way as [`ForgeError::NativeHistoryCorrupt`]'s `details`
/// (the shared [`NativeHistoryCorruptKind`]), so the error payload and the doctor report can
/// never disagree on the break-kind string. Empty in a healthy repo.
#[derive(Debug, Clone, Serialize)]
pub struct NativeHistoryFinding {
    pub kind: NativeHistoryCorruptKind,
    pub commit_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub related_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LedgerViewFinding {
    pub kind: LedgerViewFindingKind,
    pub view_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerViewFindingKind {
    CorruptStateJson,
    UnparseableCommitId,
}

#[derive(Debug, Clone, Serialize)]
pub struct GcDryRunReport {
    pub dry_run: bool,
    pub unreachable_snapshots: Vec<String>,
    pub unreachable_evidence: Vec<String>,
    pub unreachable_native_objects: Vec<String>,
    pub protected_native_objects: Vec<String>,
    pub pack_candidate_native_objects: Vec<String>,
    pub loose_duplicate_native_objects: Vec<String>,
    pub deletable_native_packs: Vec<String>,
    pub protection_window_days: u64,
    pub storage: StorageAccounting,
    pub storage_policy: StoragePolicy,
    pub storage_budget: StorageBudgetStatus,
    pub plan_digest: String,
    pub deleted: Vec<String>,
    pub created_packs: Vec<String>,
    pub deleted_packs: Vec<String>,
}

const VISIBILITY_PRIVATE: &str = "private";
const VISIBILITY_TEAM: &str = "team";
const VISIBILITY_PUBLIC: &str = "public";
const VISIBILITY_EMBARGOED: &str = "embargoed";
const CAPABILITY_SEE_STUB: &str = "see_stub";
const CAPABILITY_INSPECT_CONTENT: &str = "inspect_content";
const CAPABILITY_INSPECT_EVIDENCE: &str = "inspect_evidence";
const CAPABILITY_SYNC_MATERIALIZE: &str = "sync_materialize";
const CAPABILITY_PUBLISH_REVEAL: &str = "publish_reveal";
const DEFAULT_EMBARGO_RELEASE_CONTENT_CLASSES: &[&str] =
    &["release_inputs", "sanitized_provenance"];
const EMBARGO_RELEASE_REVOCATION_WARNING: &str =
    "Revocation applies to future releases and does not claw back already delivered bundles.";
const EMBARGO_STATE_ACTIVE: &str = "active";
const EMBARGO_STATE_ACCEPTED_UNDER_EMBARGO: &str = "accepted_under_embargo";
const EMBARGO_STATE_RELEASED_UNDER_EMBARGO: &str = "released_under_embargo";
const EMBARGO_STATE_REVEALED: &str = "revealed";
const EMBARGO_STATE_PUBLISHED: &str = "published";
const EMBARGO_STATE_CLOSED: &str = "closed";
const PUBLIC_PROJECTION_PROVENANCE_ONLY: &str = "provenance_only";
const PUBLIC_PROJECTION_SANITIZED_SOURCE: &str = "sanitized_source";
const PUBLIC_PROJECTION_FULL_SOURCE: &str = "full_source";

fn ensure_work_package_exists(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<()> {
    let exists: bool = match work_package_kind {
        "intent" => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM intents WHERE repo_id = ?1 AND id = ?2)",
            params![repo_id, work_package_id],
            |row| row.get(0),
        )?,
        "attempt" => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE repo_id = ?1 AND id = ?2)",
            params![repo_id, work_package_id],
            |row| row.get(0),
        )?,
        "proposal" => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM proposals WHERE repo_id = ?1 AND id = ?2)",
            params![repo_id, work_package_id],
            |row| row.get(0),
        )?,
        _ => {
            return Err(ForgeError::VisibilityPolicyInvalid {
                reason: format!("unsupported work package kind `{work_package_kind}`"),
            }
            .into())
        }
    };
    if exists {
        Ok(())
    } else {
        Err(ForgeError::VisibilityPolicyUnmet {
            operation: "resolve_work_package".to_string(),
            work_package_kind: work_package_kind.to_string(),
            work_package_id: work_package_id.to_string(),
            capability: "exists".to_string(),
            disclosure: "hidden".to_string(),
        }
        .into())
    }
}

pub fn init_repository(
    cwd: &Path,
    request_id: Option<String>,
    content_backend: String,
) -> Result<InitRepository> {
    if !matches!(content_backend.as_str(), "git" | "native") {
        bail!("unsupported content backend");
    }
    // A NATIVE repo earns real git independence: it does not require the git binary at init —
    // its root is `cwd` (canonicalized). A GIT-backed repo still anchors on the git toplevel
    // (Forge layers on an existing git repo). This is what lets the full native lifecycle —
    // init included — run with git removed from PATH (NER-138 Phase 7 exit criterion).
    let root = if content_backend == "native" {
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf())
    } else {
        git_root(cwd)?
    };
    // NER-143 R9: refuse to initialize a forge repo nested inside an existing forge repo.
    // `forge_root`'s nearest-ancestor walk routes a subtree's commands to whichever `.forge`
    // is closer up-tree, and the nested repo's objects look unreachable to the outer repo's
    // gc (a Phase-8 deletion hazard). The check is BACKEND-AGNOSTIC because `forge_root` is:
    // a native inner repo anchors at `cwd` (so it can nest below anything), and a git inner
    // repo whose own toplevel sits below an outer repo can nest too — both are shadowing
    // hazards regardless of backend, so the guard must not be gated on `content_backend`
    // (the code-review adversarial pass flagged the native-only gating as an escape). Message
    // is path-free (S1). This checks ANCESTORS only (`root.parent()` upward), so re-init of
    // the same root never trips it and stays the already_initialized path below. (A
    // deliberately-independent nested repo is not a v0 use case; an --allow-nested opt-out is
    // future work. A narrow cross-repo-init TOCTOU window — two inits racing in
    // ancestor/descendant dirs before either's lock — is an accepted v0 limitation.)
    {
        let mut ancestor = root.parent();
        while let Some(dir) = ancestor {
            if dir.join(".forge/forge.db").exists() {
                bail!("refusing to initialize a forge repo nested inside an existing forge repo");
            }
            ancestor = dir.parent();
        }
    }
    let forge_dir = root.join(".forge");
    fs::create_dir_all(&forge_dir)
        .with_context(|| format!("failed to create {}", forge_dir.display()))?;

    // Serialize concurrent first-inits of the same repo (NER-132 U5): hold the repo
    // write lock across migration + the repository INSERT, so a racing init observes
    // the winner's committed row via read_init_repository below and returns
    // already_initialized rather than colliding on the repositories.root_path UNIQUE
    // constraint. The lock file lives in the .forge dir just created. init does not
    // route through the CLI command_result lock, so it acquires here, never nested.
    let _init_lock = repo_lock::acquire(&forge_dir)?;

    let database_path = forge_dir.join("forge.db");
    let already_had_db = database_path.exists();
    let mut connection = open_connection(&database_path)
        .with_context(|| format!("failed to open {}", database_path.display()))?;
    migrations::apply_pending_migrations(&mut connection)?;

    if let Some(existing) = read_init_repository(&connection, &root, &forge_dir, &database_path)? {
        return Ok(InitRepository {
            already_initialized: true,
            ..existing
        });
    }

    // A native repo records no git_head (it has its own ref store and never shells git);
    // recording it would reintroduce a git dependency at init.
    let git_head = if content_backend == "native" {
        None
    } else {
        git_head(&root)
    };
    let repo_id = RepositoryId::new().to_string();
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let state_json = json!({
        "repository_id": repo_id,
        "root_path": root,
        "git_head": git_head,
        "content_backend": content_backend,
        "lifecycle": "initialized"
    })
    .to_string();
    // No replay guard here: `init` has its own idempotency via the
    // `read_init_repository` short-circuit above; this only adds IMMEDIATE +
    // busy-retry for R3 consistency.
    with_immediate_retry(&mut connection, |tx| {
        tx.execute(
            "INSERT INTO repositories (id, root_path, git_head, content_backend, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo_id, root.to_string_lossy(), git_head, content_backend, now],
        )?;
        // The genesis link: parent is the documented genesis sentinel, no domain
        // digest. Stored so `doctor`'s re-walk starts from a verifiable anchor and a
        // fresh repo is never mis-flagged as a NULL-hash (tampered) op (NER-136).
        let genesis_hash = integrity::operation_link_hash(
            integrity::GENESIS_PARENT_HASH,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command: "init",
                kind: "repository_initialized",
                created_at_ms: now,
            },
            None,
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, 'init', ?4, 'repository_initialized', NULL, ?5, NULL, ?6, ?7)",
            params![
                operation_id,
                repo_id,
                request_id,
                format!("{:?}", OperationStatus::Succeeded).to_lowercase(),
                view_id,
                genesis_hash,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'initialized', ?4, ?5)",
            params![view_id, repo_id, operation_id, state_json, now],
        )?;
        tx.execute(
            "INSERT INTO current_state (
                singleton, repo_id, current_operation_id, current_view_id, attached_attempt_id, updated_at_ms
            ) VALUES (1, ?1, ?2, ?3, NULL, ?4)",
            params![repo_id, operation_id, view_id, now],
        )?;
        Ok(())
    })?;

    Ok(InitRepository {
        repository_id: repo_id,
        root_path: root.to_string_lossy().into_owned(),
        forge_dir: forge_dir.to_string_lossy().into_owned(),
        database_path: database_path.to_string_lossy().into_owned(),
        git_head,
        content_backend,
        current_operation_id: operation_id,
        current_view_id: view_id,
        already_initialized: already_had_db,
    })
}

pub fn open_repository(cwd: &Path) -> Result<RepositoryContext> {
    // Git-free root resolution (slice 3): walk up for `.forge/forge.db` rather than shelling
    // `git rev-parse`, so every post-init command works with git removed from PATH.
    let (root, worktree_path, workspace_attempt_id) = repository_location(cwd)?;
    let database_path = root.join(".forge/forge.db");
    if !database_path.exists() {
        return Err(ForgeError::NotInitialized.into());
    }
    // `open_repository` is a pure open+query: schema migrations are applied by the
    // transient `migrate()` entrypoint at the top of `command_result` (and by
    // `init_repository` under its own lock) — never here, where no lock is held.
    let connection = open_connection(&database_path)?;
    let (repo_id, content_backend, current_operation_id, current_view_id, attached_attempt_id): (
        String,
        String,
        String,
        String,
        Option<String>,
    ) = connection.query_row(
        "SELECT cs.repo_id, r.content_backend, cs.current_operation_id, cs.current_view_id, cs.attached_attempt_id
             FROM current_state cs
             JOIN repositories r ON r.id = cs.repo_id
             WHERE cs.singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    Ok(RepositoryContext {
        repo_id,
        root_path: root,
        worktree_path,
        database_path,
        content_backend,
        current_operation_id,
        current_view_id,
        attached_attempt_id,
        workspace_attempt_id,
    })
}

pub fn acquire_repository_lock(cwd: &Path) -> Result<RepoLock> {
    let context = open_repository(cwd)?;
    repo_lock::acquire(&context.root_path.join(".forge"))
}

pub fn effective_worktree_path(cwd: &Path) -> Result<PathBuf> {
    Ok(open_repository(cwd)?.worktree_path)
}

pub fn repository_root_path(cwd: &Path) -> Result<PathBuf> {
    Ok(open_repository(cwd)?.root_path)
}

pub fn repository_content_backend(cwd: &Path) -> Result<String> {
    Ok(open_repository(cwd)?.content_backend)
}

/// Acquire the repo-level advisory write lock for the repository containing `cwd`
/// (PRD §10.6, NER-132). The CLI holds the returned guard across a mutating
/// command's critical section so its determining reads and write are atomic
/// against other `forge` writers.
///
/// Returns `Ok(None)` when there is no repository to lock — `cwd` is not inside a
/// Git work tree, or `.forge` does not exist yet — so the caller's own logic
/// surfaces the canonical "not initialized" error instead of a lock-file error.
/// A genuine contention timeout surfaces as a [`LockTimeout`] (`Err`).
pub fn acquire_repo_lock(cwd: &Path) -> Result<Option<RepoLock>> {
    // Git-free root resolution (slice 3): a `.forge`-walk, so locking works without git.
    let root = match forge_root(cwd) {
        Ok(root) => root,
        Err(_) => return Ok(None),
    };
    let forge_dir = root.join(".forge");
    if !forge_dir.exists() {
        return Ok(None);
    }
    repo_lock::acquire(&forge_dir).map(Some)
}

pub fn acquire_worktree_lock(cwd: &Path, attempt_id: &str) -> Result<RepoLock> {
    let context = open_repository(cwd)?;
    attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
        selector: attempt_id.to_string(),
    })?;
    let lock_path = context
        .root_path
        .join(".forge/worktree-locks")
        .join(format!("{attempt_id}.lock"));
    repo_lock::acquire_lock_file(&lock_path)
}

/// Bring the repository's schema up to this binary's head, acquiring the repo
/// write lock **only when a migration is actually pending** (NER-133 U4).
///
/// This is the transient, self-acquiring entrypoint the CLI runs at the top of
/// `command_result`, **before** any per-command lock — so it never nests inside an
/// already-held lock. It mirrors [`acquire_repo_lock`]'s resolution so it no-ops
/// when there is nothing to migrate, letting the command's own logic surface the
/// canonical error:
/// - `cwd` is not inside a Git work tree ⇒ `Ok(())` (the command surfaces
///   not-a-git-repo / `NOT_INITIALIZED`).
/// - `.forge/forge.db` does not exist ⇒ `Ok(())` (uninitialized — the command
///   surfaces `NOT_INITIALIZED`).
/// - DB version `== HEAD` ⇒ `Ok(())` on the cheap read, **no lock taken** (the
///   common path, including every read-only command and `run`).
/// - DB version `> HEAD` ⇒ `Err(ForgeError::UnknownSchemaVersion)`: the DB was
///   written by a newer Forge; refuse without acquiring the lock. The CLI maps
///   this to `SCHEMA_VERSION_UNSUPPORTED` and short-circuits before any write.
/// - DB version `< HEAD` ⇒ acquire the repo lock **transiently**, apply the
///   pending migrations (idempotent + version-gated, so a concurrent migrator that
///   won the race is handled), then release the lock (Drop) before returning.
pub fn migrate(cwd: &Path) -> Result<()> {
    // Git-free root resolution (slice 3): a `.forge`-walk, so migration works without git.
    let root = match forge_root(cwd) {
        Ok(root) => root,
        Err(_) => return Ok(()),
    };
    let forge_dir = root.join(".forge");
    let database_path = forge_dir.join("forge.db");
    if !database_path.exists() {
        return Ok(());
    }

    let mut connection = open_connection(&database_path)?;
    let db_version = migrations::current_schema_version(&connection)?;
    let head = migrations::schema_head();

    if db_version == head {
        // Common fast path: nothing to do, take no lock.
        return Ok(());
    }
    if db_version > head {
        // A forward-versioned DB: refuse to write without taking the lock.
        return Err(ForgeError::UnknownSchemaVersion {
            db_version,
            supported_head: head,
        }
        .into());
    }

    // Pending (`db_version < head`): acquire the repo lock transiently, apply, and
    // release on Drop before returning. `apply_pending_migrations` re-reads the
    // applied versions under the lock and is idempotent, so a concurrent migrator
    // that won the race is a no-op here. Acquired exactly once, before the
    // per-command lock — never nested.
    let _lock = repo_lock::acquire(&forge_dir)?;
    migrations::apply_pending_migrations(&mut connection)?;
    register_existing_local_key_after_migration(&root, &connection)?;
    Ok(())
}

fn register_existing_local_key_after_migration(root: &Path, connection: &Connection) -> Result<()> {
    let Some(key) = signing::existing_local_key_info(root)? else {
        return Ok(());
    };
    let repo_id = connection
        .query_row(
            "SELECT id FROM repositories ORDER BY rowid LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(repo_id) = repo_id {
        signing::register_local_signing_key(
            connection,
            &repo_id,
            &key.public_key,
            &key.key_fingerprint,
            now_ms(),
        )?;
    }
    Ok(())
}

pub fn operation_for_request(cwd: &Path, request_id: &str) -> Result<Option<RequestIdOperation>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT o.id, o.command, o.status, o.error_json, o.kind, o.resulting_view_id, v.state_json
             FROM operations o
             LEFT JOIN views v ON v.id = o.resulting_view_id
             WHERE o.repo_id = ?1 AND o.request_id = ?2
             ORDER BY o.created_at_ms DESC, o.rowid DESC LIMIT 1",
            params![context.repo_id, request_id],
            |row| {
                let error_json: Option<String> = row.get(3)?;
                let state_json: Option<String> = row.get(6)?;
                Ok(RequestIdOperation {
                    operation_id: row.get(0)?,
                    command: row.get(1)?,
                    status: row.get(2)?,
                    error_json: error_json.and_then(|json| serde_json::from_str(&json).ok()),
                    kind: row.get(4)?,
                    view_id: row.get(5)?,
                    state: state_json.and_then(|json| serde_json::from_str(&json).ok()),
                })
            },
        )
        .optional()
        .map_err(Into::into)
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

pub fn doctor(cwd: &Path) -> Result<DoctorReport> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let storage = storage::storage_accounting_for_root(&context.root_path)?;
    let storage_policy = storage::storage_policy(&connection)?;
    let storage_budget = storage::storage_budget_status_for(&storage, &storage_policy);
    let mut issues = Vec::new();
    let mut foreign_key_statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut foreign_key_rows = foreign_key_statement.query([])?;
    while let Some(row) = foreign_key_rows.next()? {
        let table: String = row.get(0)?;
        let rowid: i64 = row.get(1)?;
        issues.push(format!("foreign key violation in {table} row {rowid}"));
    }
    let schema_version = connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .optional()?
        .flatten();
    if schema_version != Some(migrations::schema_head()) {
        issues.push("schema mismatch".to_string());
    }
    let state_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM current_state cs
         JOIN operations o ON o.id = cs.current_operation_id
         JOIN views v ON v.id = cs.current_view_id
         WHERE cs.singleton = 1
           AND o.repo_id = cs.repo_id
           AND v.repo_id = cs.repo_id
           AND v.operation_id = o.id
           AND o.resulting_view_id = v.id",
        [],
        |row| row.get(0),
    )?;
    if state_count != 1 {
        issues.push("invalid current operation/view".to_string());
    }
    let mut dangling_temp_files = Vec::new();
    let temp_dir = context.root_path.join(".forge/tmp");
    if temp_dir.exists() {
        for entry in fs::read_dir(&temp_dir)? {
            let entry = entry?;
            dangling_temp_files.push(entry.path().to_string_lossy().into_owned());
        }
    }
    if !dangling_temp_files.is_empty() {
        issues.push("dangling temporary files".to_string());
    }
    let native_store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let mut reachable_native_objects = std::collections::BTreeSet::new();
    // A committed `content_ref` whose object is missing or fails verification is
    // the exact failure the store-before-DB ordering (NER-132) makes impossible;
    // surface it as its own category so the exit criterion is machine-checkable.
    let mut dangling_content_refs = Vec::new();
    let mut statement = connection.prepare(
        "SELECT 'snapshot ' || id, content_ref FROM snapshots
         UNION ALL
         SELECT 'proposal revision ' || id, content_ref FROM proposal_revisions",
    )?;
    let refs = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for content_ref in refs {
        let (label, content_ref) = content_ref?;
        if let Some(tree) = content_ref.strip_prefix("git-tree:") {
            let output = Command::new("git")
                .args(["cat-file", "-e", &format!("{tree}^{{tree}}")])
                .current_dir(&context.root_path)
                .output()?;
            if !output.status.success() {
                dangling_content_refs.push(format!("missing content ref for {label}"));
            }
        } else if content_ref.starts_with("forge-tree:") {
            match native_store.verify_content_ref(&content_ref) {
                Ok(ids) => reachable_native_objects.extend(ids),
                Err(error) => {
                    dangling_content_refs
                        .push(format!("invalid native content ref for {label}: {error}"));
                }
            }
        }
    }
    issues.extend(dangling_content_refs.iter().cloned());

    // A crash-atomic restore (NER-132 U4) killed mid-flight leaves a
    // `.forge-restore-*` temp in a worktree directory; those live in the worktree,
    // not `.forge/tmp`, so scan the work tree (excluding `.git`/`.forge`) for them.
    let half_applied_worktrees = scan_restore_temps(&context.root_path)?;
    if !half_applied_worktrees.is_empty() {
        issues.push("half-applied worktree (leftover restore temp files)".to_string());
    }

    // Tamper-evidence chain pass (NER-136): re-verify every hashed row offline.
    let tampered_rows = verify_integrity_chain(&connection)?;
    if !tampered_rows.is_empty() {
        issues.push(format!("{} tampered row(s) detected", tampered_rows.len()));
    }
    let signature_issues = signing::verify_signatures(&connection)?;
    if !signature_issues.is_empty() {
        issues.push(format!(
            "{} local signature issue(s) detected",
            signature_issues.len()
        ));
    }
    let signature_key_summary = signing::signature_key_summary(&connection, &context.repo_id)?;

    // Native commit-DAG integrity pass (NER-138 Phase 7 slice 3): walk the DAG from the
    // authoritative tip and cross-check the ledger, REPORTING (not raising) cycles / dangling
    // parents / dangling trees / dangling commit_ids. The raising counterpart lives in
    // reconcile/checkout; doctor is the offline health report.
    let native_history_issues = verify_native_history(&context, &connection, &native_store)?;
    if !native_history_issues.is_empty() {
        issues.push(format!(
            "{} native-history integrity break(s) detected",
            native_history_issues.len()
        ));
    }
    let ledger_view_issues = ledger_commit_roots(&context, &connection)?.view_issues;
    if !ledger_view_issues.is_empty() {
        issues.push(format!(
            "{} corrupt ledger view row(s) detected",
            ledger_view_issues.len()
        ));
    }
    let native_pack_issues = native_store.validate_packs();
    if !native_pack_issues.is_empty() {
        issues.push(format!(
            "{} native pack/index issue(s) detected",
            native_pack_issues.len()
        ));
    }
    let warnings = doctor_warnings(&context.root_path)?;

    Ok(DoctorReport {
        ok: issues.is_empty(),
        issues,
        warnings,
        schema_version,
        storage,
        storage_policy,
        storage_budget,
        dangling_temp_files,
        dangling_content_refs,
        half_applied_worktrees,
        tampered_rows,
        native_history_issues,
        ledger_view_issues,
        native_pack_issues,
        signature_issues,
        signature_key_summary,
    })
}

fn doctor_warnings(root: &Path) -> Result<Vec<String>> {
    let mut warnings = Vec::new();
    if js_test_runner_may_scan_forge_worktrees(root)? {
        warnings.push(
            "JavaScript/TypeScript test discovery may scan Forge-managed worktrees; add `.forge/**` to test-runner excludes (for example Vitest `exclude: [...configDefaults.exclude, '.forge/**']`).".to_string(),
        );
    }
    Ok(warnings)
}

fn js_test_runner_may_scan_forge_worktrees(root: &Path) -> Result<bool> {
    let has_package_test_script = package_json_has_test_script(root)?;
    let mut relevant_files = js_test_config_files(root);
    if has_package_test_script {
        relevant_files.push(root.join("package.json"));
    }
    if relevant_files.is_empty() {
        return Ok(false);
    }
    for path in relevant_files {
        if file_mentions_forge_exclude(&path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn package_json_has_test_script(root: &Path) -> Result<bool> {
    let path = root.join("package.json");
    if !path.exists() {
        return Ok(false);
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("read {}", path.to_string_lossy()))?;
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Ok(false),
    };
    Ok(value
        .get("scripts")
        .and_then(|scripts| scripts.get("test"))
        .and_then(Value::as_str)
        .is_some())
}

fn js_test_config_files(root: &Path) -> Vec<PathBuf> {
    [
        "vite.config.js",
        "vite.config.cjs",
        "vite.config.mjs",
        "vite.config.ts",
        "vite.config.cts",
        "vite.config.mts",
        "vitest.config.js",
        "vitest.config.cjs",
        "vitest.config.mjs",
        "vitest.config.ts",
        "vitest.config.cts",
        "vitest.config.mts",
        "jest.config.js",
        "jest.config.cjs",
        "jest.config.mjs",
        "jest.config.ts",
        "playwright.config.js",
        "playwright.config.cjs",
        "playwright.config.mjs",
        "playwright.config.ts",
    ]
    .into_iter()
    .map(|name| root.join(name))
    .filter(|path| path.exists())
    .collect()
}

fn file_mentions_forge_exclude(path: &Path) -> Result<bool> {
    const MAX_CONFIG_BYTES: u64 = 1_048_576;
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_CONFIG_BYTES {
        return Ok(false);
    }
    let text =
        fs::read_to_string(path).with_context(|| format!("read {}", path.to_string_lossy()))?;
    Ok(text.contains(".forge/**")
        || text.contains(".forge/")
        || text.contains("'.forge'")
        || text.contains("\".forge\""))
}

/// `doctor`'s native commit-DAG integrity pass (NER-138 Phase 7 slice 3): walk the DAG from
/// the authoritative tip detecting cycles (visited set), dangling parents, and dangling trees,
/// then cross-check every ledger-referenced native commit resolves to an existing commit object.
/// Reports findings (does not raise) — fail-closed at the call sites that raise. Findings are
/// deduped by (kind, commit_id, related_id).
fn verify_native_history(
    context: &RepositoryContext,
    connection: &Connection,
    store: &forge_content_native::NativeObjectStore,
) -> Result<Vec<NativeHistoryFinding>> {
    verify_native_history_from_tip(
        &context.repo_id,
        connection,
        store,
        native_tip(context, connection)?,
    )
}

fn verify_native_history_from_tip(
    repo_id: &str,
    connection: &Connection,
    store: &forge_content_native::NativeObjectStore,
    tip: Option<forge_content_native::ObjectId>,
) -> Result<Vec<NativeHistoryFinding>> {
    let mut findings: Vec<NativeHistoryFinding> = Vec::new();
    let mut push = |finding: NativeHistoryFinding| {
        if !findings.iter().any(|existing| {
            existing.kind.as_str() == finding.kind.as_str()
                && existing.commit_id == finding.commit_id
                && existing.related_id == finding.related_id
        }) {
            findings.push(finding);
        }
    };

    if let Some(tip) = tip {
        let mut states = std::collections::BTreeMap::new();
        let mut stack = vec![(tip, false)];
        while let Some((commit_id, expanded)) = stack.pop() {
            if expanded {
                states.insert(commit_id, NativeVisitState::Visited);
                continue;
            }
            match states.get(&commit_id).copied() {
                Some(NativeVisitState::Visited) => continue,
                Some(NativeVisitState::Visiting) => {
                    push(NativeHistoryFinding {
                        kind: NativeHistoryCorruptKind::Cycle,
                        commit_id: commit_id.to_string(),
                        related_id: None,
                    });
                    continue;
                }
                None => {}
            }
            states.insert(commit_id.clone(), NativeVisitState::Visiting);
            let commit = match store.read_commit(&commit_id) {
                Ok(commit) => commit,
                Err(_) => {
                    push(NativeHistoryFinding {
                        kind: NativeHistoryCorruptKind::DanglingCommitId,
                        commit_id: commit_id.to_string(),
                        related_id: None,
                    });
                    continue;
                }
            };
            stack.push((commit_id.clone(), true));
            let tree_ref = format!("{}{}", forge_content::FORGE_TREE_PREFIX, commit.tree);
            if store.verify_content_ref(&tree_ref).is_err() {
                push(NativeHistoryFinding {
                    kind: NativeHistoryCorruptKind::DanglingTree,
                    commit_id: commit_id.to_string(),
                    related_id: Some(commit.tree.clone()),
                });
            }
            for parent in &commit.parents {
                match forge_content_native::ObjectId::parse(parent) {
                    Ok(parent_id) if store.read_commit(&parent_id).is_ok() => {
                        stack.push((parent_id, false));
                    }
                    _ => push(NativeHistoryFinding {
                        kind: NativeHistoryCorruptKind::DanglingParent,
                        commit_id: commit_id.to_string(),
                        related_id: Some(parent.clone()),
                    }),
                }
            }
        }
    }

    let mut check_commit =
        |commit_id: String| match forge_content_native::ObjectId::parse(&commit_id) {
            Ok(id) if store.read_commit(&id).is_ok() => {}
            _ => push(NativeHistoryFinding {
                kind: NativeHistoryCorruptKind::DanglingCommitId,
                commit_id,
                related_id: None,
            }),
        };

    // Cross-check every accepted decisions.commit_id resolves to a commit object — catches a
    // dangling commit_id even if it is off the tip's ancestry (the store-before-DB violation).
    let mut decision_commits = connection
        .prepare("SELECT commit_id FROM decisions WHERE repo_id = ?1 AND commit_id IS NOT NULL")?;
    let rows = decision_commits.query_map(params![repo_id], |row| row.get::<_, String>(0))?;
    for row in rows {
        check_commit(row?);
    }

    // Sync merge operations also introduce native commit ids. Check them independently so an
    // imported or corrupted off-tip sync_*_merged view cannot claim a phantom signed commit.
    let query = format!(
        "SELECT json_extract(v.state_json, '$.commit_id')
           FROM operations o
           JOIN views v ON v.id = o.resulting_view_id
          WHERE o.repo_id = ?1
            AND o.kind IN ({})
            AND json_extract(v.state_json, '$.commit_id') IS NOT NULL",
        sync::SYNC_MERGED_OP_KIND_SQL_IN
    );
    let mut sync_merge_commits = connection.prepare(&query)?;
    let rows = sync_merge_commits.query_map(params![repo_id], |row| row.get::<_, String>(0))?;
    for row in rows {
        check_commit(row?);
    }

    Ok(findings)
}

/// Re-verify the full tamper-evident chain offline (NER-136 §U8): every evidence and
/// decision row's own content hash, plus every operation's chain link (which folds
/// the domain digest, so a *recomputed* row hash that slipped past the cheap gate
/// check is caught here at the operation that chained the old digest). Reads a
/// consistent ordered snapshot; a head-truncated chain (a lost latest op) is reported
/// as clean, NOT a tamper, because there is no expected-count check.
fn verify_integrity_chain(conn: &Connection) -> Result<Vec<TamperedRow>> {
    let mut tampered = Vec::new();
    let evidence_marker = evidence_high_water(conn)?;
    let op_marker = op_high_water(conn)?;
    let decision_marker = decision_high_water(conn)?;

    // (a) Every evidence row's own digest.
    let mut evidence_ids = conn.prepare("SELECT id FROM evidence ORDER BY rowid")?;
    let ids: Vec<String> = evidence_ids
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in ids {
        if let IntegrityStatus::Tampered(kind) =
            verify_evidence_integrity(conn, &id, evidence_marker)?
        {
            tampered.push(TamperedRow {
                id,
                table: "evidence".to_string(),
                kind,
            });
        }
    }

    // (b) Every decision row's own digest.
    let mut decision_rows = conn.prepare(
        "SELECT id, proposal_id, proposal_revision_id, decision, actor, content_hash, created_at_ms, rowid
         FROM decisions ORDER BY rowid",
    )?;
    let decisions: Vec<StoredDecision> = decision_rows
        .query_map([], |row| {
            Ok(StoredDecision {
                id: row.get(0)?,
                proposal_id: row.get(1)?,
                proposal_revision_id: row.get(2)?,
                decision: row.get(3)?,
                actor: row.get(4)?,
                content_hash: row.get(5)?,
                created_at_ms: row.get(6)?,
                rowid: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for row in decisions {
        match row.content_hash {
            None if row.rowid > decision_marker => tampered.push(TamperedRow {
                id: row.id,
                table: "decisions".to_string(),
                kind: TamperKind::MissingHash,
            }),
            None => {}
            Some(stored) => {
                let recomputed = integrity::decision_digest(&integrity::DecisionDigestInput {
                    proposal_id: &row.proposal_id,
                    proposal_revision_id: &row.proposal_revision_id,
                    decision: &row.decision,
                    actor: &row.actor,
                    created_at_ms: row.created_at_ms,
                });
                if recomputed != stored {
                    tampered.push(TamperedRow {
                        id: row.id,
                        table: "decisions".to_string(),
                        kind: TamperKind::ContentEdit,
                    });
                }
            }
        }
    }

    // (c) Every operation-owned conflict set's own digest.
    let mut conflict_rows = conn.prepare(
        "SELECT id FROM conflict_sets
         WHERE generated_by_operation_id IS NOT NULL
         ORDER BY rowid",
    )?;
    let conflict_ids: Vec<String> = conflict_rows
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in conflict_ids {
        match recompute_conflict_set_hash(conn, &id)? {
            None => tampered.push(TamperedRow {
                id,
                table: "conflict_sets".to_string(),
                kind: TamperKind::MissingHash,
            }),
            Some((stored, recomputed)) if stored != recomputed => tampered.push(TamperedRow {
                id,
                table: "conflict_sets".to_string(),
                kind: TamperKind::ContentEdit,
            }),
            Some(_) => {}
        }
    }

    // (d) Every operation's chain link (folding its domain digest), in chain order.
    let mut op_rows = conn.prepare(
        "SELECT id, parent_operation_id, command, kind, resulting_view_id, content_hash, created_at_ms, rowid
         FROM operations ORDER BY created_at_ms, rowid",
    )?;
    let ops: Vec<StoredOp> = op_rows
        .query_map([], |row| {
            Ok(StoredOp {
                id: row.get(0)?,
                parent_operation_id: row.get(1)?,
                command: row.get(2)?,
                kind: row.get(3)?,
                resulting_view_id: row.get(4)?,
                content_hash: row.get(5)?,
                created_at_ms: row.get(6)?,
                rowid: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for row in ops {
        let Some(stored) = row.content_hash else {
            if row.rowid > op_marker {
                tampered.push(TamperedRow {
                    id: row.id,
                    table: "operations".to_string(),
                    kind: TamperKind::MissingHash,
                });
            }
            continue;
        };
        let parent_hash = op_content_hash(conn, row.parent_operation_id.as_deref())?;
        let domain_digest = op_domain_digest(conn, row.resulting_view_id.as_deref())?;
        let recomputed = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &row.id,
                command: &row.command,
                kind: &row.kind,
                created_at_ms: row.created_at_ms,
            },
            domain_digest.as_deref(),
        );
        if recomputed != stored {
            tampered.push(TamperedRow {
                id: row.id,
                table: "operations".to_string(),
                kind: TamperKind::BrokenLink,
            });
        }
    }

    Ok(tampered)
}

fn recompute_conflict_set_hash(
    conn: &Connection,
    conflict_set_id: &str,
) -> Result<Option<(String, String)>> {
    let conflict: Option<StoredConflictSet> = conn
        .query_row(
            "SELECT id, repo_id, context, paths_json, created_at_ms, base_content_ref,
                    ours_content_ref, theirs_content_ref, generated_by_operation_id,
                    resolver_backend, content_hash
             FROM conflict_sets WHERE id = ?1",
            params![conflict_set_id],
            |row| {
                Ok(StoredConflictSet {
                    id: row.get(0)?,
                    repo_id: row.get(1)?,
                    context: row.get(2)?,
                    paths_json: row.get(3)?,
                    created_at_ms: row.get(4)?,
                    base_content_ref: row.get(5)?,
                    ours_content_ref: row.get(6)?,
                    theirs_content_ref: row.get(7)?,
                    generated_by_operation_id: row.get(8)?,
                    resolver_backend: row.get(9)?,
                    content_hash: row.get(10)?,
                })
            },
        )
        .optional()?;
    let Some(conflict) = conflict else {
        return Ok(None);
    };
    let Some(stored) = conflict.content_hash.clone() else {
        return Ok(None);
    };
    let mut path_stmt = conn.prepare(
        "SELECT id, path, path_fingerprint, base_path, ours_path, theirs_path, kind,
                base_ref, ours_ref, theirs_ref, base_status, ours_status, theirs_status,
                base_mode, ours_mode, theirs_mode, created_at_ms
         FROM path_conflicts WHERE conflict_set_id = ?1 ORDER BY rowid",
    )?;
    let path_rows: Vec<StoredPathConflict> = path_stmt
        .query_map(params![conflict_set_id], |row| {
            Ok(StoredPathConflict {
                id: row.get(0)?,
                path: row.get(1)?,
                path_fingerprint: row.get(2)?,
                base_path: row.get(3)?,
                ours_path: row.get(4)?,
                theirs_path: row.get(5)?,
                kind: row.get(6)?,
                base_ref: row.get(7)?,
                ours_ref: row.get(8)?,
                theirs_ref: row.get(9)?,
                base_status: row.get(10)?,
                ours_status: row.get(11)?,
                theirs_status: row.get(12)?,
                base_mode: row.get(13)?,
                ours_mode: row.get(14)?,
                theirs_mode: row.get(15)?,
                created_at_ms: row.get(16)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let digest_rows = path_rows
        .iter()
        .map(StoredPathConflict::immutable_digest_input)
        .collect::<Vec<_>>();
    let recomputed = integrity::conflict_set_digest(&integrity::ConflictSetDigestInput {
        id: &conflict.id,
        repo_id: &conflict.repo_id,
        context: &conflict.context,
        paths_json: &conflict.paths_json,
        base_content_ref: conflict.base_content_ref.as_deref(),
        ours_content_ref: conflict.ours_content_ref.as_deref(),
        theirs_content_ref: conflict.theirs_content_ref.as_deref(),
        generated_by_operation_id: conflict.generated_by_operation_id.as_deref(),
        resolver_backend: conflict.resolver_backend.as_deref(),
        status: "unresolved",
        created_at_ms: conflict.created_at_ms,
        path_conflicts: &digest_rows,
    });
    Ok(Some((stored, recomputed)))
}

struct StoredConflictSet {
    id: String,
    repo_id: String,
    context: String,
    paths_json: String,
    created_at_ms: i64,
    base_content_ref: Option<String>,
    ours_content_ref: Option<String>,
    theirs_content_ref: Option<String>,
    generated_by_operation_id: Option<String>,
    resolver_backend: Option<String>,
    content_hash: Option<String>,
}

struct StoredPathConflict {
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
    created_at_ms: i64,
}

impl StoredPathConflict {
    fn immutable_digest_input(&self) -> integrity::PathConflictDigestInput<'_> {
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
            resolution_ref: None,
            status: "unresolved",
            created_at_ms: self.created_at_ms,
        }
    }
}

/// A decision row read back for `doctor`'s chain pass.
struct StoredDecision {
    id: String,
    proposal_id: String,
    proposal_revision_id: String,
    decision: String,
    actor: String,
    content_hash: Option<String>,
    created_at_ms: i64,
    rowid: i64,
}

/// An operation row read back for `doctor`'s chain re-walk.
struct StoredOp {
    id: String,
    parent_operation_id: Option<String>,
    command: String,
    kind: String,
    resulting_view_id: Option<String>,
    content_hash: Option<String>,
    created_at_ms: i64,
    rowid: i64,
}

#[derive(Debug, Deserialize)]
struct StoredSyncMergeState {
    lifecycle: String,
    #[allow(dead_code)]
    protocol_version: String,
    direction: String,
    remote_path: String,
    base_native_head: String,
    ours_native_head: String,
    theirs_native_head: String,
    merged_content_ref: String,
    commit_id: String,
    materialized: bool,
    imported_native_objects: i64,
    imported_ledger_rows: i64,
    sync_merge_lineage_hash: String,
}

/// The recorded `operations` rowid high-water mark (the legacy/tampered boundary for
/// operation rows).
fn op_high_water(conn: &Connection) -> Result<i64> {
    let mark: Option<i64> = conn
        .query_row(
            "SELECT op_high_water FROM integrity_marker WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(mark.unwrap_or(0))
}

/// The domain-row digest an operation folded into its chain link, recovered for the
/// `doctor` re-walk by reading the operation's view `state_json` for an `evidence_id`
/// or `decision_id` and returning that row's stored `content_hash`. `None` for
/// operations with no domain row (init, propose, attach, …).
fn op_domain_digest(conn: &Connection, view_id: Option<&str>) -> Result<Option<String>> {
    let Some(view_id) = view_id else {
        return Ok(None);
    };
    let state_json: Option<String> = conn
        .query_row(
            "SELECT state_json FROM views WHERE id = ?1",
            params![view_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(state_json) = state_json else {
        return Ok(None);
    };
    let Ok(state) = serde_json::from_str::<Value>(&state_json) else {
        return Ok(None);
    };
    if let Some(evidence_id) = state.get("evidence_id").and_then(Value::as_str) {
        return conn
            .query_row(
                "SELECT content_hash FROM evidence WHERE id = ?1",
                params![evidence_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(Into::into);
    }
    if let Some(decision_id) = state.get("decision_id").and_then(Value::as_str) {
        return conn
            .query_row(
                "SELECT content_hash FROM decisions WHERE id = ?1",
                params![decision_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(Into::into);
    }
    if state
        .get("lifecycle")
        .and_then(Value::as_str)
        .is_some_and(sync::is_sync_merged_op_kind)
    {
        let Ok(sync_state) = serde_json::from_value::<StoredSyncMergeState>(state.clone()) else {
            return Ok(None);
        };
        if !sync::is_sync_merged_op_kind(&sync_state.lifecycle) {
            return Ok(None);
        }
        let recomputed =
            integrity::sync_merge_lineage_digest(&integrity::SyncMergeLineageDigestInput {
                protocol_version: &sync_state.protocol_version,
                direction: &sync_state.direction,
                remote_path: &sync_state.remote_path,
                base_native_head: &sync_state.base_native_head,
                ours_native_head: &sync_state.ours_native_head,
                theirs_native_head: &sync_state.theirs_native_head,
                merged_content_ref: &sync_state.merged_content_ref,
                commit_id: &sync_state.commit_id,
                materialized: sync_state.materialized,
                imported_native_objects: sync_state.imported_native_objects,
                imported_ledger_rows: sync_state.imported_ledger_rows,
            });
        return Ok(Some(if sync_state.sync_merge_lineage_hash == recomputed {
            sync_state.sync_merge_lineage_hash
        } else {
            recomputed
        }));
    }
    if let Some(stored) = state.get("merge_lineage_hash").and_then(Value::as_str) {
        let recomputed = integrity::merge_lineage_digest(&integrity::MergeLineageDigestInput {
            proposal_id: state
                .get("proposal_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            proposal_revision_id: state
                .get("proposal_revision_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            snapshot_id: state
                .get("snapshot_id")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            base_head: state
                .get("base_head")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            ours_head: state
                .get("ours_head")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            base_content_ref: state
                .get("base_content_ref")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            ours_content_ref: state
                .get("ours_content_ref")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            theirs_content_ref: state
                .get("theirs_content_ref")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            merged_content_ref: state
                .get("merged_content_ref")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        });
        return Ok(Some(if recomputed == stored {
            stored.to_string()
        } else {
            recomputed
        }));
    }
    if let Some(conflict_set_id) = state.get("conflict_set_id").and_then(Value::as_str) {
        return conn
            .query_row(
                "SELECT content_hash FROM conflict_sets WHERE id = ?1",
                params![conflict_set_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(Option::flatten)
            .map_err(Into::into);
    }
    Ok(None)
}

/// Recursively scan a work tree for leftover crash-atomic-restore temp files
/// (`forge_content_native::RESTORE_TEMP_PREFIX`), skipping `.git` and `.forge`.
/// A match is the signature of a restore killed mid-flight (NER-132 U4/U7).
fn scan_restore_temps(root: &Path) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue, // unreadable dir is not a half-applied-restore signal
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                // Never descend into git's or forge's own state — at ANY depth, so a
                // submodule's nested .git (a large, unbounded object tree) is skipped
                // too. Restore temps only land in worktree dirs forge materializes
                // into, never inside a git/forge store, so this loses no real signal.
                if name == ".git" || name == ".forge" {
                    continue;
                }
                stack.push(entry.path());
            } else if name.starts_with(forge_content_native::RESTORE_TEMP_PREFIX) {
                found.push(entry.path().to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    Ok(found)
}

pub fn gc_dry_run(cwd: &Path) -> Result<GcDryRunReport> {
    Ok(gc_plan(cwd)?.into_report(true, Vec::new(), Vec::new(), Vec::new()))
}

pub fn gc_delete(cwd: &Path, expected_plan_digest: &str) -> Result<GcDryRunReport> {
    let doctor_report = doctor(cwd)?;
    if !doctor_report.ok {
        bail!("gc refuses deletion while doctor reports repository issues");
    }
    let plan = gc_plan(cwd)?;
    if plan.plan_digest != expected_plan_digest {
        return Err(ForgeError::GcPlanChanged {
            expected_digest: expected_plan_digest.to_string(),
            actual_digest: plan.plan_digest.clone(),
        }
        .into());
    }
    let context = open_repository(cwd)?;
    let native_store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let mut deleted = Vec::new();
    let mut created_packs = Vec::new();
    let mut loose_deletions = plan.loose_duplicate_native_objects.clone();
    let pack_candidate_ids = parse_native_object_ids(&plan.pack_candidate_native_objects)?;
    if let Some(pack) = native_store.write_pack_from_loose_objects(&pack_candidate_ids)? {
        created_packs.push(pack.pack_id);
        loose_deletions.extend(pack.object_ids.into_iter().map(|id| id.to_string()));
        loose_deletions.sort();
        loose_deletions.dedup();
        forge_content::maybe_crash("gc_after_pack_before_loose_delete");
    }
    for object in &loose_deletions {
        let id = forge_content_native::ObjectId::parse(object)?;
        native_store.delete_loose_duplicate(&id)?;
        deleted.push(object.clone());
        forge_content::maybe_crash("gc_after_unlink");
    }
    let mut deleted_packs = Vec::new();
    for pack_id in &plan.deletable_native_packs {
        native_store.delete_pack(pack_id)?;
        deleted_packs.push(pack_id.clone());
        forge_content::maybe_crash("gc_after_unlink");
    }
    Ok(plan.into_report(false, deleted, created_packs, deleted_packs))
}

struct GcPlan {
    unreachable_native_objects: Vec<String>,
    protected_native_objects: Vec<String>,
    pack_candidate_native_objects: Vec<String>,
    loose_duplicate_native_objects: Vec<String>,
    deletable_native_packs: Vec<String>,
    storage: StorageAccounting,
    storage_policy: StoragePolicy,
    storage_budget: StorageBudgetStatus,
    protection_window_days: u64,
    plan_digest: String,
}

impl GcPlan {
    fn into_report(
        self,
        dry_run: bool,
        deleted: Vec<String>,
        created_packs: Vec<String>,
        deleted_packs: Vec<String>,
    ) -> GcDryRunReport {
        GcDryRunReport {
            dry_run,
            unreachable_snapshots: Vec::new(),
            unreachable_evidence: Vec::new(),
            unreachable_native_objects: self.unreachable_native_objects,
            protected_native_objects: self.protected_native_objects,
            pack_candidate_native_objects: self.pack_candidate_native_objects,
            loose_duplicate_native_objects: self.loose_duplicate_native_objects,
            deletable_native_packs: self.deletable_native_packs,
            protection_window_days: self.protection_window_days,
            storage: self.storage,
            storage_policy: self.storage_policy,
            storage_budget: self.storage_budget,
            plan_digest: self.plan_digest,
            deleted,
            created_packs,
            deleted_packs,
        }
    }
}

fn gc_plan(cwd: &Path) -> Result<GcPlan> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let storage = storage::storage_accounting_for_root(&context.root_path)?;
    let storage_policy = storage::storage_policy(&connection)?;
    let storage_budget = storage::storage_budget_status_for(&storage, &storage_policy);
    let protection_window = protection_window_duration(storage_policy.protection_window_days);
    let native_store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let mut reachable = std::collections::BTreeSet::new();
    let mut statement = connection.prepare(
        "SELECT content_ref FROM snapshots
         UNION
         SELECT content_ref FROM proposal_revisions
         UNION
         SELECT base_content_ref AS content_ref FROM conflict_sets
          WHERE resolver_backend = 'native_merge' AND base_content_ref IS NOT NULL
         UNION
         SELECT ours_content_ref AS content_ref FROM conflict_sets
          WHERE resolver_backend = 'native_merge' AND ours_content_ref IS NOT NULL
         UNION
         SELECT theirs_content_ref AS content_ref FROM conflict_sets
          WHERE resolver_backend = 'native_merge' AND theirs_content_ref IS NOT NULL",
    )?;
    let refs = statement.query_map([], |row| row.get::<_, String>(0))?;
    for content_ref in refs {
        let content_ref = content_ref?;
        if content_ref.starts_with("forge-tree:") {
            if let Ok(ids) = native_store.verify_content_ref(&content_ref) {
                reachable.extend(ids);
            }
        }
    }
    // NER-138 Phase 7 slice 3: seed reachability from the AUTHORITATIVE ledger tip (not only
    // the ref-store HEAD, which a lock-free, never-reconciled gc could read stale), plus every
    // accepted `decisions.commit_id` and every op-log-referenced commit (a `checkout` target
    // writes NO decision row, so its commit is reachable only through the op-log). Each is
    // walked as a DAG root (commit → ancestry → trees). Best-effort: a dangling root is
    // surfaced by `doctor`, not fatal to this dry-run report. No-op for git-backend repos.
    let roots = ledger_commit_roots(&context, &connection)?;
    if roots
        .view_issues
        .iter()
        .any(|finding| finding.kind == LedgerViewFindingKind::CorruptStateJson)
    {
        return Err(anyhow!(
            "gc cannot read a ledger view row (corrupt views.state_json); run `forge doctor`"
        ));
    }
    if roots
        .view_issues
        .iter()
        .any(|finding| finding.kind == LedgerViewFindingKind::UnparseableCommitId)
    {
        return Err(anyhow!(
            "gc found an unparseable reachability root in the ledger; run `forge doctor`"
        ));
    }
    for id in &roots.roots {
        if let Ok(ids) = native_store.reachable_from(id) {
            reachable.extend(ids);
        }
    }
    let loose = native_store.loose_object_ids()?;
    let packed_infos = native_store.packed_object_infos()?;
    let mut packed = std::collections::BTreeSet::new();
    let mut infos_by_pack: std::collections::BTreeMap<
        String,
        Vec<forge_content_native::PackedObjectInfo>,
    > = std::collections::BTreeMap::new();
    for info in packed_infos {
        packed.insert(info.object_id.clone());
        infos_by_pack
            .entry(info.pack_id.clone())
            .or_default()
            .push(info);
    }
    let mut all = loose.clone();
    all.extend(packed.iter().cloned());
    let now = SystemTime::now();
    let now_ms = system_time_ms(now).unwrap_or(u64::MAX);
    let mut unreachable_native_objects = Vec::new();
    let mut protected_native_objects = Vec::new();
    for id in all.difference(&reachable) {
        let rendered = id.to_string();
        unreachable_native_objects.push(rendered.clone());
        let protected = if loose.contains(id) {
            loose_object_protected(&native_store, id, now, protection_window)
        } else {
            infos_by_pack
                .values()
                .flatten()
                .filter(|info| &info.object_id == id)
                .all(|info| pack_entry_protected(info, now_ms, protection_window))
        };
        if protected {
            protected_native_objects.push(rendered);
        }
    }

    let mut pack_candidate_native_objects = Vec::new();
    for id in &loose {
        if !packed.contains(id)
            && !loose_object_protected(&native_store, id, now, protection_window)
        {
            pack_candidate_native_objects.push(id.to_string());
        }
    }

    let mut loose_duplicate_native_objects = Vec::new();
    for id in loose.intersection(&packed) {
        if native_store.has_verified_packed_object(id)? {
            loose_duplicate_native_objects.push(id.to_string());
        }
    }

    let mut deletable_native_packs = Vec::new();
    for (pack_id, infos) in &infos_by_pack {
        if !infos.is_empty()
            && infos.iter().all(|info| {
                !reachable.contains(&info.object_id)
                    && !pack_entry_protected(info, now_ms, protection_window)
            })
        {
            deletable_native_packs.push(pack_id.clone());
        }
    }

    let plan_digest = gc_plan_digest(
        &pack_candidate_native_objects,
        &loose_duplicate_native_objects,
        &deletable_native_packs,
        &protected_native_objects,
        storage_policy.protection_window_days,
    );
    Ok(GcPlan {
        unreachable_native_objects,
        protected_native_objects,
        pack_candidate_native_objects,
        loose_duplicate_native_objects,
        deletable_native_packs,
        storage,
        storage_policy: storage_policy.clone(),
        storage_budget,
        protection_window_days: storage_policy.protection_window_days,
        plan_digest,
    })
}

fn gc_plan_digest(
    pack_candidate_native_objects: &[String],
    loose_duplicate_native_objects: &[String],
    deletable_native_packs: &[String],
    protected_native_objects: &[String],
    protection_window_days: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"forge-gc-plan-v2\n");
    hasher.update(format!("protection_window_days={protection_window_days}\n"));
    for id in pack_candidate_native_objects {
        hasher.update(b"pack ");
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }
    for id in loose_duplicate_native_objects {
        hasher.update(b"delete-loose ");
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }
    for pack_id in deletable_native_packs {
        hasher.update(b"delete-pack ");
        hasher.update(pack_id.as_bytes());
        hasher.update(b"\n");
    }
    for id in protected_native_objects {
        hasher.update(b"protect ");
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn parse_native_object_ids(values: &[String]) -> Result<Vec<forge_content_native::ObjectId>> {
    values
        .iter()
        .map(|value| forge_content_native::ObjectId::parse(value))
        .collect()
}

fn loose_object_protected(
    native_store: &forge_content_native::NativeObjectStore,
    id: &forge_content_native::ObjectId,
    now: SystemTime,
    protection_window: Duration,
) -> bool {
    native_store
        .object_modified_time(id)
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_none_or(|age| age < protection_window)
}

fn pack_entry_protected(
    info: &forge_content_native::PackedObjectInfo,
    now_ms: u64,
    protection_window: Duration,
) -> bool {
    let Some(packed_at_ms) = info.packed_at_ms else {
        return true;
    };
    let Some(loose_mtime_ms) = info.loose_mtime_ms else {
        return true;
    };
    let newest_ms = packed_at_ms.max(loose_mtime_ms);
    let protection_ms: u64 = match protection_window.as_millis().try_into() {
        Ok(value) => value,
        Err(_) => return true,
    };
    now_ms.saturating_sub(newest_ms) < protection_ms
}

fn protection_window_duration(days: u64) -> Duration {
    Duration::from_secs(days.saturating_mul(60 * 60 * 24))
}

fn system_time_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| duration.as_millis().try_into().ok())
}

struct LedgerCommitRoots {
    roots: std::collections::BTreeSet<forge_content_native::ObjectId>,
    view_issues: Vec<LedgerViewFinding>,
}

fn ledger_commit_roots(
    context: &RepositoryContext,
    connection: &Connection,
) -> Result<LedgerCommitRoots> {
    let mut root_strings: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut view_issues = Vec::new();
    if let Some(tip) = native_tip(context, connection)? {
        root_strings.insert(tip.to_string());
    }
    let mut decision_stmt = connection
        .prepare("SELECT commit_id FROM decisions WHERE repo_id = ?1 AND commit_id IS NOT NULL")?;
    for row in decision_stmt.query_map(params![context.repo_id], |row| row.get::<_, String>(0))? {
        root_strings.insert(row?);
    }
    let mut view_stmt = connection.prepare(
        "SELECT id, operation_id, state_json
         FROM views
         WHERE repo_id = ?1
         ORDER BY created_at_ms, rowid",
    )?;
    for row in view_stmt.query_map(params![context.repo_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (view_id, operation_id, state_json) = row?;
        let value: Value = match serde_json::from_str(&state_json) {
            Ok(value) => value,
            Err(_) => {
                view_issues.push(LedgerViewFinding {
                    kind: LedgerViewFindingKind::CorruptStateJson,
                    view_id,
                    operation_id,
                });
                continue;
            }
        };
        if let Some(commit_id) = value.get("commit_id").and_then(|value| value.as_str()) {
            if forge_content_native::ObjectId::parse(commit_id).is_err() {
                view_issues.push(LedgerViewFinding {
                    kind: LedgerViewFindingKind::UnparseableCommitId,
                    view_id,
                    operation_id,
                });
                continue;
            }
            root_strings.insert(commit_id.to_string());
        }
    }
    let mut roots = std::collections::BTreeSet::new();
    for root in root_strings {
        let id = forge_content_native::ObjectId::parse(&root).map_err(|_| {
            anyhow!("gc found an unparseable reachability root in the ledger; run `forge doctor`")
        })?;
        roots.insert(id);
    }
    Ok(LedgerCommitRoots { roots, view_issues })
}

pub fn record_failed_operation(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    code: &str,
    message: &str,
    details: Value,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    // No replay guard: this records the failure of an operation that did not
    // commit a row, so any pre-existing same-`request_id` row belongs to a
    // distinct attempt and the unique index (caught as a non-busy error by the
    // caller's `.ok()`) is the correct backstop. IMMEDIATE + retry only (R3).
    with_immediate_retry(&mut connection, |tx| {
        // A failed op is a third chain-write site (it bypasses insert_operation_view
        // with its own INSERT + CAS). It must carry a content_hash too, or it leaves a
        // NULL-hash op on the spine that `doctor`/the gate would mis-flag as tampered
        // (NER-136). No domain row, so the digest is None.
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command,
                kind: "recoverable_failure",
                created_at_ms: now,
            },
            None,
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'failed', 'recoverable_failure', ?5, ?6, ?7, ?8, ?9)",
            params![
                operation_id,
                context.repo_id,
                request_id,
                command,
                context.current_operation_id,
                view_id,
                // Persist the typed error's `details` alongside code/message so a
                // later `--request-id` replay reconstructs the SAME details the first
                // response carried (FIX C). Old rows lacking `details` fall back to
                // an empty object at replay time.
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
                    "message": message
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
            // Intentionally left UNTYPED (plain anyhow, not the retryable
            // `CurrentStateChanged`): this is the failure-recording path, whose
            // result the CLI already swallows with `.ok()` (command_result's error
            // arm). A CAS loss here just means the failure was not recorded; it must
            // not become a retryable CONFLICT.
            return Err(anyhow!("current operation changed"));
        }
        Ok(())
    })?;
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

/// Which attempts to compare (NER-137). With neither field set, `compare_attempts`
/// returns every intent that has ≥1 attempt; `intent_id` filters to one intent;
/// `attempt_id` scopes to that attempt's intent. Unknown selectors raise the existing
/// `UnknownIntent`/`UnknownAttempt` typed errors — multiple intents are *grouped*,
/// not an ambiguity error.
#[derive(Debug, Clone, Default)]
pub struct CompareSelector {
    pub intent_id: Option<String>,
    pub attempt_id: Option<String>,
}

/// The compare/rank result: competing attempts grouped per intent, each group ranked
/// (NER-137 R1/R2). The headline read surface that lets a human or agent select a
/// winner from verified data and chain `compare → accept` headlessly.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptComparison {
    pub intents: Vec<IntentComparison>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IntentComparison {
    pub intent_id: String,
    pub intent: String,
    pub attempts: Vec<AttemptCompareRow>,
}

/// One competing attempt's compare row. Ranking is **advisory**; the per-gate results,
/// metrics, integrity label, and raw changed paths are **authoritative** and always
/// present. `rank` is `None` for an attempt that is unrankable — its deciding evidence
/// failed the (cheap per-row) integrity check (`integrity == "tampered"`) or it has no
/// proposal yet — so a headless consumer that selects by numeric-minimum rank can
/// never pick a tampered attempt (NER-137 R4).
#[derive(Debug, Clone, Serialize)]
pub struct AttemptCompareRow {
    pub attempt_id: String,
    pub status: String,
    pub proposal: Option<ComparedProposal>,
    /// Secret-redacted file-level diff summary of the proposal vs its base — the paths
    /// the snapshot changed (the per-attempt "diff summary"). The richer pairwise
    /// file/hunk content diff is the CLI's backend-routed `compare --diff` path.
    pub changed_paths: Vec<String>,
    pub changed_count: usize,
    pub gates: Vec<forge_policy::GateResult>,
    pub check_status: Option<String>,
    pub metrics: StructuredMetrics,
    /// `"verified"` (deciding rows pass the cheap per-row check), `"legacy_unverified"`
    /// (a pre-Phase-5 grandfathered row), `"tampered"` (a deciding row failed the cheap
    /// check — recorded here, NOT propagated, so one bad attempt does not blank the
    /// comparison), or `"no_evidence"` (no deciding evidence / no proposal). The deep
    /// recompute-row-hash case is `doctor`'s op-walk, not this cheap label.
    pub integrity: String,
    pub decision_status: Option<String>,
    pub publication_status: Option<String>,
    pub rank: Option<u32>,
    pub rank_reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparedProposal {
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub snapshot_id: String,
    pub base_head: String,
    pub content_ref: String,
}

/// Parsed numeric metrics aggregated over the proposal-snapshot evidence (NER-137).
/// Test counts drive the ranking metric tier; clippy findings are surfaced but do not
/// influence the order in v0 (R3 narrowing — callers re-rank on these raw metrics).
#[derive(Debug, Clone, Serialize, Default)]
pub struct StructuredMetrics {
    pub tests_passed: Option<u64>,
    pub tests_failed: Option<u64>,
    pub tests_ignored: Option<u64>,
    pub clippy_findings: Option<u64>,
}

const INTEGRITY_VERIFIED: &str = "verified";
const INTEGRITY_LEGACY: &str = "legacy_unverified";
const INTEGRITY_TAMPERED: &str = "tampered";
const INTEGRITY_NO_EVIDENCE: &str = "no_evidence";

/// Evidence-based attempt comparison and ranking (NER-137, Phase 6). Read-only — opens
/// a throwaway connection, takes **no** advisory lock (it never writes). Its ranking is
/// a snapshot a concurrent lock-free `run` can invalidate, so it is **advisory**;
/// `accept`/`decide` keep the authoritative in-txn gate. Ranks on Phase 5
/// cheaply-verified evidence: a deciding row that fails the per-row check labels the
/// attempt `tampered` and leaves it unranked (`rank: null`).
pub fn compare_attempts(cwd: &Path, selector: CompareSelector) -> Result<AttemptComparison> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let marker = evidence_high_water(&connection)?;

    let intent_ids = resolve_compare_intents(&context, &connection, &selector)?;
    let mut intents = Vec::new();
    for (intent_id, intent_text) in intent_ids {
        let attempts = attempts_for_intent(&connection, &context.repo_id, &intent_id)?;
        let mut rows = Vec::new();
        for attempt in attempts {
            rows.push(build_compare_row(&context, &connection, &attempt, marker)?);
        }
        rank_compare_rows(&mut rows);
        intents.push(IntentComparison {
            intent_id,
            intent: intent_text,
            attempts: rows,
        });
    }
    Ok(AttemptComparison { intents })
}

/// Resolve the `(intent_id, intent_text)` groups a `CompareSelector` names. An
/// `attempt_id` maps to its intent; an `intent_id` filters to that one; neither
/// returns every intent with ≥1 attempt, ordered by first attempt.
fn resolve_compare_intents(
    context: &RepositoryContext,
    conn: &Connection,
    selector: &CompareSelector,
) -> Result<Vec<(String, String)>> {
    if let Some(attempt_id) = &selector.attempt_id {
        let attempt =
            attempt_by_id(context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
                selector: attempt_id.clone(),
            })?;
        return Ok(vec![(attempt.intent_id, attempt.intent)]);
    }
    if let Some(intent_id) = &selector.intent_id {
        let text: Option<String> = conn
            .query_row(
                "SELECT text FROM intents WHERE id = ?1",
                params![intent_id],
                |row| row.get(0),
            )
            .optional()?;
        let text = text.ok_or_else(|| ForgeError::UnknownIntent {
            selector: intent_id.clone(),
        })?;
        return Ok(vec![(intent_id.clone(), text)]);
    }
    // All intents that have ≥1 attempt, ordered by the intent's first attempt so the
    // grouping is deterministic.
    let mut statement = conn.prepare(
        "SELECT i.id, i.text, MIN(a.created_at_ms) AS first_attempt
         FROM intents i
         JOIN attempts a ON a.intent_id = i.id
         WHERE i.repo_id = ?1
         GROUP BY i.id, i.text
         ORDER BY first_attempt ASC, i.id ASC",
    )?;
    let rows = statement.query_map(params![context.repo_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn attempts_for_intent(
    conn: &Connection,
    repo_id: &str,
    intent_id: &str,
) -> Result<Vec<AttemptRecord>> {
    let mut statement = conn.prepare(
        "SELECT a.id, a.intent_id, i.text, a.base_head, a.status
         FROM attempts a
         JOIN intents i ON i.id = a.intent_id
         WHERE a.repo_id = ?1 AND a.intent_id = ?2
         ORDER BY a.created_at_ms ASC, a.id ASC",
    )?;
    let rows = statement.query_map(params![repo_id, intent_id], |row| {
        Ok(AttemptRecord {
            attempt_id: row.get(0)?,
            intent_id: row.get(1)?,
            intent: row.get(2)?,
            base_head: row.get(3)?,
            status: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Project an intent's parsed [`forge_policy::CheckSpec`] into the egress
/// [`IntentGate`]s, renaming `require_structured_pass` to `structured` and applying the
/// same per-arg `key=value` secret redaction [`redact_gate_result`] uses on the check
/// surface (the stored `check_spec_json` is raw, so a secret-like gate token must be
/// scrubbed before this egress — NER-257 secret-safety).
fn intent_gates(conn: &Connection, intent_id: &str) -> Result<Vec<IntentGate>> {
    let spec = intent_check_spec(conn, intent_id)?;
    Ok(spec
        .gates
        .into_iter()
        .map(|gate| IntentGate {
            program: forge_content::redact_secret_like_text(&gate.program).0,
            args: gate
                .args
                .iter()
                .map(|arg| forge_content::redact_secret_like_text(arg).0)
                .collect(),
            structured: gate.require_structured_pass,
        })
        .collect())
}

/// Derive an intent-level status from its linked attempts (NER-257): the `intents` table
/// has no status column, so `accepted` if any linked attempt has an accepted decision,
/// else `open`. Honest and migration-free; the derived field is documented as such.
fn intent_derived_status(conn: &Connection, repo_id: &str, intent_id: &str) -> Result<String> {
    // An accepted decision joins back to its attempt via proposal → attempt, and the
    // attempt to its intent. Repo-scoped on every table so a multi-repo DB never leaks
    // another repo's decision.
    let accepted: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM decisions d
             JOIN proposals p ON p.id = d.proposal_id AND p.repo_id = d.repo_id
             JOIN attempts a ON a.id = p.attempt_id AND a.repo_id = d.repo_id
             WHERE d.repo_id = ?1 AND a.intent_id = ?2 AND d.decision = 'accepted'
         )",
        params![repo_id, intent_id],
        |row| row.get(0),
    )?;
    Ok(if accepted { "accepted" } else { "open" }.to_string())
}

/// List every intent in the repo (NER-257), oldest first, each with its title, a status
/// derived from its linked attempts, the declared (secret-redacted) gate spec, and the
/// linked attempt ids. Repo-scoped — never leaks another repo's intents/attempts.
pub fn intents_list(cwd: &Path) -> Result<Vec<IntentSummary>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let mut statement = connection.prepare(
        "SELECT id, text FROM intents WHERE repo_id = ?1 ORDER BY created_at_ms ASC, id ASC",
    )?;
    let intent_rows: Vec<(String, String)> = statement
        .query_map(params![context.repo_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut summaries = Vec::with_capacity(intent_rows.len());
    for (intent_id, title) in intent_rows {
        let gates = intent_gates(&connection, &intent_id)?;
        let attempt_ids = attempts_for_intent(&connection, &context.repo_id, &intent_id)?
            .into_iter()
            .map(|attempt| attempt.attempt_id)
            .collect();
        let status = intent_derived_status(&connection, &context.repo_id, &intent_id)?;
        summaries.push(IntentSummary {
            intent_id,
            title,
            status,
            gates,
            attempt_ids,
        });
    }
    Ok(summaries)
}

/// Detail for one intent (NER-257). The repo-scoped existence check (two-column
/// `repo_id`+`id` WHERE, mirroring [`attempt_by_id`]) is mandatory BEFORE handing the id
/// to [`intent_check_spec`]/[`intent_gates`], which query by `id` alone — so a
/// cross-repo or unknown id is rejected with `UnknownIntent` rather than reading another
/// repo's spec.
pub fn intent_detail(cwd: &Path, intent_id: &str) -> Result<IntentDetail> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    intent_detail_on(&connection, &context, intent_id)
}

fn intent_detail_on(
    connection: &Connection,
    context: &RepositoryContext,
    intent_id: &str,
) -> Result<IntentDetail> {
    let title: Option<String> = connection
        .query_row(
            "SELECT text FROM intents WHERE repo_id = ?1 AND id = ?2",
            params![context.repo_id, intent_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(title) = title else {
        return Err(ForgeError::UnknownIntent {
            selector: intent_id.to_string(),
        }
        .into());
    };
    let gates = intent_gates(connection, intent_id)?;
    let attempt_ids = attempts_for_intent(connection, &context.repo_id, intent_id)?
        .into_iter()
        .map(|attempt| attempt.attempt_id)
        .collect();
    let status = intent_derived_status(connection, &context.repo_id, intent_id)?;
    Ok(IntentDetail {
        intent_id: intent_id.to_string(),
        title,
        status,
        gates,
        attempt_ids,
    })
}

/// Build one attempt's compare row. Runs the Phase 4 policy evaluation for per-gate
/// verdicts and the Phase 5 per-row integrity check, **recording** (not propagating) a
/// tamper as a label. No git is invoked here (the store stays git-free, PRD §23.4); the
/// per-attempt diff summary is the stored, secret-redacted `changed_paths`.
fn build_compare_row(
    context: &RepositoryContext,
    conn: &Connection,
    attempt: &AttemptRecord,
    marker: i64,
) -> Result<AttemptCompareRow> {
    let proposal = latest_proposal_for_attempt(context, &attempt.attempt_id)?;
    let Some(proposal) = proposal else {
        return Ok(AttemptCompareRow {
            attempt_id: attempt.attempt_id.clone(),
            status: attempt.status.clone(),
            proposal: None,
            changed_paths: Vec::new(),
            changed_count: 0,
            gates: Vec::new(),
            check_status: None,
            metrics: StructuredMetrics::default(),
            integrity: INTEGRITY_NO_EVIDENCE.to_string(),
            decision_status: None,
            publication_status: None,
            rank: None,
            rank_reason: "no proposal yet".to_string(),
        });
    };

    let spec = intent_check_spec(conn, &attempt.intent_id)?;
    let facts = evidence_facts_on(conn, &attempt.attempt_id)?;
    let outcome = forge_policy::evaluate(&spec, &proposal.snapshot_id, &facts);
    let integrity = aggregate_integrity(conn, &outcome, marker)?;
    let metrics = compare_structured_metrics(conn, &attempt.attempt_id, &proposal.snapshot_id)?;

    let (kept_paths, _dropped) = forge_content::filter_secret_risk(&proposal.changed_paths);
    let changed_count = kept_paths.len();
    let gates = outcome.gates.into_iter().map(redact_gate_result).collect();
    let decision_status =
        latest_decision_for_proposal_revision(context, &proposal.proposal_revision_id)?;
    let publication_status =
        latest_publication_for_proposal_revision(context, &proposal.proposal_revision_id)?
            .map(|_| "published".to_string());

    Ok(AttemptCompareRow {
        attempt_id: attempt.attempt_id.clone(),
        status: attempt.status.clone(),
        proposal: Some(ComparedProposal {
            proposal_id: proposal.proposal_id,
            proposal_revision_id: proposal.proposal_revision_id,
            snapshot_id: proposal.snapshot_id,
            base_head: proposal.base_head,
            content_ref: proposal.content_ref,
        }),
        changed_paths: kept_paths,
        changed_count,
        gates,
        check_status: Some(outcome.status),
        metrics,
        integrity,
        decision_status,
        publication_status,
        rank: None, // assigned by rank_compare_rows
        rank_reason: String::new(),
    })
}

/// Aggregate the per-row integrity of the gates' deciding evidence into one label,
/// fail-closed toward the strongest signal: any tampered deciding row → `tampered`;
/// else any legacy → `legacy_unverified`; else if at least one gate has deciding
/// evidence → `verified`; else `no_evidence` (e.g. a `missing` gate set).
fn aggregate_integrity(
    conn: &Connection,
    outcome: &forge_policy::CheckOutcome,
    marker: i64,
) -> Result<String> {
    let mut any_deciding = false;
    let mut any_legacy = false;
    for gate in &outcome.gates {
        if let Some(evidence_id) = &gate.evidence_id {
            any_deciding = true;
            match verify_evidence_integrity(conn, evidence_id, marker)? {
                IntegrityStatus::Tampered(_) => return Ok(INTEGRITY_TAMPERED.to_string()),
                IntegrityStatus::LegacyUnverified => any_legacy = true,
                IntegrityStatus::Verified => {}
            }
        }
    }
    Ok(if any_legacy {
        INTEGRITY_LEGACY.to_string()
    } else if any_deciding {
        INTEGRITY_VERIFIED.to_string()
    } else {
        INTEGRITY_NO_EVIDENCE.to_string()
    })
}

/// Aggregate parsed structured metrics over the proposal-snapshot evidence: sum test
/// counts across test binaries/rows and clippy findings across clippy rows.
fn compare_structured_metrics(
    conn: &Connection,
    attempt_id: &str,
    snapshot_id: &str,
) -> Result<StructuredMetrics> {
    let mut statement = conn.prepare(
        "SELECT structured_json FROM evidence
         WHERE attempt_id = ?1 AND snapshot_id = ?2 AND structured_json IS NOT NULL
         ORDER BY created_at_ms ASC, rowid ASC",
    )?;
    let rows = statement.query_map(params![attempt_id, snapshot_id], |row| {
        row.get::<_, String>(0)
    })?;
    let mut metrics = StructuredMetrics::default();
    for json in rows {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&json?) else {
            continue;
        };
        let get = |key: &str| value.get(key).and_then(serde_json::Value::as_u64);
        metrics.tests_passed = add_opt(metrics.tests_passed, get("passed"));
        metrics.tests_failed = add_opt(metrics.tests_failed, get("failed"));
        metrics.tests_ignored = add_opt(metrics.tests_ignored, get("ignored"));
        metrics.clippy_findings = add_opt(metrics.clippy_findings, get("findings"));
    }
    Ok(metrics)
}

fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (Some(x), None) => Some(x),
        (None, Some(y)) => Some(y),
        (Some(x), Some(y)) => Some(x + y),
    }
}

/// Which of the two adjacent rows the caller is describing: the higher-ranked `Above`
/// (phrase the discriminator as its advantage) or the lower-ranked `Below` (phrase it as
/// the reason it landed lower).
enum TieSide {
    Above,
    Below,
}

/// Name the FIRST sort-key field (after the gate tier, assumed equal here) on which the
/// higher-ranked `above` strictly beats `below` — i.e. the tie-break that actually placed
/// `below` after `above`. The label is phrased from `side`'s perspective: `Above` reads as
/// the winner's advantage, `Below` as why the loser ranked lower. Returns `None` only when
/// the two rows are equal on every tie-break field (a stable-order tie), so the caller can
/// fall back to a neutral phrasing rather than claim a discriminator that did not apply
/// (NER-256).
fn tie_break_discriminator(
    above: &AttemptCompareRow,
    below: &AttemptCompareRow,
    side: TieSide,
) -> Option<String> {
    let integrity_rank = |row: &AttemptCompareRow| match row.integrity.as_str() {
        INTEGRITY_VERIFIED => 0u8,
        INTEGRITY_LEGACY => 1,
        _ => 2,
    };
    if integrity_rank(below) != integrity_rank(above) {
        let target = match side {
            TieSide::Above => above,
            TieSide::Below => below,
        };
        let label = match target.integrity.as_str() {
            INTEGRITY_VERIFIED => "verified evidence",
            INTEGRITY_LEGACY => "legacy_unverified evidence",
            _ => "no-evidence integrity",
        };
        return Some(label.to_string());
    }
    let empty = |row: &AttemptCompareRow| row.changed_count == 0;
    if empty(below) != empty(above) {
        // The winner has the non-empty diff; the loser has the empty one.
        return Some(match side {
            TieSide::Above => "non-empty diff".to_string(),
            TieSide::Below => "empty diff".to_string(),
        });
    }
    let below_failed = below.metrics.tests_failed.unwrap_or(u64::MAX);
    let above_failed = above.metrics.tests_failed.unwrap_or(u64::MAX);
    if below_failed != above_failed {
        return Some(match side {
            TieSide::Above => format!("fewer failing tests ({above_failed} vs {below_failed})"),
            TieSide::Below => format!("more failing tests ({below_failed} vs {above_failed})"),
        });
    }
    let below_passed = below.metrics.tests_passed.unwrap_or(0);
    let above_passed = above.metrics.tests_passed.unwrap_or(0);
    if below_passed != above_passed {
        return Some(match side {
            TieSide::Above => format!("more passing tests ({above_passed} vs {below_passed})"),
            TieSide::Below => format!("fewer passing tests ({below_passed} vs {above_passed})"),
        });
    }
    None
}

/// Assign a deterministic total-order rank to the rankable rows of one intent group
/// (NER-137 R3/R4). Rankable = has a proposal AND not `tampered`. Order: all-required-
/// gates-passing first; within a tier fewer parsed test failures, then more parsed
/// passing; stable ties keep first-attempt order (input is created-order). Tampered /
/// no-proposal rows get `rank: null` and are placed after the ranked rows.
fn rank_compare_rows(rows: &mut Vec<AttemptCompareRow>) {
    let rankable =
        |row: &AttemptCompareRow| row.proposal.is_some() && row.integrity != INTEGRITY_TAMPERED;
    // Partition while preserving input (created) order for stable ties.
    let mut ranked: Vec<AttemptCompareRow> = Vec::new();
    let mut unranked: Vec<AttemptCompareRow> = Vec::new();
    for row in rows.drain(..) {
        if rankable(&row) {
            ranked.push(row);
        } else {
            unranked.push(row);
        }
    }
    // NER-256 tie-break, applied ONLY after gates_passing so a passing gate still
    // strictly outranks a non-passing one. When the gate status ties (e.g. all gates
    // missing/unmet), prefer (a) higher-integrity evidence, then (b) a non-empty diff,
    // then (c) fewer test failures / more passes — so a zero-change, no-evidence attempt
    // can no longer outrank a verified attempt with a real diff purely on created order.
    // Tampered rows never reach this sort (partitioned into `unranked`), so integrity
    // here is only verified / legacy_unverified / no_evidence.
    let sort_key = |row: &AttemptCompareRow| {
        let gates_passing = if row.check_status.as_deref() == Some("passed") {
            0u8
        } else {
            1
        };
        let integrity_rank = match row.integrity.as_str() {
            INTEGRITY_VERIFIED => 0u8,
            INTEGRITY_LEGACY => 1,
            _ => 2, // INTEGRITY_NO_EVIDENCE
        };
        let empty_diff = if row.changed_count > 0 { 0u8 } else { 1 };
        (
            gates_passing,
            integrity_rank,
            empty_diff,
            row.metrics.tests_failed.unwrap_or(u64::MAX),
            std::cmp::Reverse(row.metrics.tests_passed.unwrap_or(0)),
        )
    };
    ranked.sort_by_key(sort_key);
    // Did EVERY ranked attempt land in the same gate tier? Only then is "gates tie" an
    // honest description for a non-passing row: if any attempt passed its gates, the
    // non-passing remainder did not tie on gates — they lost on `gates_passing` (NER-256
    // correctness review). When the gate tier truly ties, the per-row reason names the
    // discriminator that actually separated THIS row from the one ranked just above it
    // (integrity, diff, or test counts) rather than a fixed integrity+diff label that may
    // not have differentiated anything (NER-256 adversarial review).
    let gates_truly_tie = ranked
        .first()
        .map(|first| {
            let first_gate = sort_key(first).0;
            ranked.iter().all(|row| sort_key(row).0 == first_gate)
        })
        .unwrap_or(true);
    for index in 0..ranked.len() {
        let rank = (index + 1) as u32;
        let passed = ranked[index].check_status.as_deref() == Some("passed");
        let mut reason = if passed {
            format!(
                "rank {rank}: all required gates passing ({} failing tests, {} passing)",
                ranked[index].metrics.tests_failed.unwrap_or(0),
                ranked[index].metrics.tests_passed.unwrap_or(0)
            )
        } else {
            let status = ranked[index]
                .check_status
                .as_deref()
                .unwrap_or("unknown")
                .to_string();
            if !gates_truly_tie {
                // A passing-gate attempt outranked this one: gates were NOT tied, this row
                // simply lost. Keep the old, accurate phrasing for the non-passing remainder.
                format!("rank {rank}: required gates not satisfied (check status: {status})")
            } else {
                // Genuine gate tie. Name the field that actually broke the tie. For a
                // non-winner, compare against the row ranked immediately above (why it
                // landed below). For the rank-1 winner, compare against the row below it
                // (why it ranked first). The discriminator is phrased from THIS row's
                // perspective so a rank-only consumer reads the real differentiator —
                // integrity, diff, or test counts — not a fixed integrity+diff label.
                let label = match index.checked_sub(1) {
                    Some(prev) => {
                        tie_break_discriminator(&ranked[prev], &ranked[index], TieSide::Below)
                    }
                    None => ranked.get(index + 1).and_then(|next| {
                        tie_break_discriminator(&ranked[index], next, TieSide::Above)
                    }),
                };
                match label {
                    Some(label) => format!(
                        "rank {rank}: gates tie (check status: {status}); ranked by {label}"
                    ),
                    // No neighbour differed (stable-order tie) or single-row group.
                    None => format!(
                        "rank {rank}: gates tie (check status: {status}); ranked by created order"
                    ),
                }
            }
        };
        // A legacy_unverified attempt is rankable (its deciding evidence predates
        // Phase 5 and was never hash-verified), so a rank-only consumer must still see
        // that caveat in the explanation (NER-137 code-review).
        if ranked[index].integrity == INTEGRITY_LEGACY {
            reason.push_str(
                " — NOTE: deciding evidence is legacy_unverified (pre-Phase-5, not hash-verified)",
            );
        }
        ranked[index].rank = Some(rank);
        ranked[index].rank_reason = reason;
    }
    for row in &mut unranked {
        row.rank = None;
        row.rank_reason = if row.proposal.is_none() {
            "unranked: no proposal yet".to_string()
        } else {
            "unranked: deciding evidence failed the integrity check (tampered) — verify with `doctor`".to_string()
        };
    }
    ranked.append(&mut unranked);
    *rows = ranked;
}

/// The stored chain hash of an operation, or the genesis sentinel when the parent
/// is absent (the `init` genesis op) or predates Phase 5 (a legacy NULL hash). It is
/// the `parent_hash` input to the next link, so a chain always anchors on one
/// canonical value (NER-136). Read on the writer's `&tx` so the folded parent and
/// the singleton CAS pointer are the same row.
fn op_content_hash(conn: &Connection, operation_id: Option<&str>) -> Result<String> {
    let Some(operation_id) = operation_id else {
        return Ok(integrity::GENESIS_PARENT_HASH.to_string());
    };
    let stored: Option<String> = conn
        .query_row(
            "SELECT content_hash FROM operations WHERE id = ?1",
            params![operation_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(stored.unwrap_or_else(|| integrity::GENESIS_PARENT_HASH.to_string()))
}

fn insert_operation_view(
    tx: &Transaction<'_>,
    repo_id: &str,
    parent_operation_id: Option<&str>,
    input: OperationViewInput,
) -> Result<OperationViewResult> {
    insert_operation_view_chained(tx, repo_id, parent_operation_id, input, None)
}

/// Append an operation/view, folding `domain_digest` (the evidence/decision row's
/// own `content_hash`, or `None` for ops with no domain row) plus the parent op's
/// hash into `operations.content_hash` — the tamper-evident chain spine (NER-136).
/// Computed inside the writer's IMMEDIATE txn; the parent read is on the same `&tx`.
fn insert_operation_view_chained(
    tx: &Transaction<'_>,
    repo_id: &str,
    parent_operation_id: Option<&str>,
    input: OperationViewInput,
    domain_digest: Option<&str>,
) -> Result<OperationViewResult> {
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let status = format!("{:?}", OperationStatus::Succeeded).to_lowercase();
    let view_kind = format!("{:?}", input.view_kind).to_lowercase();
    let parent_hash = op_content_hash(tx, parent_operation_id)?;
    let content_hash = integrity::operation_link_hash(
        &parent_hash,
        &integrity::OperationDigestInput {
            operation_id: &operation_id,
            command: &input.command,
            kind: &input.kind,
            created_at_ms: now,
        },
        domain_digest,
    );

    tx.execute(
        "INSERT INTO operations (
            id, repo_id, request_id, command, status, kind, parent_operation_id,
            resulting_view_id, error_json, content_hash, created_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)",
        params![
            operation_id,
            repo_id,
            input.request_id,
            input.command,
            status,
            input.kind,
            parent_operation_id,
            view_id,
            content_hash,
            now
        ],
    )?;
    tx.execute(
        "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            view_id,
            repo_id,
            operation_id,
            view_kind,
            input.state.to_string(),
            now
        ],
    )?;
    let expected_operation = parent_operation_id.context("missing parent operation")?;
    let updated = tx.execute(
        "UPDATE current_state
         SET current_operation_id = ?1, current_view_id = ?2, updated_at_ms = ?3
         WHERE singleton = 1 AND current_operation_id = ?4",
        params![operation_id, view_id, now, expected_operation],
    )?;
    if updated != 1 {
        // The optimistic singleton CAS lost the race: another writer advanced
        // `current_state` between this command's determining read and its write.
        // Surface it TYPED so the CLI classifies it `retryable` (code CONFLICT) and
        // does NOT persist it under the `--request-id` — a retry re-executes against
        // fresh state instead of replaying a poisoned failure (NER-133 FIX D / R7).
        // Caveat: re-executing re-runs the command; for `forge run` that re-executes
        // the child process (run records evidence via this fn and is the lock
        // carve-out). See the `notes.retry_side_effects` entry in `forge schema`.
        return Err(ForgeError::CurrentStateChanged.into());
    }
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

/// How long a connection waits on a held write lock before SQLite returns
/// `SQLITE_BUSY`. Generous because contention here is brief (small txns) and the
/// bounded retry in `with_immediate_retry` is only a defensive backstop.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

fn open_connection(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)?;
    // `busy_timeout` and `synchronous` are per-connection and must be re-applied
    // on every open; `journal_mode=WAL` is persistent (header byte) but is cheap
    // to re-assert. WAL lets readers run without blocking the single writer, so
    // many `forge` processes can share one `.forge/forge.db` (R2).
    connection.busy_timeout(BUSY_TIMEOUT)?;
    // `journal_mode` returns a row, so `pragma_update` errors with
    // `ExecuteReturnedResults`; `execute_batch` is the correct call.
    connection.execute_batch("PRAGMA journal_mode=WAL;")?;
    // NORMAL is the crash-safe WAL pairing: only the last commit can be lost on
    // power loss, never the database. (FULL at decision points is deferred.)
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    Ok(connection)
}

/// Upper bound on `IMMEDIATE`-txn attempts before a transient lock error is
/// surfaced. `busy_timeout` already absorbs ordinary contention at `BEGIN`, so
/// this only matters for `SQLITE_BUSY_SNAPSHOT` (which `busy_timeout` does not
/// retry) and the rare post-timeout `SQLITE_BUSY`.
const WRITE_TXN_MAX_ATTEMPTS: u32 = 6;

/// Run `body` inside a `BEGIN IMMEDIATE` transaction, committing on success, and
/// retry the whole transaction on transient `SQLITE_BUSY` / `SQLITE_BUSY_SNAPSHOT`
/// with bounded, jittered backoff (R3).
///
/// `IMMEDIATE` takes the write lock at `BEGIN`, so a read-then-write body cannot
/// hit the deferred-upgrade `SQLITE_BUSY_SNAPSHOT` race; the 517 catch below is a
/// defensive backstop. Non-busy errors (including [`RequestIdReplay`]) propagate
/// immediately without retry.
///
/// `body` is `FnMut` because it may run once per retry attempt, so it must not
/// move captured values out — writer closures therefore `.clone()` any owned
/// input they consume (e.g. `request_id`, `OperationViewInput`) on each call.
pub(crate) fn with_immediate_retry<T, F>(connection: &mut Connection, mut body: F) -> Result<T>
where
    F: FnMut(&Transaction<'_>) -> Result<T>,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        match run_immediate_once(connection, &mut body) {
            Ok(value) => return Ok(value),
            Err(error) => {
                if attempt < WRITE_TXN_MAX_ATTEMPTS && is_retryable_busy(&error) {
                    sleep_backoff(attempt);
                    continue;
                }
                return Err(error);
            }
        }
    }
}

/// Single `IMMEDIATE` attempt. Split out so the `&mut Connection` borrow is
/// released between retries (the txn is dropped — and thus rolled back — on any
/// error before commit).
fn run_immediate_once<T, F>(connection: &mut Connection, body: &mut F) -> Result<T>
where
    F: FnMut(&Transaction<'_>) -> Result<T>,
{
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let value = body(&tx)?;
    tx.commit()?;
    Ok(value)
}

/// True if any link in the error's source chain is a `SQLITE_BUSY`-class failure.
/// Matching the primary `DatabaseBusy` code covers every `SQLITE_BUSY_*` extended
/// code, including `SQLITE_BUSY_SNAPSHOT` (517); the explicit 517 check documents
/// that intent for reviewers.
fn is_retryable_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(rusqlite::Error::SqliteFailure(err, _)) =
            cause.downcast_ref::<rusqlite::Error>()
        {
            matches!(
                err.code,
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
            ) || err.extended_code == 517
        } else {
            false
        }
    })
}

/// Short, jittered backoff between busy retries. Jitter mixes the process id
/// (distinct per concurrent process) with the wall-clock nanosecond (distinct
/// per attempt) over a 0–24 ms window, so concurrent processes desynchronize
/// rather than retrying in lockstep even when their clocks read the same coarse
/// nanosecond. No `rand` dependency is pulled in.
fn sleep_backoff(attempt: u32) {
    let base_ms = (1u64 << attempt.min(5)).min(50); // 2, 4, 8, 16, 32, 50…
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let jitter_ms = u64::from(std::process::id()).wrapping_add(u64::from(nanos)) % 25;
    std::thread::sleep(Duration::from_millis(base_ms + jitter_ms));
}

/// Signals that a writer observed an already-recorded operation for the same
/// `(repo_id, request_id)` inside its `IMMEDIATE` transaction (U5). The writer
/// rolls back without inserting domain rows; the CLI replays the carried
/// operation's original result instead of treating this as a fresh write.
#[derive(Debug, Clone)]
pub struct RequestIdReplay {
    pub operation: RequestIdOperation,
}

impl std::fmt::Display for RequestIdReplay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "request id already recorded for command {}",
            self.operation.command
        )
    }
}

impl std::error::Error for RequestIdReplay {}

/// Re-check, as the first statement of an `IMMEDIATE` write transaction, whether
/// this `(repo_id, request_id)` already produced a committed operation row. If so
/// (a concurrent retry won the race), abort with [`RequestIdReplay`] carrying the
/// existing row so the caller can replay rather than collide at commit (U5,
/// option a). The same `created_at_ms DESC, rowid DESC` ordering as the CLI
/// pre-flight read keeps replay deterministic.
fn replay_guard(tx: &Transaction<'_>, repo_id: &str, request_id: Option<&str>) -> Result<()> {
    let Some(request_id) = request_id else {
        return Ok(());
    };
    let existing = tx
        .query_row(
            "SELECT o.id, o.command, o.status, o.error_json, o.kind, o.resulting_view_id, v.state_json
             FROM operations o
             LEFT JOIN views v ON v.id = o.resulting_view_id
             WHERE o.repo_id = ?1 AND o.request_id = ?2
             ORDER BY o.created_at_ms DESC, o.rowid DESC LIMIT 1",
            params![repo_id, request_id],
            |row| {
                let error_json: Option<String> = row.get(3)?;
                let state_json: Option<String> = row.get(6)?;
                Ok(RequestIdOperation {
                    operation_id: row.get(0)?,
                    command: row.get(1)?,
                    status: row.get(2)?,
                    error_json: error_json.and_then(|json| serde_json::from_str(&json).ok()),
                    kind: row.get(4)?,
                    view_id: row.get(5)?,
                    state: state_json.and_then(|json| serde_json::from_str(&json).ok()),
                })
            },
        )
        .optional()?;
    match existing {
        Some(operation) => Err(RequestIdReplay { operation }.into()),
        None => Ok(()),
    }
}

fn read_init_repository(
    connection: &Connection,
    root: &Path,
    forge_dir: &Path,
    database_path: &Path,
) -> Result<Option<InitRepository>> {
    let row = connection
        .query_row(
            "SELECT r.id, r.git_head, r.content_backend, cs.current_operation_id, cs.current_view_id
             FROM repositories r
             JOIN current_state cs ON cs.repo_id = r.id
             WHERE r.root_path = ?1",
            params![root.to_string_lossy()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;

    Ok(row.map(
        |(repository_id, git_head, content_backend, current_operation_id, current_view_id)| {
            InitRepository {
                repository_id,
                root_path: root.to_string_lossy().into_owned(),
                forge_dir: forge_dir.to_string_lossy().into_owned(),
                database_path: database_path.to_string_lossy().into_owned(),
                git_head,
                content_backend,
                current_operation_id,
                current_view_id,
                already_initialized: true,
            }
        },
    ))
}

/// Find the Forge repository root by walking up from `cwd` for the nearest ancestor that
/// contains the `.forge/forge.db` repo marker. GIT-FREE (NER-138 Phase 7 slice 3): post-`init`
/// commands resolve the root without the git binary, so the native lifecycle
/// (start→save→…→accept→restore→log→checkout→undo) runs with git removed from PATH. `init`
/// still anchors a *git-backed* repo on the git toplevel (Forge layers on an existing git
/// repo); a *native* repo's root is established at init without git. Returns
/// `NotInitialized` when no `.forge/forge.db` is found up the tree.
fn repository_location(cwd: &Path) -> Result<(PathBuf, PathBuf, Option<String>)> {
    let start = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let mut current: &Path = &start;
    loop {
        if current.join(".forge/forge.db").exists() {
            return Ok((current.to_path_buf(), current.to_path_buf(), None));
        }
        let marker_path = current.join(forge_content::WORKSPACE_MARKER_FILE);
        if marker_path.exists() {
            let marker: WorkspaceMarker = serde_json::from_slice(
                &fs::read(&marker_path)
                    .map_err(|error| anyhow!("read workspace marker: {}", error.kind()))?,
            )
            .map_err(|_| anyhow!("workspace marker is corrupt"))?;
            let root = PathBuf::from(marker.repo_root);
            if !root.join(".forge/forge.db").exists() {
                return Err(ForgeError::NotInitialized.into());
            }
            return Ok((root, current.to_path_buf(), Some(marker.attempt_id)));
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return Err(ForgeError::NotInitialized.into()),
        }
    }
}

fn forge_root(cwd: &Path) -> Result<PathBuf> {
    repository_location(cwd).map(|(root, _, _)| root)
}

fn git_root(cwd: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--show-toplevel")
        .current_dir(cwd)
        .output()
        .context("failed to run git")?;

    if !output.status.success() {
        return Err(anyhow!(
            "forge init must run inside an existing Git repository"
        ));
    }

    let root = String::from_utf8(output.stdout)?.trim().to_string();
    if root.is_empty() {
        return Err(anyhow!("git returned an empty repository root"));
    }

    Ok(PathBuf::from(root))
}

fn git_head(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("--verify")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compare_row(
        attempt_id: &str,
        has_proposal: bool,
        integrity: &str,
        check_status: Option<&str>,
        tests_failed: Option<u64>,
        tests_passed: Option<u64>,
    ) -> AttemptCompareRow {
        compare_row_with_changes(
            attempt_id,
            has_proposal,
            integrity,
            check_status,
            tests_failed,
            tests_passed,
            0,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn compare_row_with_changes(
        attempt_id: &str,
        has_proposal: bool,
        integrity: &str,
        check_status: Option<&str>,
        tests_failed: Option<u64>,
        tests_passed: Option<u64>,
        changed_count: usize,
    ) -> AttemptCompareRow {
        AttemptCompareRow {
            attempt_id: attempt_id.to_string(),
            status: "active".to_string(),
            proposal: has_proposal.then(|| ComparedProposal {
                proposal_id: format!("prop_{attempt_id}"),
                proposal_revision_id: format!("rev_{attempt_id}"),
                snapshot_id: format!("snap_{attempt_id}"),
                base_head: "base".to_string(),
                content_ref: "git-tree:deadbeef".to_string(),
            }),
            changed_paths: Vec::new(),
            changed_count,
            gates: Vec::new(),
            check_status: check_status.map(str::to_string),
            metrics: StructuredMetrics {
                tests_passed,
                tests_failed,
                tests_ignored: None,
                clippy_findings: None,
            },
            integrity: integrity.to_string(),
            decision_status: None,
            publication_status: None,
            rank: None,
            rank_reason: String::new(),
        }
    }

    #[test]
    fn review_changed_paths_redact_local_private_labels() {
        let paths = vec![
            "src/public.rs".to_string(),
            "src/private.rs".to_string(),
            "docs/notes.md".to_string(),
        ];
        let labels = vec![LocalPrivatePathLabel {
            work_package_kind: "proposal".to_string(),
            work_package_id: "proposal_1".to_string(),
            path: "src/private.rs".to_string(),
            path_label_id: "path_label_1".to_string(),
            path_hash: "sha256:private".to_string(),
            visibility: "private".to_string(),
        }];

        let sanitized = crate::proposals::sanitize_review_changed_paths(&paths, &labels);

        assert_eq!(sanitized[0].path, "src/public.rs");
        assert_eq!(sanitized[0].status, "changed");
        assert_eq!(sanitized[1].path, "[restricted private path]");
        assert_eq!(sanitized[1].status, "restricted");
        assert_eq!(sanitized[2].path, "docs/notes.md");
    }

    #[test]
    fn rank_passing_attempt_above_failing() {
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("failed"),
                Some(2),
                Some(48),
            ),
            compare_row(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(50),
            ),
        ];
        rank_compare_rows(&mut rows);
        // The passing attempt ranks first regardless of input order.
        assert_eq!(rows[0].attempt_id, "b");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].attempt_id, "a");
        assert_eq!(rows[1].rank, Some(2));
    }

    #[test]
    fn rank_fewer_failures_first_within_tier() {
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(48),
            ),
            compare_row(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(50),
            ),
        ];
        rank_compare_rows(&mut rows);
        // Same failures (0), so more passing wins.
        assert_eq!(rows[0].attempt_id, "b");
    }

    #[test]
    fn tampered_attempt_is_unranked_and_placed_last() {
        let mut rows = vec![
            // The would-be winner by exit code, but tampered.
            compare_row(
                "a",
                true,
                INTEGRITY_TAMPERED,
                Some("passed"),
                Some(0),
                Some(99),
            ),
            // An honest but failing attempt.
            compare_row(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("failed"),
                Some(1),
                Some(10),
            ),
        ];
        rank_compare_rows(&mut rows);
        // The honest attempt is the rank-1 winner; the tampered one is unranked & last.
        assert_eq!(rows[0].attempt_id, "b");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].attempt_id, "a");
        assert_eq!(rows[1].rank, None);
        assert!(rows[1].rank_reason.contains("tampered"));
    }

    #[test]
    fn all_tampered_group_yields_no_rank_one() {
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_TAMPERED,
                Some("passed"),
                Some(0),
                Some(1),
            ),
            compare_row(
                "b",
                true,
                INTEGRITY_TAMPERED,
                Some("passed"),
                Some(0),
                Some(2),
            ),
        ];
        rank_compare_rows(&mut rows);
        // A numeric-min consumer cannot select a tampered attempt: no row has rank 1.
        assert!(rows.iter().all(|row| row.rank.is_none()));
    }

    #[test]
    fn no_proposal_attempt_is_unranked() {
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(5),
            ),
            compare_row("b", false, INTEGRITY_NO_EVIDENCE, None, None, None),
        ];
        rank_compare_rows(&mut rows);
        assert_eq!(rows[0].attempt_id, "a");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].rank, None);
        assert!(rows[1].rank_reason.contains("no proposal"));
    }

    #[test]
    fn ranking_is_a_stable_total_order() {
        // Two identical-metric rows keep input (created) order — deterministic.
        let build = || {
            vec![
                compare_row(
                    "a",
                    true,
                    INTEGRITY_VERIFIED,
                    Some("passed"),
                    Some(0),
                    Some(10),
                ),
                compare_row(
                    "b",
                    true,
                    INTEGRITY_VERIFIED,
                    Some("passed"),
                    Some(0),
                    Some(10),
                ),
            ]
        };
        let mut first = build();
        rank_compare_rows(&mut first);
        let mut second = build();
        rank_compare_rows(&mut second);
        let ids_first: Vec<_> = first.iter().map(|r| r.attempt_id.clone()).collect();
        let ids_second: Vec<_> = second.iter().map(|r| r.attempt_id.clone()).collect();
        assert_eq!(ids_first, ids_second);
        assert_eq!(ids_first, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn rank_verified_nonempty_above_no_evidence_empty_on_tied_gates() {
        // NER-256: when the gate/check status ties at non-"passed" (here both "missing"),
        // a zero-change, no_evidence attempt must NOT outrank a verified attempt with a
        // real diff just because it was created first. `a` is placed FIRST in input
        // (earlier created order) but is empty + no_evidence; `b` is verified + non-empty.
        // Before the tie-break the stable sort kept `a` first; now `b` ranks 1.
        let mut rows = vec![
            compare_row_with_changes(
                "a",
                true,
                INTEGRITY_NO_EVIDENCE,
                Some("missing"),
                None,
                None,
                0,
            ),
            compare_row_with_changes(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("missing"),
                None,
                None,
                3,
            ),
        ];
        rank_compare_rows(&mut rows);
        assert_eq!(rows[0].attempt_id, "b");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].attempt_id, "a");
        assert_eq!(rows[1].rank, Some(2));
        // The rank_reason must name the tie-break that ACTUALLY applied. Integrity is the
        // first field that differs (verified vs no_evidence), so it — not the diff — is the
        // deciding discriminator the reason should cite (NER-256 adversarial review: the old
        // code always claimed "verified evidence + non-empty diff" even when only one of
        // those fields differentiated the rows).
        assert!(
            rows[0].rank_reason.contains("gates tie"),
            "winner rank_reason should mention the gate tie-break: {}",
            rows[0].rank_reason
        );
        assert!(
            rows[0].rank_reason.contains("verified evidence"),
            "winner rank_reason should cite its verified-evidence advantage: {}",
            rows[0].rank_reason
        );
        // The loser's reason states why IT landed below: its no-evidence integrity, the
        // first field on which it lost to the verified winner.
        assert!(
            rows[1].rank_reason.contains("gates tie")
                && rows[1].rank_reason.contains("no-evidence integrity"),
            "loser rank_reason should state its no-evidence integrity: {}",
            rows[1].rank_reason
        );
    }

    #[test]
    fn rank_reason_names_diff_discriminator_when_integrity_ties_on_gate_tie() {
        // NER-256 adversarial review: when integrity AND gates tie, the diff size is the
        // real discriminator. The reason must cite the diff — NOT a fixed integrity label.
        let mut rows = vec![
            compare_row_with_changes(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("missing"),
                None,
                None,
                0,
            ),
            compare_row_with_changes(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("missing"),
                None,
                None,
                4,
            ),
        ];
        rank_compare_rows(&mut rows);
        assert_eq!(rows[0].attempt_id, "b");
        assert_eq!(rows[1].attempt_id, "a");
        assert!(
            rows[0].rank_reason.contains("non-empty diff"),
            "winner reason should cite its non-empty diff: {}",
            rows[0].rank_reason
        );
        assert!(
            rows[1].rank_reason.contains("empty diff"),
            "loser reason should cite its empty diff: {}",
            rows[1].rank_reason
        );
    }

    #[test]
    fn rank_reason_names_test_count_discriminator_when_integrity_and_diff_tie() {
        // NER-256 adversarial review: three non-passing attempts, all verified + non-empty,
        // differing ONLY in tests_failed. The discriminator is tests_failed — the reason
        // must say so, not the (identical, non-differentiating) integrity + diff labels.
        let mut rows = vec![
            compare_row_with_changes(
                "x",
                true,
                INTEGRITY_VERIFIED,
                Some("missing"),
                Some(0),
                Some(5),
                2,
            ),
            compare_row_with_changes(
                "y",
                true,
                INTEGRITY_VERIFIED,
                Some("missing"),
                Some(3),
                Some(5),
                2,
            ),
            compare_row_with_changes(
                "z",
                true,
                INTEGRITY_VERIFIED,
                Some("missing"),
                Some(10),
                Some(5),
                2,
            ),
        ];
        rank_compare_rows(&mut rows);
        assert_eq!(rows[0].attempt_id, "x");
        assert_eq!(rows[1].attempt_id, "y");
        assert_eq!(rows[2].attempt_id, "z");
        // The middle row lost to x on failing-test count, not integrity or diff.
        assert!(
            rows[1].rank_reason.contains("more failing tests"),
            "y's reason should cite the failing-test discriminator, not integrity/diff: {}",
            rows[1].rank_reason
        );
        assert!(
            rows[2].rank_reason.contains("more failing tests"),
            "z's reason should cite the failing-test discriminator: {}",
            rows[2].rank_reason
        );
        // The winner reads positively: it has the fewest failing tests.
        assert!(
            rows[0].rank_reason.contains("fewer failing tests"),
            "x's reason should cite its fewer-failing-tests advantage: {}",
            rows[0].rank_reason
        );
    }

    #[test]
    fn rank_reason_says_gates_not_satisfied_when_a_passing_attempt_outranks() {
        // NER-256 correctness review: when one attempt passes its gates, the non-passing
        // remainder did NOT tie on gates — they lost. Their reason must say "required gates
        // not satisfied", not "gates tie" (which would falsely claim equal gate status).
        let mut rows = vec![
            compare_row(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(5),
            ),
            compare_row(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("failed"),
                Some(2),
                Some(3),
            ),
            compare_row(
                "c",
                true,
                INTEGRITY_NO_EVIDENCE,
                Some("missing"),
                None,
                None,
            ),
        ];
        rank_compare_rows(&mut rows);
        assert_eq!(rows[0].attempt_id, "a");
        assert!(
            rows[0].rank_reason.contains("all required gates passing"),
            "passing attempt reason: {}",
            rows[0].rank_reason
        );
        for loser in &rows[1..] {
            assert!(
                loser.rank_reason.contains("required gates not satisfied"),
                "non-passing attempt must NOT claim a gate tie when a passing attempt exists: {}",
                loser.rank_reason
            );
            assert!(
                !loser.rank_reason.contains("gates tie"),
                "non-passing attempt must not claim a gate tie here: {}",
                loser.rank_reason
            );
        }
    }

    #[test]
    fn rank_reason_legacy_caveat_on_gate_tie() {
        // NER-256 testing review: a legacy_unverified attempt is rankable but its deciding
        // evidence was never hash-verified. On a gate tie it must (a) rank between verified
        // and no_evidence and (b) carry the legacy caveat in its rank_reason.
        let mut rows = vec![
            compare_row_with_changes(
                "v",
                true,
                INTEGRITY_VERIFIED,
                Some("missing"),
                None,
                None,
                1,
            ),
            compare_row_with_changes("l", true, INTEGRITY_LEGACY, Some("missing"), None, None, 1),
            compare_row_with_changes(
                "n",
                true,
                INTEGRITY_NO_EVIDENCE,
                Some("missing"),
                None,
                None,
                1,
            ),
        ];
        rank_compare_rows(&mut rows);
        assert_eq!(rows[0].attempt_id, "v");
        assert_eq!(rows[1].attempt_id, "l");
        assert_eq!(rows[2].attempt_id, "n");
        assert!(
            rows[1].rank_reason.contains("legacy_unverified"),
            "legacy attempt reason must carry the legacy caveat: {}",
            rows[1].rank_reason
        );
    }

    #[test]
    fn rank_tie_break_does_not_override_passing_gate() {
        // NER-256 guardrail: gates_passing stays the FIRST sort key, so a passing-gate
        // attempt outranks a non-passing one EVEN IF the non-passing one has stronger
        // integrity and a non-empty diff. Here `b` passes but is empty + (still verified),
        // while `a` is non-passing, verified, non-empty — `b` must still rank 1.
        let mut rows = vec![
            compare_row_with_changes(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("failed"),
                Some(1),
                Some(9),
                5,
            ),
            compare_row_with_changes(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(9),
                0,
            ),
        ];
        rank_compare_rows(&mut rows);
        assert_eq!(rows[0].attempt_id, "b");
        assert_eq!(rows[0].rank, Some(1));
        assert_eq!(rows[1].attempt_id, "a");
    }

    #[test]
    fn latest_selector_breaks_same_ms_ties_by_rowid() {
        // The nine "latest" selectors append `, rowid DESC` so rows sharing a
        // created_at_ms are returned in deterministic insertion order (highest rowid =
        // most recently inserted). Proven directly against SQLite, independent of the
        // multi-process coverage deferred to Phase 1b.
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE snapshots (id TEXT PRIMARY KEY, attempt_id TEXT, created_at_ms INTEGER);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO snapshots (id, attempt_id, created_at_ms) VALUES ('first', 'a', 100)",
                [],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO snapshots (id, attempt_id, created_at_ms) VALUES ('second', 'a', 100)",
                [],
            )
            .unwrap();
        let latest: String = connection
            .query_row(
                "SELECT id FROM snapshots WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                params!["a"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(latest, "second");
    }

    #[test]
    fn busy_classification_retries_only_busy_class_errors() {
        use rusqlite::ffi::Error as FfiError;

        // SQLITE_BUSY (5) and SQLITE_BUSY_SNAPSHOT (517) are the retryable cases.
        let busy = anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(5), None));
        assert!(is_retryable_busy(&busy), "plain SQLITE_BUSY must retry");

        let busy_snapshot =
            anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(517), None));
        assert!(
            is_retryable_busy(&busy_snapshot),
            "SQLITE_BUSY_SNAPSHOT (517) must retry"
        );

        // Detected even when wrapped further up the anyhow source chain.
        let wrapped = anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(517), None))
            .context("while committing writer txn");
        assert!(
            is_retryable_busy(&wrapped),
            "chain walk must find a busy cause"
        );

        // A constraint violation (e.g. the request-id unique index) is NOT retryable.
        let constraint =
            anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(19), None));
        assert!(
            !is_retryable_busy(&constraint),
            "SQLITE_CONSTRAINT must not retry"
        );

        // Non-SQLite errors, including the replay sentinel, are not retryable.
        assert!(!is_retryable_busy(&anyhow!("plain failure")));
        assert!(!is_retryable_busy(
            &RequestIdReplay {
                operation: RequestIdOperation {
                    operation_id: "op_x".to_string(),
                    command: "save".to_string(),
                    status: "succeeded".to_string(),
                    error_json: None,
                    kind: None,
                    view_id: None,
                    state: None,
                },
            }
            .into()
        ));
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    /// FIX D: `insert_operation_view`'s optimistic singleton CAS, when its captured
    /// parent operation no longer matches live `current_state`, raises the typed,
    /// retryable `ForgeError::CurrentStateChanged` (code `CONFLICT`) — NOT a plain
    /// `anyhow!`. This is what the CLI's `is_transient_error` keys on to skip
    /// recording the failure under the `--request-id`. Driven as a focused in-crate
    /// unit test because `insert_operation_view` is private and the production fns
    /// re-read `current_state` per call (so a stale parent can't be pinned through
    /// the public API alone).
    #[test]
    fn insert_operation_view_stale_parent_raises_current_state_changed() {
        let mut connection = Connection::open_in_memory().expect("open in-memory db");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("enable fks");
        connection
            .execute_batch(include_str!("../migrations/001_init.sql"))
            .expect("baseline schema");
        // Phase 5 adds operations.content_hash, which insert_operation_view now writes.
        connection
            .execute_batch(include_str!("../migrations/004_integrity_and_actor.sql"))
            .expect("phase 5 integrity columns");

        // Seed: a repo, a genesis operation+view that `current_state` points at, and
        // a SECOND operation row (`op_stale`) that does NOT match current_state.
        connection
            .execute_batch(
                "INSERT INTO repositories (id, root_path, created_at_ms)
                     VALUES ('repo_1', '/tmp/repo', 0);
                 INSERT INTO operations (id, repo_id, command, status, kind, created_at_ms)
                     VALUES ('op_genesis', 'repo_1', 'init', 'succeeded', 'init', 0);
                 INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
                     VALUES ('view_genesis', 'repo_1', 'op_genesis', 'initialized', '{}', 0);
                 INSERT INTO current_state
                     (singleton, repo_id, current_operation_id, current_view_id, updated_at_ms)
                     VALUES (1, 'repo_1', 'op_genesis', 'view_genesis', 0);
                 INSERT INTO operations (id, repo_id, command, status, kind, created_at_ms)
                     VALUES ('op_stale', 'repo_1', 'save', 'succeeded', 'save', 0);",
            )
            .expect("seed rows");

        let error = with_immediate_retry(&mut connection, |tx| {
            // Pass `op_stale` as the parent: it exists (satisfies the FK) but does
            // NOT equal current_state.current_operation_id (`op_genesis`), so the
            // CAS `WHERE current_operation_id = 'op_stale'` updates zero rows.
            insert_operation_view(
                tx,
                "repo_1",
                Some("op_stale"),
                OperationViewInput {
                    request_id: None,
                    command: "save".to_string(),
                    kind: "snapshot_saved".to_string(),
                    view_kind: ViewKind::Initialized,
                    state: json!({ "lifecycle": "test" }),
                },
            )
        })
        .expect_err("a stale parent must lose the CAS");

        let forge_error = error
            .downcast_ref::<ForgeError>()
            .expect("the CAS failure is a typed ForgeError");
        assert_eq!(*forge_error, ForgeError::CurrentStateChanged);
        assert_eq!(forge_error.code(), "CONFLICT");
        assert!(forge_error.retryable());
        assert_eq!(forge_error.after_ms(), Some(50));
    }

    #[test]
    fn record_conflict_set_redacts_secret_paths() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "forge@example.test"]);
        run_git(root, &["config", "user.name", "Forge Test"]);
        fs::write(root.join("README.md"), "hello\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(root, &["commit", "-m", "initial"]);

        init_repository(root, None, "git".to_string()).expect("init repository");

        let paths = vec![
            "src/main.rs".to_string(),
            ".env".to_string(),
            "k.pem".to_string(),
        ];
        let id = record_conflict_set(root, "stale_base_accept", "HEAD0", "HEAD1", &paths)
            .expect("record conflict set");
        assert!(id.starts_with("conflict_"), "unexpected id: {id}");

        let database_path = root.join(".forge/forge.db");
        let connection = Connection::open(&database_path).expect("open db");
        let (context, paths_json): (String, String) = connection
            .query_row(
                "SELECT context, paths_json FROM conflict_sets WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query conflict row");

        assert_eq!(context, "stale_base_accept");
        let value: Value = serde_json::from_str(&paths_json).expect("parse paths_json");
        assert_eq!(value["expected_head"], "HEAD0");
        assert_eq!(value["actual_head"], "HEAD1");
        assert_eq!(value["redacted_count"], 2);
        assert!(
            paths_json.contains("src/main.rs"),
            "non-secret path must be kept: {paths_json}"
        );
        assert!(
            !paths_json.contains(".env"),
            "secret-risk path leaked: {paths_json}"
        );
        assert!(
            !paths_json.contains("k.pem"),
            "secret-risk path leaked: {paths_json}"
        );
    }

    /// NER-257: `intent_detail` parses the declared gate spec (program, args, and the
    /// `require_structured_pass` → `structured` flag) and links the started attempt id;
    /// an unknown id raises the typed `UnknownIntent`.
    #[test]
    fn intent_detail_returns_gates_and_attempts() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "forge@example.test"]);
        run_git(root, &["config", "user.name", "Forge Test"]);
        fs::write(root.join("README.md"), "hello\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(root, &["commit", "-m", "initial"]);
        init_repository(root, None, "git".to_string()).expect("init repository");

        let check_spec_json = check_spec_json_from_requires(
            &["cargo test".to_string()],
            &["cargo clippy".to_string()],
        );
        let started = start_attempt(
            root,
            None,
            "two gates".to_string(),
            "HEAD0".to_string(),
            check_spec_json,
        )
        .expect("start attempt");

        let detail = intent_detail(root, &started.intent_id).expect("intent detail");
        assert_eq!(detail.intent_id, started.intent_id);
        assert_eq!(detail.title, "two gates");
        assert_eq!(detail.status, "open");
        assert_eq!(detail.gates.len(), 2);

        let plain = detail
            .gates
            .iter()
            .find(|gate| gate.args == ["test"])
            .expect("plain gate");
        assert_eq!(plain.program, "cargo");
        assert!(!plain.structured);
        let structured = detail
            .gates
            .iter()
            .find(|gate| gate.args == ["clippy"])
            .expect("structured gate");
        assert!(structured.structured);

        assert_eq!(detail.attempt_ids, vec![started.attempt_id.clone()]);

        let listed = intents_list(root).expect("intents list");
        assert!(listed
            .iter()
            .any(|intent| intent.intent_id == started.intent_id));

        let err = intent_detail(root, "intent_missing").expect_err("unknown intent errors");
        let typed = err.downcast_ref::<ForgeError>().expect("typed ForgeError");
        assert_eq!(typed.code(), "UNKNOWN_INTENT");
    }

    /// NER-257 secret-safety: a secret-like `key=value` gate token in the stored
    /// `check_spec_json` is redacted before `intent_detail` egress (the stored spec is
    /// raw), mirroring the check-surface `redact_gate_result` pass.
    #[test]
    fn intent_detail_redacts_secret_like_gate_tokens() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "forge@example.test"]);
        run_git(root, &["config", "user.name", "Forge Test"]);
        fs::write(root.join("README.md"), "hello\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(root, &["commit", "-m", "initial"]);
        init_repository(root, None, "git".to_string()).expect("init repository");

        let check_spec_json =
            check_spec_json_from_requires(&["deploy --token=ghp_supersecret".to_string()], &[]);
        let started = start_attempt(
            root,
            None,
            "secret gate".to_string(),
            "HEAD0".to_string(),
            check_spec_json,
        )
        .expect("start attempt");

        let detail = intent_detail(root, &started.intent_id).expect("intent detail");
        let serialized = serde_json::to_string(&detail.gates).expect("serialize gates");
        assert!(
            !serialized.contains("ghp_supersecret"),
            "secret-like gate token must be redacted: {serialized}"
        );
        assert!(serialized.contains("[REDACTED]"));
    }

    #[test]
    fn visibility_defaults_grants_revocation_and_projection_decisions() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "forge@example.test"]);
        run_git(root, &["config", "user.name", "Forge Test"]);
        fs::write(root.join("README.md"), "hello\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(root, &["commit", "-m", "initial"]);
        init_repository(root, None, "git".to_string()).expect("init repository");

        let policy = visibility_policy(root).expect("visibility policy");
        assert_eq!(policy.default_work_package_visibility, "public");
        assert!(policy
            .supported_capabilities
            .contains(&"sync_materialize".to_string()));

        let started = start_attempt(
            root,
            None,
            "private extension".to_string(),
            "HEAD0".to_string(),
            None,
        )
        .expect("start attempt");

        let public = projection_decision(
            root,
            "attempt",
            &started.attempt_id,
            "reviewer@example.test",
            "sync_materialize",
        )
        .expect("public projection decision");
        assert!(public.allowed);
        assert_eq!(public.disclosure, "full");

        set_work_package_visibility(
            root,
            "attempt",
            &started.attempt_id,
            "private",
            "maintainer",
            Some("invite-only review"),
        )
        .expect("set private");

        let hidden = projection_decision(
            root,
            "attempt",
            &started.attempt_id,
            "reviewer@example.test",
            "sync_materialize",
        )
        .expect("hidden projection decision");
        assert!(!hidden.allowed);
        assert_eq!(hidden.visibility, "private");
        assert_eq!(hidden.disclosure, "hidden");

        grant_visibility_capability(
            root,
            "attempt",
            &started.attempt_id,
            "reviewer@example.test",
            "see_stub",
            "maintainer",
            Some("coordination"),
        )
        .expect("grant stub");
        let stub = projection_decision(
            root,
            "attempt",
            &started.attempt_id,
            "reviewer@example.test",
            "sync_materialize",
        )
        .expect("stub projection decision");
        assert!(!stub.allowed);
        assert_eq!(stub.disclosure, "stub");

        grant_visibility_capability(
            root,
            "attempt",
            &started.attempt_id,
            "reviewer@example.test",
            "sync_materialize",
            "maintainer",
            Some("private review"),
        )
        .expect("grant materialize");
        let allowed = projection_decision(
            root,
            "attempt",
            &started.attempt_id,
            "reviewer@example.test",
            "sync_materialize",
        )
        .expect("allowed projection decision");
        assert!(allowed.allowed);
        assert_eq!(allowed.disclosure, "full");

        revoke_visibility_capability(
            root,
            "attempt",
            &started.attempt_id,
            "reviewer@example.test",
            "sync_materialize",
            "maintainer",
            Some("review complete"),
        )
        .expect("revoke materialize");
        let revoked = projection_decision(
            root,
            "attempt",
            &started.attempt_id,
            "reviewer@example.test",
            "sync_materialize",
        )
        .expect("revoked projection decision");
        assert!(!revoked.allowed);
        assert_eq!(revoked.disclosure, "stub");

        let database_path = root.join(".forge/forge.db");
        let connection = Connection::open(database_path).expect("open db");
        let audit_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM visibility_audit", [], |row| {
                row.get(0)
            })
            .expect("audit count");
        assert_eq!(audit_count, 4);
    }

    #[test]
    fn attempts_from_private_intents_and_proposals_inherit_visibility() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "forge@example.test"]);
        run_git(root, &["config", "user.name", "Forge Test"]);
        fs::write(root.join("README.md"), "hello\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(root, &["commit", "-m", "initial"]);
        init_repository(root, None, "git".to_string()).expect("init repository");

        let first = start_attempt(
            root,
            None,
            "private parent".to_string(),
            "HEAD0".to_string(),
            None,
        )
        .expect("start first attempt");
        set_work_package_visibility(
            root,
            "intent",
            &first.intent_id,
            "private",
            "maintainer",
            Some("private line of work"),
        )
        .expect("set intent private");

        let second =
            start_attempt_for_intent(root, None, first.intent_id.clone(), "HEAD0".to_string())
                .expect("start attempt for private intent");
        let second_decision = projection_decision(
            root,
            "attempt",
            &second.attempt_id,
            "outsider@example.test",
            "inspect_content",
        )
        .expect("second attempt decision");
        assert_eq!(second_decision.visibility, "private");
        assert!(!second_decision.allowed);

        attach_attempt(root, None, &second.attempt_id, "git-tree:test-private")
            .expect("attach second attempt");
        save_snapshot(
            root,
            None,
            Some(&second.attempt_id),
            "git-tree:test-private".to_string(),
            vec!["README.md".to_string()],
        )
        .expect("save snapshot");
        let proposal = propose(root, None, Some(&second.attempt_id), None).expect("propose");
        let proposal_decision = projection_decision(
            root,
            "proposal",
            &proposal.proposal_id,
            "outsider@example.test",
            "inspect_content",
        )
        .expect("proposal decision");
        assert_eq!(proposal_decision.visibility, "private");
        assert!(!proposal_decision.allowed);
    }

    #[test]
    fn private_decrypt_authority_requires_grant_and_active_encryption_key() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "forge@example.test"]);
        run_git(root, &["config", "user.name", "Forge Test"]);
        fs::write(root.join("README.md"), "hello\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(root, &["commit", "-m", "initial"]);
        init_repository(root, None, "git".to_string()).expect("init repository");

        let org =
            init_org_governance(root, None, "maintainer", Some("bootstrap org")).expect("init org");
        let started = start_attempt(
            root,
            None,
            "private extension".to_string(),
            "HEAD0".to_string(),
            None,
        )
        .expect("start attempt");
        set_work_package_visibility(
            root,
            "attempt",
            &started.attempt_id,
            "private",
            &org.owner_actor_id,
            Some("private review"),
        )
        .expect("set private");

        let missing_grant =
            private_decrypt_authority(root, "attempt", &started.attempt_id, &org.owner_actor_id)
                .expect_err("grant is required");
        assert_eq!(
            missing_grant
                .downcast_ref::<ForgeError>()
                .expect("typed error")
                .code(),
            "PRIVATE_DECRYPT_AUTHORITY_MISSING"
        );

        grant_visibility_capability(
            root,
            "attempt",
            &started.attempt_id,
            &org.owner_actor_id,
            "sync_materialize",
            &org.owner_actor_id,
            Some("review private path"),
        )
        .expect("grant materialize");
        let missing_key =
            private_decrypt_authority(root, "attempt", &started.attempt_id, &org.owner_actor_id)
                .expect_err("encryption key is required");
        assert_eq!(
            missing_key
                .downcast_ref::<ForgeError>()
                .expect("typed error")
                .details()["reason"],
            "missing_active_encryption_key"
        );

        let identity = forge_private::EncryptionIdentity::generate();
        let binding = bind_org_encryption_key(
            root,
            &org.owner_actor_id,
            identity.recipient().as_str(),
            &org.owner_actor_id,
            Some("bind owner encryption key"),
        )
        .expect("bind encryption key");
        let authority =
            private_decrypt_authority(root, "attempt", &started.attempt_id, &org.owner_actor_id)
                .expect("decrypt authority");
        assert_eq!(authority.principal_id, org.owner_actor_id);
        assert_eq!(authority.key_fingerprint, binding.key_fingerprint);

        let database_path = root.join(".forge/forge.db");
        let connection = Connection::open(database_path).expect("open db");
        connection
            .execute(
                "UPDATE org_encryption_key_bindings
                 SET state = 'revoked', revocation_reason = 'rotated'
                 WHERE key_fingerprint = ?1",
                params![binding.key_fingerprint],
            )
            .expect("revoke key");
        let revoked =
            private_decrypt_authority(root, "attempt", &started.attempt_id, &org.owner_actor_id)
                .expect_err("revoked key fails closed");
        assert_eq!(
            revoked
                .downcast_ref::<ForgeError>()
                .expect("typed error")
                .details()["reason"],
            "missing_active_encryption_key"
        );
    }

    #[test]
    fn private_overlay_rows_do_not_store_plaintext_path_names() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        run_git(root, &["init"]);
        run_git(root, &["config", "user.email", "forge@example.test"]);
        run_git(root, &["config", "user.name", "Forge Test"]);
        fs::write(root.join("README.md"), "hello\n").expect("write readme");
        run_git(root, &["add", "README.md"]);
        run_git(root, &["commit", "-m", "initial"]);
        let repo = init_repository(root, None, "git".to_string()).expect("init repository");
        let started = start_attempt(
            root,
            None,
            "private extension".to_string(),
            "HEAD0".to_string(),
            None,
        )
        .expect("start attempt");

        let private_path = "src/private_ext.rs";
        let path_hash = scoped_private_path_hash(
            &repo.repository_id,
            "attempt",
            &started.attempt_id,
            private_path,
        );
        let label = record_private_path_label(
            root,
            "attempt",
            &started.attempt_id,
            &path_hash,
            "age-envelope-for-display-path",
            "private",
        )
        .expect("record label");
        assert_ne!(label.path_hash, private_path);
        assert!(!label.encrypted_display_path.contains(private_path));

        let payload = record_encrypted_private_payload(
            root,
            EncryptedPrivatePayloadInput {
                work_package_kind: "attempt".to_string(),
                work_package_id: started.attempt_id.clone(),
                snapshot_id: None,
                path_label_id: label.path_label_id.clone(),
                path_hash: path_hash.clone(),
                envelope_format: forge_private::ENVELOPE_FORMAT_AGE_X25519_V1.to_string(),
                recipient_fingerprint: "age-x25519:recipient".to_string(),
                ciphertext_digest: "a".repeat(64),
                private_object_path: ".forge/private/objects/sha256/aa".to_string(),
                encrypted_metadata_json: "{\"encrypted\":true}".to_string(),
            },
        )
        .expect("record payload");
        assert_eq!(payload.path_hash, path_hash);

        let database_path = root.join(".forge/forge.db");
        let connection = Connection::open(database_path).expect("open db");
        let rows_json: String = connection
            .query_row(
                "SELECT json_group_array(json_object(
                    'path_hash', path_hash,
                    'encrypted_display_path', encrypted_display_path
                )) FROM private_path_labels",
                [],
                |row| row.get(0),
            )
            .expect("query labels");
        let payloads_json: String = connection
            .query_row(
                "SELECT json_group_array(json_object(
                    'path_hash', path_hash,
                    'private_object_path', private_object_path,
                    'encrypted_metadata_json', encrypted_metadata_json
                )) FROM encrypted_private_payloads",
                [],
                |row| row.get(0),
            )
            .expect("query payloads");
        assert!(
            !rows_json.contains(private_path),
            "private label row leaked path: {rows_json}"
        );
        assert!(
            !payloads_json.contains(private_path),
            "private payload row leaked path: {payloads_json}"
        );
    }
}
