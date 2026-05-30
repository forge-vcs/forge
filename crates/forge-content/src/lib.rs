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

/// The placeholder substituted for any redacted secret span in a captured excerpt.
const REDACTED: &str = "[REDACTED]";

/// One detector class that fired during redaction, so each redaction can surface as
/// a distinct `warnings[]` entry (NER-136 R8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionKind {
    /// A line-oriented `key=value` / `key: value` secret (the historic redactor).
    KeyValue,
    /// A bare high-entropy token with no key prefix.
    HighEntropyToken,
    /// A secret embedded as a JSON `"key":"value"` pair.
    JsonSecret,
    /// A PEM private-key block body.
    PemPrivateKey,
    /// A `scheme://user:pass@host` credential URL password.
    CredentialUrl,
}

/// Harden captured command output against secret leakage before it is persisted
/// (and hashed) into the evidence ledger (NER-136 §U4). Runs, in order: PEM blocks,
/// `scheme://user:pass@host` credential URLs, JSON-embedded secrets, line-oriented
/// `key=value` secrets, and bare high-entropy tokens. Returns the redacted text plus
/// one [`RedactionKind`] per redaction occurrence so the CLI can emit a warning per
/// redaction. Degrades gracefully — it replaces the matched span with `[REDACTED]`
/// and preserves surrounding context, never dropping the whole excerpt.
///
/// **Known residual (documented in `forge schema`):** a bare 40- or 64-hex token is
/// exempted to avoid redacting Forge's own git SHAs and SHA-256 content hashes, so a
/// secret that happens to be exactly that shape is a false negative.
pub fn redact_evidence_excerpt(text: &str) -> (String, Vec<RedactionKind>) {
    let mut kinds = Vec::new();
    let mut output = text.to_string();
    output = redact_pem_blocks(&output, &mut kinds);
    output = redact_credential_urls(&output, &mut kinds);
    output = redact_json_secrets(&output, &mut kinds);
    output = redact_key_value_lines(&output, &mut kinds);
    output = redact_high_entropy_tokens(&output, &mut kinds);
    (output, kinds)
}

fn pem_block_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // Non-greedy body between matching BEGIN/END markers (gitleaks #1475: a greedy
        // body over-captures an adjacent secret). `(?s)` so `.` spans newlines.
        regex::Regex::new(
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        )
        .expect("valid PEM regex")
    })
}

fn pem_truncated_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // A BEGIN with no matching END — the excerpt was cut at the 4096 cap. Redact
        // header-to-end-of-buffer so a partial key body is never persisted.
        regex::Regex::new(r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*\z")
            .expect("valid truncated-PEM regex")
    })
}

fn redact_pem_blocks(text: &str, kinds: &mut Vec<RedactionKind>) -> String {
    let mut count = 0usize;
    let stage_one = pem_block_regex().replace_all(text, |_: &regex::Captures| {
        count += 1;
        "[REDACTED PEM PRIVATE KEY]".to_string()
    });
    let stage_two = pem_truncated_regex().replace_all(&stage_one, |_: &regex::Captures| {
        count += 1;
        "[REDACTED PEM PRIVATE KEY (truncated)]".to_string()
    });
    for _ in 0..count {
        kinds.push(RedactionKind::PemPrivateKey);
    }
    stage_two.into_owned()
}

fn credential_url_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // Anchored on `scheme://` so scp-style `git@host:path` (no scheme) is excluded;
        // requires a `:password@`, so userinfo-without-password is left untouched. Only
        // the password capture group is redacted.
        regex::Regex::new(
            r"(?P<pre>[a-zA-Z][a-zA-Z0-9+.\-]*://[^/\s:@]+:)(?P<pw>[^@\s/]+)(?P<post>@)",
        )
        .expect("valid credential-url regex")
    })
}

fn redact_credential_urls(text: &str, kinds: &mut Vec<RedactionKind>) -> String {
    let mut count = 0usize;
    let out = credential_url_regex().replace_all(text, |caps: &regex::Captures| {
        count += 1;
        format!("{}{}{}", &caps["pre"], REDACTED, &caps["post"])
    });
    for _ in 0..count {
        kinds.push(RedactionKind::CredentialUrl);
    }
    out.into_owned()
}

fn json_secret_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        // A JSON-ish `"<secret-key>": "<value>"` pair (case-insensitive key match). No
        // recursive parse — a regex degrades gracefully on a truncated/invalid blob.
        regex::Regex::new(
            r#"(?i)(?P<pre>"[a-z0-9_\-]*(?:token|password|passwd|secret|api[_-]?key|access[_-]?key|private[_-]?key)[a-z0-9_\-]*"\s*:\s*")(?P<val>[^"]*)(?P<post>")"#,
        )
        .expect("valid json-secret regex")
    })
}

fn redact_json_secrets(text: &str, kinds: &mut Vec<RedactionKind>) -> String {
    let mut count = 0usize;
    let out = json_secret_regex().replace_all(text, |caps: &regex::Captures| {
        count += 1;
        format!("{}{}{}", &caps["pre"], REDACTED, &caps["post"])
    });
    for _ in 0..count {
        kinds.push(RedactionKind::JsonSecret);
    }
    out.into_owned()
}

/// The line-oriented `key=value` redactor as a pass over the excerpt, recording a
/// `KeyValue` kind per redacted line.
fn redact_key_value_lines(text: &str, kinds: &mut Vec<RedactionKind>) -> String {
    let mut redacted = Vec::new();
    for line in text.lines() {
        let Some(separator) = line.find(['=', ':']) else {
            redacted.push(line.to_string());
            continue;
        };
        let key = line[..separator].trim().to_ascii_lowercase();
        if is_secret_like_key(&key) {
            kinds.push(RedactionKind::KeyValue);
            redacted.push(format!(
                "{}{}{}",
                &line[..separator],
                &line[separator..=separator],
                REDACTED
            ));
        } else {
            redacted.push(line.to_string());
        }
    }
    let mut output = redacted.join("\n");
    if text.ends_with('\n') {
        output.push('\n');
    }
    output
}

fn entropy_token_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // Candidate runs of secret-alphabet characters, length-gated at 20 (the
    // false-positive floor the detect-secrets/trufflehog tools use).
    RE.get_or_init(|| {
        regex::Regex::new(r"[A-Za-z0-9+/=_\-]{20,}").expect("valid entropy-token regex")
    })
}

fn uuid_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
        )
        .expect("valid uuid regex")
    })
}

/// Shannon entropy in bits per character.
fn shannon_entropy(token: &str) -> f64 {
    if token.is_empty() {
        return 0.0;
    }
    let mut counts = std::collections::HashMap::new();
    for byte in token.bytes() {
        *counts.entry(byte).or_insert(0u64) += 1;
    }
    let len = token.len() as f64;
    counts
        .values()
        .map(|&count| {
            let p = count as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// True if a length-gated candidate run is a high-entropy secret. Pure-hex runs of
/// length 7/8/40/64 (git short/long SHAs, SHA-1, SHA-256 — which Forge itself emits,
/// including its own content hashes) and UUIDs are exempted to bound false positives.
fn is_high_entropy_secret(token: &str) -> bool {
    if uuid_regex().is_match(token) {
        return false;
    }
    let is_hex = token.bytes().all(|b| b.is_ascii_hexdigit());
    if is_hex {
        if matches!(token.len(), 7 | 8 | 40 | 64) {
            return false; // git SHA / SHA-1 / SHA-256 shape — exempt (documented residual)
        }
        return shannon_entropy(token) >= 3.0;
    }
    shannon_entropy(token) >= 4.5
}

fn redact_high_entropy_tokens(text: &str, kinds: &mut Vec<RedactionKind>) -> String {
    let mut count = 0usize;
    let out = entropy_token_regex().replace_all(text, |caps: &regex::Captures| {
        let token = &caps[0];
        if is_high_entropy_secret(token) {
            count += 1;
            REDACTED.to_string()
        } else {
            token.to_string()
        }
    });
    for _ in 0..count {
        kinds.push(RedactionKind::HighEntropyToken);
    }
    out.into_owned()
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

    // --- NER-136 §U4: hardened redaction leak corpus ---

    #[test]
    fn bare_high_entropy_base64_token_is_redacted() {
        // A 40-char base64-ish token with no key= prefix.
        let token = "Zx9Kq2mLp7vR4tWnHy6bUaScDfGhJkLmNpQrStVw";
        let (out, kinds) = redact_evidence_excerpt(&format!("downloaded {token} ok"));
        assert!(!out.contains(token), "bare token must be redacted");
        assert!(out.contains("[REDACTED]"));
        assert!(kinds.contains(&RedactionKind::HighEntropyToken));
    }

    #[test]
    fn forge_own_hashes_and_uuids_are_not_redacted() {
        // git SHA-1 (40 hex), SHA-256 (64 hex), git short SHA (7/8 hex), and a UUID —
        // Forge emits these (incl. its own content_hash), so they must survive.
        let git_sha = "0c2585df471e15fdc245390ba0561c716177fab1"; // 40 hex
        let sha256 = "0c2585df471e15fdc245390ba0561c716177fab1995cd7cab283d725b80adb51"; // 64 hex
        let short = "0c2585d"; // 7 hex
        let uuid = "550e8400-e29b-41d4-a716-446655440000";
        let text = format!("commit {git_sha} hash {sha256} short {short} id {uuid}");
        let (out, kinds) = redact_evidence_excerpt(&text);
        assert_eq!(
            out, text,
            "Forge's own hash/UUID shapes must not be redacted"
        );
        assert!(kinds.is_empty());
    }

    #[test]
    fn json_embedded_secret_redacts_value_keeps_key() {
        let (out, kinds) = redact_evidence_excerpt(r#"{"api_key":"ghp_supersecretvalue","ok":1}"#);
        assert!(!out.contains("ghp_supersecretvalue"));
        assert!(out.contains("\"api_key\""), "the key name is preserved");
        assert!(out.contains("[REDACTED]"));
        assert!(kinds.contains(&RedactionKind::JsonSecret));
    }

    #[test]
    fn pem_private_key_block_is_fully_redacted() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIBjunkbase64body\nmoresecret==\n-----END RSA PRIVATE KEY-----";
        let (out, kinds) = redact_evidence_excerpt(&format!("before\n{pem}\nafter"));
        assert!(!out.contains("MIIBjunkbase64body"));
        assert!(!out.contains("moresecret"));
        assert!(out.contains("before") && out.contains("after"));
        assert!(kinds.contains(&RedactionKind::PemPrivateKey));
    }

    #[test]
    fn truncated_pem_block_redacts_header_to_end() {
        // A BEGIN with no END (the excerpt was cut at the cap) must not leave a
        // partial key body persisted.
        let truncated =
            "log line\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk partial body";
        let (out, kinds) = redact_evidence_excerpt(truncated);
        assert!(!out.contains("b3BlbnNzaC1rZXk"));
        assert!(out.contains("log line"));
        assert!(kinds.contains(&RedactionKind::PemPrivateKey));
    }

    #[test]
    fn credential_url_redacts_only_the_password() {
        let (out, kinds) =
            redact_evidence_excerpt("connect postgres://user:s3cr3tPass@db.host/app now");
        assert!(!out.contains("s3cr3tPass"), "password must be redacted");
        assert!(out.contains("postgres://user:[REDACTED]@db.host/app"));
        assert!(kinds.contains(&RedactionKind::CredentialUrl));
    }

    #[test]
    fn scp_style_and_userinfo_without_password_are_untouched() {
        // scp-style git URL (no scheme, no password) and userinfo-without-password.
        let text = "remote git@github.com:org/repo and https://token123longuserinfo@host/x";
        let (out, kinds) = redact_evidence_excerpt(text);
        assert!(
            out.contains("git@github.com:org/repo"),
            "scp-style must be untouched"
        );
        assert!(
            !kinds.contains(&RedactionKind::CredentialUrl),
            "no scheme://user:pass@ pattern present"
        );
    }

    #[test]
    fn line_oriented_key_value_secret_still_redacted() {
        let (out, kinds) = redact_evidence_excerpt("API_TOKEN=supersecretvalue\nplain line");
        assert!(!out.contains("supersecretvalue"));
        assert!(out.contains("plain line"));
        assert!(kinds.contains(&RedactionKind::KeyValue));
    }

    #[test]
    fn clean_output_yields_no_redactions() {
        let text = "all 12 tests passed in 0.42s\ncompiling forge-store v0.1.0";
        let (out, kinds) = redact_evidence_excerpt(text);
        assert_eq!(out, text);
        assert!(kinds.is_empty());
    }

    #[test]
    fn shannon_entropy_low_for_repeated_chars_high_for_random() {
        assert!(shannon_entropy("aaaaaaaaaaaaaaaaaaaa") < 1.0);
        assert!(shannon_entropy("Zx9Kq2mLp7vR4tWnHy6b") > 3.5);
    }
}
