mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn git(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn warnings(output: &Value) -> Vec<String> {
    output["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .map(|warning| warning.as_str().expect("warning string").to_string())
        .collect()
}

/// Drive a full lifecycle through `accept`, writing a secret-named file (`.env`)
/// alongside a normal file so the worktree carries both. `init_args` selects the
/// content backend.
///
/// Note: the shared snapshot/export exclusion predicate strips secret-named paths
/// at *snapshot* time (in both backends' worktree scans), so the resulting content
/// object/tree never contains `.env` to begin with. The export-layer tree rewrite
/// (NER-133 U6) is therefore a defense-in-depth backstop that re-verifies the FINAL
/// git tree; its drop+warning behavior is exercised directly in
/// `forge-export-git`'s unit tests (where a tree containing a secret can be
/// constructed). Here we assert the user-facing security outcome: the exported
/// branch tree and pr-body never carry the secret.
fn prepare_proposal_with_secret(repo: &TestRepo, init_args: &[&str]) {
    repo.forge().args(init_args).assert().success();
    repo.forge()
        .args(["--json", "start", "export proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "exported\n").expect("write readme");
    std::fs::write(repo.path().join(".env"), "SECRET=abc123\n").expect("write env");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();
}

fn assert_branch_excludes_secret(repo: &TestRepo, branch: &str) {
    let tree_listing = git(repo.path(), &["ls-tree", "-r", "--name-only", branch]);
    let entries: Vec<&str> = tree_listing.lines().collect();
    assert!(
        !entries.contains(&".env"),
        "exported branch tree must not contain .env, got {entries:?}"
    );
    assert!(
        entries.contains(&"README.md"),
        "exported branch tree must keep the normal file, got {entries:?}"
    );
}

#[test]
fn export_branch_excludes_secret_path_git_backend() {
    let repo = TestRepo::new_git();
    prepare_proposal_with_secret(&repo, &["--json", "init"]);

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/secret-git"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/secret-git");
    assert_branch_excludes_secret(&repo, "forge/secret-git");
}

#[test]
fn export_branch_excludes_secret_path_native_backend() {
    // The native backend materializes through `synthesize_git_tree`'s `git add -A`;
    // the secret must still be absent from the final committed tree (NER-133 U6).
    let repo = TestRepo::new_git();
    prepare_proposal_with_secret(&repo, &["--json", "init", "--content-backend", "native"]);

    let show = json_output(repo.forge().args(["--json", "show"]).assert().success());
    assert!(show["data"]["latest_proposal"]["content_ref"]
        .as_str()
        .unwrap()
        .starts_with("forge-tree:"));

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/secret-native"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/secret-native");
    assert_branch_excludes_secret(&repo, "forge/secret-native");
}

#[test]
fn export_pr_body_omits_secret_path() {
    let repo = TestRepo::new_git();
    prepare_proposal_with_secret(&repo, &["--json", "init"]);

    let output = json_output(
        repo.forge()
            .args(["--json", "export", "pr-body"])
            .assert()
            .success(),
    );
    let body = output["data"]["body"].as_str().expect("body string");
    assert!(
        !body.contains(".env"),
        "pr-body must omit .env, got:\n{body}"
    );
    assert!(
        body.contains("README.md"),
        "pr-body must keep README.md, got:\n{body}"
    );
}

#[test]
fn export_without_secret_paths_emits_no_warnings() {
    let repo = TestRepo::new_git();
    // No secret-named file in this proposal.
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "clean proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "clean\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/clean"])
            .assert()
            .success(),
    );
    assert!(
        warnings(&exported).is_empty(),
        "clean export must emit no warnings, got {:?}",
        warnings(&exported)
    );

    let pr_body = json_output(
        repo.forge()
            .args(["--json", "export", "pr-body"])
            .assert()
            .success(),
    );
    assert!(
        warnings(&pr_body).is_empty(),
        "clean pr-body must emit no warnings, got {:?}",
        warnings(&pr_body)
    );
}
