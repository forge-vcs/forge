mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn pr_body_summarizes_intent_paths_evidence_check_and_decision() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "write the release note"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "release note\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();

    let output = json_output(
        repo.forge()
            .args(["--json", "export", "pr-body"])
            .assert()
            .success(),
    );
    let body = output["data"]["body"].as_str().unwrap();
    assert!(body.contains("Intent: write the release note"));
    assert!(body.contains("README.md"));
    assert!(body.contains("Evidence"));
    assert!(body.contains("passed"));
    assert!(body.contains("accepted"));
}
