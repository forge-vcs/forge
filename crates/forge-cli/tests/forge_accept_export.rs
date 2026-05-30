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

fn prepare_proposal(repo: &TestRepo) {
    prepare_proposal_with_init(repo, &["--json", "init"]);
}

fn prepare_native_proposal(repo: &TestRepo) {
    prepare_proposal_with_init(repo, &["--json", "init", "--content-backend", "native"]);
}

fn prepare_proposal_with_init(repo: &TestRepo, init_args: &[&str]) {
    repo.forge().args(init_args).assert().success();
    repo.forge()
        .args(["--json", "start", "export proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "exported\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
}

#[test]
fn accept_records_decision_and_export_branch_leaves_current_branch() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    let current_branch = git(repo.path(), &["branch", "--show-current"]);

    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    assert_eq!(accepted["data"]["decision"], "accepted");
    assert_eq!(
        git(repo.path(), &["branch", "--show-current"]),
        current_branch
    );

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/exported"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/exported");
    assert_eq!(
        git(repo.path(), &["branch", "--show-current"]),
        current_branch
    );
    git(repo.path(), &["rev-parse", "--verify", "forge/exported"]);

    let overwrite = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/exported"])
            .assert()
            .failure(),
    );
    assert_eq!(overwrite["errors"][0]["code"], "BRANCH_EXISTS");
}

#[test]
fn reject_prevents_export() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "reject"]).assert().success();

    let output = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/rejected"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "REJECTED");
}

#[test]
fn export_fails_when_base_is_stale() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();

    std::fs::write(repo.path().join("stale.txt"), "move head\n").expect("write stale file");
    git(repo.path(), &["add", "stale.txt"]);
    git(repo.path(), &["commit", "-m", "move target"]);

    let output = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/stale"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "STALE_BASE");
    assert!(output["operation_id"].as_str().unwrap().starts_with("op_"));
}

#[test]
fn export_reconciles_existing_branch_when_it_matches_expected_commit() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();
    let show = json_output(repo.forge().args(["--json", "show"]).assert().success());
    let proposal = &show["data"]["latest_proposal"];
    let base_head = proposal["base_head"].as_str().unwrap();
    let tree = proposal["content_ref"]
        .as_str()
        .unwrap()
        .strip_prefix("git-tree:")
        .unwrap();
    let commit = git(
        repo.path(),
        &[
            "commit-tree",
            tree,
            "-p",
            base_head,
            "-m",
            "Forge accepted proposal",
        ],
    );
    let commit = commit.trim();
    git(
        repo.path(),
        &["update-ref", "refs/heads/forge/recovered", commit],
    );

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/recovered"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/recovered");
    assert_eq!(exported["data"]["commit_id"], commit);
}

#[test]
fn accept_fails_when_base_is_stale() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    std::fs::write(repo.path().join("stale-before-accept.txt"), "move head\n")
        .expect("write stale file");
    git(repo.path(), &["add", "stale-before-accept.txt"]);
    git(repo.path(), &["commit", "-m", "move target before accept"]);

    let output = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(output["errors"][0]["code"], "STALE_BASE");
}

#[test]
fn export_requires_acceptance_for_exact_latest_revision() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();

    std::fs::write(repo.path().join("README.md"), "new proposal\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    let latest = json_output(repo.forge().args(["--json", "propose"]).assert().success());

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "export",
                "branch",
                "--proposal",
                latest["data"]["proposal_id"].as_str().unwrap(),
                "forge/not-accepted-latest",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "NOT_ACCEPTED");
}

#[test]
fn export_carries_a_structured_provenance_trailer() {
    // NER-137 U5: the published commit replaces the constant "Forge accepted proposal"
    // message with a structured Forge-* trailer; exactly one digest line.
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge()
        .args(["--json", "accept", "--actor", "alice"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/with-trailer"])
        .assert()
        .success();

    let message = git(
        repo.path(),
        &["show", "-s", "--format=%B", "forge/with-trailer"],
    );
    assert!(message.contains("Forge-Proposal-Id: "), "{message}");
    assert!(message.contains("Forge-Proposal-Revision-Id: "));
    assert!(message.contains("Forge-Decision-Actor: alice"));
    assert!(message.contains("Forge-Gates: "));

    let digest_lines: Vec<&str> = message
        .lines()
        .filter(|l| l.starts_with("Forge-Provenance-Digest: "))
        .collect();
    assert_eq!(digest_lines.len(), 1, "exactly one digest line");
    let digest = digest_lines[0]
        .trim_start_matches("Forge-Provenance-Digest: ")
        .trim();
    assert_eq!(digest.len(), 64, "64-hex provenance digest");
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    // The split that an earlier draft carried must NOT appear.
    assert!(!message.contains("Forge-Evidence-Digest"));
    assert!(!message.contains("Forge-Publication-Digest"));
}

#[test]
fn native_accepted_proposal_exports_to_git_branch() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let show = json_output(repo.forge().args(["--json", "show"]).assert().success());
    assert!(show["data"]["latest_proposal"]["content_ref"]
        .as_str()
        .unwrap()
        .starts_with("forge-tree:"));
    let current_branch = git(repo.path(), &["branch", "--show-current"]);

    repo.forge().args(["--json", "accept"]).assert().success();
    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/native-exported"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/native-exported");
    assert_eq!(
        git(repo.path(), &["branch", "--show-current"]),
        current_branch
    );
    assert_eq!(
        git(repo.path(), &["show", "forge/native-exported:README.md"]),
        "exported\n"
    );
}
