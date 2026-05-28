mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn doctor_passes_healthy_repo_and_reports_dangling_temp_files() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let healthy = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(healthy["data"]["ok"], true);

    let tmp = repo.path().join(".forge/tmp");
    std::fs::create_dir_all(&tmp).expect("create tmp");
    std::fs::write(tmp.join("interrupted"), "partial").expect("write temp marker");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue == "dangling temporary files"));
}

#[test]
fn gc_dry_run_reports_without_deleting() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let report = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    assert_eq!(report["data"]["dry_run"], true);
    assert!(report["data"]["deleted"].as_array().unwrap().is_empty());
}

#[test]
fn doctor_reports_mismatched_current_view() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open forge db");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("disable foreign keys for corruption fixture");
    connection
        .execute(
            "UPDATE operations SET resulting_view_id = 'view_missing'",
            [],
        )
        .expect("corrupt current operation");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue == "invalid current operation/view"));
}

#[test]
fn doctor_reports_foreign_key_violations() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open forge db");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("disable foreign keys for corruption fixture");
    connection
        .execute(
            "INSERT INTO attempts (id, repo_id, intent_id, base_head, status, created_at_ms)
             VALUES ('attempt_dangling', 'repo_missing', 'intent_missing', 'HEAD', 'active', 1)",
            [],
        )
        .expect("insert dangling attempt");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue.as_str().unwrap().contains("foreign key violation")));
}
