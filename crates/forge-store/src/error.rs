//! Typed `ForgeError` taxonomy (NER-133 U1).
//!
//! Forge's machine contract historically decoded the error *code* from free-text
//! error messages: a substring ladder in the CLI (`error_code()`) plus `bail!`
//! string contracts scattered across `forge-store`, `forge-export-git`, and the
//! CLI itself. That made the contract fragile-by-construction — a reworded
//! message silently changed an agent-visible code, and `retry.retryable` could
//! not be classified per-error.
//!
//! `ForgeError` is the *sanctioned* taxonomy exception to CLAUDE.md's "no custom
//! error types" rule. It generalizes the existing `RequestIdReplay` / `LockTimeout`
//! sentinel pattern: a variant is constructed at the failure site, carried inside
//! an `anyhow::Error`, and recovered at the CLI via `downcast_ref`. No writer
//! return signature changes — the variant rides the existing `Result<_>`.
//!
//! Each variant owns its agent-visible `code()` (the exact string the deleted
//! `error_code()` produced), its `retryable()` classification, an `after_ms()`
//! backoff hint, and a structured (secret-redacted) `details()` payload.

use forge_protocol::RETRY_BACKOFF_MS;
use serde::Serialize;
use serde_json::{json, Value};

/// Placeholder substituted for a secret-risk path in any machine-visible payload,
/// so a secret filename never reaches `errors[].details` or the persisted ledger.
const REDACTED_PATH_PLACEHOLDER: &str = "[secret-risk path redacted]";

/// The class of integrity break `doctor`/the gate detected on a hashed row (NER-136).
/// A closed enum so [`ForgeError::EvidenceTampered`]'s `details` can never carry a
/// free-form string (e.g. an excerpt) into a machine-visible payload. Serializes as
/// snake_case (`content_edit`/`broken_link`/`missing_hash`) — kept in lockstep with
/// [`TamperKind::as_str`] by the `serde`-vs-`as_str` parity test in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TamperKind {
    /// The row's recomputed content hash does not match its stored `content_hash`.
    ContentEdit,
    /// An operation's chain link does not match its predecessor (deletion/reorder).
    BrokenLink,
    /// A hash is NULL on a row created after the migration high-water mark.
    MissingHash,
}

impl TamperKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TamperKind::ContentEdit => "content_edit",
            TamperKind::BrokenLink => "broken_link",
            TamperKind::MissingHash => "missing_hash",
        }
    }
}

/// The class of native-history corruption `doctor`/`reconcile`/`log`/checkout detected
/// when walking the commit DAG (NER-138 Phase 7 slice 3). A closed enum so
/// [`ForgeError::NativeHistoryCorrupt`]'s `details` — and the `DoctorReport` finding it
/// also feeds — can never carry a free-form string (a path/excerpt) into a machine-visible
/// payload. Serializes as snake_case, kept in lockstep with [`NativeHistoryCorruptKind::as_str`]
/// by the `serde`-vs-`as_str` parity test in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHistoryCorruptKind {
    /// A commit is reachable from itself through its parent chain (forged/corrupt store).
    Cycle,
    /// A commit names a parent commit whose object is absent from the store.
    DanglingParent,
    /// A commit names a root tree whose object is absent from the store.
    DanglingTree,
    /// A `decisions.commit_id` (or ledger-recorded tip) references a commit with no object.
    DanglingCommitId,
}

impl NativeHistoryCorruptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NativeHistoryCorruptKind::Cycle => "cycle",
            NativeHistoryCorruptKind::DanglingParent => "dangling_parent",
            NativeHistoryCorruptKind::DanglingTree => "dangling_tree",
            NativeHistoryCorruptKind::DanglingCommitId => "dangling_commit_id",
        }
    }
}

/// Typed Forge error taxonomy. Constructed at the failure site, carried inside an
/// `anyhow::Error`, recovered at the CLI by `downcast_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    /// `accept`/`export` ran against a moved HEAD.
    ///
    /// Schema-doc note (NER-138 slice 3 / NER-143 R11): for a NATIVE repo, `expected_head`
    /// now carries the accepted `commit_id` (commit-on-accept advanced the base past the
    /// pre-accept `base_head`), not `base_head` itself — correct and internally consistent
    /// post-commit-on-accept. A git-backend repo is unchanged. Both fields stay opaque
    /// `f1:`/git ids, never paths.
    StaleBase {
        expected_head: String,
        actual_head: String,
    },
    /// A snapshot-consuming command found unsaved worktree changes.
    DirtyWorktree { paths: Vec<String> },
    /// More than one active attempt matched a default (unqualified) selector.
    AmbiguousAttempt { candidate_ids: Vec<String> },
    /// A named attempt did not resolve.
    UnknownAttempt { selector: String },
    /// More than one proposal matched a default (unqualified) selector.
    AmbiguousProposal { candidate_ids: Vec<String> },
    /// A named proposal did not resolve (or did not belong to the attempt).
    UnknownProposal { selector: String },
    /// A named intent did not resolve.
    UnknownIntent { selector: String },
    /// No active attempt exists to operate on.
    NoActiveAttempt,
    /// No snapshot has been saved for the active attempt.
    NoSnapshot,
    /// No proposal exists for the attempt.
    NoProposal,
    /// The proposal is not in the accepted state.
    NotAccepted,
    /// The proposal was explicitly rejected.
    Rejected,
    /// The target export branch already exists with diverging content.
    BranchExists { name: String },
    /// The repository has not been `forge init`-ed.
    NotInitialized,
    /// A `--request-id` was reused for a different command.
    RequestIdConflict { existing_command: String },
    /// The genuine optimistic singleton CAS in `insert_operation_view` lost the
    /// race (another writer advanced `current_state` concurrently). **Transient /
    /// retryable** — the only domain error a client may safely re-run.
    CurrentStateChanged,
    /// The DB schema is ahead of this binary's supported head — read-only refuse.
    /// (Raised by a later unit; defined now so the taxonomy is complete.)
    UnknownSchemaVersion {
        db_version: i64,
        supported_head: i64,
    },
    /// A migration failed to apply. (Raised by a later unit; defined now.)
    MigrationFailed { version: i64, message: String },
    /// `save`/`restore` targeted an attempt different from the one the worktree is
    /// currently materialized for (`attached_attempt_id`). Recording the worktree's
    /// content under `requested_attempt` would silently contaminate it with
    /// `attached_attempt`'s content (NER-134). Deterministic — re-run after
    /// `attempt attach <requested_attempt>`. Both fields are opaque minted attempt
    /// ids, never paths, so [`ForgeError::details`] emits them un-redacted.
    AttemptWorktreeMismatch {
        requested_attempt: String,
        attached_attempt: String,
    },
    /// `accept` is gated on a passing check by default (NER-135 R6) but the
    /// proposal's check did not pass. `status` is the overall check status
    /// (`failed`/`missing`/`stale`); `unmet` lists the `"program arg…"` identities
    /// of the non-passed gates. Deterministic — run the required gates on the
    /// proposed snapshot (or `accept --allow-unverified` to bypass with a warning).
    /// `unmet` entries are redacted for secret-like `key=value` argv in
    /// [`ForgeError::details`]; [`std::fmt::Display`] never prints them.
    CheckNotPassed { status: String, unmet: Vec<String> },
    /// A hashed evidence or decision row failed integrity verification (NER-136): its
    /// recomputed content hash does not match what was chained into the op-log spine,
    /// a chain link is broken, or a post-watermark hash is missing. **Fail-closed and
    /// never bypassable** — unlike a policy verdict, `accept --allow-unverified` does
    /// NOT skip it. Deterministic — re-record honest evidence. `id` is an opaque row
    /// id and `kind` a closed enum, so `details` carries no excerpt/command text.
    EvidenceTampered { id: String, kind: TamperKind },
    /// A published commit's provenance trailer does not match the local ledger
    /// (NER-137): `verify-branch` recomputed the content-addressed digest from the
    /// deciding evidence/decision rows and it differs from the `Forge-Provenance-Digest`
    /// the commit carries — the commit was rewritten, or a ledger row was edited without
    /// re-export. Fail-closed and non-retryable. A PASS proves trailer↔current-ledger
    /// consistency, NOT authenticity (an attacker who rewrites the ledger AND re-exports
    /// still matches — cross-machine authenticity is Phase 9 signing). `details` carries
    /// only the opaque proposal id and the two digests (no excerpt/path).
    ProvenanceMismatch {
        proposal_id: String,
        published_digest: String,
        recomputed_digest: String,
    },
    /// A published commit's local signature fingerprint trailer does not match the
    /// locally verified decision signature fingerprint. This is local authenticity
    /// only, not hosted or third-party attestation.
    LocalSignatureMismatch {
        proposal_id: String,
        published_fingerprint: String,
        recomputed_fingerprint: String,
    },
    /// A commit handed to `verify-branch` carries no Forge provenance trailer
    /// (NER-137): it was not produced by `forge export branch` (a plain git commit, or
    /// one predating Phase 6). Distinct from `PROVENANCE_MISMATCH` (a trailer that
    /// disagrees with the ledger) so an agent gating CI can tell "not a Forge artifact"
    /// from "tampered/mismatched". `branch` is the ref the caller passed; `missing_field`
    /// names the absent trailer line. Non-retryable; carries no path/excerpt.
    MissingProvenanceTrailer {
        branch: String,
        missing_field: String,
    },
    /// The native commit DAG failed an integrity walk (NER-138 Phase 7 slice 3): a parent
    /// cycle, a dangling parent/tree object, or a `decisions.commit_id`/ledger tip whose
    /// commit object is absent. Distinguishes genuine corruption from transient IO (which
    /// stays path-free `anyhow`) so an agent can tell "the history is broken" from "a read
    /// failed". Fail-closed and **non-retryable**. `commit_id` is the subject commit and
    /// `related_id` the missing parent/tree (or the cycle-closing ancestor) when applicable
    /// — both opaque `f1:` ids, never paths/excerpts, so `details` emits them un-redacted.
    NativeHistoryCorrupt {
        kind: NativeHistoryCorruptKind,
        commit_id: String,
        related_id: Option<String>,
    },
    /// A read-only conflict inspection command named a conflict set that is not in
    /// the local ledger. The selector is intentionally not echoed because it may
    /// be path-like user input.
    ConflictSetNotFound { conflict_set_id: String },
    /// A real GC delete was confirmed against a stale dry-run plan digest.
    /// Deterministic and non-retryable: rerun `forge gc --dry-run`, review the new
    /// plan, and pass its digest back to `forge gc --yes --plan-digest`.
    GcPlanChanged {
        expected_digest: String,
        actual_digest: String,
    },
    /// A command is valid only for a specific content backend/ref family. Used when
    /// a v0 command is published but deliberately native-only for this slice, so
    /// agents can distinguish "unsupported backend" from an unexpected command
    /// failure. Details carry closed backend labels, never paths or content.
    UnsupportedContentBackend {
        command: String,
        required: String,
        actual: String,
    },
    /// A configured trust policy required a stronger rung than the selected
    /// proposal/revision can currently prove. Deterministic and non-retryable:
    /// re-record/sign the required subjects or lower the local policy.
    TrustPolicyUnmet {
        action: String,
        required_trust: String,
        signature_issues: Vec<crate::SignatureFinding>,
    },
    /// The user configured a trust level this binary does not implement.
    UnsupportedTrustLevel {
        level: String,
        supported: Vec<String>,
    },
}

impl ForgeError {
    /// The exact agent-visible code string. These must remain byte-stable: they
    /// are the published `forge.cli.v0` error registry.
    pub fn code(&self) -> &'static str {
        match self {
            ForgeError::StaleBase { .. } => "STALE_BASE",
            ForgeError::DirtyWorktree { .. } => "DIRTY_WORKTREE",
            ForgeError::AmbiguousAttempt { .. } => "AMBIGUOUS_ATTEMPT",
            ForgeError::UnknownAttempt { .. } => "UNKNOWN_ATTEMPT",
            ForgeError::AmbiguousProposal { .. } => "AMBIGUOUS_PROPOSAL",
            ForgeError::UnknownProposal { .. } => "UNKNOWN_PROPOSAL",
            ForgeError::UnknownIntent { .. } => "UNKNOWN_INTENT",
            ForgeError::NoActiveAttempt => "NO_ACTIVE_ATTEMPT",
            ForgeError::NoSnapshot => "NO_SNAPSHOT",
            ForgeError::NoProposal => "NO_PROPOSAL",
            ForgeError::NotAccepted => "NOT_ACCEPTED",
            ForgeError::Rejected => "REJECTED",
            ForgeError::BranchExists { .. } => "BRANCH_EXISTS",
            ForgeError::NotInitialized => "NOT_INITIALIZED",
            ForgeError::RequestIdConflict { .. } => "REQUEST_ID_CONFLICT",
            ForgeError::CurrentStateChanged => "CONFLICT",
            ForgeError::UnknownSchemaVersion { .. } => "SCHEMA_VERSION_UNSUPPORTED",
            ForgeError::MigrationFailed { .. } => "MIGRATION_FAILED",
            ForgeError::AttemptWorktreeMismatch { .. } => "ATTEMPT_WORKTREE_MISMATCH",
            ForgeError::CheckNotPassed { .. } => "CHECK_NOT_PASSED",
            ForgeError::EvidenceTampered { .. } => "EVIDENCE_TAMPERED",
            ForgeError::ProvenanceMismatch { .. } => "PROVENANCE_MISMATCH",
            ForgeError::LocalSignatureMismatch { .. } => "LOCAL_SIGNATURE_MISMATCH",
            ForgeError::MissingProvenanceTrailer { .. } => "MISSING_PROVENANCE_TRAILER",
            ForgeError::NativeHistoryCorrupt { .. } => "NATIVE_HISTORY_CORRUPT",
            ForgeError::ConflictSetNotFound { .. } => "CONFLICT_SET_NOT_FOUND",
            ForgeError::GcPlanChanged { .. } => "GC_PLAN_CHANGED",
            ForgeError::UnsupportedContentBackend { .. } => "UNSUPPORTED_CONTENT_BACKEND",
            ForgeError::TrustPolicyUnmet { .. } => "TRUST_POLICY_UNMET",
            ForgeError::UnsupportedTrustLevel { .. } => "UNSUPPORTED_TRUST_LEVEL",
        }
    }

    /// Whether a client may safely re-run the command. True only for the genuine
    /// transient CAS in `insert_operation_view` (`CurrentStateChanged`); the
    /// standalone [`crate::LockTimeout`] is also retryable but is classified at the
    /// CLI where it is downcast.
    pub fn retryable(&self) -> bool {
        matches!(self, ForgeError::CurrentStateChanged)
    }

    /// Advisory backoff hint in milliseconds for retryable variants.
    pub fn after_ms(&self) -> Option<u64> {
        if self.retryable() {
            Some(RETRY_BACKOFF_MS)
        } else {
            None
        }
    }

    /// Structured, secret-redacted payload for `errors[].details`.
    pub fn details(&self) -> Value {
        match self {
            ForgeError::StaleBase {
                expected_head,
                actual_head,
            } => json!({ "expected_head": expected_head, "actual_head": actual_head }),
            ForgeError::DirtyWorktree { paths } => redact_paths(paths),
            ForgeError::AmbiguousAttempt { candidate_ids }
            | ForgeError::AmbiguousProposal { candidate_ids } => {
                json!({ "candidate_ids": candidate_ids })
            }
            ForgeError::UnknownAttempt { selector }
            | ForgeError::UnknownProposal { selector }
            | ForgeError::UnknownIntent { selector } => json!({ "selector": selector }),
            ForgeError::BranchExists { name } => json!({ "name": name }),
            ForgeError::RequestIdConflict { existing_command } => {
                json!({ "existing_command": existing_command })
            }
            ForgeError::UnknownSchemaVersion {
                db_version,
                supported_head,
            } => json!({ "db_version": db_version, "supported_head": supported_head }),
            ForgeError::MigrationFailed { version, message } => {
                json!({ "version": version, "message": message })
            }
            ForgeError::AttemptWorktreeMismatch {
                requested_attempt,
                attached_attempt,
            } => json!({
                "requested_attempt": requested_attempt,
                "attached_attempt": attached_attempt,
            }),
            ForgeError::CheckNotPassed { status, unmet } => {
                // Gate identities are argv strings persisted (intents.check_spec_json)
                // and surfaced WITHOUT execution, so — unlike captured evidence, which
                // requires running the command and already redacts its output — they
                // get a redaction pass here for secret-like `key=value` argv (NER-135).
                // Redact PER WHITESPACE TOKEN: `redact_secret_like_text` keys on the
                // first `=`/`:` of its input, so a space-joined identity would only
                // check its first token (code-review F2); tokenizing first catches a
                // secret in any position. Full non-`key=value` scanning is Phase 5.
                let redacted: Vec<String> = unmet
                    .iter()
                    .map(|identity| {
                        identity
                            .split_whitespace()
                            .map(|token| forge_content::redact_secret_like_text(token).0)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect();
                json!({ "status": status, "unmet": redacted })
            }
            ForgeError::EvidenceTampered { id, kind } => {
                // Only an opaque row id and a closed-enum kind — never an excerpt or
                // command string (details is a machine-visible egress).
                json!({ "id": id, "kind": kind.as_str() })
            }
            ForgeError::ProvenanceMismatch {
                proposal_id,
                published_digest,
                recomputed_digest,
            } => json!({
                "proposal_id": proposal_id,
                "published_digest": published_digest,
                "recomputed_digest": recomputed_digest,
            }),
            ForgeError::LocalSignatureMismatch {
                proposal_id,
                published_fingerprint,
                recomputed_fingerprint,
            } => json!({
                "proposal_id": proposal_id,
                "published_fingerprint": published_fingerprint,
                "recomputed_fingerprint": recomputed_fingerprint,
            }),
            ForgeError::MissingProvenanceTrailer {
                branch,
                missing_field,
            } => json!({ "branch": branch, "missing_field": missing_field }),
            ForgeError::NativeHistoryCorrupt {
                kind,
                commit_id,
                related_id,
            } => json!({
                // Only the closed-enum kind and opaque f1: commit ids — never a path
                // or excerpt (details is a machine-visible egress).
                "kind": kind.as_str(),
                "commit_id": commit_id,
                "related_id": related_id,
            }),
            ForgeError::ConflictSetNotFound { .. } => json!({ "selector_present": true }),
            ForgeError::GcPlanChanged {
                expected_digest,
                actual_digest,
            } => json!({ "expected_digest": expected_digest, "actual_digest": actual_digest }),
            ForgeError::UnsupportedContentBackend {
                command,
                required,
                actual,
            } => json!({ "command": command, "required": required, "actual": actual }),
            ForgeError::TrustPolicyUnmet {
                action,
                required_trust,
                signature_issues,
            } => json!({
                "action": action,
                "required_trust": required_trust,
                "signature_issues": signature_issues,
            }),
            ForgeError::UnsupportedTrustLevel { level, supported } => {
                json!({ "level": level, "supported": supported })
            }
            _ => Value::Object(Default::default()),
        }
    }
}

/// Build the `DirtyWorktree` details object, replacing any secret-risk path with a
/// placeholder and reporting how many were redacted so the count is observable
/// without leaking the names.
fn redact_paths(paths: &[String]) -> Value {
    let mut redacted_count = 0u64;
    let displayed: Vec<String> = paths
        .iter()
        .map(|path| {
            if forge_content::is_secret_risk_path(path) {
                redacted_count += 1;
                REDACTED_PATH_PLACEHOLDER.to_string()
            } else {
                path.clone()
            }
        })
        .collect();
    json!({ "paths": displayed, "redacted_count": redacted_count })
}

impl std::fmt::Display for ForgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForgeError::StaleBase {
                expected_head,
                actual_head,
            } => write!(
                f,
                "stale base: HEAD moved from {expected_head} to {actual_head}"
            ),
            ForgeError::DirtyWorktree { .. } => {
                write!(f, "dirty worktree has unsaved changes")
            }
            ForgeError::AmbiguousAttempt { candidate_ids } => {
                write!(f, "ambiguous attempt: {}", candidate_ids.join(","))
            }
            ForgeError::UnknownAttempt { selector } => {
                write!(f, "unknown attempt {selector}")
            }
            ForgeError::AmbiguousProposal { candidate_ids } => {
                write!(f, "ambiguous proposal: {}", candidate_ids.join(","))
            }
            ForgeError::UnknownProposal { selector } => {
                write!(f, "unknown proposal {selector}")
            }
            ForgeError::UnknownIntent { selector } => {
                write!(f, "unknown intent {selector}")
            }
            ForgeError::NoActiveAttempt => write!(f, "no active attempt"),
            ForgeError::NoSnapshot => write!(f, "no snapshot saved for active attempt"),
            ForgeError::NoProposal => write!(f, "no proposal exists"),
            ForgeError::NotAccepted => write!(f, "proposal is not accepted"),
            ForgeError::Rejected => write!(f, "proposal was rejected"),
            ForgeError::BranchExists { name } => {
                write!(f, "branch already exists: {name}")
            }
            ForgeError::NotInitialized => {
                write!(f, "forge repository is not initialized")
            }
            ForgeError::RequestIdConflict { existing_command } => {
                write!(
                    f,
                    "request id already used for command {existing_command}"
                )
            }
            ForgeError::CurrentStateChanged => write!(f, "current operation changed"),
            ForgeError::UnknownSchemaVersion {
                db_version,
                supported_head,
            } => write!(
                f,
                "schema version {db_version} is newer than this binary supports (head {supported_head}); refusing to write"
            ),
            ForgeError::MigrationFailed { version, message } => {
                write!(f, "migration {version} failed: {message}")
            }
            ForgeError::AttemptWorktreeMismatch {
                requested_attempt,
                attached_attempt,
            } => write!(
                f,
                "worktree is materialized for attempt {attached_attempt}, not the requested {requested_attempt}; run `forge attempt attach {requested_attempt}` first"
            ),
            ForgeError::CheckNotPassed { status, unmet } => write!(
                f,
                "check did not pass (status: {status}); {} required gate(s) unmet",
                unmet.len()
            ),
            ForgeError::EvidenceTampered { id, kind } => write!(
                f,
                "integrity check failed for row {id} ({}); the recorded evidence/decision was tampered with",
                kind.as_str()
            ),
            ForgeError::ProvenanceMismatch {
                proposal_id,
                published_digest,
                recomputed_digest,
            } => write!(
                f,
                "provenance mismatch for proposal {proposal_id}: published trailer digest {published_digest} does not match the digest recomputed from the local ledger ({recomputed_digest})"
            ),
            ForgeError::LocalSignatureMismatch {
                proposal_id,
                published_fingerprint,
                recomputed_fingerprint,
            } => write!(
                f,
                "local signature mismatch for proposal {proposal_id}: published fingerprint {published_fingerprint} does not match locally verified fingerprint {recomputed_fingerprint}"
            ),
            ForgeError::MissingProvenanceTrailer {
                branch,
                missing_field,
            } => write!(
                f,
                "commit {branch} carries no Forge provenance trailer (missing {missing_field}); it was not produced by `forge export branch`"
            ),
            ForgeError::NativeHistoryCorrupt {
                kind,
                commit_id,
                related_id,
            } => match related_id {
                Some(related) => write!(
                    f,
                    "native history corrupt ({}) at commit {commit_id} (related {related})",
                    kind.as_str()
                ),
                None => write!(
                    f,
                    "native history corrupt ({}) at commit {commit_id}",
                    kind.as_str()
                ),
            },
            ForgeError::ConflictSetNotFound { .. } => write!(f, "conflict set not found"),
            ForgeError::GcPlanChanged {
                expected_digest,
                actual_digest,
            } => write!(
                f,
                "gc plan changed: expected {expected_digest}, recomputed {actual_digest}; rerun `forge gc --dry-run`"
            ),
            ForgeError::UnsupportedContentBackend {
                command,
                required,
                actual,
            } => write!(f, "{command} requires {required}, but found {actual}"),
            ForgeError::TrustPolicyUnmet {
                action,
                required_trust,
                signature_issues,
            } => write!(
                f,
                "{action} requires trust level {required_trust}; {} signature issue(s) prevent that claim",
                signature_issues.len()
            ),
            ForgeError::UnsupportedTrustLevel { level, supported } => write!(
                f,
                "unsupported trust level {level}; supported levels: {}",
                supported.join(", ")
            ),
        }
    }
}

impl std::error::Error for ForgeError {}

/// A single entry in the published error-code registry: the agent-visible `code`,
/// its retry classification, and the JSON keys its [`ForgeError::details`] emits.
///
/// Hand-mirrors one [`ForgeError`] variant each. The drift-guard test below pins
/// `error_registry().len()` to the number of variants, so a newly-added variant
/// is a compile-then-test failure until its registry entry exists — the registry
/// cannot silently drift from the enum it documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorCodeSpec {
    pub code: &'static str,
    pub retryable: bool,
    pub after_ms: Option<u64>,
    pub details_keys: &'static [&'static str],
}

/// Every code [`ForgeError`] can emit, for the published `forge schema` registry.
///
/// One entry per `ForgeError` variant. Keep this in lockstep with the enum — the
/// drift-guard test asserts both directions (every variant's `.code()` appears
/// here, and the length matches the variant count).
pub fn error_registry() -> &'static [ErrorCodeSpec] {
    &[
        ErrorCodeSpec {
            code: "STALE_BASE",
            retryable: false,
            after_ms: None,
            details_keys: &["expected_head", "actual_head"],
        },
        ErrorCodeSpec {
            code: "DIRTY_WORKTREE",
            retryable: false,
            after_ms: None,
            details_keys: &["paths", "redacted_count"],
        },
        ErrorCodeSpec {
            code: "AMBIGUOUS_ATTEMPT",
            retryable: false,
            after_ms: None,
            details_keys: &["candidate_ids"],
        },
        ErrorCodeSpec {
            code: "UNKNOWN_ATTEMPT",
            retryable: false,
            after_ms: None,
            details_keys: &["selector"],
        },
        ErrorCodeSpec {
            code: "AMBIGUOUS_PROPOSAL",
            retryable: false,
            after_ms: None,
            details_keys: &["candidate_ids"],
        },
        ErrorCodeSpec {
            code: "UNKNOWN_PROPOSAL",
            retryable: false,
            after_ms: None,
            details_keys: &["selector"],
        },
        ErrorCodeSpec {
            code: "UNKNOWN_INTENT",
            retryable: false,
            after_ms: None,
            details_keys: &["selector"],
        },
        ErrorCodeSpec {
            code: "NO_ACTIVE_ATTEMPT",
            retryable: false,
            after_ms: None,
            details_keys: &[],
        },
        ErrorCodeSpec {
            code: "NO_SNAPSHOT",
            retryable: false,
            after_ms: None,
            details_keys: &[],
        },
        ErrorCodeSpec {
            code: "NO_PROPOSAL",
            retryable: false,
            after_ms: None,
            details_keys: &[],
        },
        ErrorCodeSpec {
            code: "NOT_ACCEPTED",
            retryable: false,
            after_ms: None,
            details_keys: &[],
        },
        ErrorCodeSpec {
            code: "REJECTED",
            retryable: false,
            after_ms: None,
            details_keys: &[],
        },
        ErrorCodeSpec {
            code: "BRANCH_EXISTS",
            retryable: false,
            after_ms: None,
            details_keys: &["name"],
        },
        ErrorCodeSpec {
            code: "NOT_INITIALIZED",
            retryable: false,
            after_ms: None,
            details_keys: &[],
        },
        ErrorCodeSpec {
            code: "REQUEST_ID_CONFLICT",
            retryable: false,
            after_ms: None,
            details_keys: &["existing_command"],
        },
        ErrorCodeSpec {
            code: "CONFLICT",
            retryable: true,
            after_ms: Some(RETRY_BACKOFF_MS),
            details_keys: &[],
        },
        ErrorCodeSpec {
            code: "SCHEMA_VERSION_UNSUPPORTED",
            retryable: false,
            after_ms: None,
            details_keys: &["db_version", "supported_head"],
        },
        ErrorCodeSpec {
            code: "MIGRATION_FAILED",
            retryable: false,
            after_ms: None,
            details_keys: &["version", "message"],
        },
        ErrorCodeSpec {
            code: "ATTEMPT_WORKTREE_MISMATCH",
            retryable: false,
            after_ms: None,
            details_keys: &["requested_attempt", "attached_attempt"],
        },
        ErrorCodeSpec {
            code: "CHECK_NOT_PASSED",
            retryable: false,
            after_ms: None,
            details_keys: &["status", "unmet"],
        },
        ErrorCodeSpec {
            code: "EVIDENCE_TAMPERED",
            retryable: false,
            after_ms: None,
            details_keys: &["id", "kind"],
        },
        ErrorCodeSpec {
            code: "PROVENANCE_MISMATCH",
            retryable: false,
            after_ms: None,
            details_keys: &["proposal_id", "published_digest", "recomputed_digest"],
        },
        ErrorCodeSpec {
            code: "LOCAL_SIGNATURE_MISMATCH",
            retryable: false,
            after_ms: None,
            details_keys: &[
                "proposal_id",
                "published_fingerprint",
                "recomputed_fingerprint",
            ],
        },
        ErrorCodeSpec {
            code: "MISSING_PROVENANCE_TRAILER",
            retryable: false,
            after_ms: None,
            details_keys: &["branch", "missing_field"],
        },
        ErrorCodeSpec {
            code: "NATIVE_HISTORY_CORRUPT",
            retryable: false,
            after_ms: None,
            details_keys: &["kind", "commit_id", "related_id"],
        },
        ErrorCodeSpec {
            code: "CONFLICT_SET_NOT_FOUND",
            retryable: false,
            after_ms: None,
            details_keys: &["selector_present"],
        },
        ErrorCodeSpec {
            code: "GC_PLAN_CHANGED",
            retryable: false,
            after_ms: None,
            details_keys: &["expected_digest", "actual_digest"],
        },
        ErrorCodeSpec {
            code: "UNSUPPORTED_CONTENT_BACKEND",
            retryable: false,
            after_ms: None,
            details_keys: &["command", "required", "actual"],
        },
        ErrorCodeSpec {
            code: "TRUST_POLICY_UNMET",
            retryable: false,
            after_ms: None,
            details_keys: &["action", "required_trust", "signature_issues"],
        },
        ErrorCodeSpec {
            code: "UNSUPPORTED_TRUST_LEVEL",
            retryable: false,
            after_ms: None,
            details_keys: &["level", "supported"],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Every code the deleted `error_code()` produced must be produced by some
    /// `ForgeError::code()` — including the CLI-originated codes.
    #[test]
    fn codes_match_the_pre_change_registry() {
        assert_eq!(
            ForgeError::StaleBase {
                expected_head: "a".into(),
                actual_head: "b".into()
            }
            .code(),
            "STALE_BASE"
        );
        assert_eq!(
            ForgeError::DirtyWorktree { paths: vec![] }.code(),
            "DIRTY_WORKTREE"
        );
        assert_eq!(
            ForgeError::AmbiguousAttempt {
                candidate_ids: vec![]
            }
            .code(),
            "AMBIGUOUS_ATTEMPT"
        );
        assert_eq!(
            ForgeError::UnknownAttempt {
                selector: "x".into()
            }
            .code(),
            "UNKNOWN_ATTEMPT"
        );
        assert_eq!(
            ForgeError::AmbiguousProposal {
                candidate_ids: vec![]
            }
            .code(),
            "AMBIGUOUS_PROPOSAL"
        );
        assert_eq!(
            ForgeError::UnknownProposal {
                selector: "x".into()
            }
            .code(),
            "UNKNOWN_PROPOSAL"
        );
        assert_eq!(
            ForgeError::UnknownIntent {
                selector: "x".into()
            }
            .code(),
            "UNKNOWN_INTENT"
        );
        assert_eq!(ForgeError::NoActiveAttempt.code(), "NO_ACTIVE_ATTEMPT");
        assert_eq!(ForgeError::NoSnapshot.code(), "NO_SNAPSHOT");
        assert_eq!(ForgeError::NoProposal.code(), "NO_PROPOSAL");
        assert_eq!(ForgeError::NotAccepted.code(), "NOT_ACCEPTED");
        assert_eq!(ForgeError::Rejected.code(), "REJECTED");
        assert_eq!(
            ForgeError::BranchExists { name: "x".into() }.code(),
            "BRANCH_EXISTS"
        );
        assert_eq!(ForgeError::NotInitialized.code(), "NOT_INITIALIZED");
        assert_eq!(
            ForgeError::RequestIdConflict {
                existing_command: "start".into()
            }
            .code(),
            "REQUEST_ID_CONFLICT"
        );
        assert_eq!(ForgeError::CurrentStateChanged.code(), "CONFLICT");
        assert_eq!(
            ForgeError::UnknownSchemaVersion {
                db_version: 3,
                supported_head: 2
            }
            .code(),
            "SCHEMA_VERSION_UNSUPPORTED"
        );
        assert_eq!(
            ForgeError::MigrationFailed {
                version: 2,
                message: "boom".into()
            }
            .code(),
            "MIGRATION_FAILED"
        );
        assert_eq!(
            ForgeError::AttemptWorktreeMismatch {
                requested_attempt: "attempt_x".into(),
                attached_attempt: "attempt_w".into()
            }
            .code(),
            "ATTEMPT_WORKTREE_MISMATCH"
        );
        assert_eq!(
            ForgeError::CheckNotPassed {
                status: "failed".into(),
                unmet: vec!["cargo test".into()]
            }
            .code(),
            "CHECK_NOT_PASSED"
        );
        assert_eq!(
            ForgeError::EvidenceTampered {
                id: "evidence_x".into(),
                kind: TamperKind::ContentEdit,
            }
            .code(),
            "EVIDENCE_TAMPERED"
        );
        assert_eq!(
            ForgeError::ProvenanceMismatch {
                proposal_id: "proposal_x".into(),
                published_digest: "aaa".into(),
                recomputed_digest: "bbb".into(),
            }
            .code(),
            "PROVENANCE_MISMATCH"
        );
        assert_eq!(
            ForgeError::MissingProvenanceTrailer {
                branch: "forge/x".into(),
                missing_field: "provenance_digest".into(),
            }
            .code(),
            "MISSING_PROVENANCE_TRAILER"
        );
        assert_eq!(
            ForgeError::NativeHistoryCorrupt {
                kind: NativeHistoryCorruptKind::DanglingParent,
                commit_id: "f1:commit:sha256:aa".into(),
                related_id: Some("f1:commit:sha256:bb".into()),
            }
            .code(),
            "NATIVE_HISTORY_CORRUPT"
        );
    }

    #[test]
    fn only_transient_variants_are_retryable() {
        assert!(ForgeError::CurrentStateChanged.retryable());
        assert_eq!(ForgeError::CurrentStateChanged.after_ms(), Some(50));

        for deterministic in [
            ForgeError::NoActiveAttempt,
            ForgeError::NoSnapshot,
            ForgeError::Rejected,
            ForgeError::NotInitialized,
            ForgeError::StaleBase {
                expected_head: "a".into(),
                actual_head: "b".into(),
            },
        ] {
            assert!(!deterministic.retryable());
            assert_eq!(deterministic.after_ms(), None);
        }
    }

    #[test]
    fn details_carry_expected_keys() {
        let stale = ForgeError::StaleBase {
            expected_head: "aaa".into(),
            actual_head: "bbb".into(),
        }
        .details();
        assert_eq!(stale["expected_head"], "aaa");
        assert_eq!(stale["actual_head"], "bbb");

        let ambiguous = ForgeError::AmbiguousAttempt {
            candidate_ids: vec!["one".into(), "two".into()],
        }
        .details();
        assert_eq!(ambiguous["candidate_ids"][0], "one");
        assert_eq!(ambiguous["candidate_ids"][1], "two");
    }

    /// NER-134 security invariant: the mismatch payload carries exactly the two
    /// opaque attempt-id keys and nothing path- or content-shaped, so it is exempt
    /// from redaction by construction. Mirrors `details_carry_expected_keys`.
    #[test]
    fn attempt_worktree_mismatch_details_carry_only_ids() {
        let details = ForgeError::AttemptWorktreeMismatch {
            requested_attempt: "attempt_req".into(),
            attached_attempt: "attempt_att".into(),
        }
        .details();
        assert_eq!(details["requested_attempt"], "attempt_req");
        assert_eq!(details["attached_attempt"], "attempt_att");
        let object = details.as_object().expect("details object");
        assert_eq!(
            object.len(),
            2,
            "details must carry exactly the two id keys"
        );
    }

    /// NER-136 security invariant: the tamper payload carries exactly an opaque row
    /// id and a closed-enum break kind — never an excerpt or command string, which
    /// would be a secret-leaking egress. Mirrors `attempt_worktree_mismatch_…`.
    #[test]
    fn evidence_tampered_details_carry_only_ids() {
        let details = ForgeError::EvidenceTampered {
            id: "evidence_abc".into(),
            kind: TamperKind::ContentEdit,
        }
        .details();
        assert_eq!(details["id"], "evidence_abc");
        assert_eq!(details["kind"], "content_edit");
        let object = details.as_object().expect("details object");
        assert_eq!(object.len(), 2, "details must carry exactly id + kind");
    }

    /// NER-137 security invariant: the provenance-mismatch payload carries only the
    /// opaque proposal id and the two digests — no excerpt or path. Mirrors the other
    /// `*_details_carry_only_ids` guards.
    #[test]
    fn provenance_mismatch_details_carry_only_ids() {
        let details = ForgeError::ProvenanceMismatch {
            proposal_id: "proposal_abc".into(),
            published_digest: "deadbeef".into(),
            recomputed_digest: "feedface".into(),
        }
        .details();
        assert_eq!(details["proposal_id"], "proposal_abc");
        assert_eq!(details["published_digest"], "deadbeef");
        assert_eq!(details["recomputed_digest"], "feedface");
        let object = details.as_object().expect("details object");
        assert_eq!(
            object.len(),
            3,
            "details must carry exactly proposal_id + the two digests"
        );
    }

    #[test]
    fn missing_provenance_trailer_details_carry_only_ids() {
        let details = ForgeError::MissingProvenanceTrailer {
            branch: "forge/x".into(),
            missing_field: "provenance_digest".into(),
        }
        .details();
        assert_eq!(details["branch"], "forge/x");
        assert_eq!(details["missing_field"], "provenance_digest");
        let object = details.as_object().expect("details object");
        assert_eq!(
            object.len(),
            2,
            "details must carry exactly branch + missing_field"
        );
    }

    /// `TamperKind`'s serde representation (used by `DoctorReport.tampered_rows`) must
    /// match its `as_str()` (used by `EvidenceTampered.details`), so the two
    /// machine-visible surfaces never disagree on the break-kind string.
    #[test]
    fn tamper_kind_serde_matches_as_str() {
        for kind in [
            TamperKind::ContentEdit,
            TamperKind::BrokenLink,
            TamperKind::MissingHash,
        ] {
            assert_eq!(
                serde_json::to_value(kind).expect("serialize"),
                kind.as_str()
            );
        }
    }

    /// NER-138 slice 3: `NativeHistoryCorruptKind`'s serde representation (used by the
    /// `DoctorReport` corruption finding) must match its `as_str()` (used by
    /// `NativeHistoryCorrupt.details`), so the error payload and the doctor report never
    /// disagree on the kind string. Mirrors `tamper_kind_serde_matches_as_str`.
    #[test]
    fn native_history_corrupt_kind_serde_matches_as_str() {
        for kind in [
            NativeHistoryCorruptKind::Cycle,
            NativeHistoryCorruptKind::DanglingParent,
            NativeHistoryCorruptKind::DanglingTree,
            NativeHistoryCorruptKind::DanglingCommitId,
        ] {
            assert_eq!(
                serde_json::to_value(kind).expect("serialize"),
                kind.as_str()
            );
        }
    }

    /// NER-138 slice 3 security invariant: the native-history-corrupt payload carries only
    /// the closed-enum kind and opaque `f1:` commit ids — never a path or excerpt. Mirrors
    /// the other `*_details_carry_only_ids` guards.
    #[test]
    fn native_history_corrupt_details_carry_only_ids() {
        let details = ForgeError::NativeHistoryCorrupt {
            kind: NativeHistoryCorruptKind::DanglingParent,
            commit_id: "f1:commit:sha256:aaaa".into(),
            related_id: Some("f1:commit:sha256:bbbb".into()),
        }
        .details();
        assert_eq!(details["kind"], "dangling_parent");
        assert_eq!(details["commit_id"], "f1:commit:sha256:aaaa");
        assert_eq!(details["related_id"], "f1:commit:sha256:bbbb");
        let object = details.as_object().expect("details object");
        assert_eq!(
            object.len(),
            3,
            "details must carry exactly kind + commit_id + related_id"
        );
    }

    /// S1: `NativeHistoryCorrupt`'s `Display` (which reaches the untyped envelope via
    /// `{:#}` on the wrapping `anyhow::Error`) must be path-free for every kind — the
    /// `details_carry_only_ids` guard covers the structured payload, this covers the
    /// Display egress so a future `write!` cannot smuggle a path into the message.
    #[test]
    fn native_history_corrupt_display_is_path_free() {
        for kind in [
            NativeHistoryCorruptKind::Cycle,
            NativeHistoryCorruptKind::DanglingParent,
            NativeHistoryCorruptKind::DanglingTree,
            NativeHistoryCorruptKind::DanglingCommitId,
        ] {
            let error = ForgeError::NativeHistoryCorrupt {
                kind,
                commit_id: "f1:commit:sha256:aaaa".into(),
                related_id: Some("f1:commit:sha256:bbbb".into()),
            };
            let display = format!("{error}");
            let chained = format!("{:#}", anyhow::Error::new(error));
            for rendered in [&display, &chained] {
                assert!(
                    !rendered.contains('/') && !rendered.contains('\\'),
                    "NativeHistoryCorrupt Display leaked a path separator: {rendered}"
                );
            }
        }
    }

    /// NER-135: the `CHECK_NOT_PASSED` details carry exactly `status` + `unmet`, and
    /// each `unmet` gate identity is run through the shared `key=value` secret
    /// redactor so a secret accidentally embedded in a `--require` gate spec (which is
    /// persisted and surfaced WITHOUT execution) does not leak through error details.
    #[test]
    fn details_redact_secret_like_unmet() {
        let details = ForgeError::CheckNotPassed {
            status: "failed".into(),
            unmet: vec![
                "cargo test".into(),
                "deploy --token=ghp_supersecret".into(),
                // Multi-token identity where the secret is NOT the first `=` on the
                // line — the per-token redaction must still catch it (code-review F2).
                "cargo test FOO=bar --token=ghp_secondsecret".into(),
            ],
        }
        .details();
        assert_eq!(details["status"], "failed");
        let unmet = details["unmet"].as_array().expect("unmet array");
        let serialized = Value::Array(unmet.clone()).to_string();
        assert!(serialized.contains("cargo test"), "plain gate kept");
        assert!(
            !serialized.contains("ghp_supersecret"),
            "secret-like argv value must be redacted in unmet"
        );
        assert!(
            !serialized.contains("ghp_secondsecret"),
            "a secret-like token after a non-secret key=value must still be redacted"
        );
        assert!(
            serialized.contains("FOO=bar"),
            "non-secret key=value token is preserved"
        );
        assert!(serialized.contains("[REDACTED]"));
        let object = details.as_object().expect("details object");
        assert_eq!(object.len(), 2, "details carry exactly status + unmet");
    }

    #[test]
    fn dirty_worktree_details_redact_secret_paths() {
        let details = ForgeError::DirtyWorktree {
            paths: vec![
                "src/main.rs".into(),
                ".env".into(),
                "server/private.pem".into(),
            ],
        }
        .details();
        let paths = details["paths"].as_array().expect("paths array");
        let serialized = Value::Array(paths.clone()).to_string();
        assert!(serialized.contains("src/main.rs"));
        assert!(
            !serialized.contains(".env"),
            "secret filename must not appear in details"
        );
        assert!(
            !serialized.contains("private.pem"),
            "secret filename must not appear in details"
        );
        assert_eq!(details["redacted_count"], 2);
    }

    #[test]
    fn round_trips_through_anyhow() {
        let error: anyhow::Error = ForgeError::NoSnapshot.into();
        let recovered = error
            .downcast_ref::<ForgeError>()
            .expect("downcast recovers the typed error");
        assert_eq!(recovered.code(), "NO_SNAPSHOT");
    }

    /// Drift guard for the published `forge schema` registry: a representative
    /// instance of EVERY variant must have its `.code()` present in
    /// `error_registry()`, AND the registry length must equal the variant count,
    /// so a newly-added variant cannot ship without a registry entry. Same
    /// discipline as `codes_match_the_pre_change_registry`.
    #[test]
    fn registry_covers_every_variant() {
        // One representative instance per variant. Adding a variant without
        // extending this list is a compile error (the match below is exhaustive).
        let all = [
            ForgeError::StaleBase {
                expected_head: "a".into(),
                actual_head: "b".into(),
            },
            ForgeError::DirtyWorktree { paths: vec![] },
            ForgeError::AmbiguousAttempt {
                candidate_ids: vec![],
            },
            ForgeError::UnknownAttempt {
                selector: "x".into(),
            },
            ForgeError::AmbiguousProposal {
                candidate_ids: vec![],
            },
            ForgeError::UnknownProposal {
                selector: "x".into(),
            },
            ForgeError::UnknownIntent {
                selector: "x".into(),
            },
            ForgeError::NoActiveAttempt,
            ForgeError::NoSnapshot,
            ForgeError::NoProposal,
            ForgeError::NotAccepted,
            ForgeError::Rejected,
            ForgeError::BranchExists { name: "x".into() },
            ForgeError::NotInitialized,
            ForgeError::RequestIdConflict {
                existing_command: "start".into(),
            },
            ForgeError::CurrentStateChanged,
            ForgeError::UnknownSchemaVersion {
                db_version: 3,
                supported_head: 2,
            },
            ForgeError::MigrationFailed {
                version: 2,
                message: "boom".into(),
            },
            ForgeError::AttemptWorktreeMismatch {
                requested_attempt: "attempt_x".into(),
                attached_attempt: "attempt_w".into(),
            },
            ForgeError::CheckNotPassed {
                status: "failed".into(),
                unmet: vec!["cargo test".into()],
            },
            ForgeError::EvidenceTampered {
                id: "evidence_x".into(),
                kind: TamperKind::BrokenLink,
            },
            ForgeError::ProvenanceMismatch {
                proposal_id: "proposal_x".into(),
                published_digest: "aaa".into(),
                recomputed_digest: "bbb".into(),
            },
            ForgeError::LocalSignatureMismatch {
                proposal_id: "proposal_x".into(),
                published_fingerprint: "aaa".into(),
                recomputed_fingerprint: "bbb".into(),
            },
            ForgeError::MissingProvenanceTrailer {
                branch: "forge/x".into(),
                missing_field: "provenance_digest".into(),
            },
            ForgeError::NativeHistoryCorrupt {
                kind: NativeHistoryCorruptKind::Cycle,
                commit_id: "f1:commit:sha256:aa".into(),
                related_id: None,
            },
            ForgeError::ConflictSetNotFound {
                conflict_set_id: "conflict_missing".into(),
            },
            ForgeError::GcPlanChanged {
                expected_digest: "aaa".into(),
                actual_digest: "bbb".into(),
            },
            ForgeError::UnsupportedContentBackend {
                command: "merge".into(),
                required: "native".into(),
                actual: "git".into(),
            },
            ForgeError::TrustPolicyUnmet {
                action: "accept".into(),
                required_trust: "locally_signed".into(),
                signature_issues: vec![],
            },
            ForgeError::UnsupportedTrustLevel {
                level: "hosted_runner_signed".into(),
                supported: vec!["self_reported".into(), "locally_signed".into()],
            },
        ];

        // Exhaustiveness check: if a variant is added, this match fails to compile
        // until `all` (and the registry) are extended.
        for variant in &all {
            match variant {
                ForgeError::StaleBase { .. }
                | ForgeError::DirtyWorktree { .. }
                | ForgeError::AmbiguousAttempt { .. }
                | ForgeError::UnknownAttempt { .. }
                | ForgeError::AmbiguousProposal { .. }
                | ForgeError::UnknownProposal { .. }
                | ForgeError::UnknownIntent { .. }
                | ForgeError::NoActiveAttempt
                | ForgeError::NoSnapshot
                | ForgeError::NoProposal
                | ForgeError::NotAccepted
                | ForgeError::Rejected
                | ForgeError::BranchExists { .. }
                | ForgeError::NotInitialized
                | ForgeError::RequestIdConflict { .. }
                | ForgeError::CurrentStateChanged
                | ForgeError::UnknownSchemaVersion { .. }
                | ForgeError::MigrationFailed { .. }
                | ForgeError::AttemptWorktreeMismatch { .. }
                | ForgeError::CheckNotPassed { .. }
                | ForgeError::EvidenceTampered { .. }
                | ForgeError::ProvenanceMismatch { .. }
                | ForgeError::LocalSignatureMismatch { .. }
                | ForgeError::MissingProvenanceTrailer { .. }
                | ForgeError::NativeHistoryCorrupt { .. }
                | ForgeError::ConflictSetNotFound { .. }
                | ForgeError::GcPlanChanged { .. }
                | ForgeError::UnsupportedContentBackend { .. }
                | ForgeError::TrustPolicyUnmet { .. }
                | ForgeError::UnsupportedTrustLevel { .. } => {}
            }
        }

        let registry = error_registry();
        for variant in &all {
            assert!(
                registry.iter().any(|spec| spec.code == variant.code()),
                "registry is missing an entry for {}",
                variant.code()
            );
        }
        assert_eq!(
            registry.len(),
            all.len(),
            "error_registry() must have exactly one entry per ForgeError variant"
        );

        // Retryability/after_ms in the registry must match the runtime classifiers.
        for variant in &all {
            let spec = registry
                .iter()
                .find(|spec| spec.code == variant.code())
                .expect("registry entry");
            assert_eq!(spec.retryable, variant.retryable(), "{}", variant.code());
            assert_eq!(spec.after_ms, variant.after_ms(), "{}", variant.code());
        }
    }
}
