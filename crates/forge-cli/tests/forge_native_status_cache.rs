mod common;

use common::TestRepo;
use serde_json::Value;
use std::fs;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn init_native(repo: &TestRepo) {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "status cache"])
        .assert()
        .success();
}

fn save_with_file(repo: &TestRepo, path: &str, body: &str) -> String {
    fs::write(repo.path().join(path), body).expect("write file");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());
    saved["data"]["content_ref"].as_str().unwrap().to_string()
}

fn diff_working(repo: &TestRepo, content_ref: &str) -> Value {
    json_output(
        repo.forge()
            .args(["--json", "diff", "--working", "--to", content_ref])
            .assert()
            .success(),
    )
}

#[test]
fn working_diff_creates_status_cache_and_reports_one_file_edit() {
    let repo = TestRepo::new_git();
    init_native(&repo);
    let content_ref = save_with_file(&repo, "tracked.txt", "one\n");

    let clean = diff_working(&repo, &content_ref);
    assert!(
        clean["data"]["files"].as_array().unwrap().is_empty(),
        "clean diff should be empty: {clean}"
    );
    assert!(
        repo.path().join(".forge/status-cache.json").exists(),
        "working diff should persist the derived status cache"
    );

    fs::write(repo.path().join("tracked.txt"), "one\ntwo\n").expect("edit file");
    let diff = diff_working(&repo, &content_ref);
    let files = diff["data"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "one edited path expected: {diff}");
    assert_eq!(files[0]["path"], "tracked.txt");
    assert_eq!(files[0]["status"], "M");
    assert_eq!(files[0]["insertions"], 1);
}

#[test]
fn corrupt_status_cache_rebuilds_without_failing_user_command() {
    let repo = TestRepo::new_git();
    init_native(&repo);
    let content_ref = save_with_file(&repo, "tracked.txt", "one\n");
    diff_working(&repo, &content_ref);

    fs::write(repo.path().join(".forge/status-cache.json"), b"{not json").expect("corrupt cache");
    fs::write(repo.path().join("tracked.txt"), "one\ntwo\n").expect("edit file");
    let diff = diff_working(&repo, &content_ref);

    assert_eq!(
        diff["status"], "success",
        "corrupt cache must rebuild: {diff}"
    );
    assert_eq!(diff["data"]["files"][0]["path"], "tracked.txt");
}

#[test]
fn status_cache_excludes_secret_risk_paths() {
    let repo = TestRepo::new_git();
    init_native(&repo);
    let content_ref = save_with_file(&repo, "visible.txt", "visible\n");
    fs::write(repo.path().join(".env"), "TOKEN=secret\n").expect("write secret");

    let diff = diff_working(&repo, &content_ref);
    assert!(
        diff["data"]["files"].as_array().unwrap().is_empty(),
        "secret-risk path must be policy-excluded from diff: {diff}"
    );
    let cache = fs::read_to_string(repo.path().join(".forge/status-cache.json"))
        .expect("read status cache");
    assert!(!cache.contains(".env"), "secret path leaked into cache");
    assert!(
        cache.contains("visible.txt"),
        "visible path should be cached"
    );
}
