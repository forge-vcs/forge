use anyhow::Result;
use serde::Serialize;
use std::path::Path;

pub const SECRET_RISK_SENSITIVITY: &str = "secret_risk";
pub const GIT_TREE_PREFIX: &str = "git-tree:";
pub const FORGE_TREE_PREFIX: &str = "forge-tree:";

/// Filename prefix for the per-file temp written during a crash-atomic restore
/// (NER-132 U4). These temps live transiently in worktree directories (for a
/// same-filesystem rename); the native backend materializes through them and
/// `forge_store::doctor` scans for orphans by this prefix. Defined here, in the
/// shared base crate, so both content backends exclude it identically.
pub const RESTORE_TEMP_PREFIX: &str = ".forge-restore-";

/// True if `path`'s final component is a crash-atomic-restore temp
/// (`.forge-restore-*`). Such a temp orphaned by a restore killed mid-flight must
/// never be captured into a snapshot or export, so both backends exclude it via
/// `is_ignored_by_policy`.
pub fn is_restore_temp_path(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with(RESTORE_TEMP_PREFIX))
}

/// Test-only crash injection (NER-132 U6). In **debug builds only**, if the
/// `FORGE_CRASH_POINT` environment variable names `point`, hard-abort the process
/// (`std::process::abort`) to simulate a kill at a durability boundary — skipping
/// all `Drop`/flush, exactly like a SIGKILL or sandbox teardown. In release builds
/// `cfg!(debug_assertions)` is `false`, so the entire check is dead code with zero
/// overhead. Only the crash-injection harness sets the env var; no production path
/// does. Lives in forge-content because both forge-cli (the save boundary) and
/// forge-content-native (the restore boundary) inject through it.
pub fn maybe_crash(point: &str) {
    if cfg!(debug_assertions)
        && matches!(std::env::var("FORGE_CRASH_POINT"), Ok(active) if active == point)
    {
        std::process::abort();
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotContent {
    pub content_ref: String,
    pub changed_paths: Vec<String>,
}

/// A pluggable content store for snapshotting and restoring worktrees.
///
/// **Store-before-DB durability contract (NER-132):** `snapshot_worktree` returns
/// only after every object backing the returned `content_ref` is durable on disk
/// (file contents *and* the directory entry fsynced). Forge commits the referencing
/// row to SQLite *after* this call returns, so a committed `content_ref` always
/// implies a durably-retained object — a crash can lose the not-yet-committed row,
/// but never leave a committed row pointing at a missing object. The native backend
/// satisfies this via temp-file + fsync + atomic rename + parent-dir fsync in
/// `write_object`; the git backend delegates to git's loose-object durability.
pub trait ContentBackend {
    fn snapshot_worktree(&self, repo_root: &Path) -> Result<SnapshotContent>;
    /// Restore the worktree to the tree named by `content_ref`. The native backend
    /// materializes each file crash-atomically (temp-file + rename + fsync; NER-132 U4).
    fn restore_snapshot(&self, repo_root: &Path, content_ref: &str) -> Result<()>;

    /// The backend's current base-revision anchor, used to stamp a fresh attempt's
    /// `base_head` and for stale-base detection (NER-134). Opaque to the core; for the
    /// git backend it is the current `HEAD` commit. Confining this behind the trait
    /// keeps git-worktree semantics out of core lifecycle code (PRD §23.4) and leaves
    /// the seam for the Phase 7 native walker.
    ///
    /// **Security invariant (S1):** implementations must return only an opaque
    /// revision identifier and must NOT embed filesystem paths in `anyhow` error
    /// context — such strings bubble into the untyped envelope `message`, bypassing
    /// the typed-error secret-path redaction that protects `details`.
    fn current_base(&self, repo_root: &Path) -> Result<String>;

    /// The restorable `content_ref` that materializes `base` (a value previously
    /// returned by [`ContentBackend::current_base`], i.e. an attempt's `base_head`)
    /// into the worktree (NER-134).
    ///
    /// **Security invariant (S2):** the returned ref must name a tree that already
    /// excludes `is_ignored_by_policy` paths (today inherited from git's tree), so a
    /// future native implementation cannot regress `.env`/private-key exclusion.
    /// **Security invariant (S1):** as with `current_base`, no filesystem paths in
    /// error context.
    fn base_content_ref(&self, repo_root: &Path, base: &str) -> Result<String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentRefKind<'a> {
    GitTree(&'a str),
    ForgeTree(&'a str),
    Unsupported,
}

pub fn classify_content_ref(content_ref: &str) -> ContentRefKind<'_> {
    if let Some(value) = content_ref.strip_prefix(GIT_TREE_PREFIX) {
        ContentRefKind::GitTree(value)
    } else if let Some(value) = content_ref.strip_prefix(FORGE_TREE_PREFIX) {
        ContentRefKind::ForgeTree(value)
    } else {
        ContentRefKind::Unsupported
    }
}

pub fn is_secret_risk_path(path: &str) -> bool {
    let normalized = path.trim_start_matches("./");
    let filename = normalized.rsplit('/').next().unwrap_or(normalized);
    let lower = filename.to_ascii_lowercase();

    lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower == "id_rsa"
        || lower == "id_dsa"
        || lower == "id_ecdsa"
        || lower == "id_ed25519"
        || lower.contains("credential")
        || lower.contains("credentials")
        || lower.contains("secret")
}

/// The single, shared export/snapshot exclusion predicate. Both content backends
/// (`forge-content-git` and `forge-content-native`) and the export-git tree rewrite
/// consult this so the secret/internal-path exclusion set cannot drift between them
/// (NER-133 U6). Excludes Forge's own metadata (`.forge`), git's metadata (`.git` —
/// harmless for the git backend, which never reports `.git/` worktree paths; note
/// `.gitignore`/`.gitattributes`/`.github/` are NOT under `.git/` and remain
/// eligible), crash-atomic-restore temps, and secret-risk-named paths.
pub fn is_ignored_by_policy(path: &str) -> bool {
    path == ".forge"
        || path.starts_with(".forge/")
        || path == ".git"
        || path.starts_with(".git/")
        || is_restore_temp_path(path)
        || is_secret_risk_path(path)
}

/// Partition `paths` into `(kept, dropped)` by `is_secret_risk_path`, preserving the
/// input order within each bucket. Used by the `pr_body` egress surface (NER-133 U6)
/// to list only non-secret paths while surfacing the dropped ones as warnings.
pub fn filter_secret_risk(paths: &[String]) -> (Vec<String>, Vec<String>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for path in paths {
        if is_secret_risk_path(path) {
            dropped.push(path.clone());
        } else {
            kept.push(path.clone());
        }
    }
    (kept, dropped)
}

pub fn redact_secret_like_text(text: &str) -> (String, bool) {
    let mut redacted_any = false;
    let mut redacted = Vec::new();

    for line in text.lines() {
        let Some(separator) = line.find(['=', ':']) else {
            redacted.push(line.to_string());
            continue;
        };
        let key = line[..separator].trim().to_ascii_lowercase();
        if is_secret_like_key(&key) {
            redacted_any = true;
            redacted.push(format!(
                "{}{}[REDACTED]",
                &line[..separator],
                &line[separator..=separator]
            ));
        } else {
            redacted.push(line.to_string());
        }
    }

    let mut output = redacted.join("\n");
    if text.ends_with('\n') {
        output.push('\n');
    }
    (output, redacted_any)
}

fn is_secret_like_key(key: &str) -> bool {
    key.contains("token")
        || key.contains("password")
        || key.contains("passwd")
        || key.contains("secret")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("access_key")
        || key.contains("private_key")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_policy_excludes_metadata_temps_and_secrets() {
        // Forge + git metadata, restore temps, and secret-risk names are all excluded
        // by the single shared predicate (NER-133 U6).
        assert!(is_ignored_by_policy(".forge"));
        assert!(is_ignored_by_policy(".forge/forge.db"));
        assert!(is_ignored_by_policy(".git"));
        assert!(is_ignored_by_policy(".git/config"));
        assert!(is_ignored_by_policy(".forge-restore-abc123"));
        assert!(is_ignored_by_policy(".env"));
        assert!(is_ignored_by_policy("certs/server.pem"));
        // git-adjacent dotfiles are NOT under `.git/` and stay eligible.
        assert!(!is_ignored_by_policy(".gitignore"));
        assert!(!is_ignored_by_policy(".gitattributes"));
        assert!(!is_ignored_by_policy(".github/workflows/ci.yml"));
        assert!(!is_ignored_by_policy("README.md"));
    }

    #[test]
    fn filter_secret_risk_partitions_order_preserving() {
        let paths = vec![
            "a.txt".to_string(),
            ".env".to_string(),
            "b.txt".to_string(),
            "certs/key.pem".to_string(),
            "c.txt".to_string(),
        ];
        let (kept, dropped) = filter_secret_risk(&paths);
        assert_eq!(kept, vec!["a.txt", "b.txt", "c.txt"]);
        assert_eq!(dropped, vec![".env", "certs/key.pem"]);
    }
}
