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

/// FIX F (defense-in-depth): the native `synthesize_git_tree` path now deletes
/// secret-risk-named files from the temp worktree BEFORE `git add -A`, so the
/// secret bytes never enter `.git/objects`. The user-facing assertion is that the
/// exported native tree excludes the secret (covered by
/// `export_branch_excludes_secret_path_native_backend`); here we additionally
/// assert the secret string is absent from the materialized-then-exported tree's
/// blobs by grepping the exported branch content.
#[test]
fn native_export_does_not_stage_secret_blob() {
    let repo = TestRepo::new_git();
    prepare_proposal_with_secret(&repo, &["--json", "init", "--content-backend", "native"]);

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/secret-native-blob"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/secret-native-blob");
    assert_branch_excludes_secret(&repo, "forge/secret-native-blob");

    // The secret value must not be reachable from the exported branch tree: no blob
    // under the tree carries the .env contents.
    let listing = git(
        repo.path(),
        &["ls-tree", "-r", "--name-only", "forge/secret-native-blob"],
    );
    assert!(
        !listing.lines().any(|line| line == ".env"),
        "exported native tree must not reference a .env blob, got:\n{listing}"
    );
    for path in listing.lines() {
        let blob = git(
            repo.path(),
            &["show", &format!("forge/secret-native-blob:{path}")],
        );
        assert!(
            !blob.contains("SECRET=abc123"),
            "no exported blob may carry the secret value (path {path})"
        );
    }
}

/// FIX J (U6 warnings): exercising a NON-empty `warnings[]` end-to-end through the
/// CLI is impossible because the snapshot-time exclusion strips secret-named paths
/// before they ever reach a content tree — so the export-layer rewrite finds
/// nothing to drop and `warnings[]` stays empty. The drop+warning (the `excluded`
/// vec the CLI maps to `warnings[]`) is therefore exercised where a tree CAN
/// contain a secret: `forge-export-git`'s `export_branch_reports_dropped_secret_in_excluded`
/// unit test. This CLI test documents and pins the end-to-end invariant: a normal
/// secret-bearing accept produces a clean export with EMPTY warnings (the secret is
/// gone by snapshot time, not merely warned about at export time).
#[test]
fn cli_export_warnings_empty_because_secret_stripped_at_snapshot_time() {
    let repo = TestRepo::new_git();
    prepare_proposal_with_secret(&repo, &["--json", "init"]);

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/u6-warnings"])
            .assert()
            .success(),
    );
    // Empty because the secret never reached the tree (stripped at snapshot time),
    // not because the export rewrite failed to detect it. The non-empty-warnings
    // path is proven in forge-export-git's unit test (see doc comment above).
    assert!(
        warnings(&exported).is_empty(),
        "end-to-end warnings are empty (secret stripped pre-tree), got {:?}",
        warnings(&exported)
    );
    assert_branch_excludes_secret(&repo, "forge/u6-warnings");
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
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
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
