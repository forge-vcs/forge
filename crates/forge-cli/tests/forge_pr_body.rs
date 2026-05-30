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

#[test]
fn pr_body_cites_the_competing_attempts() {
    // NER-137 R9: the PR body cites the competing attempts against the declared intent
    // (not just one latest evidence row).
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete on the note"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap().to_string();
    let attempt_a = first["data"]["attempt_id"].as_str().unwrap().to_string();
    std::fs::write(repo.path().join("README.md"), "alpha\n").expect("write a");
    repo.forge()
        .args(["--json", "save", "--attempt", &attempt_a])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "run",
            "--attempt",
            &attempt_a,
            "--",
            "sh",
            "-c",
            "true",
        ])
        .assert()
        .success();
    let prop_a = json_output(
        repo.forge()
            .args(["--json", "propose", "--attempt", &attempt_a])
            .assert()
            .success(),
    );

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", &intent_id])
            .assert()
            .success(),
    );
    let attempt_b = second["data"]["attempt_id"].as_str().unwrap().to_string();
    repo.forge()
        .args(["--json", "attempt", "attach", &attempt_b])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "beta\n").expect("write b");
    repo.forge()
        .args(["--json", "save", "--attempt", &attempt_b])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "run",
            "--attempt",
            &attempt_b,
            "--",
            "sh",
            "-c",
            "true",
        ])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "propose", "--attempt", &attempt_b])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "export",
                "pr-body",
                "--attempt",
                &attempt_a,
                "--proposal",
                prop_a["data"]["proposal_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    let body = output["data"]["body"].as_str().unwrap();
    assert!(body.contains("Competing Attempts"), "{body}");
    assert!(body.contains(&attempt_a), "cites this attempt");
    assert!(body.contains(&attempt_b), "cites the rival attempt");
    assert!(body.contains("← this proposal"));
}
