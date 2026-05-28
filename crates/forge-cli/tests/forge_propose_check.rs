mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn propose_show_and_check_pass_with_successful_evidence() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "ship proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "proposal\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();

    let proposed = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    assert!(proposed["data"]["proposal_id"]
        .as_str()
        .unwrap()
        .starts_with("proposal_"));

    let shown = json_output(repo.forge().args(["--json", "show"]).assert().success());
    assert_eq!(
        shown["data"]["latest_proposal"]["proposal_id"],
        proposed["data"]["proposal_id"]
    );

    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "passed");
}

#[test]
fn propose_requires_snapshot_and_check_reports_missing_evidence() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "no evidence yet"])
        .assert()
        .success();

    let no_snapshot = json_output(repo.forge().args(["--json", "propose"]).assert().failure());
    assert_eq!(no_snapshot["errors"][0]["code"], "NO_SNAPSHOT");
    assert!(no_snapshot["operation_id"]
        .as_str()
        .unwrap()
        .starts_with("op_"));

    std::fs::write(repo.path().join("README.md"), "proposal\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();
    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "missing");
}

#[test]
fn check_marks_evidence_stale_when_snapshot_changes_after_run() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "stale evidence"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "first\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "second\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();

    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "stale");
    assert!(checked["data"]["reason"]
        .as_str()
        .unwrap()
        .contains("does not match proposal revision"));
}
