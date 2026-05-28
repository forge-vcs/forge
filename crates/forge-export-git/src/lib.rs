use anyhow::{anyhow, Result};
use forge_content::{classify_content_ref, ContentRefKind};
use std::path::Path;
use std::process::Command;

pub fn export_branch(
    repo_root: &Path,
    branch_name: &str,
    base_commit: &str,
    current_target: &str,
    content_ref: &str,
    message: &str,
) -> Result<String> {
    if current_target != base_commit {
        return Err(anyhow!("stale base"));
    }
    let tree = git_tree_for_content_ref(repo_root, content_ref)?;
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
            return Ok(existing_commit);
        }
        return Err(anyhow!("branch already exists"));
    }
    forge_content_git::create_branch_from_git_tree(
        repo_root,
        branch_name,
        base_commit,
        &tree,
        message,
    )
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
