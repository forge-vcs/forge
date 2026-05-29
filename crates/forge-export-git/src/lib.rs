use anyhow::{anyhow, Result};
use forge_content::{classify_content_ref, ContentRefKind};
use forge_store::ForgeError;
use std::path::Path;
use std::process::Command;

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
    let index_dir = tempfile::tempdir()?;
    let index_path = index_dir.path().join("index");
    git_with_index_and_worktree(repo_root, worktree.path(), &index_path, &["add", "-A", "."])?;
    Ok(
        git_with_index_and_worktree(repo_root, worktree.path(), &index_path, &["write-tree"])?
            .trim()
            .to_string(),
    )
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
}
