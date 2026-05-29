use anyhow::{anyhow, Context, Result};
use forge_content::{is_ignored_by_policy, ContentBackend, SnapshotContent};
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
        for path in &target_paths {
            git(repo_root, &["checkout", tree, "--", path])?;
        }
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

    fn current_base(&self, repo_root: &Path) -> Result<String> {
        current_head(repo_root)
    }

    fn base_content_ref(&self, repo_root: &Path, base: &str) -> Result<String> {
        content_ref_for_commit_tree(repo_root, base)
    }
}

pub fn current_head(repo_root: &Path) -> Result<String> {
    Ok(git(repo_root, &["rev-parse", "--verify", "HEAD"])?
        .trim()
        .to_string())
}

pub fn content_ref_for_commit_tree(repo_root: &Path, commit: &str) -> Result<String> {
    let tree = git(repo_root, &["rev-parse", &format!("{commit}^{{tree}}")])?;
    Ok(format!("git-tree:{}", tree.trim()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_sidecars_are_excluded_by_policy() {
        // WAL (enabled in forge-store::open_connection) makes forge.db travel with
        // `-wal`/`-shm` sidecars holding committed-but-uncheckpointed data; this
        // backend must exclude them from exported trees just as the native one does.
        assert!(is_ignored_by_policy(".forge/forge.db"));
        assert!(is_ignored_by_policy(".forge/forge.db-wal"));
        assert!(is_ignored_by_policy(".forge/forge.db-shm"));
        // The NER-132 advisory lock file is covered by the same blanket `.forge/`
        // prefix; pin it symmetrically so the two backends cannot drift.
        assert!(is_ignored_by_policy(".forge/forge.lock"));
        assert!(is_ignored_by_policy(".forge"));
        // Restore temps live in worktree dirs (NER-132 U4); exclude them symmetrically
        // so an orphaned temp never lands in a git-backed snapshot/export.
        assert!(is_ignored_by_policy(".forge-restore-abc123"));
        assert!(is_ignored_by_policy("src/nested/.forge-restore-xyz"));
        // Symmetric secret/internal-path assertions: both backends now route to the
        // shared `forge_content::is_ignored_by_policy`, so the exclusion set cannot
        // drift (NER-133 U6). The git backend also gains `.git` exclusion — harmless
        // since git never reports `.git/` worktree paths.
        assert!(is_ignored_by_policy(".env"));
        assert!(is_ignored_by_policy("certs/server.pem"));
        assert!(is_ignored_by_policy(".git"));
        assert!(is_ignored_by_policy(".git/config"));
        assert!(!is_ignored_by_policy("README.md"));
    }
}
