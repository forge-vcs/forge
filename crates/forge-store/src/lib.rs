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

mod embargo;
mod error;
mod integrity;
mod migrations;
mod repo_lock;
mod signing;
mod storage;
mod sync;
mod visibility;
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
pub use repo_lock::{LockTimeout, RepoLock};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceMarker {
    repo_root: String,
    attempt_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartAttempt {
    pub intent_id: String,
    pub attempt_id: String,
    pub base_head: String,
    pub attached: bool,
    pub workspace_path: String,
    pub operation_id: String,
    pub current_view_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub attempt_id: String,
    pub intent_id: String,
    pub intent: String,
    pub base_head: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptSummary {
    pub attempt_id: String,
    pub intent_id: String,
    pub intent: String,
    pub base_head: String,
    pub status: String,
    pub attached: bool,
    pub workspace_path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptShowRecord {
    pub attempt: AttemptSummary,
    pub latest_snapshot: Option<SnapshotSummary>,
    pub latest_evidence: Option<EvidenceSummary>,
    pub proposals: Vec<ProposalMetadata>,
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
pub struct SnapshotRecord {
    pub snapshot_id: String,
    pub attempt_id: String,
    pub parent_snapshot_id: Option<String>,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
    pub operation_id: String,
    pub current_view_id: String,
}

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
pub struct PublicationRecord {
    pub publication_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub branch_name: String,
    pub commit_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrustPolicy {
    pub min_accept_trust: String,
    pub min_export_trust: String,
    pub supported_trust_levels: Vec<String>,
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
pub struct OrgEncryptionKeyBindingRecord {
    pub binding_id: String,
    pub principal_id: String,
    pub key_fingerprint: String,
    pub public_key: String,
    pub binding_authority: String,
    pub state: String,
    pub valid_from_revision: i64,
    pub valid_until_revision: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivatePathLabelRecord {
    pub path_label_id: String,
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path_hash: String,
    pub encrypted_display_path: String,
    pub visibility: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPrivatePathLabel {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub visibility: String,
}

#[derive(Debug, Clone)]
pub struct EncryptedPrivatePayloadInput {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub snapshot_id: Option<String>,
    pub path_label_id: String,
    pub path_hash: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub private_object_path: String,
    pub encrypted_metadata_json: String,
}

#[derive(Debug, Clone)]
pub struct SaveSnapshotPrivateOverlayInput {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub private_object_path: String,
    pub encrypted_metadata_json: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EncryptedPrivatePayloadRecord {
    pub payload_id: String,
    pub work_package_kind: String,
    pub work_package_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    pub path_label_id: String,
    pub path_hash: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub private_object_path: String,
    pub encrypted_metadata_json: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateDecryptAuthority {
    pub principal_id: String,
    pub key_fingerprint: String,
    pub public_key: String,
    pub recipient_fingerprint: String,
    pub policy_revision: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalEncryptedPrivateObject {
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub private_object_path: String,
}

#[derive(Debug, Clone)]
pub struct PrivateOverlayTransportRecord {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub snapshot_id: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub path: String,
    pub visibility: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PrivateOverlayMaterializeInput {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub path: String,
    pub visibility: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct MaterializedPrivateOverlay {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub path: String,
    pub visibility: String,
    pub plaintext: Vec<u8>,
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

#[derive(Debug, Clone)]
pub struct StaleBaseConflictInput {
    pub context: String,
    pub expected_head: String,
    pub actual_head: String,
    pub base_content_ref: String,
    pub ours_content_ref: String,
    pub theirs_content_ref: String,
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StaleBaseConflict {
    pub input: StaleBaseConflictInput,
}

#[derive(Debug, Clone)]
pub struct MergeConflictInput {
    pub context: String,
    pub proposal_id: Option<String>,
    pub base_head: Option<String>,
    pub ours_head: Option<String>,
    pub base_content_ref: String,
    pub ours_content_ref: String,
    pub theirs_content_ref: String,
    pub conflicts: Vec<forge_content_native::NativeMergeConflict>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MergeConflictRecord {
    pub conflict_set_id: String,
    pub operation_id: String,
    pub view_id: String,
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
pub struct ConflictResolutionRecord {
    pub conflict_set_id: String,
    pub proposal_id: String,
    pub proposal_revision_id: String,
    pub snapshot_id: String,
    pub evidence_id: String,
    pub resolution_ref: String,
    pub operation_id: String,
    pub view_id: String,
}

impl StaleBaseConflict {
    pub fn forge_error(&self) -> ForgeError {
        ForgeError::StaleBase {
            expected_head: self.input.expected_head.clone(),
            actual_head: self.input.actual_head.clone(),
        }
    }
}

impl std::fmt::Display for StaleBaseConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.forge_error().fmt(f)
    }
}

impl std::error::Error for StaleBaseConflict {}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictListRecord {
    pub conflicts: Vec<ConflictSetSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictShowRecord {
    pub conflict: ConflictSetSummary,
    pub path_conflicts: Vec<PathConflictSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<ConflictResolutionSuggestion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictSetSummary {
    pub conflict_set_id: String,
    pub context: String,
    pub base_content_ref: Option<String>,
    pub ours_content_ref: Option<String>,
    pub theirs_content_ref: Option<String>,
    pub generated_by_operation_id: Option<String>,
    pub resolver_backend: Option<String>,
    pub status: String,
    pub path_conflict_count: i64,
    pub redacted_count: i64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathConflictSummary {
    pub path_conflict_id: String,
    pub path_fingerprint: String,
    pub kind: String,
    pub base_ref: Option<String>,
    pub ours_ref: Option<String>,
    pub theirs_ref: Option<String>,
    pub base_status: Option<String>,
    pub ours_status: Option<String>,
    pub theirs_status: Option<String>,
    pub base_mode: Option<String>,
    pub ours_mode: Option<String>,
    pub theirs_mode: Option<String>,
    pub resolution_ref: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictResolutionSuggestion {
    pub suggestion_id: String,
    pub rank: i64,
    pub resolution_ref: String,
    pub strategy: String,
    pub confidence: String,
    pub requires_explicit_resolve: bool,
    pub provenance: ConflictResolutionSuggestionProvenance,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictResolutionSuggestionProvenance {
    pub conflict_set_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_revision_id: Option<String>,
    pub evidence_input_count: i64,
    pub evidence_input_status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence_input_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_input_status: Option<String>,
    pub intent_input_status: String,
    pub path_conflict_ids: Vec<String>,
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct ConflictSuggestionInputs {
    proposal_id: Option<String>,
    proposal_revision_id: Option<String>,
    evidence_input_ids: Vec<String>,
    check_input_status: Option<String>,
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
pub struct SnapshotSummary {
    pub snapshot_id: String,
    pub content_ref: String,
    pub changed_paths: Vec<String>,
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
pub struct ResolvedAttempt {
    pub attempt: AttemptRecord,
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

const TRUST_SELF_REPORTED: &str = "self_reported";
const TRUST_LOCALLY_OBSERVED: &str = "locally_observed";
const TRUST_LOCALLY_SIGNED: &str = "locally_signed";
const TRUST_HOSTED_RUNNER_OBSERVED: &str = "hosted_runner_observed";
const TRUST_HOSTED_RUNNER_SIGNED: &str = "hosted_runner_signed";
const TRUST_THIRD_PARTY_ATTESTED: &str = "third_party_attested";
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

pub fn scoped_private_path_hash(
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    path: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"forge-private-path-v1-unkeyed-deprecated\n");
    hasher.update(repo_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(work_package_kind.as_bytes());
    hasher.update(b"\n");
    hasher.update(work_package_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(path.as_bytes());
    format!("sha256:{}", hex_bytes(&hasher.finalize()))
}

pub fn keyed_private_path_hash(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    path: &str,
) -> Result<String> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let key = local_private_path_hash_key(&context)?;
    let hmac_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
    let mut payload = Vec::new();
    payload.extend_from_slice(b"forge-private-path-v2\n");
    payload.extend_from_slice(context.repo_id.as_bytes());
    payload.extend_from_slice(b"\n");
    payload.extend_from_slice(work_package_kind.as_bytes());
    payload.extend_from_slice(b"\n");
    payload.extend_from_slice(work_package_id.as_bytes());
    payload.extend_from_slice(b"\n");
    payload.extend_from_slice(path.as_bytes());
    let tag = ring::hmac::sign(&hmac_key, &payload);
    Ok(format!("hmac-sha256:{}", hex_bytes(tag.as_ref())))
}

pub fn bind_org_encryption_key(
    cwd: &Path,
    principal_id: &str,
    public_key: &str,
    binding_authority: &str,
    reason: Option<&str>,
) -> Result<OrgEncryptionKeyBindingRecord> {
    let recipient =
        EncryptionRecipient::parse(public_key).map_err(|_| ForgeError::PrivateContentInvalid {
            reason: "invalid_encryption_recipient".to_string(),
        })?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        let org = org_status_on(tx, &context.repo_id)?;
        if !org.enabled {
            return Err(ForgeError::OrgNotEnabled.into());
        }
        ensure_active_org_principal(tx, &context.repo_id, principal_id)?;
        ensure_active_org_principal(tx, &context.repo_id, binding_authority)?;
        ensure_active_org_role(
            tx,
            &context.repo_id,
            binding_authority,
            &["owner", "maintainer"],
        )?;

        let now = now_ms();
        let binding_id = new_id("org_enc_key");
        let key_fingerprint = recipient.fingerprint().to_string();
        tx.execute(
            "INSERT INTO org_encryption_key_bindings (
                id, repo_id, principal_id, key_fingerprint, public_key, binding_authority,
                state, valid_from_revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?8)
             ON CONFLICT(repo_id, key_fingerprint)
             DO UPDATE SET
                principal_id = excluded.principal_id,
                public_key = excluded.public_key,
                binding_authority = excluded.binding_authority,
                state = 'active',
                valid_from_revision = excluded.valid_from_revision,
                valid_until_revision = NULL,
                revocation_reason = NULL,
                updated_at_ms = excluded.updated_at_ms",
            params![
                binding_id,
                context.repo_id,
                principal_id,
                key_fingerprint,
                public_key,
                binding_authority,
                org.policy_revision,
                now,
            ],
        )?;
        insert_private_content_audit(
            tx,
            &context.repo_id,
            None,
            None,
            None,
            None,
            Some(principal_id),
            Some(&key_fingerprint),
            "bind_encryption_key",
            reason,
            now,
        )?;
        org_encryption_key_binding_on(tx, &context.repo_id, &key_fingerprint)?
            .ok_or_else(|| anyhow!("org encryption key binding missing after insert"))
    })
}

pub fn record_private_path_label(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    path_hash: &str,
    encrypted_display_path: &str,
    visibility: &str,
) -> Result<PrivatePathLabelRecord> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_label(visibility)?;
    validate_private_hash(path_hash)?;
    if encrypted_display_path.trim().is_empty() {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "empty_encrypted_display_path".to_string(),
        }
        .into());
    }
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let now = now_ms();
        let label_id = new_id("private_path");
        tx.execute(
            "INSERT INTO private_path_labels (
                id, repo_id, work_package_kind, work_package_id, path_hash,
                encrypted_display_path, visibility, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(repo_id, work_package_kind, work_package_id, path_hash)
             DO UPDATE SET
                encrypted_display_path = excluded.encrypted_display_path,
                visibility = excluded.visibility,
                updated_at_ms = excluded.updated_at_ms",
            params![
                label_id,
                context.repo_id,
                work_package_kind,
                work_package_id,
                path_hash,
                encrypted_display_path,
                visibility,
                now,
            ],
        )?;
        private_path_label_on(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            path_hash,
        )?
        .ok_or_else(|| anyhow!("private path label missing after insert"))
    })
}

pub fn set_local_private_path_label(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    path: &str,
    visibility: &str,
) -> Result<PrivatePathLabelRecord> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_label(visibility)?;
    if visibility == VISIBILITY_PUBLIC {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "public_private_path_label".to_string(),
        }
        .into());
    }
    let context = open_repository(cwd)?;
    let path = normalize_private_label_path(path)?;
    let path_hash = keyed_private_path_hash(cwd, work_package_kind, work_package_id, &path)?;
    let label = record_private_path_label(
        cwd,
        work_package_kind,
        work_package_id,
        &path_hash,
        &format!("local-private-path:{path_hash}"),
        visibility,
    )?;
    let mut labels = read_local_private_path_labels(&context)?;
    labels.retain(|existing| {
        !(existing.work_package_kind == work_package_kind
            && existing.work_package_id == work_package_id
            && (existing.path_hash == path_hash || existing.path == path))
    });
    labels.push(LocalPrivatePathLabel {
        work_package_kind: work_package_kind.to_string(),
        work_package_id: work_package_id.to_string(),
        path,
        path_label_id: label.path_label_id.clone(),
        path_hash,
        visibility: visibility.to_string(),
    });
    write_local_private_path_labels(&context, &labels)?;
    Ok(label)
}

pub fn local_private_path_labels(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<Vec<LocalPrivatePathLabel>> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let labels = read_local_private_path_labels(&context)?;
    if labels.is_empty()
        && private_path_label_count(&context, work_package_kind, work_package_id)? > 0
    {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "missing_local_private_path_labels".to_string(),
        }
        .into());
    }
    Ok(labels
        .into_iter()
        .filter(|label| {
            label.work_package_kind == work_package_kind && label.work_package_id == work_package_id
        })
        .collect())
}

pub fn local_private_path_exclusions(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<Vec<String>> {
    let labels = local_private_path_labels(cwd, work_package_kind, work_package_id)?;
    for label in &labels {
        let full_path = cwd.join(&label.path);
        if !full_path.is_file() {
            return Err(ForgeError::PrivateContentInvalid {
                reason: "private_path_not_regular_file".to_string(),
            }
            .into());
        }
    }
    Ok(labels.into_iter().map(|label| label.path).collect())
}

pub fn capture_local_private_overlays(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<Vec<SaveSnapshotPrivateOverlayInput>> {
    let labels = local_private_path_labels(cwd, work_package_kind, work_package_id)?;
    let mut overlays = Vec::with_capacity(labels.len());
    for label in labels {
        let full_path = cwd.join(&label.path);
        if !full_path.is_file() {
            return Err(ForgeError::PrivateContentInvalid {
                reason: "private_path_not_regular_file".to_string(),
            }
            .into());
        }
        let plaintext = fs::read(&full_path).with_context(|| "read private path payload")?;
        let encrypted = encrypt_private_payload_to_local_store(cwd, &plaintext)?;
        overlays.push(SaveSnapshotPrivateOverlayInput {
            work_package_kind: label.work_package_kind,
            work_package_id: label.work_package_id,
            path_label_id: label.path_label_id,
            path_hash: label.path_hash,
            envelope_format: encrypted.envelope_format,
            recipient_fingerprint: encrypted.recipient_fingerprint,
            ciphertext_digest: encrypted.ciphertext_digest,
            private_object_path: encrypted.private_object_path,
            encrypted_metadata_json: "{}".to_string(),
        });
    }
    Ok(overlays)
}

pub fn record_encrypted_private_payload(
    cwd: &Path,
    input: EncryptedPrivatePayloadInput,
) -> Result<EncryptedPrivatePayloadRecord> {
    validate_work_package_kind(&input.work_package_kind)?;
    validate_private_hash(&input.path_hash)?;
    validate_private_hash(&input.ciphertext_digest)?;
    if input.envelope_format.trim().is_empty()
        || input.recipient_fingerprint.trim().is_empty()
        || input.private_object_path.trim().is_empty()
        || input.encrypted_metadata_json.trim().is_empty()
    {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "empty_private_payload_metadata".to_string(),
        }
        .into());
    }

    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        insert_encrypted_private_payload_on(tx, &context.repo_id, input.clone(), now_ms())
    })
}

pub fn private_decrypt_authority(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    principal_id: &str,
) -> Result<PrivateDecryptAuthority> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    ensure_work_package_exists(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
    )?;
    private_decrypt_authority_on(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
        principal_id,
    )
}

pub fn encrypt_private_payload_to_local_store(
    cwd: &Path,
    plaintext: &[u8],
) -> Result<LocalEncryptedPrivateObject> {
    let context = open_repository(cwd)?;
    let identity = local_encryption_identity(&context)?;
    let recipient = identity.recipient();
    let encrypted =
        forge_private::encrypt_private_payload(&recipient, plaintext).map_err(|_| {
            ForgeError::PrivateContentInvalid {
                reason: "private_payload_encrypt_failed".to_string(),
            }
        })?;
    let relative_path = PathBuf::from(".forge")
        .join("private")
        .join("objects")
        .join("sha256")
        .join(&encrypted.ciphertext_digest);
    let object_path = context.root_path.join(&relative_path);
    write_private_object_durable(&object_path, &encrypted.ciphertext)?;
    Ok(LocalEncryptedPrivateObject {
        envelope_format: encrypted.envelope_format,
        recipient_fingerprint: encrypted.recipient_fingerprint,
        ciphertext_digest: encrypted.ciphertext_digest,
        private_object_path: relative_path.to_string_lossy().replace('\\', "/"),
    })
}

fn write_private_object_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("private object path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| "create private object dir")?;
    signing::set_private_dir_permissions(parent)?;
    let temp_path = parent.join(format!(
        ".tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("private-object")
    ));
    {
        let mut file =
            fs::File::create(&temp_path).with_context(|| "create private object temp")?;
        use std::io::Write as _;
        file.write_all(bytes)
            .with_context(|| "write private ciphertext temp")?;
        file.sync_all()
            .with_context(|| "fsync private ciphertext")?;
    }
    signing::set_private_file_permissions(&temp_path)?;
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        if !path.exists() {
            return Err(error).with_context(|| "install private ciphertext object");
        }
    }
    fsync_dir(parent).with_context(|| "fsync private object dir")?;
    Ok(())
}

fn read_private_object(context: &RepositoryContext, relative_path: &str) -> Result<Vec<u8>> {
    let relative = normalize_private_object_path(relative_path)?;
    fs::read(context.root_path.join(relative)).with_context(|| "read private ciphertext object")
}

fn normalize_private_object_path(relative_path: &str) -> Result<PathBuf> {
    if relative_path.starts_with('/')
        || relative_path.contains('\\')
        || relative_path.contains("..")
        || !relative_path.starts_with(".forge/private/objects/sha256/")
    {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "invalid_private_object_path".to_string(),
        }
        .into());
    }
    let path = PathBuf::from(relative_path);
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "invalid_private_object_path".to_string(),
                }
                .into());
            }
        }
    }
    Ok(path)
}

fn write_materialized_private_file(cwd: &Path, path: &str, bytes: &[u8]) -> Result<()> {
    let path = normalize_private_label_path(path)?;
    ensure_no_symlink_components(cwd, &path)?;
    let full_path = cwd.join(&path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).with_context(|| "create materialized private parent")?;
        ensure_no_symlink_components(cwd, &path)?;
    }
    fs::write(&full_path, bytes).with_context(|| "write materialized private path")?;
    Ok(())
}

fn ensure_no_symlink_components(cwd: &Path, path: &str) -> Result<()> {
    let mut current = PathBuf::from(cwd);
    for component in Path::new(path).components() {
        let Component::Normal(component) = component else {
            return Err(ForgeError::PrivateContentInvalid {
                reason: "invalid_private_path_label".to_string(),
            }
            .into());
        };
        current.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "private_path_symlink_escape".to_string(),
                }
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn fsync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| "open directory for fsync")?
        .sync_all()
        .with_context(|| "fsync directory")
}

#[cfg(not(unix))]
fn fsync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn local_encryption_recipient(cwd: &Path) -> Result<String> {
    let context = open_repository(cwd)?;
    Ok(local_encryption_identity(&context)?
        .recipient()
        .as_str()
        .to_string())
}

pub fn private_overlay_transports_for_snapshots(
    cwd: &Path,
    snapshot_ids: &[String],
    recipient_principal_id: &str,
) -> Result<Vec<PrivateOverlayTransportRecord>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let labels = read_local_private_path_labels(&context)?;
    let identity = local_encryption_identity(&context)?;
    let mut transports = Vec::new();

    for snapshot_id in snapshot_ids {
        let mut statement = connection.prepare(
            "SELECT id, work_package_kind, work_package_id, snapshot_id, path_label_id,
                    path_hash, envelope_format, recipient_fingerprint, ciphertext_digest,
                    private_object_path, encrypted_metadata_json, created_at_ms
             FROM encrypted_private_payloads
             WHERE repo_id = ?1 AND snapshot_id = ?2
             ORDER BY rowid",
        )?;
        let rows = statement.query_map(params![context.repo_id, snapshot_id], |row| {
            Ok(EncryptedPrivatePayloadRecord {
                payload_id: row.get(0)?,
                work_package_kind: row.get(1)?,
                work_package_id: row.get(2)?,
                snapshot_id: row.get(3)?,
                path_label_id: row.get(4)?,
                path_hash: row.get(5)?,
                envelope_format: row.get(6)?,
                recipient_fingerprint: row.get(7)?,
                ciphertext_digest: row.get(8)?,
                private_object_path: row.get(9)?,
                encrypted_metadata_json: row.get(10)?,
                created_at_ms: row.get(11)?,
            })
        })?;
        for row in rows {
            let record = row?;
            let authority = match private_decrypt_authority_on(
                &connection,
                &context.repo_id,
                &record.work_package_kind,
                &record.work_package_id,
                recipient_principal_id,
            ) {
                Ok(authority) => authority,
                Err(error)
                    if matches!(
                        error.downcast_ref::<ForgeError>(),
                        Some(ForgeError::PrivateDecryptAuthorityMissing { .. })
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let Some(label) = labels.iter().find(|label| {
                label.work_package_kind == record.work_package_kind
                    && label.work_package_id == record.work_package_id
                    && label.path_label_id == record.path_label_id
                    && label.path_hash == record.path_hash
            }) else {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "missing_local_private_path_labels".to_string(),
                }
                .into());
            };
            let local_ciphertext = read_private_object(&context, &record.private_object_path)?;
            let plaintext = forge_private::decrypt_private_payload(
                &identity,
                &EncryptedPayload {
                    envelope_format: record.envelope_format.clone(),
                    recipient_fingerprint: record.recipient_fingerprint.clone(),
                    ciphertext: local_ciphertext,
                    ciphertext_digest: record.ciphertext_digest.clone(),
                },
            )
            .map_err(|_| ForgeError::PrivateContentInvalid {
                reason: "private_payload_decrypt_failed".to_string(),
            })?;
            let recipient = EncryptionRecipient::parse(&authority.public_key).map_err(|_| {
                ForgeError::PrivateContentInvalid {
                    reason: "invalid_encryption_recipient".to_string(),
                }
            })?;
            let encrypted = forge_private::encrypt_private_payload(&recipient, &plaintext)
                .map_err(|_| ForgeError::PrivateContentInvalid {
                    reason: "private_payload_encrypt_failed".to_string(),
                })?;
            transports.push(PrivateOverlayTransportRecord {
                work_package_kind: record.work_package_kind,
                work_package_id: record.work_package_id,
                snapshot_id: snapshot_id.clone(),
                path_label_id: record.path_label_id,
                path_hash: record.path_hash,
                path: label.path.clone(),
                visibility: label.visibility.clone(),
                envelope_format: encrypted.envelope_format,
                recipient_fingerprint: encrypted.recipient_fingerprint,
                ciphertext_digest: encrypted.ciphertext_digest,
                ciphertext: encrypted.ciphertext,
            });
        }
    }
    Ok(transports)
}

pub fn prepare_materialized_private_overlay(
    cwd: &Path,
    input: PrivateOverlayMaterializeInput,
) -> Result<MaterializedPrivateOverlay> {
    validate_work_package_kind(&input.work_package_kind)?;
    validate_visibility_label(&input.visibility)?;
    if input.visibility == VISIBILITY_PUBLIC {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "public_private_path_label".to_string(),
        }
        .into());
    }
    validate_private_hash(&input.path_hash)?;
    validate_private_hash(&input.ciphertext_digest)?;
    let context = open_repository(cwd)?;
    let identity = local_encryption_identity(&context)?;
    if identity.recipient().fingerprint() != input.recipient_fingerprint {
        return Err(ForgeError::PrivateDecryptAuthorityMissing {
            principal_id: input.recipient_fingerprint,
            reason: "recipient_private_key_missing".to_string(),
        }
        .into());
    }
    let path = normalize_private_label_path(&input.path)?;
    let plaintext = forge_private::decrypt_private_payload(
        &identity,
        &EncryptedPayload {
            envelope_format: input.envelope_format,
            recipient_fingerprint: input.recipient_fingerprint,
            ciphertext: input.ciphertext,
            ciphertext_digest: input.ciphertext_digest,
        },
    )
    .map_err(|_| ForgeError::PrivateContentInvalid {
        reason: "private_payload_decrypt_failed".to_string(),
    })?;
    Ok(MaterializedPrivateOverlay {
        work_package_kind: input.work_package_kind,
        work_package_id: input.work_package_id,
        path_label_id: input.path_label_id,
        path_hash: input.path_hash,
        path,
        visibility: input.visibility,
        plaintext,
    })
}

pub fn install_materialized_private_overlays(
    cwd: &Path,
    overlays: &[MaterializedPrivateOverlay],
) -> Result<usize> {
    for overlay in overlays {
        set_local_private_path_label(
            cwd,
            &overlay.work_package_kind,
            &overlay.work_package_id,
            &overlay.path,
            &overlay.visibility,
        )?;
    }
    for overlay in overlays {
        write_materialized_private_file(cwd, &overlay.path, &overlay.plaintext)?;
    }
    Ok(overlays.len())
}

fn validate_private_hash(value: &str) -> Result<()> {
    let hex = value
        .strip_prefix("hmac-sha256:")
        .or_else(|| value.strip_prefix("sha256:"))
        .unwrap_or(value);
    let valid = hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(ForgeError::PrivateContentInvalid {
            reason: "invalid_private_digest".to_string(),
        }
        .into())
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn normalize_private_label_path(path: &str) -> Result<String> {
    let path = path.trim();
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(':')
        || path.contains("..")
        || path.contains('*')
        || path.contains('?')
        || path.contains('[')
        || path.contains(']')
        || path.contains('\\')
    {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "invalid_private_path_label".to_string(),
        }
        .into());
    }
    for component in Path::new(path).components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "invalid_private_path_label".to_string(),
                }
                .into());
            }
        }
    }
    Ok(path.trim_start_matches("./").to_string())
}

fn local_private_label_path(context: &RepositoryContext) -> PathBuf {
    context
        .root_path
        .join(".forge")
        .join("private")
        .join("path-labels.json")
}

fn local_private_path_hash_key_path(context: &RepositoryContext) -> PathBuf {
    context
        .root_path
        .join(".forge")
        .join("keys")
        .join("private-path-hash.key")
}

fn local_private_path_hash_key(context: &RepositoryContext) -> Result<Vec<u8>> {
    let path = local_private_path_hash_key_path(context);
    if path.exists() {
        let encoded = fs::read_to_string(&path).with_context(|| "read private path hash key")?;
        let key = decode_hex(encoded.trim()).map_err(|_| ForgeError::PrivateContentInvalid {
            reason: "invalid_private_path_hash_key".to_string(),
        })?;
        if key.len() == 32 {
            return Ok(key);
        }
        return Err(ForgeError::PrivateContentInvalid {
            reason: "invalid_private_path_hash_key".to_string(),
        }
        .into());
    }

    if private_path_label_count_all(context)? > 0 {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "missing_private_path_hash_key".to_string(),
        }
        .into());
    }

    let rng = ring::rand::SystemRandom::new();
    let mut key = [0_u8; 32];
    ring::rand::SecureRandom::fill(&rng, &mut key).map_err(|_| {
        ForgeError::PrivateContentInvalid {
            reason: "generate_private_path_hash_key_failed".to_string(),
        }
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| "create local key dir")?;
        signing::set_private_dir_permissions(parent)?;
    }
    fs::write(&path, format!("{}\n", hex_bytes(&key)))
        .with_context(|| "write private path hash key")?;
    signing::set_private_file_permissions(&path)?;
    Ok(key.to_vec())
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        bail!("odd-length hex");
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex"),
    }
}

fn read_local_private_path_labels(
    context: &RepositoryContext,
) -> Result<Vec<LocalPrivatePathLabel>> {
    let path = local_private_label_path(context);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&path).with_context(|| "read local private path labels")?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&bytes).with_context(|| "parse local private path labels")
}

fn private_path_label_count(
    context: &RepositoryContext,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<usize> {
    let connection = open_connection(&context.database_path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM private_path_labels
         WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3",
        params![context.repo_id, work_package_kind, work_package_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

fn private_path_label_count_all(context: &RepositoryContext) -> Result<usize> {
    let connection = open_connection(&context.database_path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM private_path_labels WHERE repo_id = ?1",
        params![context.repo_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

fn write_local_private_path_labels(
    context: &RepositoryContext,
    labels: &[LocalPrivatePathLabel],
) -> Result<()> {
    let path = local_private_label_path(context);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| "create local private label dir")?;
        signing::set_private_dir_permissions(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(labels)?;
    fs::write(&path, bytes).with_context(|| "write local private path labels")?;
    signing::set_private_file_permissions(&path)?;
    Ok(())
}

fn local_encryption_identity_path(context: &RepositoryContext) -> PathBuf {
    context
        .root_path
        .join(".forge")
        .join("keys")
        .join("local-age-x25519.txt")
}

fn local_encryption_identity(
    context: &RepositoryContext,
) -> Result<forge_private::EncryptionIdentity> {
    let path = local_encryption_identity_path(context);
    if path.exists() {
        let secret = fs::read_to_string(&path).with_context(|| "read local encryption key")?;
        return forge_private::EncryptionIdentity::from_secret_str(secret.trim()).map_err(|_| {
            ForgeError::PrivateContentInvalid {
                reason: "invalid_local_encryption_identity".to_string(),
            }
            .into()
        });
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| "create local key dir")?;
        signing::set_private_dir_permissions(parent)?;
    }
    let identity = forge_private::EncryptionIdentity::generate();
    fs::write(&path, format!("{}\n", identity.to_secret_string()))
        .with_context(|| "write local encryption key")?;
    signing::set_private_file_permissions(&path)?;
    Ok(identity)
}

fn ensure_active_org_principal(conn: &Connection, repo_id: &str, principal_id: &str) -> Result<()> {
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

fn ensure_active_org_role(
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

fn embargo_authority_on(
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

fn ensure_private_path_label_matches(
    conn: &Connection,
    repo_id: &str,
    path_label_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    path_hash: &str,
) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM private_path_labels
            WHERE repo_id = ?1
              AND id = ?2
              AND work_package_kind = ?3
              AND work_package_id = ?4
              AND path_hash = ?5
        )",
        params![
            repo_id,
            path_label_id,
            work_package_kind,
            work_package_id,
            path_hash
        ],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(ForgeError::PrivateContentInvalid {
            reason: "private_path_label_mismatch".to_string(),
        }
        .into())
    }
}

fn ensure_snapshot_exists(conn: &Connection, repo_id: &str, snapshot_id: &str) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE repo_id = ?1 AND id = ?2)",
        params![repo_id, snapshot_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(ForgeError::PrivateContentInvalid {
            reason: "missing_snapshot".to_string(),
        }
        .into())
    }
}

fn org_encryption_key_binding_on(
    conn: &Connection,
    repo_id: &str,
    key_fingerprint: &str,
) -> Result<Option<OrgEncryptionKeyBindingRecord>> {
    conn.query_row(
        "SELECT id, principal_id, key_fingerprint, public_key, binding_authority, state,
                valid_from_revision, valid_until_revision, created_at_ms, updated_at_ms
         FROM org_encryption_key_bindings
         WHERE repo_id = ?1 AND key_fingerprint = ?2",
        params![repo_id, key_fingerprint],
        |row| {
            Ok(OrgEncryptionKeyBindingRecord {
                binding_id: row.get(0)?,
                principal_id: row.get(1)?,
                key_fingerprint: row.get(2)?,
                public_key: row.get(3)?,
                binding_authority: row.get(4)?,
                state: row.get(5)?,
                valid_from_revision: row.get(6)?,
                valid_until_revision: row.get(7)?,
                created_at_ms: row.get(8)?,
                updated_at_ms: row.get(9)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn private_path_label_on(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    path_hash: &str,
) -> Result<Option<PrivatePathLabelRecord>> {
    conn.query_row(
        "SELECT id, work_package_kind, work_package_id, path_hash, encrypted_display_path,
                visibility, created_at_ms, updated_at_ms
         FROM private_path_labels
         WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3 AND path_hash = ?4",
        params![repo_id, work_package_kind, work_package_id, path_hash],
        |row| {
            Ok(PrivatePathLabelRecord {
                path_label_id: row.get(0)?,
                work_package_kind: row.get(1)?,
                work_package_id: row.get(2)?,
                path_hash: row.get(3)?,
                encrypted_display_path: row.get(4)?,
                visibility: row.get(5)?,
                created_at_ms: row.get(6)?,
                updated_at_ms: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn encrypted_private_payload_on(
    conn: &Connection,
    repo_id: &str,
    payload_id: &str,
) -> Result<Option<EncryptedPrivatePayloadRecord>> {
    conn.query_row(
        "SELECT id, work_package_kind, work_package_id, snapshot_id, path_label_id,
                path_hash, envelope_format, recipient_fingerprint, ciphertext_digest,
                private_object_path, encrypted_metadata_json, created_at_ms
         FROM encrypted_private_payloads
         WHERE repo_id = ?1 AND id = ?2",
        params![repo_id, payload_id],
        |row| {
            Ok(EncryptedPrivatePayloadRecord {
                payload_id: row.get(0)?,
                work_package_kind: row.get(1)?,
                work_package_id: row.get(2)?,
                snapshot_id: row.get(3)?,
                path_label_id: row.get(4)?,
                path_hash: row.get(5)?,
                envelope_format: row.get(6)?,
                recipient_fingerprint: row.get(7)?,
                ciphertext_digest: row.get(8)?,
                private_object_path: row.get(9)?,
                encrypted_metadata_json: row.get(10)?,
                created_at_ms: row.get(11)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn insert_encrypted_private_payload_on(
    tx: &Transaction<'_>,
    repo_id: &str,
    input: EncryptedPrivatePayloadInput,
    now: i64,
) -> Result<EncryptedPrivatePayloadRecord> {
    ensure_work_package_exists(
        tx,
        repo_id,
        &input.work_package_kind,
        &input.work_package_id,
    )?;
    ensure_private_path_label_matches(
        tx,
        repo_id,
        &input.path_label_id,
        &input.work_package_kind,
        &input.work_package_id,
        &input.path_hash,
    )?;
    if let Some(snapshot_id) = input.snapshot_id.as_deref() {
        ensure_snapshot_exists(tx, repo_id, snapshot_id)?;
    }
    let payload_id = new_id("private_payload");
    tx.execute(
        "INSERT INTO encrypted_private_payloads (
            id, repo_id, work_package_kind, work_package_id, snapshot_id, path_label_id,
            path_hash, envelope_format, recipient_fingerprint, ciphertext_digest,
            private_object_path, encrypted_metadata_json, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            payload_id,
            repo_id,
            input.work_package_kind,
            input.work_package_id,
            input.snapshot_id,
            input.path_label_id,
            input.path_hash,
            input.envelope_format,
            input.recipient_fingerprint,
            input.ciphertext_digest,
            input.private_object_path,
            input.encrypted_metadata_json,
            now,
        ],
    )?;
    encrypted_private_payload_on(tx, repo_id, &payload_id)?
        .ok_or_else(|| anyhow!("encrypted private payload missing after insert"))
}

#[allow(clippy::too_many_arguments)]
fn insert_private_content_audit(
    tx: &Transaction<'_>,
    repo_id: &str,
    work_package_kind: Option<&str>,
    work_package_id: Option<&str>,
    snapshot_id: Option<&str>,
    path_label_id: Option<&str>,
    principal_id: Option<&str>,
    key_fingerprint: Option<&str>,
    action: &str,
    reason: Option<&str>,
    now: i64,
) -> Result<()> {
    let audit_id = new_id("private_audit");
    tx.execute(
        "INSERT INTO private_content_audit (
            id, repo_id, work_package_kind, work_package_id, snapshot_id, path_label_id,
            principal_id, key_fingerprint, action, reason, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            audit_id,
            repo_id,
            work_package_kind,
            work_package_id,
            snapshot_id,
            path_label_id,
            principal_id,
            key_fingerprint,
            action,
            reason,
            now,
        ],
    )?;
    Ok(())
}

fn private_decrypt_authority_on(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    principal_id: &str,
) -> Result<PrivateDecryptAuthority> {
    let org = org_status_on(conn, repo_id)?;
    if !org.enabled {
        return Err(ForgeError::OrgNotEnabled.into());
    }
    ensure_active_org_principal(conn, repo_id, principal_id)?;
    ensure_active_org_role(
        conn,
        repo_id,
        principal_id,
        &[
            "owner",
            "maintainer",
            "member",
            "external_reviewer",
            "service",
        ],
    )?;
    if !has_active_visibility_grant(
        conn,
        repo_id,
        work_package_kind,
        work_package_id,
        principal_id,
        CAPABILITY_SYNC_MATERIALIZE,
    )? {
        let decision = projection_decision_on(
            conn,
            repo_id,
            work_package_kind,
            work_package_id,
            principal_id,
            CAPABILITY_SYNC_MATERIALIZE,
        )?;
        return Err(ForgeError::PrivateDecryptAuthorityMissing {
            principal_id: principal_id.to_string(),
            reason: format!("missing_visibility_grant:{}", decision.disclosure),
        }
        .into());
    }

    let binding: Option<(String, String, String)> = conn
        .query_row(
            "SELECT key_fingerprint, public_key, state
             FROM org_encryption_key_bindings
             WHERE repo_id = ?1
               AND principal_id = ?2
               AND state = 'active'
               AND valid_from_revision <= ?3
               AND (valid_until_revision IS NULL OR valid_until_revision > ?3)
             ORDER BY updated_at_ms DESC, created_at_ms DESC
             LIMIT 1",
            params![repo_id, principal_id, org.policy_revision],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((key_fingerprint, public_key, _state)) = binding else {
        return Err(ForgeError::PrivateDecryptAuthorityMissing {
            principal_id: principal_id.to_string(),
            reason: "missing_active_encryption_key".to_string(),
        }
        .into());
    };
    Ok(PrivateDecryptAuthority {
        principal_id: principal_id.to_string(),
        key_fingerprint: key_fingerprint.clone(),
        public_key,
        recipient_fingerprint: key_fingerprint,
        policy_revision: org.policy_revision,
    })
}

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

fn org_status_on(conn: &Connection, repo_id: &str) -> Result<OrgStatus> {
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

fn trust_policy_on(conn: &Connection) -> Result<TrustPolicy> {
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

fn trust_rank(level: &str) -> Option<u8> {
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

pub fn start_attempt(
    cwd: &Path,
    request_id: Option<String>,
    intent: String,
    base_head: String,
    check_spec_json: Option<String>,
) -> Result<StartAttempt> {
    let context = open_repository(cwd)?;
    create_attempt(
        &context,
        request_id,
        None,
        Some(intent),
        base_head,
        true,
        "start",
        check_spec_json,
    )
}

pub fn start_attempt_for_intent(
    cwd: &Path,
    request_id: Option<String>,
    intent_id: String,
    base_head: String,
) -> Result<StartAttempt> {
    let context = open_repository(cwd)?;
    create_attempt(
        &context,
        request_id,
        Some(intent_id),
        None,
        base_head,
        false,
        "attempt start",
        // `attempt start` references an existing intent and inherits its gates; it
        // never mints an intent, so it carries no check spec of its own (NER-135).
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn create_attempt(
    context: &RepositoryContext,
    request_id: Option<String>,
    intent_id: Option<String>,
    intent: Option<String>,
    base_head: String,
    attach: bool,
    command: &str,
    check_spec_json: Option<String>,
) -> Result<StartAttempt> {
    let mut connection = open_connection(&context.database_path)?;
    let (intent_id, attempt_id, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let now = now_ms();
        let (intent_id, intent_visibility) = match intent_id.clone() {
            Some(id) => {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM intents WHERE repo_id = ?1 AND id = ?2)",
                    params![context.repo_id, id],
                    |row| row.get(0),
                )?;
                if !exists {
                    return Err(ForgeError::UnknownIntent {
                        selector: id.to_string(),
                    }
                    .into());
                }
                let visibility =
                    effective_work_package_visibility_on(tx, &context.repo_id, "intent", &id)?;
                insert_work_package_visibility(
                    tx,
                    &context.repo_id,
                    "intent",
                    &id,
                    &visibility,
                    now,
                )?;
                (id, visibility)
            }
            None => {
                let id = new_id("intent");
                tx.execute(
                    "INSERT INTO intents (id, repo_id, text, check_spec_json, created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        id,
                        context.repo_id,
                        intent
                            .clone()
                            .unwrap_or_else(|| "local agent attempt".to_string()),
                        check_spec_json,
                        now
                    ],
                )?;
                let visibility = visibility_policy_on(tx)?.default_work_package_visibility;
                insert_work_package_visibility(
                    tx,
                    &context.repo_id,
                    "intent",
                    &id,
                    &visibility,
                    now,
                )?;
                (id, visibility)
            }
        };
        let attempt_id = new_id("attempt");
        tx.execute(
            "INSERT INTO attempts (id, repo_id, intent_id, base_head, status, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5)",
            params![attempt_id, context.repo_id, intent_id, base_head, now],
        )?;
        insert_work_package_visibility(
            tx,
            &context.repo_id,
            "attempt",
            &attempt_id,
            &intent_visibility,
            now,
        )?;
        tx.execute(
            "INSERT INTO attempt_workspaces (
                attempt_id, repo_id, workspace_rel_path, status,
                materialized_content_ref, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 'active', NULL, ?4, ?4)",
            params![
                attempt_id,
                context.repo_id,
                workspace_rel_path_for_attempt(&attempt_id),
                now
            ],
        )?;

        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: command.to_string(),
                kind: "attempt_started".to_string(),
                view_kind: ViewKind::Initialized,
                // NER-255: mirror the success `data` into the op view state for
                // idempotent replay (see save_snapshot for the rationale). `operation_id`
                // is overlaid on replay; `current_view_id` is omitted (minted by this
                // insert). The lifecycle/id keys stay siblings for existing json_extracts.
                state: json!({
                    "lifecycle": "attempt_active",
                    "attempt_id": attempt_id,
                    "intent_id": intent_id,
                    "replay_data": {
                        "intent_id": intent_id,
                        "attempt_id": attempt_id,
                        "base_head": base_head,
                        "attached": attach,
                        "workspace_path": workspace_rel_path_for_attempt(&attempt_id),
                    }
                }),
            },
        )?;
        if attach {
            tx.execute(
                "UPDATE current_state SET attached_attempt_id = ?1 WHERE singleton = 1",
                params![attempt_id],
            )?;
        }
        Ok((intent_id, attempt_id, op))
    })?;

    Ok(StartAttempt {
        intent_id,
        workspace_path: workspace_rel_path_for_attempt(&attempt_id),
        attempt_id,
        base_head,
        attached: attach,
        operation_id: op.operation_id,
        current_view_id: op.view_id,
    })
}

pub fn save_snapshot(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    content_ref: String,
    changed_paths: Vec<String>,
) -> Result<SnapshotRecord> {
    save_snapshot_with_private_overlays(
        cwd,
        request_id,
        attempt_id,
        content_ref,
        changed_paths,
        Vec::new(),
    )
}

pub fn save_snapshot_with_private_overlays(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: Option<&str>,
    content_ref: String,
    changed_paths: Vec<String>,
    private_overlays: Vec<SaveSnapshotPrivateOverlayInput>,
) -> Result<SnapshotRecord> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    // Write-binding verification (NER-134): the authoritative, non-bypassable guard
    // on the production write path. Refuse to record the worktree's content under an
    // attempt other than the one the worktree is materialized for.
    verify_worktree_binding(&context, &attempt.attempt_id)?;
    let mut connection = open_connection(&context.database_path)?;
    let (snapshot_id, parent_snapshot_id, op) = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        for overlay in &private_overlays {
            validate_work_package_kind(&overlay.work_package_kind)?;
            validate_private_hash(&overlay.path_hash)?;
            validate_private_hash(&overlay.ciphertext_digest)?;
            if overlay.envelope_format.trim().is_empty()
                || overlay.recipient_fingerprint.trim().is_empty()
                || overlay.private_object_path.trim().is_empty()
                || overlay.encrypted_metadata_json.trim().is_empty()
            {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "empty_private_payload_metadata".to_string(),
                }
                .into());
            }
        }
        let parent_snapshot_id: Option<String> = tx
            .query_row(
                "SELECT id FROM snapshots WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                params![attempt.attempt_id],
                |row| row.get(0),
            )
            .optional()?;
        let snapshot_id = new_id("snapshot");
        tx.execute(
            "INSERT INTO snapshots (
                id, repo_id, attempt_id, parent_snapshot_id, content_ref, changed_paths_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                snapshot_id,
                context.repo_id,
                attempt.attempt_id,
                parent_snapshot_id,
                content_ref,
                serde_json::to_string(&changed_paths)?,
                now_ms()
            ],
        )?;
        let now = now_ms();
        for overlay in private_overlays.clone() {
            insert_encrypted_private_payload_on(
                tx,
                &context.repo_id,
                EncryptedPrivatePayloadInput {
                    work_package_kind: overlay.work_package_kind,
                    work_package_id: overlay.work_package_id,
                    snapshot_id: Some(snapshot_id.clone()),
                    path_label_id: overlay.path_label_id,
                    path_hash: overlay.path_hash,
                    envelope_format: overlay.envelope_format,
                    recipient_fingerprint: overlay.recipient_fingerprint,
                    ciphertext_digest: overlay.ciphertext_digest,
                    private_object_path: overlay.private_object_path,
                    encrypted_metadata_json: overlay.encrypted_metadata_json,
                },
                now,
            )?;
        }
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "save".to_string(),
                kind: "snapshot_saved".to_string(),
                view_kind: ViewKind::Initialized,
                // NER-255: persist the success `data` payload (minus operation_id /
                // current_view_id, which are minted by this very insert) into the op
                // view state so an idempotent replay can return the ORIGINAL ids instead
                // of just {idempotent_replay, request_id}. The existing lifecycle/id keys
                // are kept as siblings (other code json_extracts $.lifecycle,
                // $.snapshot_id, $.attempt_id) — `replay_data` is added alongside, never
                // nesting them. `operation_id` is overlaid on replay from the op row;
                // `current_view_id` is intentionally omitted (not known until after this
                // insert, and not crash-recovery-critical).
                state: json!({
                    "lifecycle": "snapshot_saved",
                    "attempt_id": attempt.attempt_id,
                    "snapshot_id": snapshot_id,
                    "replay_data": {
                        "snapshot_id": snapshot_id,
                        "attempt_id": attempt.attempt_id,
                        "parent_snapshot_id": parent_snapshot_id,
                        "content_ref": content_ref,
                        "changed_paths": changed_paths,
                    }
                }),
            },
        )?;
        // NER-143 R1: the worktree now holds exactly this snapshot's tree (save captured it),
        // so it becomes the expected dirty-check baseline for the worktree that issued `save`.
        // Native attempt workspaces have independent materialized baselines; do not let a
        // workspace save poison the owner repo's root-level dirty check.
        set_context_expected_content_ref(tx, &context, &content_ref)?;
        if context.workspace_attempt_id.is_some() {
            initialize_root_expected_content_ref_if_missing(tx, &context, &attempt.base_head)?;
        }
        Ok((snapshot_id, parent_snapshot_id, op))
    })?;
    Ok(SnapshotRecord {
        snapshot_id,
        attempt_id: attempt.attempt_id,
        parent_snapshot_id,
        content_ref,
        changed_paths,
        operation_id: op.operation_id,
        current_view_id: op.view_id,
    })
}

fn workspace_rel_path_for_attempt(attempt_id: &str) -> String {
    format!(".forge/worktrees/{attempt_id}")
}

fn attempt_workspace_rel_path(context: &RepositoryContext, attempt_id: &str) -> Result<String> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT workspace_rel_path FROM attempt_workspaces
             WHERE repo_id = ?1 AND attempt_id = ?2",
            params![context.repo_id, attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map(|value| value.unwrap_or_else(|| workspace_rel_path_for_attempt(attempt_id)))
        .map_err(Into::into)
}

pub fn attempt_workspace_path(cwd: &Path, attempt_id: &str) -> Result<PathBuf> {
    let context = open_repository(cwd)?;
    attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
        selector: attempt_id.to_string(),
    })?;
    let rel = attempt_workspace_rel_path(&context, attempt_id)?;
    Ok(context.root_path.join(rel))
}

pub fn ensure_attempt_workspace_marker(cwd: &Path, attempt_id: &str) -> Result<PathBuf> {
    let context = open_repository(cwd)?;
    attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
        selector: attempt_id.to_string(),
    })?;
    let path = attempt_workspace_path(cwd, attempt_id)?;
    fs::create_dir_all(&path).map_err(|error| anyhow!("create workspace: {}", error.kind()))?;
    let marker = WorkspaceMarker {
        repo_root: context.root_path.to_string_lossy().into_owned(),
        attempt_id: attempt_id.to_string(),
    };
    let marker_path = path.join(forge_content::WORKSPACE_MARKER_FILE);
    fs::write(&marker_path, serde_json::to_vec(&marker)?)
        .map_err(|error| anyhow!("write workspace marker: {}", error.kind()))?;
    Ok(path)
}

pub fn record_attempt_workspace_materialized(
    cwd: &Path,
    attempt_id: &str,
    content_ref: &str,
) -> Result<()> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        tx.execute(
            "UPDATE attempt_workspaces
             SET materialized_content_ref = ?1, updated_at_ms = ?2
             WHERE repo_id = ?3 AND attempt_id = ?4",
            params![content_ref, now_ms(), context.repo_id, attempt_id],
        )?;
        Ok(())
    })
}

/// Write-binding verification (NER-134): refuse to act on the worktree under
/// `target_attempt_id` when the worktree is currently materialized for a
/// *different* attempt (`current_state.attached_attempt_id`).
///
/// The raw `attached_attempt_id` is consulted regardless of the attached attempt's
/// active/abandoned status: if the worktree holds attempt `W`'s content, recording
/// it under `X != W` is cross-attempt contamination even if `W` was later abandoned.
/// `None` (nothing materialized — the single-attempt v0 flow, or after a forced
/// detach) is always allowed: contamination requires a *different* attempt to have
/// been materialized, and materialization always sets `attached_attempt_id`. The
/// check runs only after attempt resolution succeeds, so an unqualified ambiguous
/// selector still surfaces `AmbiguousAttempt` first.
fn verify_worktree_binding(context: &RepositoryContext, target_attempt_id: &str) -> Result<()> {
    if let Some(workspace_attempt_id) = context.workspace_attempt_id.as_deref() {
        if workspace_attempt_id != target_attempt_id {
            return Err(ForgeError::AttemptWorktreeMismatch {
                requested_attempt: target_attempt_id.to_string(),
                attached_attempt: workspace_attempt_id.to_string(),
            }
            .into());
        }
        return Ok(());
    }
    if let Some(attached) = context.attached_attempt_id.as_deref() {
        if attached != target_attempt_id {
            return Err(ForgeError::AttemptWorktreeMismatch {
                requested_attempt: target_attempt_id.to_string(),
                attached_attempt: attached.to_string(),
            }
            .into());
        }
    }
    Ok(())
}

/// Resolve the `save` target attempt and verify the worktree binding *before* the
/// CLI snapshots the worktree (NER-134), so a mismatch fails fast without writing
/// orphan content objects. Returns the resolved attempt id; the CLI passes it back
/// as an explicit selector to [`save_snapshot`], whose own [`verify_worktree_binding`]
/// call is the authoritative guard. Pure read — takes no advisory lock.
pub fn verify_save_target(cwd: &Path, attempt_id: Option<&str>) -> Result<String> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    verify_worktree_binding(&context, &attempt.attempt_id)?;
    Ok(attempt.attempt_id)
}

/// The attempt that owns `snapshot_id` (NER-134 Piece 1b), so `restore` can refuse
/// to materialize a snapshot belonging to an attempt other than the bound one.
pub fn snapshot_owner_attempt_id(cwd: &Path, snapshot_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT attempt_id FROM snapshots WHERE repo_id = ?1 AND id = ?2",
            params![context.repo_id, snapshot_id],
            |row| row.get(0),
        )
        .optional()?
        // Defensive: `restore` resolves `snapshot_content_ref` first, so a missing
        // snapshot already errors before this is reached.
        .ok_or_else(|| ForgeError::NoSnapshot.into())
}

pub fn snapshot_content_ref(cwd: &Path, snapshot_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT content_ref FROM snapshots WHERE id = ?1",
            params![snapshot_id],
            |row| row.get(0),
        )
        .with_context(|| format!("unknown snapshot {snapshot_id}"))
}

pub fn latest_snapshot_content_ref(cwd: &Path, attempt_id: Option<&str>) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let attempt = resolve_attempt_in_context(&context, attempt_id)?.attempt;
    Ok(latest_snapshot_for_attempt(&context, &attempt.attempt_id)?
        .map(|snapshot| snapshot.content_ref))
}

/// The content ref the effective worktree is EXPECTED to hold — the tree the last materializing
/// op put there. Owner-root worktrees use `current_state.expected_content_ref` (migration 007,
/// NER-143 R1). Native attempt workspaces use their own `attempt_workspaces.materialized_content_ref`
/// so a save/run/propose loop in one isolated workspace does not poison root-level dirty checks
/// or another attempt workspace's baseline.
///
/// `None` for a pre-007 repo or a fresh worktree before its first materialize; the dirty-check
/// then falls back to the latest-snapshot baseline. This is the crash-safe baseline: a non-save
/// op materializes a different tree than the latest *saved* snapshot, so comparing the worktree
/// against "latest saved" spuriously fails chained navigation (undo twice) — comparing against
/// "expected" does not.
pub fn expected_content_ref(cwd: &Path) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    if let Some(attempt_id) = context.workspace_attempt_id.as_deref() {
        return Ok(connection
            .query_row(
                "SELECT materialized_content_ref FROM attempt_workspaces
                 WHERE repo_id = ?1 AND attempt_id = ?2",
                params![context.repo_id, attempt_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten());
    }
    Ok(connection
        .query_row(
            "SELECT expected_content_ref FROM current_state WHERE singleton = 1",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Set `current_state.expected_content_ref` to the tree a root worktree materializing op just put
/// in the owner repo (NER-143 R1). Called inside the recorder's `IMMEDIATE` txn — atomic with the
/// op-log advance, so a `CurrentStateChanged` CAS-loss rolls BOTH back together. DR-F2: this is a
/// DEDICATED UPDATE in each materializing recorder, never folded into the shared
/// `insert_operation_view` CAS (which every op hits — folding it there would clobber the expected
/// ref on a non-materializing `accept`/`run`/`propose`/`check`).
fn set_expected_content_ref(tx: &Connection, content_ref: &str) -> Result<()> {
    tx.execute(
        "UPDATE current_state SET expected_content_ref = ?1 WHERE singleton = 1",
        params![content_ref],
    )?;
    Ok(())
}

fn set_workspace_expected_content_ref(
    tx: &Connection,
    context: &RepositoryContext,
    attempt_id: &str,
    content_ref: &str,
) -> Result<()> {
    tx.execute(
        "UPDATE attempt_workspaces
         SET materialized_content_ref = ?1, updated_at_ms = ?2
         WHERE repo_id = ?3 AND attempt_id = ?4",
        params![content_ref, now_ms(), context.repo_id, attempt_id],
    )?;
    Ok(())
}

fn set_context_expected_content_ref(
    tx: &Connection,
    context: &RepositoryContext,
    content_ref: &str,
) -> Result<()> {
    if let Some(attempt_id) = context.workspace_attempt_id.as_deref() {
        set_workspace_expected_content_ref(tx, context, attempt_id, content_ref)
    } else {
        set_expected_content_ref(tx, content_ref)
    }
}

fn initialize_root_expected_content_ref_if_missing(
    tx: &Connection,
    context: &RepositoryContext,
    base_head: &str,
) -> Result<()> {
    let existing = tx.query_row(
        "SELECT expected_content_ref FROM current_state WHERE singleton = 1",
        [],
        |row| row.get::<_, Option<String>>(0),
    )?;
    if existing.is_some() || context.content_backend != "native" {
        return Ok(());
    }
    let id = match forge_content_native::ObjectId::parse(base_head) {
        Ok(id) if matches!(id.kind(), Ok(forge_content_native::ObjectKind::Commit)) => id,
        _ => return Ok(()),
    };
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let commit = store.read_commit(&id)?;
    set_expected_content_ref(
        tx,
        &format!("{}{}", forge_content::FORGE_TREE_PREFIX, commit.tree),
    )
}

pub fn record_restore(
    cwd: &Path,
    request_id: Option<String>,
    snapshot_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "restore".to_string(),
                kind: "snapshot_restored".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": "snapshot_restored", "snapshot_id": snapshot_id }),
            },
        )?;
        // NER-143 R1: restore just materialized this snapshot's tree into the worktree.
        set_context_expected_content_ref(tx, &context, content_ref)?;
        Ok(op)
    })?;
    Ok(op)
}

/// Resolve a historical commit id to the `forge-tree:` content ref of its tree, for
/// `forge checkout` (NER-138 Phase 7 slice 3). Fail-closed BEFORE any materialization:
/// - a non-parseable / non-commit id, or a never-written id the ledger does not reference,
///   is a USER error → a path-free `anyhow` ("unknown commit", mapped to COMMAND_FAILED),
///   NOT corruption (so a typo never inflates the perceived corruption rate);
/// - a commit/tree the ledger references but whose object is missing is genuine corruption
///   → typed `NativeHistoryCorrupt` (DanglingCommitId / DanglingTree).
pub fn checkout_target_content_ref(cwd: &Path, commit_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let id = match forge_content_native::ObjectId::parse(commit_id) {
        Ok(id) if matches!(id.kind(), Ok(forge_content_native::ObjectKind::Commit)) => id,
        _ => bail!("unknown commit: not a native commit id in this repository"),
    };
    let connection = open_connection(&context.database_path)?;
    let commit = match store.read_commit(&id) {
        Ok(commit) => commit,
        Err(_) => {
            let query = format!(
                "SELECT 1 FROM decisions WHERE repo_id = ?1 AND commit_id = ?2
                 UNION ALL
                 SELECT 1
                   FROM operations o
                   JOIN views v ON v.id = o.resulting_view_id
                  WHERE o.repo_id = ?1
                    AND o.kind IN ({})
                    AND json_extract(v.state_json, '$.commit_id') = ?2
                 LIMIT 1",
                sync::SYNC_MERGED_OP_KIND_SQL_IN
            );
            let referenced: bool = connection
                .query_row(&query, params![context.repo_id, commit_id], |_| Ok(true))
                .optional()?
                .unwrap_or(false);
            if referenced {
                return Err(ForgeError::NativeHistoryCorrupt {
                    kind: NativeHistoryCorruptKind::DanglingCommitId,
                    commit_id: commit_id.to_string(),
                    related_id: None,
                }
                .into());
            }
            bail!("unknown commit: not in this repository's native history");
        }
    };
    let content_ref = format!("{}{}", forge_content::FORGE_TREE_PREFIX, commit.tree);
    // The tree (and everything it reaches) must exist before we clobber the worktree.
    store
        .verify_content_ref(&content_ref)
        .map_err(|_| ForgeError::NativeHistoryCorrupt {
            kind: NativeHistoryCorruptKind::DanglingTree,
            commit_id: commit_id.to_string(),
            related_id: Some(commit.tree.clone()),
        })?;
    Ok(content_ref)
}

/// Record a `forge checkout` in the op-log (NER-138 Phase 7 slice 3) so `undo` can reverse
/// it and gc treats the materialized commit as a reachability root. The target `commit_id`
/// is in the view `state_json`. Does NOT advance the base anchor (checkout is materialize-only
/// — a `save` afterward still diffs against the unchanged base HEAD; see the slice-3 plan).
pub fn record_checkout(
    cwd: &Path,
    request_id: Option<String>,
    commit_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "checkout".to_string(),
                kind: "commit_checked_out".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": "commit_checked_out", "commit_id": commit_id }),
            },
        )?;
        // NER-143 R1: checkout just materialized this commit's tree into the worktree.
        set_context_expected_content_ref(tx, &context, content_ref)?;
        Ok(op)
    })?;
    Ok(op)
}

pub fn set_materialized_expected_content_ref(cwd: &Path, content_ref: &str) -> Result<()> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        set_context_expected_content_ref(tx, &context, content_ref)
    })
}

/// What a `forge undo` will restore (NER-138 Phase 7 slice 3 / NER-143 R3+R4).
///
/// **Semantics (deliberate v0 cut):** undo reverses the **last save of the attached attempt** by
/// restoring that save's parent snapshot (the `snapshots.parent_snapshot_id` chain — robust, no
/// cross-table timestamp comparison). It is NOT the op-log `current_state` rewind: after a
/// non-save head op (accept/checkout/run) undo still reverses the last *save*, not that head op.
/// The full op-log-rewind model is future work.
///
/// `undone_operation_id` (NER-143 R4) is the `save` operation that produced the snapshot being
/// reversed (the attempt's latest), resolved from that save's view — NOT the op-log head, which
/// after a non-save head op would mislabel the audit field.
#[derive(Debug, Clone, Serialize)]
pub struct UndoTarget {
    pub undone_operation_id: String,
    pub content_ref: String,
    pub restored_snapshot_id: String,
}

/// Resolve what `forge undo` will restore: the parent of the **attached attempt's** latest
/// snapshot. Read-only and fail-closed with a clear, path-free "nothing to undo" when there is no
/// snapshot, or the latest snapshot is the first (no earlier state to restore). Restoring past the
/// first snapshot (to the base) is future work.
pub fn undo_target(cwd: &Path) -> Result<UndoTarget> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    // NER-143 R3: bind to the attached attempt. The repo-wide latest snapshot could belong to a
    // DIFFERENT attempt than the one the worktree is bound to, so undo could otherwise restore
    // attempt X's content into attempt Y's worktree (the dirty-check resolves the attached
    // attempt's latest, so the two would also disagree). The `parent_snapshot_id` chain stays
    // within an attempt (`save_snapshot` chains per-attempt), so binding the latest-snapshot
    // selection to the attached attempt makes "undo the last save" mean "this attempt's last
    // save" and never crosses attempts.
    let attempt = resolve_attempt_in_context(&context, None)?.attempt;
    let latest: Option<(String, Option<String>)> = connection
        .query_row(
            "SELECT id, parent_snapshot_id FROM snapshots \
             WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![attempt.attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((latest_id, parent_snapshot_id)) = latest else {
        bail!("nothing to undo: this repository has no snapshots");
    };
    let Some(parent_id) = parent_snapshot_id else {
        bail!("nothing to undo: already at the first saved snapshot");
    };
    let content_ref: String = connection.query_row(
        "SELECT content_ref FROM snapshots WHERE id = ?1",
        params![parent_id],
        |row| row.get(0),
    )?;
    // NER-143 R4: the undone operation is the SAVE that produced the latest snapshot (the one
    // being reversed) — found via that save view's `snapshot_id` — not the op-log head. Scoped
    // to `snapshot_saved` views: a later `restore`/checkout of the same snapshot ALSO carries
    // `$.snapshot_id`, so without the lifecycle filter the ORDER BY would pick that restore op
    // and mislabel the audit field (code-review finding). Falls back to the op-log head only if
    // no save view is found (defensive; a save always records one). `json_extract` is exact (no
    // LIKE false-matches); SQLite's JSON1 is bundled.
    let undone_operation_id: String = connection
        .query_row(
            "SELECT operation_id FROM views \
             WHERE repo_id = ?1 AND json_extract(state_json, '$.snapshot_id') = ?2 \
             AND json_extract(state_json, '$.lifecycle') = 'snapshot_saved' \
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![context.repo_id, latest_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_else(|| context.current_operation_id.clone());
    Ok(UndoTarget {
        undone_operation_id,
        content_ref,
        restored_snapshot_id: parent_id,
    })
}

/// Record a `forge undo` in the op-log (NER-138 Phase 7 slice 3). Append-only: undo is a
/// FORWARD operation that restores prior content — it NEVER deletes a `decisions` row (so an
/// undone accept's `commit_id` stays a permanent gc reachability root) or any op-log row. The
/// undone operation + restored snapshot are in the view `state_json` for auditability.
pub fn record_undo(
    cwd: &Path,
    request_id: Option<String>,
    undone_operation_id: &str,
    restored_snapshot_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "undo".to_string(),
                kind: "operation_undone".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": "undone",
                    "undone_operation_id": undone_operation_id,
                    "restored_snapshot_id": restored_snapshot_id,
                }),
            },
        )?;
        // NER-143 R1: undo just materialized the restored snapshot's tree into the worktree.
        set_context_expected_content_ref(tx, &context, content_ref)?;
        Ok(op)
    })?;
    Ok(op)
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
fn redact_gate_result(gate: forge_policy::GateResult) -> forge_policy::GateResult {
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
fn representative_evidence_id(outcome: &forge_policy::CheckOutcome) -> Option<String> {
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
fn intent_check_spec(conn: &Connection, intent_id: &str) -> Result<forge_policy::CheckSpec> {
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
fn evidence_facts_on(
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
fn evaluate_check_on(
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
enum IntegrityStatus {
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
fn evidence_high_water(conn: &Connection) -> Result<i64> {
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
fn verify_evidence_integrity(
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

/// Heal a torn commit-on-accept (NER-138 Phase 7 slice 3): a crash after the decision row
/// committed but before the ref-store HEAD advanced leaves HEAD one or more commits behind
/// the ledger. The SQLite ledger is authoritative and HEAD is a reconcilable cache, so this
/// advances HEAD to the latest accepted `decisions.commit_id` when it lags. Runs at the
/// command boundary under the held advisory lock (serialized, cannot race), BEFORE the base
/// anchor is read and before the preflight-replay short-circuit (so a same-`request_id`
/// replay of a torn accept still heals HEAD). A no-op on git repos (no native `commit_id`,
/// no ref store) and before any justified commit. HEAD only ever moves FORWARD: the ledger
/// tip must descend from the current HEAD (else the store is corrupt → `NativeHistoryCorrupt`),
/// and a missing tip/parent object (the store-before-DB violation) is surfaced typed.
pub fn reconcile_native_head(cwd: &Path) -> Result<()> {
    let context = match open_repository(cwd) {
        Ok(context) => context,
        // Not a forge repo / not initialized: nothing to reconcile. The command's own
        // `open_repository` surfaces the real NOT_INITIALIZED — reconcile is best-effort.
        Err(_) => return Ok(()),
    };
    let refs = forge_content_native::NativeRefStore::new(&context.root_path);
    let current_head = refs.read_head()?;
    let connection = open_connection(&context.database_path)?;
    let tip = native_tip(&context, &connection)?;
    let Some(tip) = tip else {
        return Ok(()); // genesis-only / git repo — nothing to reconcile
    };
    if current_head.as_ref() == Some(&tip) {
        return Ok(()); // HEAD already current
    }
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    // Walk the tip's ancestry: every object must exist (a miss is the store-before-DB
    // violation), there is no cycle, and the current HEAD must be an ancestor of the tip
    // (HEAD lags, never forks). Merge history may reach the same shared ancestor more
    // than once; repeated visited commits are normal diamond ancestry, not corruption.
    let commits = walk_native_commits(&store, &tip)?;
    for (cid, commit) in &commits {
        let tree_ref = format!("{}{}", forge_content::FORGE_TREE_PREFIX, commit.tree);
        if store.verify_content_ref(&tree_ref).is_err() {
            return Err(ForgeError::NativeHistoryCorrupt {
                kind: NativeHistoryCorruptKind::DanglingTree,
                commit_id: cid.to_string(),
                related_id: Some(commit.tree.clone()),
            }
            .into());
        }
    }
    let head_reached = current_head
        .as_ref()
        .map(|head| commits.iter().any(|(cid, _)| cid == head))
        .unwrap_or(true);
    if !head_reached {
        // The ledger tip does not descend from the current HEAD — a fork, which lock-
        // serialized accepts cannot produce: the store is corrupt.
        return Err(ForgeError::NativeHistoryCorrupt {
            kind: NativeHistoryCorruptKind::DanglingParent,
            commit_id: tip.to_string(),
            related_id: current_head.map(|head| head.to_string()),
        }
        .into());
    }
    refs.set_head(&tip)?;
    Ok(())
}

/// The authoritative native history tip: the latest native-history-producing ledger entry
/// (accepted decisions or clean sync merge operations) that descends from the cached HEAD if any
/// exists, else the ref-store HEAD (the genesis), else `None`. Imported peer decisions can carry
/// divergent native commit IDs and peer wall-clock timestamps, so timestamp ordering alone is not
/// allowed to advance the local tip across a fork.
fn native_tip(
    context: &RepositoryContext,
    connection: &Connection,
) -> Result<Option<forge_content_native::ObjectId>> {
    let refs = forge_content_native::NativeRefStore::new(&context.root_path);
    let current_head = refs.read_head()?;
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let query = format!(
        "SELECT commit_id, source_rank FROM (
                 SELECT d.commit_id AS commit_id,
                        d.created_at_ms AS created_at_ms,
                        0 AS source_rank,
                        d.rowid AS tie_rowid
                   FROM decisions d
                  WHERE d.repo_id = ?1
                    AND d.commit_id IS NOT NULL
                 UNION ALL
                 SELECT json_extract(v.state_json, '$.commit_id') AS commit_id,
                        o.created_at_ms AS created_at_ms,
                        1 AS source_rank,
                        o.rowid AS tie_rowid
                  FROM operations o
                   JOIN views v ON v.id = o.resulting_view_id
                  WHERE o.repo_id = ?1
                    AND o.kind IN ({})
                    AND json_extract(v.state_json, '$.commit_id') IS NOT NULL
             )
             ORDER BY created_at_ms DESC, source_rank DESC, tie_rowid DESC, commit_id DESC",
        sync::SYNC_MERGED_OP_KIND_SQL_IN
    );
    let mut statement = connection.prepare(&query)?;
    let candidates = statement
        .query_map(params![context.repo_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let (Some(head), Some(candidate)) = (current_head.as_ref(), candidates.first()) {
        if candidate == &head.to_string() {
            return Ok(current_head);
        }
    }
    for candidate in candidates {
        let candidate = forge_content_native::ObjectId::parse(&candidate)?;
        let Some(head) = current_head.as_ref() else {
            return Ok(Some(candidate));
        };
        if &candidate == head {
            return Ok(Some(candidate));
        }
        match walk_native_commits(&store, &candidate) {
            Ok(commits) if commits.iter().any(|(cid, _)| cid == head) => {
                return Ok(Some(candidate));
            }
            Ok(_) => {}
            // Tip selection only trusts candidates whose objects can be walked; doctor still
            // reports dangling decision/op commit ids independently via verify_native_history.
            Err(_) => {}
        }
    }
    Ok(current_head)
}

/// One commit in the native history, as surfaced by `forge log` through the JSON contract
/// ("show every change under this intent and the evidence that justified it"). Optional
/// justification fields are omitted when absent (genesis), matching the on-object shape.
#[derive(Debug, Clone, Serialize)]
pub struct CommitView {
    pub commit_id: String,
    pub tree: String,
    pub parents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_revision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authored_time: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_digest: Option<String>,
}

/// Walk the native commit DAG from the authoritative tip (NER-138 Phase 7 slice 3),
/// tip→genesis, returning each commit's justification. Read-only (no lock, no HEAD write).
/// When `intent` is `Some`, only commits whose `intent_id` matches are returned — the literal
/// "show every change under this intent" query. A missing tip/parent object surfaces typed
/// `NativeHistoryCorrupt` (DanglingCommitId for the tip, DanglingParent deeper); a parent
/// cycle is `Cycle`.
pub fn native_log(cwd: &Path, intent: Option<&str>) -> Result<Vec<CommitView>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let tip = native_tip(&context, &connection)?;
    let mut out = Vec::new();
    let Some(tip) = tip else {
        return Ok(out);
    };
    for (cid, commit) in walk_native_commits(&store, &tip)? {
        let matches = intent
            .map(|want| commit.intent_id.as_deref() == Some(want))
            .unwrap_or(true);
        if matches {
            out.push(CommitView {
                commit_id: cid.to_string(),
                tree: commit.tree.clone(),
                parents: commit.parents.clone(),
                intent_id: commit.intent_id.clone(),
                proposal_revision_id: commit.proposal_revision_id.clone(),
                decision_id: commit.decision_id.clone(),
                actor: commit.actor.clone(),
                authored_time: commit.authored_time,
                evidence_digest: commit
                    .evidence_digest
                    .as_ref()
                    .map(|h| h.as_str().to_string()),
            });
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeVisitState {
    Visiting,
    Visited,
}

fn walk_native_commits(
    store: &forge_content_native::NativeObjectStore,
    tip: &forge_content_native::ObjectId,
) -> Result<
    Vec<(
        forge_content_native::ObjectId,
        forge_content_native::CommitObject,
    )>,
> {
    let mut out = Vec::new();
    let mut states = std::collections::BTreeMap::new();
    let mut stack = vec![(tip.clone(), false)];
    while let Some((cid, expanded)) = stack.pop() {
        if expanded {
            states.insert(cid, NativeVisitState::Visited);
            continue;
        }
        match states.get(&cid).copied() {
            Some(NativeVisitState::Visited) => continue,
            Some(NativeVisitState::Visiting) => {
                return Err(ForgeError::NativeHistoryCorrupt {
                    kind: NativeHistoryCorruptKind::Cycle,
                    commit_id: cid.to_string(),
                    related_id: None,
                }
                .into());
            }
            None => {}
        }
        states.insert(cid.clone(), NativeVisitState::Visiting);
        let commit = store.read_commit(&cid).map_err(|_| {
            let kind = if &cid == tip {
                NativeHistoryCorruptKind::DanglingCommitId
            } else {
                NativeHistoryCorruptKind::DanglingParent
            };
            anyhow::Error::from(ForgeError::NativeHistoryCorrupt {
                kind,
                commit_id: cid.to_string(),
                related_id: None,
            })
        })?;
        out.push((cid.clone(), commit.clone()));
        stack.push((cid, true));
        for parent in commit.parents.iter().rev() {
            stack.push((forge_content_native::ObjectId::parse(parent)?, false));
        }
    }
    Ok(out)
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
fn decision_high_water(conn: &Connection) -> Result<i64> {
    let mark: Option<i64> = conn
        .query_row(
            "SELECT decision_high_water FROM integrity_marker WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(mark.unwrap_or(0))
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

pub fn latest_decision(cwd: &Path) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    latest_decision_for_context(&context)
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

/// Persist a single `conflict_sets` row recording a stale-base divergence, then
/// return the new conflict-set id (NER-133 U7). This is a pure metadata insert —
/// no merge/diff engine — written before the CLI raises [`ForgeError::StaleBase`]
/// so the divergence survives the bail.
///
/// `context` is `"stale_base_accept"` or `"stale_base_export"`. `paths_json`
/// carries `{expected_head, actual_head, paths, redacted_count}`; any secret-risk
/// path in `paths` is dropped via [`forge_content::filter_secret_risk`] before
/// serialization, so a secret-risk filename never reaches the stored JSON — only
/// its count appears.
///
/// The caller already holds the per-command advisory lock (`accept`/`export
/// branch` are mutating), so this does NOT acquire the lock; it is just a single
/// `IMMEDIATE` DB transaction with no lock nesting.
pub fn record_conflict_set(
    cwd: &Path,
    context: &str,
    expected_head: &str,
    actual_head: &str,
    paths: &[String],
) -> Result<String> {
    let repo = open_repository(cwd)?;
    let (kept, dropped) = forge_content::filter_secret_risk(paths);
    let paths_json = json!({
        "expected_head": expected_head,
        "actual_head": actual_head,
        "paths": kept,
        "redacted_count": dropped.len(),
    })
    .to_string();
    let conflict_set_id = new_id("conflict");
    let mut connection = open_connection(&repo.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        tx.execute(
            "INSERT INTO conflict_sets (id, repo_id, context, paths_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![conflict_set_id, repo.repo_id, context, paths_json, now_ms()],
        )?;
        Ok(())
    })?;
    Ok(conflict_set_id)
}

pub fn record_failed_operation_with_conflict(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    code: &str,
    message: &str,
    details: Value,
    conflict: &StaleBaseConflictInput,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let conflict_set_id = new_id("conflict");
    let now = now_ms();
    with_immediate_retry(&mut connection, |tx| {
        let prepared_conflict =
            prepare_stale_base_conflict(&context, &operation_id, &conflict_set_id, now, conflict)?;
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command,
                kind: "recoverable_failure",
                created_at_ms: now,
            },
            Some(&prepared_conflict.content_hash),
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'failed', 'recoverable_failure', ?5, ?6, ?7, ?8, ?9)",
            params![
                operation_id,
                context.repo_id,
                request_id.clone(),
                command,
                context.current_operation_id,
                view_id,
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
                    "message": message,
                    "conflict_set_id": conflict_set_id,
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
        insert_prepared_conflict(tx, &context, &operation_id, &prepared_conflict)?;
        Ok(())
    })?;
    Ok(OperationViewResult {
        operation_id,
        view_id,
    })
}

pub fn record_merge_conflict(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    input: &MergeConflictInput,
) -> Result<MergeConflictRecord> {
    record_merge_conflict_inner(cwd, request_id, command, input, false)
}

pub(crate) fn record_merge_conflict_inner(
    cwd: &Path,
    request_id: Option<String>,
    command: &str,
    input: &MergeConflictInput,
    dedup_unrequested_sync_conflict: bool,
) -> Result<MergeConflictRecord> {
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let conflict_set_id = new_id("conflict");
    let now = now_ms();
    with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        if request_id.is_none() && dedup_unrequested_sync_conflict {
            if let Some(existing) = existing_native_merge_conflict(tx, &context.repo_id, input)? {
                return Ok(existing);
            }
        }
        let prepared_conflict =
            prepare_merge_conflict(&context, &operation_id, &conflict_set_id, now, input)?;
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command,
                kind: "merge_conflict",
                created_at_ms: now,
            },
            Some(&prepared_conflict.content_hash),
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'success', 'merge_conflict', ?5, ?6, NULL, ?7, ?8)",
            params![
                operation_id,
                context.repo_id,
                request_id,
                command,
                context.current_operation_id,
                view_id,
                content_hash,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'merge_conflict', ?4, ?5)",
            params![
                view_id,
                context.repo_id,
                operation_id,
                json!({
                    "lifecycle": "merge_conflict",
                    "conflict_set_id": conflict_set_id,
                    "proposal_id": input.proposal_id.clone(),
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
        insert_prepared_conflict(tx, &context, &operation_id, &prepared_conflict)?;
        Ok(MergeConflictRecord {
            conflict_set_id: conflict_set_id.clone(),
            operation_id: operation_id.clone(),
            view_id: view_id.clone(),
        })
    })
}

fn existing_native_merge_conflict(
    tx: &Transaction<'_>,
    repo_id: &str,
    input: &MergeConflictInput,
) -> Result<Option<MergeConflictRecord>> {
    tx.query_row(
        "SELECT cs.id, cs.generated_by_operation_id, o.resulting_view_id
         FROM conflict_sets cs
         JOIN operations o ON o.id = cs.generated_by_operation_id
         WHERE cs.repo_id = ?1
           AND cs.context = ?2
           AND cs.base_content_ref = ?3
           AND cs.ours_content_ref = ?4
           AND cs.theirs_content_ref = ?5
           AND cs.resolver_backend = 'native_merge'
           AND cs.status IN ('unresolved', 'partially_resolved', 'resolved')
         ORDER BY cs.created_at_ms DESC, cs.rowid DESC
         LIMIT 1",
        params![
            repo_id,
            input.context,
            input.base_content_ref,
            input.ours_content_ref,
            input.theirs_content_ref
        ],
        |row| {
            Ok(MergeConflictRecord {
                conflict_set_id: row.get(0)?,
                operation_id: row.get(1)?,
                view_id: row.get(2)?,
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

pub fn resolve_conflict_with_tree(
    cwd: &Path,
    request_id: Option<String>,
    conflict_set_id: &str,
    resolution_ref: &str,
) -> Result<ConflictResolutionRecord> {
    let context = open_repository(cwd)?;
    if resolution_ref.starts_with(forge_content::FORGE_TREE_PREFIX) {
        forge_content_native::NativeObjectStore::new(&context.root_path)
            .verify_content_ref(resolution_ref)?;
    } else {
        return Err(anyhow!(
            "conflict resolution requires a forge-tree content ref"
        ));
    }
    let signer = signing::LocalSigner::load_or_create(&context.root_path)?;
    let mut connection = open_connection(&context.database_path)?;
    let operation_id = OperationId::new().to_string();
    let view_id = ViewId::new().to_string();
    let now = now_ms();
    let mut out: Option<ConflictResolutionRecord> = None;
    with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let (paths_json, status, resolver_backend): (String, String, Option<String>) = tx
            .query_row(
                "SELECT paths_json, status, resolver_backend FROM conflict_sets WHERE id = ?1 AND repo_id = ?2",
                params![conflict_set_id, context.repo_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?
            .ok_or_else(|| ForgeError::ConflictSetNotFound {
                conflict_set_id: conflict_set_id.to_string(),
            })?;
        if resolver_backend.as_deref() != Some("native_merge") {
            return Err(ForgeError::UnsupportedContentBackend {
                command: "conflict resolve".to_string(),
                required: "native_merge".to_string(),
                actual: resolver_backend.unwrap_or_else(|| "unknown".to_string()),
            }
            .into());
        }
        if status == "resolved" {
            return Err(anyhow!("conflict set is already resolved"));
        }
        let proposal_id = serde_json::from_str::<Value>(&paths_json)
            .ok()
            .and_then(|value| {
                value
                    .get("proposal_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow!("conflict set has no proposal binding"))?;
        let proposal = proposal_by_id_on(tx, &context, &proposal_id)?.ok_or_else(|| {
            ForgeError::UnknownProposal {
                selector: proposal_id.clone(),
            }
        })?;
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
                resolution_ref,
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
                resolution_ref,
                changed_paths_json,
                now
            ],
        )?;
        let evidence_id = new_id("evidence");
        let evidence_args = vec![
            "conflict".to_string(),
            "resolve".to_string(),
            conflict_set_id.to_string(),
            "--tree".to_string(),
            resolution_ref.to_string(),
        ];
        let actor = "unknown".to_string();
        let cwd = ".".to_string();
        let evidence_hash = integrity::evidence_digest(&integrity::EvidenceDigestInput {
            attempt_id: &proposal.attempt_id,
            snapshot_id: None,
            command: "forge",
            args: &evidence_args,
            cwd: &cwd,
            exit_code: 0,
            started_at_ms: now,
            ended_at_ms: now,
            timed_out: false,
            stdout_excerpt: "",
            stderr_excerpt: "",
            stdout_truncated: false,
            stderr_truncated: false,
            sensitivity: "normal",
            actor: &actor,
            structured_json: None,
            created_at_ms: now,
        });
        tx.execute(
            "INSERT INTO evidence (
                id, repo_id, attempt_id, snapshot_id, command, args_json, cwd, exit_code,
                started_at_ms, ended_at_ms, stdout_excerpt, stderr_excerpt,
                stdout_truncated, stderr_truncated, timed_out, sensitivity, visibility,
                trust, actor, structured_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, ?4, 'forge', ?5, ?6, 0, ?7, ?8, '', '', 0, 0, 0,
                      'normal', 'internal', 'local', ?9, NULL, ?10, ?11)",
            params![
                evidence_id,
                context.repo_id,
                proposal.attempt_id,
                Option::<String>::None,
                serde_json::to_string(&evidence_args)?,
                cwd,
                now,
                now,
                actor,
                evidence_hash,
                now
            ],
        )?;
        signer.sign_subject(
            tx,
            &context.repo_id,
            "evidence",
            &evidence_id,
            &evidence_hash,
            now,
        )?;
        tx.execute(
            "UPDATE proposals SET snapshot_id = ?1, content_ref = ?2, status = 'draft' WHERE id = ?3",
            params![snapshot_id, resolution_ref, proposal.proposal_id],
        )?;
        tx.execute(
            "UPDATE path_conflicts SET status = 'resolved', resolution_ref = ?1 WHERE conflict_set_id = ?2",
            params![resolution_ref, conflict_set_id],
        )?;
        tx.execute(
            "UPDATE conflict_sets SET status = 'resolved' WHERE id = ?1",
            params![conflict_set_id],
        )?;
        set_context_expected_content_ref(tx, &context, resolution_ref)?;
        let parent_hash = op_content_hash(tx, Some(&context.current_operation_id))?;
        let content_hash = integrity::operation_link_hash(
            &parent_hash,
            &integrity::OperationDigestInput {
                operation_id: &operation_id,
                command: "conflict resolve",
                kind: "conflict_resolved",
                created_at_ms: now,
            },
            Some(&evidence_hash),
        );
        tx.execute(
            "INSERT INTO operations (
                id, repo_id, request_id, command, status, kind, parent_operation_id,
                resulting_view_id, error_json, content_hash, created_at_ms
            ) VALUES (?1, ?2, ?3, 'conflict resolve', 'success', 'conflict_resolved', ?4, ?5, NULL, ?6, ?7)",
            params![
                operation_id,
                context.repo_id,
                request_id.clone(),
                context.current_operation_id,
                view_id,
                content_hash,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'conflict_resolved', ?4, ?5)",
            params![
                view_id,
                context.repo_id,
                operation_id,
                json!({
                    "lifecycle": "conflict_resolved",
                    "conflict_set_id": conflict_set_id,
                    "proposal_id": proposal.proposal_id,
                    "proposal_revision_id": revision_id,
                    "snapshot_id": snapshot_id,
                    "evidence_id": evidence_id,
                    "resolution_ref": resolution_ref,
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
        out = Some(ConflictResolutionRecord {
            conflict_set_id: conflict_set_id.to_string(),
            proposal_id: proposal.proposal_id,
            proposal_revision_id: revision_id,
            snapshot_id,
            evidence_id,
            resolution_ref: resolution_ref.to_string(),
            operation_id: operation_id.clone(),
            view_id: view_id.clone(),
        });
        Ok(())
    })?;
    out.ok_or_else(|| anyhow!("conflict resolution did not produce a record"))
}

pub fn preflight_conflict_resolution(
    cwd: &Path,
    conflict_set_id: &str,
    resolution_ref: &str,
) -> Result<()> {
    let context = open_repository(cwd)?;
    if resolution_ref.starts_with(forge_content::FORGE_TREE_PREFIX) {
        forge_content_native::NativeObjectStore::new(&context.root_path)
            .verify_content_ref(resolution_ref)?;
    } else {
        return Err(anyhow!(
            "conflict resolution requires a forge-tree content ref"
        ));
    }
    let connection = open_connection(&context.database_path)?;
    let (paths_json, status, resolver_backend): (String, String, Option<String>) = connection
        .query_row(
            "SELECT paths_json, status, resolver_backend FROM conflict_sets WHERE id = ?1 AND repo_id = ?2",
            params![conflict_set_id, context.repo_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| ForgeError::ConflictSetNotFound {
            conflict_set_id: conflict_set_id.to_string(),
        })?;
    if resolver_backend.as_deref() != Some("native_merge") {
        return Err(ForgeError::UnsupportedContentBackend {
            command: "conflict resolve".to_string(),
            required: "native_merge".to_string(),
            actual: resolver_backend.unwrap_or_else(|| "unknown".to_string()),
        }
        .into());
    }
    if status == "resolved" {
        return Err(anyhow!("conflict set is already resolved"));
    }
    let proposal_id = serde_json::from_str::<Value>(&paths_json)
        .ok()
        .and_then(|value| {
            value
                .get("proposal_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| anyhow!("conflict set has no proposal binding"))?;
    proposal_by_id_on(&connection, &context, &proposal_id)?.ok_or({
        ForgeError::UnknownProposal {
            selector: proposal_id,
        }
    })?;
    Ok(())
}

fn prepare_stale_base_conflict(
    context: &RepositoryContext,
    operation_id: &str,
    conflict_set_id: &str,
    now: i64,
    input: &StaleBaseConflictInput,
) -> Result<PreparedConflict> {
    let (kept, dropped) = forge_content::filter_secret_risk(&input.changed_paths);
    let paths_json = json!({
        "expected_head": input.expected_head,
        "actual_head": input.actual_head,
        "paths": kept,
        "redacted_count": dropped.len(),
    })
    .to_string();
    let mut path_rows = Vec::with_capacity(kept.len());
    for path in kept {
        path_rows.push(ConflictPathRow {
            id: new_id("path_conflict"),
            path_fingerprint: integrity::path_fingerprint(&path),
            base_path: Some(path.clone()),
            ours_path: Some(path.clone()),
            theirs_path: Some(path.clone()),
            path,
            kind: "content".to_string(),
            base_ref: Some(input.base_content_ref.clone()),
            ours_ref: Some(input.ours_content_ref.clone()),
            theirs_ref: Some(input.theirs_content_ref.clone()),
            base_status: None,
            ours_status: None,
            theirs_status: None,
            base_mode: None,
            ours_mode: None,
            theirs_mode: None,
            resolution_ref: None,
            status: "unresolved".to_string(),
            created_at_ms: now,
        });
    }
    let digest_rows = path_rows
        .iter()
        .map(|row| row.digest_input())
        .collect::<Vec<_>>();
    let content_hash = integrity::conflict_set_digest(&integrity::ConflictSetDigestInput {
        id: conflict_set_id,
        repo_id: &context.repo_id,
        context: &input.context,
        paths_json: &paths_json,
        base_content_ref: Some(&input.base_content_ref),
        ours_content_ref: Some(&input.ours_content_ref),
        theirs_content_ref: Some(&input.theirs_content_ref),
        generated_by_operation_id: Some(operation_id),
        resolver_backend: Some("stale_base"),
        status: "unresolved",
        created_at_ms: now,
        path_conflicts: &digest_rows,
    });
    Ok(PreparedConflict {
        id: conflict_set_id.to_string(),
        context: input.context.clone(),
        paths_json,
        base_content_ref: input.base_content_ref.clone(),
        ours_content_ref: input.ours_content_ref.clone(),
        theirs_content_ref: input.theirs_content_ref.clone(),
        resolver_backend: "stale_base".to_string(),
        status: "unresolved".to_string(),
        content_hash,
        path_rows,
        created_at_ms: now,
        repo_id: context.repo_id.clone(),
    })
}

fn prepare_merge_conflict(
    context: &RepositoryContext,
    operation_id: &str,
    conflict_set_id: &str,
    now: i64,
    input: &MergeConflictInput,
) -> Result<PreparedConflict> {
    let mut redacted_count = 0usize;
    let mut kept_paths = Vec::new();
    let mut path_rows = Vec::new();
    for conflict in &input.conflicts {
        if forge_content::is_secret_risk_path(&conflict.path) {
            redacted_count += 1;
            continue;
        }
        kept_paths.push(conflict.path.clone());
        path_rows.push(ConflictPathRow {
            id: new_id("path_conflict"),
            path_fingerprint: integrity::path_fingerprint(&conflict.path),
            path: conflict.path.clone(),
            base_path: conflict_path_if_present(&conflict.base_status, &conflict.path),
            ours_path: conflict_path_if_present(&conflict.ours_status, &conflict.path),
            theirs_path: conflict_path_if_present(&conflict.theirs_status, &conflict.path),
            kind: native_conflict_kind(conflict.kind).to_string(),
            base_ref: conflict.base_ref.clone(),
            ours_ref: conflict.ours_ref.clone(),
            theirs_ref: conflict.theirs_ref.clone(),
            base_status: conflict.base_status.clone(),
            ours_status: conflict.ours_status.clone(),
            theirs_status: conflict.theirs_status.clone(),
            base_mode: conflict.base_mode.map(|mode| format!("{mode:o}")),
            ours_mode: conflict.ours_mode.map(|mode| format!("{mode:o}")),
            theirs_mode: conflict.theirs_mode.map(|mode| format!("{mode:o}")),
            resolution_ref: None,
            status: "unresolved".to_string(),
            created_at_ms: now,
        });
    }
    let paths_json = json!({
        "proposal_id": input.proposal_id,
        "base_head": input.base_head,
        "ours_head": input.ours_head,
        "paths": kept_paths,
        "redacted_count": redacted_count,
    })
    .to_string();
    let digest_rows = path_rows
        .iter()
        .map(|row| row.digest_input())
        .collect::<Vec<_>>();
    let content_hash = integrity::conflict_set_digest(&integrity::ConflictSetDigestInput {
        id: conflict_set_id,
        repo_id: &context.repo_id,
        context: &input.context,
        paths_json: &paths_json,
        base_content_ref: Some(&input.base_content_ref),
        ours_content_ref: Some(&input.ours_content_ref),
        theirs_content_ref: Some(&input.theirs_content_ref),
        generated_by_operation_id: Some(operation_id),
        resolver_backend: Some("native_merge"),
        status: "unresolved",
        created_at_ms: now,
        path_conflicts: &digest_rows,
    });
    Ok(PreparedConflict {
        id: conflict_set_id.to_string(),
        context: input.context.clone(),
        paths_json,
        base_content_ref: input.base_content_ref.clone(),
        ours_content_ref: input.ours_content_ref.clone(),
        theirs_content_ref: input.theirs_content_ref.clone(),
        resolver_backend: "native_merge".to_string(),
        status: "unresolved".to_string(),
        content_hash,
        path_rows,
        created_at_ms: now,
        repo_id: context.repo_id.clone(),
    })
}

fn conflict_path_if_present(status: &Option<String>, path: &str) -> Option<String> {
    (status.as_deref() == Some("present")).then(|| path.to_string())
}

fn native_conflict_kind(kind: forge_content_native::NativeMergeConflictKind) -> &'static str {
    match kind {
        forge_content_native::NativeMergeConflictKind::Content => "content",
        forge_content_native::NativeMergeConflictKind::Binary => "binary",
        forge_content_native::NativeMergeConflictKind::DeleteModify => "delete_modify",
        forge_content_native::NativeMergeConflictKind::Rename => "rename",
        forge_content_native::NativeMergeConflictKind::DirFile => "dir_file",
        forge_content_native::NativeMergeConflictKind::Mode => "mode",
        forge_content_native::NativeMergeConflictKind::Symlink => "symlink",
    }
}

fn insert_prepared_conflict(
    tx: &Transaction<'_>,
    _context: &RepositoryContext,
    operation_id: &str,
    prepared: &PreparedConflict,
) -> Result<()> {
    tx.execute(
        "INSERT INTO conflict_sets (
            id, repo_id, context, paths_json, created_at_ms, base_content_ref,
            ours_content_ref, theirs_content_ref, generated_by_operation_id,
            resolver_backend, status, content_hash
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            prepared.id,
            prepared.repo_id,
            prepared.context,
            prepared.paths_json,
            prepared.created_at_ms,
            prepared.base_content_ref,
            prepared.ours_content_ref,
            prepared.theirs_content_ref,
            operation_id,
            prepared.resolver_backend,
            prepared.status,
            prepared.content_hash,
        ],
    )?;
    for row in &prepared.path_rows {
        tx.execute(
            "INSERT INTO path_conflicts (
                id, conflict_set_id, path, path_fingerprint, base_path, ours_path, theirs_path,
                kind, base_ref, ours_ref, theirs_ref, base_status, ours_status, theirs_status,
                base_mode, ours_mode, theirs_mode, resolution_ref, status, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                row.id,
                prepared.id,
                row.path,
                row.path_fingerprint,
                row.base_path,
                row.ours_path,
                row.theirs_path,
                row.kind,
                row.base_ref,
                row.ours_ref,
                row.theirs_ref,
                row.base_status,
                row.ours_status,
                row.theirs_status,
                row.base_mode,
                row.ours_mode,
                row.theirs_mode,
                row.resolution_ref,
                row.status,
                row.created_at_ms,
            ],
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PreparedConflict {
    id: String,
    repo_id: String,
    context: String,
    paths_json: String,
    base_content_ref: String,
    ours_content_ref: String,
    theirs_content_ref: String,
    resolver_backend: String,
    status: String,
    content_hash: String,
    path_rows: Vec<ConflictPathRow>,
    created_at_ms: i64,
}

#[derive(Debug, Clone)]
struct ConflictPathRow {
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
    resolution_ref: Option<String>,
    status: String,
    created_at_ms: i64,
}

impl ConflictPathRow {
    fn digest_input(&self) -> integrity::PathConflictDigestInput<'_> {
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
            resolution_ref: self.resolution_ref.as_deref(),
            status: &self.status,
            created_at_ms: self.created_at_ms,
        }
    }
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

pub fn conflict_list(cwd: &Path) -> Result<ConflictListRecord> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    Ok(ConflictListRecord {
        conflicts: query_conflict_summaries(&connection, None)?,
    })
}

pub fn conflict_show(
    cwd: &Path,
    conflict_set_id: &str,
    suggest: bool,
) -> Result<ConflictShowRecord> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let mut conflicts = query_conflict_summaries(&connection, Some(conflict_set_id))?;
    let Some(conflict) = conflicts.pop() else {
        return Err(ForgeError::ConflictSetNotFound {
            conflict_set_id: conflict_set_id.to_string(),
        }
        .into());
    };
    let mut statement = connection.prepare(
        "SELECT id, path_fingerprint, kind, base_ref, ours_ref, theirs_ref,
                base_status, ours_status, theirs_status, base_mode, ours_mode,
                theirs_mode, resolution_ref, status
         FROM path_conflicts
         WHERE conflict_set_id = ?1
         ORDER BY rowid",
    )?;
    let path_conflicts = statement
        .query_map(params![conflict_set_id], |row| {
            Ok(PathConflictSummary {
                path_conflict_id: row.get(0)?,
                path_fingerprint: row.get(1)?,
                kind: row.get(2)?,
                base_ref: row.get(3)?,
                ours_ref: row.get(4)?,
                theirs_ref: row.get(5)?,
                base_status: row.get(6)?,
                ours_status: row.get(7)?,
                theirs_status: row.get(8)?,
                base_mode: row.get(9)?,
                ours_mode: row.get(10)?,
                theirs_mode: row.get(11)?,
                resolution_ref: row.get(12)?,
                status: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<PathConflictSummary>>>()?;
    let suggestions = if suggest {
        let paths_json = conflict_paths_json(&connection, conflict_set_id)?;
        let inputs = conflict_suggestion_inputs(
            &connection,
            &context.repo_id,
            &paths_json,
            conflict.theirs_content_ref.as_deref(),
        )?;
        conflict_resolution_suggestions(&conflict, &path_conflicts, &inputs)
    } else {
        Vec::new()
    };
    Ok(ConflictShowRecord {
        conflict,
        path_conflicts,
        suggestions,
    })
}

fn conflict_resolution_suggestions(
    conflict: &ConflictSetSummary,
    path_conflicts: &[PathConflictSummary],
    inputs: &ConflictSuggestionInputs,
) -> Vec<ConflictResolutionSuggestion> {
    if conflict.status != "unresolved"
        || conflict.resolver_backend.as_deref() != Some("native_merge")
    {
        return Vec::new();
    }

    let path_conflict_ids = path_conflicts
        .iter()
        .map(|path_conflict| path_conflict.path_conflict_id.clone())
        .collect::<Vec<_>>();
    let source_refs = [
        conflict.base_content_ref.as_ref(),
        conflict.ours_content_ref.as_ref(),
        conflict.theirs_content_ref.as_ref(),
    ]
    .into_iter()
    .flatten()
    .cloned()
    .collect::<Vec<_>>();
    let evidence_input_count = inputs.evidence_input_ids.len() as i64;
    let evidence_input_status = if evidence_input_count == 0 {
        "empty"
    } else {
        "present"
    };
    let provenance = ConflictResolutionSuggestionProvenance {
        conflict_set_id: conflict.conflict_set_id.clone(),
        proposal_id: inputs.proposal_id.clone(),
        proposal_revision_id: inputs.proposal_revision_id.clone(),
        evidence_input_count,
        evidence_input_status: evidence_input_status.to_string(),
        evidence_input_ids: inputs.evidence_input_ids.clone(),
        check_input_status: inputs.check_input_status.clone(),
        intent_input_status: "conflict_set_metadata".to_string(),
        path_conflict_ids,
        source_refs,
    };
    let mut suggestions = Vec::new();
    if let Some(resolution_ref) = &conflict.ours_content_ref {
        suggestions.push(ConflictResolutionSuggestion {
            suggestion_id: "suggestion_keep_current_head".to_string(),
            rank: 1,
            resolution_ref: resolution_ref.clone(),
            strategy: "keep_current_head_tree".to_string(),
            confidence: "low".to_string(),
            requires_explicit_resolve: true,
            provenance: provenance.clone(),
        });
    }
    if let Some(resolution_ref) = &conflict.theirs_content_ref {
        let duplicate = conflict.ours_content_ref.as_ref() == Some(resolution_ref);
        if !duplicate {
            suggestions.push(ConflictResolutionSuggestion {
                suggestion_id: "suggestion_use_proposal_tree".to_string(),
                rank: suggestions.len() as i64 + 1,
                resolution_ref: resolution_ref.clone(),
                strategy: "use_proposal_tree".to_string(),
                confidence: "low".to_string(),
                requires_explicit_resolve: true,
                provenance,
            });
        }
    }
    suggestions
}

fn conflict_paths_json(connection: &Connection, conflict_set_id: &str) -> Result<String> {
    connection
        .query_row(
            "SELECT paths_json FROM conflict_sets WHERE id = ?1",
            params![conflict_set_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn conflict_suggestion_inputs(
    connection: &Connection,
    repo_id: &str,
    paths_json: &str,
    theirs_content_ref: Option<&str>,
) -> Result<ConflictSuggestionInputs> {
    let proposal_id = serde_json::from_str::<Value>(paths_json)
        .ok()
        .and_then(|value| {
            value
                .get("proposal_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let Some(proposal_id) = proposal_id else {
        return Ok(ConflictSuggestionInputs::default());
    };
    let Some(theirs_content_ref) = theirs_content_ref else {
        return Ok(ConflictSuggestionInputs {
            proposal_id: Some(proposal_id),
            ..ConflictSuggestionInputs::default()
        });
    };
    let proposal: Option<(String, String)> = connection
        .query_row(
            "SELECT p.attempt_id, pr.id
             FROM proposals p
             JOIN proposal_revisions pr ON pr.proposal_id = p.id
             WHERE p.repo_id = ?1 AND p.id = ?2 AND pr.content_ref = ?3
             ORDER BY pr.created_at_ms DESC, pr.rowid DESC LIMIT 1",
            params![repo_id, proposal_id, theirs_content_ref],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((attempt_id, proposal_revision_id)) = proposal else {
        return Ok(ConflictSuggestionInputs {
            proposal_id: Some(proposal_id),
            ..ConflictSuggestionInputs::default()
        });
    };
    let mut evidence_statement = connection.prepare(
        "SELECT id FROM evidence
         WHERE repo_id = ?1 AND attempt_id = ?2
         ORDER BY created_at_ms DESC, rowid DESC",
    )?;
    let evidence_input_ids = evidence_statement
        .query_map(params![repo_id, attempt_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let check_input_status = connection
        .query_row(
            "SELECT status FROM check_results
             WHERE repo_id = ?1 AND proposal_revision_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![repo_id, proposal_revision_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(ConflictSuggestionInputs {
        proposal_id: Some(proposal_id),
        proposal_revision_id: Some(proposal_revision_id),
        evidence_input_ids,
        check_input_status,
    })
}

fn query_conflict_summaries(
    connection: &Connection,
    conflict_set_id: Option<&str>,
) -> Result<Vec<ConflictSetSummary>> {
    let sql = if conflict_set_id.is_some() {
        "SELECT cs.id, cs.context, cs.paths_json, cs.base_content_ref, cs.ours_content_ref,
                cs.theirs_content_ref, cs.generated_by_operation_id, cs.resolver_backend,
                cs.status, COUNT(pc.id)
         FROM conflict_sets cs
         LEFT JOIN path_conflicts pc ON pc.conflict_set_id = cs.id
         WHERE cs.id = ?1
         GROUP BY cs.id
         ORDER BY cs.created_at_ms, cs.rowid"
    } else {
        "SELECT cs.id, cs.context, cs.paths_json, cs.base_content_ref, cs.ours_content_ref,
                cs.theirs_content_ref, cs.generated_by_operation_id, cs.resolver_backend,
                cs.status, COUNT(pc.id)
         FROM conflict_sets cs
         LEFT JOIN path_conflicts pc ON pc.conflict_set_id = cs.id
         GROUP BY cs.id
         ORDER BY cs.created_at_ms, cs.rowid"
    };
    let mut statement = connection.prepare(sql)?;
    let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<ConflictSetSummary> {
        let paths_json: String = row.get(2)?;
        let redacted_count = conflict_redacted_count(&paths_json);
        let warnings = if redacted_count > 0 {
            vec![format!(
                "redacted {redacted_count} secret-risk path(s) from conflict metadata"
            )]
        } else {
            Vec::new()
        };
        Ok(ConflictSetSummary {
            conflict_set_id: row.get(0)?,
            context: row.get(1)?,
            base_content_ref: row.get(3)?,
            ours_content_ref: row.get(4)?,
            theirs_content_ref: row.get(5)?,
            generated_by_operation_id: row.get(6)?,
            resolver_backend: row.get(7)?,
            status: row.get(8)?,
            path_conflict_count: row.get(9)?,
            redacted_count,
            warnings,
        })
    };
    let rows = if let Some(id) = conflict_set_id {
        statement
            .query_map(params![id], map_row)?
            .collect::<rusqlite::Result<_>>()?
    } else {
        statement
            .query_map([], map_row)?
            .collect::<rusqlite::Result<_>>()?
    };
    Ok(rows)
}

fn conflict_redacted_count(paths_json: &str) -> i64 {
    serde_json::from_str::<Value>(paths_json)
        .ok()
        .and_then(|value| value.get("redacted_count").and_then(Value::as_i64))
        .unwrap_or(0)
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

pub fn resolve_attempt(cwd: &Path, attempt_id: Option<&str>) -> Result<ResolvedAttempt> {
    let context = open_repository(cwd)?;
    resolve_attempt_in_context(&context, attempt_id)
}

fn resolve_attempt_in_context(
    context: &RepositoryContext,
    attempt_id: Option<&str>,
) -> Result<ResolvedAttempt> {
    if let Some(attempt_id) = attempt_id {
        let attempt =
            attempt_by_id(context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
                selector: attempt_id.to_string(),
            })?;
        return Ok(ResolvedAttempt { attempt });
    }

    if let Some(workspace_attempt_id) = context.workspace_attempt_id.as_deref() {
        if let Some(attempt) = attempt_by_id(context, workspace_attempt_id)? {
            if attempt.status == "active" {
                return Ok(ResolvedAttempt { attempt });
            }
        }
    }

    if let Some(attached_attempt_id) = context.attached_attempt_id.as_deref() {
        if let Some(attempt) = attempt_by_id(context, attached_attempt_id)? {
            if attempt.status == "active" {
                return Ok(ResolvedAttempt { attempt });
            }
        }
    }

    let attempts = active_attempts(context)?;
    match attempts.as_slice() {
        [] => Err(ForgeError::NoActiveAttempt.into()),
        [attempt] => Ok(ResolvedAttempt {
            attempt: attempt.clone(),
        }),
        _ => Err(ForgeError::AmbiguousAttempt {
            candidate_ids: attempts
                .iter()
                .map(|attempt| attempt.attempt_id.clone())
                .collect(),
        }
        .into()),
    }
}

fn attempt_by_id(context: &RepositoryContext, attempt_id: &str) -> Result<Option<AttemptRecord>> {
    let connection = open_connection(&context.database_path)?;
    connection
        .query_row(
            "SELECT a.id, a.intent_id, i.text, a.base_head, a.status
             FROM attempts a
             JOIN intents i ON i.id = a.intent_id
             WHERE a.repo_id = ?1 AND a.id = ?2",
            params![context.repo_id, attempt_id],
            |row| {
                Ok(AttemptRecord {
                    attempt_id: row.get(0)?,
                    intent_id: row.get(1)?,
                    intent: row.get(2)?,
                    base_head: row.get(3)?,
                    status: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn active_attempts(context: &RepositoryContext) -> Result<Vec<AttemptRecord>> {
    let connection = open_connection(&context.database_path)?;
    let mut statement = connection.prepare(
        "SELECT a.id, a.intent_id, i.text, a.base_head, a.status
         FROM attempts a
         JOIN intents i ON i.id = a.intent_id
         WHERE a.repo_id = ?1 AND a.status = 'active'
         ORDER BY a.created_at_ms ASC",
    )?;
    let rows = statement.query_map(params![context.repo_id], |row| {
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

pub fn list_attempts(cwd: &Path) -> Result<Vec<AttemptSummary>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let mut statement = connection.prepare(
        "SELECT a.id, a.intent_id, i.text, a.base_head, a.status,
                COALESCE(aw.workspace_rel_path, '.forge/worktrees/' || a.id)
         FROM attempts a
         JOIN intents i ON i.id = a.intent_id
         LEFT JOIN attempt_workspaces aw ON aw.attempt_id = a.id AND aw.repo_id = a.repo_id
         WHERE a.repo_id = ?1
         ORDER BY a.created_at_ms ASC",
    )?;
    let attached = context.attached_attempt_id.clone();
    let rows = statement.query_map(params![context.repo_id], |row| {
        let attempt_id: String = row.get(0)?;
        Ok(AttemptSummary {
            attached: attached.as_deref() == Some(attempt_id.as_str()),
            attempt_id,
            intent_id: row.get(1)?,
            intent: row.get(2)?,
            base_head: row.get(3)?,
            status: row.get(4)?,
            workspace_path: row.get(5)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
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

/// The latest proposal's `content_ref` for an attempt — the diffable tree the pairwise
/// `compare --diff` path feeds to the CLI diff router. Errors `UnknownAttempt`
/// when the attempt does not exist, `NoProposal` when it has no proposal yet.
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

pub fn show_attempt(cwd: &Path, attempt_id: &str) -> Result<AttemptShowRecord> {
    let context = open_repository(cwd)?;
    let attempt =
        attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?;
    Ok(AttemptShowRecord {
        attempt: AttemptSummary {
            attached: context.attached_attempt_id.as_deref() == Some(attempt.attempt_id.as_str()),
            attempt_id: attempt.attempt_id.clone(),
            intent_id: attempt.intent_id.clone(),
            intent: attempt.intent.clone(),
            base_head: attempt.base_head.clone(),
            status: attempt.status.clone(),
            workspace_path: attempt_workspace_rel_path(&context, &attempt.attempt_id)?,
        },
        latest_snapshot: latest_snapshot_for_attempt(&context, &attempt.attempt_id)?,
        latest_evidence: latest_evidence_for_attempt(&context, &attempt.attempt_id)?,
        proposals: proposal_metadata_for_attempt(&context, &attempt.attempt_id)?,
    })
}

pub fn attach_attempt(
    cwd: &Path,
    request_id: Option<String>,
    attempt_id: &str,
    content_ref: &str,
) -> Result<OperationViewResult> {
    let context = open_repository(cwd)?;
    let attempt =
        attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?;
    let mut connection = open_connection(&context.database_path)?;
    let op = with_immediate_retry(&mut connection, |tx| {
        replay_guard(tx, &context.repo_id, request_id.as_deref())?;
        let op = insert_operation_view(
            tx,
            &context.repo_id,
            Some(&context.current_operation_id),
            OperationViewInput {
                request_id: request_id.clone(),
                command: "attempt attach".to_string(),
                kind: "attempt_attached".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({
                    "lifecycle": "attempt_attached",
                    "attempt_id": attempt.attempt_id
                }),
            },
        )?;
        tx.execute(
            "UPDATE current_state SET attached_attempt_id = ?1 WHERE singleton = 1",
            params![attempt.attempt_id],
        )?;
        // NER-143 R1: `attempt attach` is the FIFTH materializing op — the CLI restores the
        // attached attempt's tree into the worktree before calling this. Set the expected
        // baseline atomically with the attach (code-review P1: without this, an attach-then-nav
        // spuriously fails DIRTY_WORKTREE because `expected` still points at the prior attempt's
        // tree). Same dedicated-UPDATE discipline as the other recorders (DR-F2).
        set_context_expected_content_ref(tx, &context, content_ref)?;
        Ok(op)
    })?;
    Ok(op)
}

pub fn attempt_materialization_ref(cwd: &Path, attempt_id: &str) -> Result<Option<String>> {
    let context = open_repository(cwd)?;
    let attempt =
        attempt_by_id(&context, attempt_id)?.ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?;
    Ok(latest_snapshot_for_attempt(&context, &attempt.attempt_id)?
        .map(|snapshot| snapshot.content_ref))
}

pub fn attempt_base_head(cwd: &Path, attempt_id: &str) -> Result<String> {
    let context = open_repository(cwd)?;
    Ok(attempt_by_id(&context, attempt_id)?
        .ok_or_else(|| ForgeError::UnknownAttempt {
            selector: attempt_id.to_string(),
        })?
        .base_head)
}

fn latest_snapshot_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<SnapshotSummary>> {
    let connection = open_connection(&context.database_path)?;
    latest_snapshot_on(&connection, attempt_id)
}

/// Determining "latest snapshot" read against a caller-supplied connection. A
/// writer passes its own `IMMEDIATE` transaction (`&tx` deref-coerces to
/// `&Connection`) so the read-then-write is atomic on one connection (U4).
fn latest_snapshot_on(
    connection: &Connection,
    attempt_id: &str,
) -> Result<Option<SnapshotSummary>> {
    connection
        .query_row(
            "SELECT id, content_ref, changed_paths_json FROM snapshots
             WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            params![attempt_id],
            |row| {
                let changed_paths_json: String = row.get(2)?;
                Ok(SnapshotSummary {
                    snapshot_id: row.get(0)?,
                    content_ref: row.get(1)?,
                    changed_paths: serde_json::from_str(&changed_paths_json).unwrap_or_default(),
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn latest_evidence_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<EvidenceSummary>> {
    let connection = open_connection(&context.database_path)?;
    latest_evidence_on(&connection, attempt_id)
}

/// Determining "latest evidence" read against a caller-supplied connection (see
/// [`latest_snapshot_on`]); used inside `record_check`'s `IMMEDIATE` txn (U4).
fn latest_evidence_on(
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
fn resolve_proposal(
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

fn proposal_by_id(
    context: &RepositoryContext,
    proposal_id: &str,
) -> Result<Option<ProposalSummary>> {
    let connection = open_connection(&context.database_path)?;
    proposal_by_id_on(&connection, context, proposal_id)
}

fn proposal_by_id_on(
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

fn resolved_merge_parents_for_proposal_on(
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

fn proposals_for_attempt(
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

fn latest_proposal_for_attempt(
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

fn sanitize_review_changed_paths(
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

fn proposal_metadata_for_attempt(
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

fn latest_check_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<CheckSummary>> {
    let proposal = match latest_proposal_for_attempt(context, attempt_id)? {
        Some(proposal) => proposal,
        None => return Ok(None),
    };
    latest_check_for_proposal_revision(context, &proposal.proposal_revision_id)
}

fn latest_check_for_proposal_revision(
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

fn latest_decision_for_context(context: &RepositoryContext) -> Result<Option<String>> {
    let attempt = match resolve_attempt_in_context(context, None) {
        Ok(attempt) => attempt.attempt,
        Err(_) => return Ok(None),
    };
    latest_decision_for_attempt(context, &attempt.attempt_id)
}

fn latest_decision_for_attempt(
    context: &RepositoryContext,
    attempt_id: &str,
) -> Result<Option<String>> {
    let proposal = match latest_proposal_for_attempt(context, attempt_id)? {
        Some(proposal) => proposal,
        None => return Ok(None),
    };
    latest_decision_for_proposal_revision(context, &proposal.proposal_revision_id)
}

fn latest_decision_for_proposal_revision(
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

fn latest_publication_for_proposal_revision(
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

        let sanitized = sanitize_review_changed_paths(&paths, &labels);

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
