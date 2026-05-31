use anyhow::{anyhow, Result};
use forge_content::{classify_content_ref, ContentBackend, ContentRefKind};
use forge_store::ForgeError;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

/// Fixed identity/date/message for the synthesized git parent of a NATIVE base
/// (NER-138 Phase 7 slice 2). Pinning every commit input keeps the synthesized parent
/// SHA deterministic for a given base tree, so idempotent re-export reconciles instead
/// of erroring `BRANCH_EXISTS`. These are git-interop scaffolding, not user identity.
const FORGE_SYNTH_IDENTITY: &str = "Forge";
const FORGE_SYNTH_EMAIL: &str = "forge@localhost";
const FORGE_SYNTH_DATE: &str = "@0 +0000";
const FORGE_SYNTH_BASE_MESSAGE: &str = "Forge native base";

/// Per-file hunk-body byte cap, mirroring `forge_evidence::EXCERPT_LIMIT` (4096). A
/// local const so the diff adapter does not depend on `forge-evidence`; the value is
/// the same so diff hunks and captured evidence excerpts share one bound. A hunk
/// longer than this is truncated with `truncated: true`.
const HUNK_LIMIT: usize = 4096;

/// One file's change between two trees (NER-137, Phase 6). `status` is git's
/// name-status letter (`A`/`M`/`D`/`R…`/`C…`). `insertions`/`deletions` come from
/// numstat (`None` for a binary file, which git reports as `-`). `hunk` carries the
/// redacted, bounded unified diff body and is populated only when hunks were
/// requested and the file is non-secret and non-binary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub status: String,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
    pub binary: bool,
    pub hunk: Option<String>,
    pub truncated: bool,
}

/// The content-level diff between two proposals' trees, produced via the git adapter
/// (NER-137 feature 3 — native diff with rename detection is Phase 8). Secret-risk
/// paths are dropped from `files` and listed in `dropped_secret_paths` so the caller
/// can surface them as a warning rather than leaking the filename.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TreeDiff {
    pub files: Vec<FileDiff>,
    pub dropped_secret_paths: Vec<String>,
}

/// Diff two proposals' `content_ref`s at file (and optionally hunk) granularity,
/// **via the git adapter** (NER-137 U2). Both refs are resolved to git tree hashes
/// through [`git_tree_for_content_ref`] — the one resolver the export path already
/// uses — so `git-tree:` and `forge-tree:` refs diff uniformly. Secret-risk-named
/// paths are dropped before any hunk is read; binary files emit no hunk; emitted
/// hunks are redacted (`redact_evidence_excerpt`) and bounded to [`HUNK_LIMIT`].
///
/// `include_hunks` is lazy by design (NER-137): the per-attempt compare summary uses
/// the cheap name-status/numstat (`include_hunks = false`), and the expensive hunk
/// bodies are produced only on the explicit pairwise `compare --diff` path — so a
/// bare `forge compare` over many intents never fans out into per-file `git diff`
/// subprocesses.
pub fn diff_trees(
    repo_root: &Path,
    content_ref_a: &str,
    content_ref_b: &str,
    include_hunks: bool,
) -> Result<TreeDiff> {
    let tree_a = git_tree_for_content_ref(repo_root, content_ref_a)?;
    let tree_b = git_tree_for_content_ref(repo_root, content_ref_b)?;

    // `-z` (NUL-delimited, never C-quoted) is load-bearing for the secret-path drop:
    // without it git C-quotes any path containing a tab/newline/non-ASCII byte (e.g.
    // `.env\ttest` -> `".env\ttest"`), which would slip `is_secret_risk_path` and leak
    // the filename. `--no-renames` keeps both passes keyed on the same plain path (a
    // rename otherwise shows in numstat as `old => new`, losing its counts).
    // name-status -z: `<STATUS>\0<path>\0` records. numstat -z: `<ins>\t<del>\t<path>\0`.
    let name_status = git(
        repo_root,
        &[
            "diff",
            "-z",
            "--no-renames",
            "--name-status",
            &tree_a,
            &tree_b,
        ],
    )?;
    let numstat = git(
        repo_root,
        &["diff", "-z", "--no-renames", "--numstat", &tree_a, &tree_b],
    )?;

    let mut counts: std::collections::HashMap<String, (Option<u64>, Option<u64>, bool)> =
        std::collections::HashMap::new();
    for record in numstat.split('\0') {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, '\t');
        let (Some(ins), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let binary = ins == "-" || del == "-";
        counts.insert(
            path.to_string(),
            (ins.parse().ok(), del.parse().ok(), binary),
        );
    }

    let mut files = Vec::new();
    let mut dropped_secret_paths = Vec::new();
    // name-status -z emits alternating `status`, `path` fields (NUL-separated).
    let mut fields = name_status.split('\0');
    while let Some(status) = fields.next() {
        if status.is_empty() {
            break; // trailing field after the last NUL
        }
        let Some(path) = fields.next() else { break };
        if forge_content::is_secret_risk_path(path) {
            dropped_secret_paths.push(path.to_string());
            continue;
        }
        let (insertions, deletions, binary) =
            counts.get(path).copied().unwrap_or((None, None, false));
        let (hunk, truncated) = if include_hunks && !binary {
            read_hunk(repo_root, &tree_a, &tree_b, path)?
        } else {
            (None, false)
        };
        files.push(FileDiff {
            path: path.to_string(),
            status: status.to_string(),
            insertions,
            deletions,
            binary,
            hunk,
            truncated,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    dropped_secret_paths.sort();
    Ok(TreeDiff {
        files,
        dropped_secret_paths,
    })
}

/// Read one file's unified-diff hunk between two trees, redact it, and bound it to
/// [`HUNK_LIMIT`]. The diff body is captured-output-shaped, so it goes through the
/// same `redact_evidence_excerpt` (entropy/JSON/PEM/credential-URL detectors, with
/// the hex/UUID allowlist that spares Forge's own SHAs) the evidence path uses.
fn read_hunk(
    repo_root: &Path,
    tree_a: &str,
    tree_b: &str,
    path: &str,
) -> Result<(Option<String>, bool)> {
    let raw = git(repo_root, &["diff", tree_a, tree_b, "--", path])?;
    if raw.is_empty() {
        return Ok((None, false));
    }
    let (redacted, _kinds) = forge_content::redact_evidence_excerpt(&raw);
    let truncated = redacted.len() > HUNK_LIMIT;
    let bounded = if truncated {
        // Truncate on a char boundary so we never split a UTF-8 sequence.
        let mut end = HUNK_LIMIT;
        while end > 0 && !redacted.is_char_boundary(end) {
            end -= 1;
        }
        redacted[..end].to_string()
    } else {
        redacted
    };
    Ok((Some(bounded), truncated))
}

/// The `Forge-*` provenance trailers parsed from a published commit message (NER-137).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForgeTrailers {
    pub proposal_id: Option<String>,
    pub proposal_revision_id: Option<String>,
    pub provenance_digest: Option<String>,
    pub decision_actor: Option<String>,
    pub gates: Option<String>,
}

/// The result of a successful `verify-branch` (NER-137 U6). A clean verification means
/// the published `Forge-Provenance-Digest` matches the digest recomputed from the local
/// ledger — trailer↔current-ledger consistency, NOT cross-machine authenticity.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TrailerVerification {
    pub verified: bool,
    pub proposal_id: String,
    pub provenance_digest: String,
}

/// Read a commit's full message body via `git show -s --format=%B` (NER-137).
pub fn read_commit_message(repo_root: &Path, branch_or_commit: &str) -> Result<String> {
    git(repo_root, &["show", "-s", "--format=%B", branch_or_commit])
}

/// Parse the `Forge-*` provenance trailer lines out of a commit message. A single-pass
/// line scan (not git's trailer canonicalization) so it is robust to the human prose
/// preceding the trailer block.
pub fn parse_forge_trailers(message: &str) -> ForgeTrailers {
    let mut trailers = ForgeTrailers::default();
    for line in message.lines() {
        if let Some(value) = line.strip_prefix("Forge-Proposal-Id:") {
            trailers.proposal_id = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Forge-Proposal-Revision-Id:") {
            trailers.proposal_revision_id = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Forge-Provenance-Digest:") {
            trailers.provenance_digest = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Forge-Decision-Actor:") {
            trailers.decision_actor = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Forge-Gates:") {
            trailers.gates = Some(value.trim().to_string());
        }
    }
    trailers
}

/// Verify a published branch's provenance trailer against the local ledger (NER-137 U6,
/// R7). Reads the commit's `Forge-*` trailers, recomputes the provenance digest from the
/// deciding evidence/decision rows (`forge_store::build_publication_trailer`, which also
/// re-verifies the deciding evidence and raises `EVIDENCE_TAMPERED` on a tampered row),
/// and **fails closed** with `PROVENANCE_MISMATCH` when the published digest differs.
///
/// A PASS confirms the published trailer is consistent with the **current local ledger**
/// — it detects a rewritten commit message or a naively-edited ledger row. It is NOT an
/// authenticity proof: an attacker who rewrites the ledger rows AND re-exports still
/// matches (the cheap-check boundary; cross-machine authenticity is Phase 9 signing).
pub fn verify_publication_trailer(
    repo_root: &Path,
    branch_or_commit: &str,
) -> Result<TrailerVerification> {
    let message = read_commit_message(repo_root, branch_or_commit)?;
    let trailers = parse_forge_trailers(&message);
    let revision_id =
        trailers
            .proposal_revision_id
            .ok_or_else(|| ForgeError::MissingProvenanceTrailer {
                branch: branch_or_commit.to_string(),
                missing_field: "proposal_revision_id".to_string(),
            })?;
    let published =
        trailers
            .provenance_digest
            .ok_or_else(|| ForgeError::MissingProvenanceTrailer {
                branch: branch_or_commit.to_string(),
                missing_field: "provenance_digest".to_string(),
            })?;

    let recomputed = forge_store::build_publication_trailer(repo_root, &revision_id)?;
    if recomputed.provenance_digest != published {
        return Err(ForgeError::ProvenanceMismatch {
            proposal_id: trailers
                .proposal_id
                .unwrap_or_else(|| recomputed.proposal_id.clone()),
            published_digest: published,
            recomputed_digest: recomputed.provenance_digest,
        }
        .into());
    }
    Ok(TrailerVerification {
        verified: true,
        proposal_id: recomputed.proposal_id,
        provenance_digest: recomputed.provenance_digest,
    })
}

/// Export the accepted proposal as a new branch, returning `(commit_id, excluded)`.
///
/// `excluded` is the list of secret-risk-named paths dropped from the resulting git
/// tree by the default-deny policy (NER-133 U6). The drop is enforced on the FINAL
/// git tree via [`filter_secret_paths_from_tree`], uniformly across both content-ref
/// kinds, so the native `synthesize_git_tree` `git add -A` path cannot reintroduce a
/// secret file. The `BranchExists`-identical early return reports no exclusions
/// (`vec![]`) — the existing commit is reused unchanged.
pub fn export_branch(
    repo_root: &Path,
    branch_name: &str,
    base_commit: &str,
    current_target: &str,
    content_ref: &str,
    message: &str,
) -> Result<(String, Vec<String>)> {
    if current_target != base_commit {
        return Err(ForgeError::StaleBase {
            expected_head: base_commit.to_string(),
            actual_head: current_target.to_string(),
        }
        .into());
    }
    // NER-138 Phase 7 slice 2: a native repo's `base_commit` is now an `f1:commit:` id,
    // not a git commit SHA, so it cannot be a `git commit-tree` parent directly. Resolve
    // it to a deterministic synthesized git commit; a git-backend base passes through
    // unchanged. The stale-base re-check above stays on the original anchors (both native
    // or both git); only the git-interop parent uses the resolved SHA.
    let parent = resolve_git_base_commit(repo_root, base_commit)?;
    let raw_tree = git_tree_for_content_ref(repo_root, content_ref)?;
    let (tree, excluded) = filter_secret_paths_from_tree(repo_root, &raw_tree)?;
    if forge_content_git::branch_exists(repo_root, branch_name) {
        let existing_commit = git(repo_root, &["rev-parse", branch_name])?
            .trim()
            .to_string();
        let existing_tree = git(repo_root, &["show", "-s", "--format=%T", &existing_commit])?
            .trim()
            .to_string();
        let existing_parent = git(repo_root, &["show", "-s", "--format=%P", &existing_commit])?
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        if existing_tree == tree && existing_parent == parent {
            return Ok((existing_commit, Vec::new()));
        }
        return Err(ForgeError::BranchExists {
            name: branch_name.to_string(),
        }
        .into());
    }
    let commit_id = forge_content_git::create_branch_from_git_tree(
        repo_root,
        branch_name,
        &parent,
        &tree,
        message,
    )?;
    Ok((commit_id, excluded))
}

/// Resolve a base anchor to a git commit SHA usable as a `git commit-tree` parent
/// (NER-138 Phase 7 slice 2). A git-backend base is already a git commit SHA → returned
/// unchanged. A native-backend base is an `f1:commit:` id → its tree is synthesized into a
/// git tree (reusing the export path's `synthesize_git_tree`) and committed as a
/// **deterministic parentless** git commit, so idempotent re-export reconciles to the same
/// parent SHA. The native base tree was built by the policy-filtered walker, so the
/// synthesized git tree already excludes `is_ignored_by_policy` paths (.env/keys); S2 is
/// preserved end-to-end. S1: a missing/corrupt native base surfaces a path-free error
/// (`base_content_ref` and the git invocations carry no filesystem path).
fn resolve_git_base_commit(repo_root: &Path, base_anchor: &str) -> Result<String> {
    // A git-backend base is a git commit SHA (it does not parse as a native ObjectId); a
    // native base is an `f1:commit:` id. Route the discriminator through `ObjectId::parse`
    // so the wire-format knowledge lives in forge-content-native (the canonical parser),
    // not a string literal here that a future format bump (slice 3 object-kind headers)
    // could silently desync.
    let is_native_commit = forge_content_native::ObjectId::parse(base_anchor)
        .map(|id| matches!(id.kind(), Ok(forge_content_native::ObjectKind::Commit)))
        .unwrap_or(false);
    if !is_native_commit {
        return Ok(base_anchor.to_string());
    }
    let base_ref =
        forge_content_native::NativeContentBackend.base_content_ref(repo_root, base_anchor)?;
    let base_tree = synthesize_git_tree(repo_root, &base_ref)?;
    synthesize_deterministic_commit(repo_root, &base_tree)
}

/// `git commit-tree <tree>` (no parent) with a fully fixed environment (identity, epoch
/// date, message) and `core.autocrlf=false`, so the resulting commit SHA is deterministic
/// for a given tree. Determinism is load-bearing: idempotent re-export compares this
/// synthesized parent against the existing branch's parent. (Cross-machine determinism
/// additionally depends on the synthesized tree's blob bytes being identical; within one
/// environment — the re-export reconciliation case — this is fully deterministic.)
fn synthesize_deterministic_commit(repo_root: &Path, tree: &str) -> Result<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "core.autocrlf=false",
            "commit-tree",
            tree,
            "-m",
            FORGE_SYNTH_BASE_MESSAGE,
        ])
        .current_dir(repo_root)
        .env("GIT_AUTHOR_NAME", FORGE_SYNTH_IDENTITY)
        .env("GIT_AUTHOR_EMAIL", FORGE_SYNTH_EMAIL)
        .env("GIT_COMMITTER_NAME", FORGE_SYNTH_IDENTITY)
        .env("GIT_COMMITTER_EMAIL", FORGE_SYNTH_EMAIL)
        .env("GIT_AUTHOR_DATE", FORGE_SYNTH_DATE)
        .env("GIT_COMMITTER_DATE", FORGE_SYNTH_DATE)
        .output()?;
    if !output.status.success() {
        // S1: stderr from commit-tree (e.g. "fatal: not a valid object name") carries no
        // filesystem path; do not interpolate any path here.
        return Err(anyhow!(
            "git commit-tree failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Rewrite `tree` to drop every secret-risk-named entry, returning the (possibly
/// unchanged) tree hash and the dropped paths (NER-133 U6). When no entry is
/// secret-risk the original tree is returned untouched (fast path). Otherwise the
/// tree is rebuilt through a temporary index (no worktree needed): `read-tree`,
/// `rm --cached` each dropped path, then `write-tree`.
fn filter_secret_paths_from_tree(repo_root: &Path, tree: &str) -> Result<(String, Vec<String>)> {
    // `-z` (NUL-delimited, never C-quoted) is load-bearing for the secret-path drop:
    // without it `ls-tree` C-quotes any path containing a tab/newline/non-ASCII byte
    // (e.g. `.env\u{a0}prod` -> `".env\u{a0}prod"`), which would slip
    // `is_secret_risk_path` and leak the secret-named blob into the published export tree
    // (NER-142, the egress twin of the `diff_trees` `-z` fix above). `-z` emits one
    // `<path>\0` record per entry, so split on `\0` and drop the trailing empty field.
    let listing = git(repo_root, &["ls-tree", "-r", "-z", "--name-only", tree])?;
    let dropped: Vec<String> = listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .filter(|path| forge_content::is_secret_risk_path(path))
        .map(str::to_string)
        .collect();
    if dropped.is_empty() {
        return Ok((tree.to_string(), Vec::new()));
    }
    let index_dir = tempfile::tempdir()?;
    let index_path = index_dir.path().join("index");
    git_with_index(repo_root, &index_path, &["read-tree", tree])?;
    for path in &dropped {
        // `:(literal)` pathspec magic: match PATH byte-for-byte, never as a glob. Without
        // it a secret-risk name containing a pathspec metacharacter (`*`, `[`, `?`) or a
        // leading `:` is collected into `dropped` above (it matches `is_secret_risk_path`)
        // but silently NOT removed here — `rm` reads `.env[prod]` as a wildcard that fails
        // to match the literal file, and `--ignore-unmatch` swallows the miss, leaving the
        // secret in the published tree (NER-142: the write-side twin of the `-z` read fix
        // above — together they make the drop robust to every adversarial filename).
        let literal = format!(":(literal){path}");
        git_with_index(
            repo_root,
            &index_path,
            &["rm", "--cached", "--ignore-unmatch", &literal],
        )?;
    }
    let new_tree = git_with_index(repo_root, &index_path, &["write-tree"])?
        .trim()
        .to_string();
    Ok((new_tree, dropped))
}

fn git_tree_for_content_ref(repo_root: &Path, content_ref: &str) -> Result<String> {
    match classify_content_ref(content_ref) {
        ContentRefKind::GitTree(tree) => Ok(tree.to_string()),
        ContentRefKind::ForgeTree(_) => synthesize_git_tree(repo_root, content_ref),
        ContentRefKind::Unsupported => Err(anyhow!("unsupported content ref")),
    }
}

fn synthesize_git_tree(repo_root: &Path, content_ref: &str) -> Result<String> {
    let worktree = tempfile::tempdir()?;
    forge_content_native::materialize_content_ref(repo_root, worktree.path(), content_ref)?;
    // Defense-in-depth (NER-133 FIX F): delete secret-risk-named files from the temp
    // worktree BEFORE `git add -A`, so their bytes never enter `.git/objects` as
    // loose blobs in the first place. `filter_secret_paths_from_tree` remains the
    // backstop that drops them from the FINAL tree, but that backstop runs only
    // after `add` has already hashed the file into the object store; removing them
    // here closes that window for the native materialize path.
    remove_secret_risk_files(worktree.path(), worktree.path())?;
    let index_dir = tempfile::tempdir()?;
    let index_path = index_dir.path().join("index");
    // `-c core.autocrlf=false`: store the materialized native bytes verbatim (no line-ending
    // normalization) so the synthesized git tree SHA is content-faithful to the native tree
    // and reproducible regardless of the operator's global git config. This matches the same
    // pin on `synthesize_deterministic_commit` and hardens cross-machine export determinism.
    git_with_index_and_worktree(
        repo_root,
        worktree.path(),
        &index_path,
        &["-c", "core.autocrlf=false", "add", "-A", "."],
    )?;
    Ok(git_with_index_and_worktree(
        repo_root,
        worktree.path(),
        &index_path,
        &["-c", "core.autocrlf=false", "write-tree"],
    )?
    .trim()
    .to_string())
}

/// Recursively delete files whose path (relative to `root`) is secret-risk by name
/// (NER-133 FIX F). Walks `dir`, removing matching files so they are never staged.
/// `.git` is skipped — the temp worktree has none yet, but the guard is cheap.
fn remove_secret_risk_files(root: &Path, dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            remove_secret_risk_files(root, &path)?;
        } else {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let relative_str = relative.to_string_lossy();
            if forge_content::is_secret_risk_path(&relative_str) {
                std::fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

fn git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Run `git` against `repo_root`'s object database with a scratch index but NO
/// worktree — for index-only plumbing (`read-tree`/`rm --cached`/`write-tree`) used
/// by the secret-path tree rewrite (NER-133 U6).
fn git_with_index(repo_root: &Path, index_path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_DIR", repo_root.join(".git"))
        .env("GIT_INDEX_FILE", index_path)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn git_with_index_and_worktree(
    repo_root: &Path,
    worktree: &Path,
    index_path: &Path,
    args: &[&str],
) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_DIR", repo_root.join(".git"))
        .env("GIT_WORK_TREE", worktree)
        .env("GIT_INDEX_FILE", index_path)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        for args in [
            ["init", "-q"].as_slice(),
            ["config", "user.email", "t@example.test"].as_slice(),
            ["config", "user.name", "Forge Test"].as_slice(),
        ] {
            git(dir.path(), args).expect("git setup");
        }
        dir
    }

    /// Build a git tree containing the given `(name, contents)` files via a scratch
    /// index, returning the tree hash. Exercises the same plumbing the export path
    /// uses, but lets us construct a tree that DOES contain a secret-named file —
    /// something the snapshot scans never produce — so the rewrite has work to do.
    fn build_tree(repo: &Path, files: &[(&str, &str)]) -> String {
        let index_dir = tempfile::tempdir().expect("index dir");
        let index_path = index_dir.path().join("index");
        for (name, contents) in files {
            let blob = {
                let output = Command::new("git")
                    .args(["hash-object", "-w", "--stdin"])
                    .env("GIT_DIR", repo.join(".git"))
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut child| {
                        use std::io::Write as _;
                        child.stdin.take().unwrap().write_all(contents.as_bytes())?;
                        child.wait_with_output()
                    })
                    .expect("hash-object");
                String::from_utf8(output.stdout).unwrap().trim().to_string()
            };
            git_with_index(
                repo,
                &index_path,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{blob},{name}"),
                ],
            )
            .expect("update-index");
        }
        git_with_index(repo, &index_path, &["write-tree"])
            .expect("write-tree")
            .trim()
            .to_string()
    }

    #[test]
    fn rewrite_drops_secret_entries_and_reports_them() {
        let repo = init_repo();
        let tree = build_tree(
            repo.path(),
            &[
                ("README.md", "hello\n"),
                (".env", "SECRET=abc\n"),
                ("certs/server.pem", "-----BEGIN-----\n"),
            ],
        );

        let (new_tree, dropped) =
            filter_secret_paths_from_tree(repo.path(), &tree).expect("rewrite");

        assert_ne!(
            new_tree, tree,
            "tree must be rewritten when secrets present"
        );
        let listing = git(repo.path(), &["ls-tree", "-r", "--name-only", &new_tree]).unwrap();
        let entries: Vec<&str> = listing.lines().collect();
        assert_eq!(entries, vec!["README.md"]);
        assert_eq!(
            dropped,
            vec![".env".to_string(), "certs/server.pem".to_string()]
        );
    }

    /// FIX J (U6 warnings): `export_branch` must REPORT the dropped secret-risk
    /// paths in its `excluded` return — the vec the CLI turns into non-empty
    /// `warnings[]`. End-to-end through the CLI this can't be exercised because the
    /// snapshot-time exclusion strips secrets before they ever reach a tree; here we
    /// hand-build a git-tree content ref that DOES contain a secret and drive the
    /// real `export_branch`, asserting the secret is both dropped from the tree and
    /// named in `excluded`.
    #[test]
    fn export_branch_reports_dropped_secret_in_excluded() {
        let repo = init_repo();
        // Seed a base commit so `base_commit` resolves and the branch can be created.
        std::fs::write(repo.path().join("seed.txt"), "seed\n").expect("seed");
        git(repo.path(), &["add", "."]).expect("git add");
        git(repo.path(), &["commit", "-m", "base"]).expect("git commit");
        let base_commit = git(repo.path(), &["rev-parse", "HEAD"])
            .expect("rev-parse")
            .trim()
            .to_string();

        // A git-tree content ref carrying a secret-named file the export must drop.
        let tree = build_tree(
            repo.path(),
            &[("README.md", "hi\n"), (".env", "SECRET=1\n")],
        );
        let content_ref = format!("git-tree:{tree}");

        let (_commit, excluded) = export_branch(
            repo.path(),
            "forge/with-secret",
            &base_commit,
            &base_commit,
            &content_ref,
            "msg",
        )
        .expect("export branch");

        assert_eq!(
            excluded,
            vec![".env".to_string()],
            "the dropped secret path must be reported (becomes a CLI warning)"
        );
        let listing = git(
            repo.path(),
            &["ls-tree", "-r", "--name-only", "forge/with-secret"],
        )
        .expect("ls-tree branch");
        assert!(
            !listing.lines().any(|line| line == ".env"),
            "exported branch tree must not contain .env"
        );
    }

    #[test]
    fn rewrite_drops_a_secret_path_with_non_ascii_bytes() {
        // NER-142: `ls-tree` C-quotes a path with a tab/newline/non-ASCII byte unless
        // `-z` is passed; the quoted form `".env.café"` would slip `is_secret_risk_path`
        // and leave the secret blob in the rewritten export tree. With `-z` the true
        // unquoted path reaches the filter and is dropped. `.env.café` matches `.env.*`.
        let repo = init_repo();
        let tree = build_tree(
            repo.path(),
            &[("README.md", "hello\n"), (".env.café", "SECRET=1\n")],
        );

        let (new_tree, dropped) =
            filter_secret_paths_from_tree(repo.path(), &tree).expect("rewrite");

        assert_ne!(
            new_tree, tree,
            "tree must be rewritten when a non-ascii secret is present"
        );
        assert_eq!(
            dropped,
            vec![".env.café".to_string()],
            "the non-ascii secret path must be reported as dropped, not leaked"
        );
        // And it must be physically absent from the rewritten tree (read back with -z so
        // the assertion itself can't be fooled by C-quoting).
        let listing = git(
            repo.path(),
            &["ls-tree", "-r", "-z", "--name-only", &new_tree],
        )
        .unwrap();
        let entries: Vec<&str> = listing.split('\0').filter(|p| !p.is_empty()).collect();
        assert_eq!(entries, vec!["README.md"]);
    }

    #[test]
    fn rewrite_drops_a_secret_path_with_pathspec_glob_metacharacters() {
        // NER-142 (write side): a secret-risk name containing a pathspec metacharacter
        // (`[`) is collected into `dropped` by `is_secret_risk_path`, but a plain
        // `rm --cached '.env[prod]'` reads `[prod]` as a glob character class that never
        // matches the literal file — and `--ignore-unmatch` hides the miss, leaving the
        // secret in the tree. The `:(literal)` pathspec forces a verbatim match so it is
        // actually removed. `.env[prod]` matches `.env.*`/`.env*` secret-risk naming.
        let repo = init_repo();
        let tree = build_tree(
            repo.path(),
            &[("README.md", "hello\n"), (".env[prod]", "SECRET=1\n")],
        );

        let (new_tree, dropped) =
            filter_secret_paths_from_tree(repo.path(), &tree).expect("rewrite");

        assert_eq!(
            dropped,
            vec![".env[prod]".to_string()],
            "the glob-metacharacter secret path must be reported as dropped"
        );
        // The report is only honest if the file is ACTUALLY gone from the tree.
        let listing = git(
            repo.path(),
            &["ls-tree", "-r", "-z", "--name-only", &new_tree],
        )
        .unwrap();
        let entries: Vec<&str> = listing.split('\0').filter(|p| !p.is_empty()).collect();
        assert_eq!(
            entries,
            vec!["README.md"],
            "the glob-metacharacter secret must be physically removed, not just reported"
        );
    }

    #[test]
    fn rewrite_is_a_noop_when_no_secret_entries() {
        let repo = init_repo();
        let tree = build_tree(
            repo.path(),
            &[("README.md", "hello\n"), ("src/main.rs", "fn main(){}\n")],
        );

        let (new_tree, dropped) =
            filter_secret_paths_from_tree(repo.path(), &tree).expect("rewrite");

        assert_eq!(new_tree, tree, "clean tree returned unchanged (fast path)");
        assert!(dropped.is_empty());
    }

    fn ref_of(tree: &str) -> String {
        format!("git-tree:{tree}")
    }

    #[test]
    fn diff_trees_reports_modified_file_with_counts_and_hunk() {
        let repo = init_repo();
        let a = build_tree(repo.path(), &[("README.md", "one\ntwo\n")]);
        let b = build_tree(repo.path(), &[("README.md", "one\ntwo\nthree\n")]);

        let diff = diff_trees(repo.path(), &ref_of(&a), &ref_of(&b), true).expect("diff");
        assert_eq!(diff.files.len(), 1);
        let file = &diff.files[0];
        assert_eq!(file.path, "README.md");
        assert_eq!(file.status, "M");
        assert_eq!(file.insertions, Some(1));
        assert_eq!(file.deletions, Some(0));
        assert!(!file.binary);
        assert!(file.hunk.as_deref().unwrap().contains("+three"));
        assert!(diff.dropped_secret_paths.is_empty());
    }

    #[test]
    fn diff_trees_reports_added_and_deleted() {
        let repo = init_repo();
        let a = build_tree(repo.path(), &[("keep.txt", "x\n"), ("gone.txt", "y\n")]);
        let b = build_tree(repo.path(), &[("keep.txt", "x\n"), ("new.txt", "z\n")]);

        let diff = diff_trees(repo.path(), &ref_of(&a), &ref_of(&b), false).expect("diff");
        let by_path: std::collections::HashMap<_, _> =
            diff.files.iter().map(|f| (f.path.as_str(), f)).collect();
        assert_eq!(by_path["gone.txt"].status, "D");
        assert_eq!(by_path["new.txt"].status, "A");
        // include_hunks = false -> no hunk bodies.
        assert!(diff.files.iter().all(|f| f.hunk.is_none()));
    }

    #[test]
    fn diff_trees_identical_trees_is_empty() {
        let repo = init_repo();
        let a = build_tree(repo.path(), &[("README.md", "same\n")]);
        let diff = diff_trees(repo.path(), &ref_of(&a), &ref_of(&a), true).expect("diff");
        assert!(diff.files.is_empty());
        assert!(diff.dropped_secret_paths.is_empty());
    }

    #[test]
    fn diff_trees_drops_secret_paths_and_reads_no_hunk_for_them() {
        let repo = init_repo();
        let a = build_tree(repo.path(), &[("README.md", "hi\n")]);
        let b = build_tree(
            repo.path(),
            &[("README.md", "hi there\n"), (".env", "SECRET=abc\n")],
        );
        let diff = diff_trees(repo.path(), &ref_of(&a), &ref_of(&b), true).expect("diff");
        assert!(
            diff.files.iter().all(|f| f.path != ".env"),
            ".env must be dropped from the diff"
        );
        assert_eq!(diff.dropped_secret_paths, vec![".env".to_string()]);
        // The non-secret change is still present with its hunk.
        assert!(diff.files.iter().any(|f| f.path == "README.md"));
    }

    #[test]
    fn diff_trees_keeps_counts_for_a_rename() {
        // With `--no-renames` a rename shows as D(old)+A(new), both keyed on a plain
        // path so numstat counts are preserved (the rename `old => new` numstat key
        // would otherwise lose them).
        let repo = init_repo();
        let a = build_tree(repo.path(), &[("foo.txt", "line1\nline2\n")]);
        let b = build_tree(repo.path(), &[("bar.txt", "line1\nline2\n")]);
        let diff = diff_trees(repo.path(), &ref_of(&a), &ref_of(&b), false).expect("diff");
        let by_path: std::collections::HashMap<_, _> =
            diff.files.iter().map(|f| (f.path.as_str(), f)).collect();
        assert_eq!(by_path["foo.txt"].status, "D");
        assert_eq!(by_path["foo.txt"].deletions, Some(2));
        assert_eq!(by_path["bar.txt"].status, "A");
        assert_eq!(by_path["bar.txt"].insertions, Some(2));
    }

    #[test]
    fn diff_trees_drops_a_secret_path_with_non_ascii_bytes() {
        // `-z` emits paths unquoted; without it git C-quotes a non-ASCII path and the
        // secret-risk drop would miss it (a filename leak). `.env.café` matches `.env.*`.
        let repo = init_repo();
        let a = build_tree(repo.path(), &[("README.md", "x\n")]);
        let b = build_tree(
            repo.path(),
            &[("README.md", "x\n"), (".env.café", "SECRET=1\n")],
        );
        let diff = diff_trees(repo.path(), &ref_of(&a), &ref_of(&b), true).expect("diff");
        assert!(
            diff.files.iter().all(|f| f.path != ".env.café"),
            "non-ascii secret path must be dropped, not leaked"
        );
        assert!(diff.dropped_secret_paths.iter().any(|p| p == ".env.café"));
    }

    #[test]
    fn diff_trees_bounds_a_large_hunk() {
        let repo = init_repo();
        let a = build_tree(repo.path(), &[("big.txt", "start\n")]);
        // A change far larger than HUNK_LIMIT so the hunk body must be truncated.
        let big = format!("start\n{}", "line\n".repeat(2000));
        let b = build_tree(repo.path(), &[("big.txt", &big)]);
        let diff = diff_trees(repo.path(), &ref_of(&a), &ref_of(&b), true).expect("diff");
        let file = diff.files.iter().find(|f| f.path == "big.txt").unwrap();
        assert!(file.truncated, "hunk should be marked truncated");
        assert!(file.hunk.as_deref().unwrap().len() <= HUNK_LIMIT);
    }

    // --- NER-138 Phase 7 slice 2: native-base git-export interop ---

    #[test]
    fn resolve_git_base_commit_passes_through_a_git_sha() {
        // A git-backend base (a real git commit SHA, no f1:commit: prefix) is returned
        // unchanged, so git-repo export behavior is byte-identical to before.
        let repo = init_repo();
        std::fs::write(repo.path().join("seed.txt"), "s\n").unwrap();
        git(repo.path(), &["add", "."]).unwrap();
        git(repo.path(), &["commit", "-m", "base"]).unwrap();
        let sha = git(repo.path(), &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(resolve_git_base_commit(repo.path(), &sha).unwrap(), sha);
    }

    #[test]
    fn export_branch_with_native_base_reconciles_idempotently() {
        // A native repo's base is an f1:commit: id; export must synthesize a git parent,
        // produce a reviewable branch, and re-export idempotently (deterministic parent).
        let repo = init_repo();
        std::fs::write(repo.path().join("app.txt"), "v1\n").unwrap();
        let backend = forge_content_native::NativeContentBackend;
        let base = backend.current_base(repo.path()).unwrap();
        assert!(base.starts_with("f1:commit:"), "native base: {base}");
        std::fs::write(repo.path().join("app.txt"), "v2\n").unwrap();
        let content_ref = backend.snapshot_worktree(repo.path()).unwrap().content_ref;

        let (commit, _excluded) = export_branch(
            repo.path(),
            "forge/native-x",
            &base,
            &base,
            &content_ref,
            "msg",
        )
        .expect("native export");
        let listing = git(
            repo.path(),
            &["ls-tree", "-r", "--name-only", "forge/native-x"],
        )
        .unwrap();
        assert!(listing.lines().any(|l| l == "app.txt"));
        // Re-export the same proposal to the same branch: deterministic parent + matching
        // tree → idempotent reconcile to the same commit (not BRANCH_EXISTS).
        let (commit2, _) = export_branch(
            repo.path(),
            "forge/native-x",
            &base,
            &base,
            &content_ref,
            "msg",
        )
        .expect("idempotent re-export");
        assert_eq!(
            commit, commit2,
            "re-export must reconcile to the same commit"
        );
    }

    #[test]
    fn synthesized_native_base_parent_is_deterministic_across_repos() {
        // Same base tree in two fresh git repos → identical synthesized parent SHA (the
        // cross-environment determinism reconciliation depends on).
        let mk = || {
            let repo = init_repo();
            std::fs::write(repo.path().join("same.txt"), "identical\n").unwrap();
            let base = forge_content_native::NativeContentBackend
                .current_base(repo.path())
                .unwrap();
            let parent = resolve_git_base_commit(repo.path(), &base).unwrap();
            (repo, parent)
        };
        let (_r1, p1) = mk();
        let (_r2, p2) = mk();
        assert_eq!(
            p1, p2,
            "identical base tree must synthesize the same parent SHA"
        );
    }

    #[test]
    fn export_branch_with_empty_native_base_does_not_error() {
        // Genesis over an empty worktree → an empty base tree; synthesis + commit-tree
        // must handle the empty-tree case.
        let repo = init_repo();
        let backend = forge_content_native::NativeContentBackend;
        let base = backend.current_base(repo.path()).unwrap(); // empty genesis
        std::fs::write(repo.path().join("a.txt"), "hi\n").unwrap();
        let content_ref = backend.snapshot_worktree(repo.path()).unwrap().content_ref;
        export_branch(
            repo.path(),
            "forge/empty-base",
            &base,
            &base,
            &content_ref,
            "msg",
        )
        .expect("empty-base export must not error");
    }

    #[test]
    fn resolve_git_base_commit_missing_native_commit_is_path_free() {
        // S1: a base pointing at a missing native commit object surfaces a path-free error.
        let repo = init_repo();
        let missing = format!("f1:commit:sha256:{}", "0".repeat(64));
        let error = resolve_git_base_commit(repo.path(), &missing).unwrap_err();
        let repo_str = repo.path().to_string_lossy();
        assert!(
            !error.to_string().contains(&*repo_str) && !format!("{error:#}").contains(&*repo_str),
            "S1: resolve_git_base_commit leaked a path: {error:#}"
        );
    }

    #[test]
    fn native_base_synthesis_excludes_non_ascii_secret() {
        // NER-142 class: a non-ASCII secret-named path must not reach the synthesized base
        // git tree. The native walker already excludes it before the base tree is built, so
        // it never enters the f1:commit: tree the parent is synthesized from.
        let repo = init_repo();
        std::fs::write(repo.path().join("keep.txt"), "ok\n").unwrap();
        std::fs::write(repo.path().join(".env.café"), "SECRET=1\n").unwrap();
        let base = forge_content_native::NativeContentBackend
            .current_base(repo.path())
            .unwrap();
        let parent = resolve_git_base_commit(repo.path(), &base).unwrap();
        let tree = git(repo.path(), &["show", "-s", "--format=%T", &parent])
            .unwrap()
            .trim()
            .to_string();
        let listing = git(repo.path(), &["ls-tree", "-r", "--name-only", &tree]).unwrap();
        assert!(listing.lines().any(|l| l == "keep.txt"));
        assert!(
            !listing.lines().any(|l| l.contains(".env.caf")),
            "secret must not appear in the synthesized base tree: {listing}"
        );
    }
}
