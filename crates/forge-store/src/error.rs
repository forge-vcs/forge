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
use serde_json::{json, Value};

/// Placeholder substituted for a secret-risk path in any machine-visible payload,
/// so a secret filename never reaches `errors[].details` or the persisted ledger.
const REDACTED_PATH_PLACEHOLDER: &str = "[secret-risk path redacted]";

/// Typed Forge error taxonomy. Constructed at the failure site, carried inside an
/// `anyhow::Error`, recovered at the CLI by `downcast_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgeError {
    /// `accept`/`export` ran against a moved HEAD.
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
                | ForgeError::MigrationFailed { .. } => {}
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
