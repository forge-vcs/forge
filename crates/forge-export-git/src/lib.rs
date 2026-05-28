use anyhow::{anyhow, Result};
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
    let tree = content_ref
        .strip_prefix("git-tree:")
        .ok_or_else(|| anyhow!("unsupported content ref"))?;
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
    forge_content_git::create_branch_from_tree(
        repo_root,
        branch_name,
        base_commit,
        content_ref,
        message,
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
