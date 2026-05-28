use anyhow::{anyhow, Context, Result};
use forge_content::{is_secret_risk_path, ContentBackend, SnapshotContent};
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

pub struct GitContentBackend;

impl ContentBackend for GitContentBackend {
    fn snapshot_worktree(&self, repo_root: &Path) -> Result<SnapshotContent> {
        let index_dir = tempdir().context("create temporary git index directory")?;
        let index_path = index_dir.path().join("index");

        let head_exists = git(repo_root, &["rev-parse", "--verify", "HEAD"]).is_ok();
        if head_exists {
            git_with_index(repo_root, &index_path, &["read-tree", "HEAD"])?;
        }

        for path in all_tracked_paths(repo_root)? {
            if is_ignored_by_policy(&path) {
                git_with_index(
                    repo_root,
                    &index_path,
                    &["rm", "-q", "--cached", "--ignore-unmatch", "--", &path],
                )?;
            } else {
                git_with_index(repo_root, &index_path, &["add", "--", &path])?;
            }
        }
        for path in untracked_paths(repo_root)? {
            git_with_index(repo_root, &index_path, &["add", "--", &path])?;
        }
        let tree = git_with_index(repo_root, &index_path, &["write-tree"])?;
        let changed_paths = changed_paths(repo_root)?;

        Ok(SnapshotContent {
            content_ref: format!("git-tree:{}", tree.trim()),
            changed_paths,
        })
    }

    fn restore_snapshot(&self, repo_root: &Path, content_ref: &str) -> Result<()> {
        let tree = content_ref
            .strip_prefix("git-tree:")
            .ok_or_else(|| anyhow!("unsupported content ref"))?;
        let target_paths = tree_paths(repo_root, tree)?;
        let current_paths = materialized_paths(repo_root)?;
        git(repo_root, &["checkout", tree, "--", "."])?;
        for path in current_paths {
            if target_paths.binary_search(&path).is_err() && !is_ignored_by_policy(&path) {
                let full_path = repo_root.join(&path);
                if full_path.is_file() || full_path.is_symlink() {
                    std::fs::remove_file(&full_path)
                        .with_context(|| format!("remove {}", full_path.display()))?;
                }
            }
        }
        Ok(())
    }
}

pub fn current_head(repo_root: &Path) -> Result<String> {
    Ok(git(repo_root, &["rev-parse", "--verify", "HEAD"])?
        .trim()
        .to_string())
}

pub fn branch_exists(repo_root: &Path, name: &str) -> bool {
    git(
        repo_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ],
    )
    .is_ok()
}

pub fn create_branch_from_tree(
    repo_root: &Path,
    branch: &str,
    base_commit: &str,
    content_ref: &str,
    message: &str,
) -> Result<String> {
    let tree = content_ref
        .strip_prefix("git-tree:")
        .ok_or_else(|| anyhow!("unsupported content ref"))?;
    create_branch_from_git_tree(repo_root, branch, base_commit, tree, message)
}

pub fn create_branch_from_git_tree(
    repo_root: &Path,
    branch: &str,
    base_commit: &str,
    tree: &str,
    message: &str,
) -> Result<String> {
    let commit = git(
        repo_root,
        &["commit-tree", tree, "-p", base_commit, "-m", message],
    )?;
    let commit = commit.trim().to_string();
    git(
        repo_root,
        &[
            "update-ref",
            &format!("refs/heads/{branch}"),
            &commit,
            "0000000000000000000000000000000000000000",
        ],
    )?;
    Ok(commit)
}

pub fn changed_paths(repo_root: &Path) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    if let Ok(output) = git(repo_root, &["diff", "--name-only", "HEAD", "--", "."]) {
        paths.extend(output.lines().map(str::to_string));
    }
    if let Ok(output) = git(repo_root, &["ls-files", "--others", "--exclude-standard"]) {
        paths.extend(output.lines().map(str::to_string));
    }
    paths.retain(|path| !is_ignored_by_policy(path));
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn git(repo_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("failed to run git {args:?}"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn git_with_index(repo_root: &Path, index_path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_INDEX_FILE", index_path)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("failed to run git {args:?} with alternate index"))?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn untracked_paths(repo_root: &Path) -> Result<Vec<String>> {
    Ok(
        git(repo_root, &["ls-files", "--others", "--exclude-standard"])?
            .lines()
            .filter(|path| !is_ignored_by_policy(path))
            .map(str::to_string)
            .collect(),
    )
}

fn all_tracked_paths(repo_root: &Path) -> Result<Vec<String>> {
    Ok(git(repo_root, &["ls-files"])?
        .lines()
        .map(str::to_string)
        .collect())
}

fn is_ignored_by_policy(path: &str) -> bool {
    path.starts_with(".forge/") || path == ".forge" || is_secret_risk_path(path)
}

fn tree_paths(repo_root: &Path, tree: &str) -> Result<Vec<String>> {
    let mut paths: Vec<String> = git(repo_root, &["ls-tree", "-r", "--name-only", tree])?
        .lines()
        .filter(|path| !is_ignored_by_policy(path))
        .map(str::to_string)
        .collect();
    paths.sort();
    Ok(paths)
}

fn materialized_paths(repo_root: &Path) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    if let Ok(output) = git(repo_root, &["ls-files"]) {
        paths.extend(
            output
                .lines()
                .filter(|path| !is_ignored_by_policy(path))
                .map(str::to_string),
        );
    }
    if let Ok(output) = git(repo_root, &["ls-files", "--others", "--exclude-standard"]) {
        paths.extend(
            output
                .lines()
                .filter(|path| !is_ignored_by_policy(path))
                .map(str::to_string),
        );
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}
