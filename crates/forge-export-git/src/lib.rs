use anyhow::{anyhow, Result};
use forge_content::{classify_content_ref, ContentRefKind};
use forge_store::ForgeError;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

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

    // name-status: one `<STATUS>\t<path>` (or `<STATUS>\t<old>\t<new>` for R/C) per file.
    let name_status = git(repo_root, &["diff", "--name-status", &tree_a, &tree_b])?;
    // numstat: `<ins>\t<del>\t<path>`; binary files report `-\t-\t<path>`.
    let numstat = git(repo_root, &["diff", "--numstat", &tree_a, &tree_b])?;

    let mut counts: std::collections::HashMap<String, (Option<u64>, Option<u64>, bool)> =
        std::collections::HashMap::new();
    for line in numstat.lines() {
        let mut parts = line.split('\t');
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
    for line in name_status.lines() {
        let mut parts = line.split('\t');
        let Some(status) = parts.next() else { continue };
        // For rename/copy the final field is the new path; for the rest it is the path.
        let Some(path) = parts.next_back() else {
            continue;
        };
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
        if existing_tree == tree && existing_parent == base_commit {
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
        base_commit,
        &tree,
        message,
    )?;
    Ok((commit_id, excluded))
}

/// Rewrite `tree` to drop every secret-risk-named entry, returning the (possibly
/// unchanged) tree hash and the dropped paths (NER-133 U6). When no entry is
/// secret-risk the original tree is returned untouched (fast path). Otherwise the
/// tree is rebuilt through a temporary index (no worktree needed): `read-tree`,
/// `rm --cached` each dropped path, then `write-tree`.
fn filter_secret_paths_from_tree(repo_root: &Path, tree: &str) -> Result<(String, Vec<String>)> {
    let listing = git(repo_root, &["ls-tree", "-r", "--name-only", tree])?;
    let dropped: Vec<String> = listing
        .lines()
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
        git_with_index(
            repo_root,
            &index_path,
            &["rm", "--cached", "--ignore-unmatch", path],
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
    git_with_index_and_worktree(repo_root, worktree.path(), &index_path, &["add", "-A", "."])?;
    Ok(
        git_with_index_and_worktree(repo_root, worktree.path(), &index_path, &["write-tree"])?
            .trim()
            .to_string(),
    )
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
}
