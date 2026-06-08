mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn doctor_storage_accounting_reports_zero_for_missing_categories() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    let storage = &report["data"]["storage"];
    assert!(storage["total_bytes"].as_u64().unwrap() > 0);
    assert!(storage["database"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(storage["packs"]["bytes"], 0);
    assert_eq!(storage["packs"]["files"], 0);
    assert_eq!(storage["evidence_outputs"]["bytes"], 0);
    assert_eq!(storage["evidence_outputs"]["files"], 0);
}

#[test]
fn doctor_storage_accounting_reports_loose_native_objects_after_save() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "storage accounting"])
        .assert()
        .success();
    std::fs::write(repo.path().join("feature.txt"), "native storage\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    let storage = &report["data"]["storage"];
    assert_eq!(report["data"]["ok"], true, "doctor report: {report}");
    assert!(
        storage["loose_objects"]["bytes"].as_u64().unwrap() > 0,
        "loose object bytes should be reported: {storage}"
    );
    assert!(
        storage["loose_objects"]["files"].as_u64().unwrap() > 0,
        "loose object files should be reported: {storage}"
    );
    assert!(
        storage["total_bytes"].as_u64().unwrap()
            >= storage["loose_objects"]["bytes"].as_u64().unwrap(),
        "total must include loose objects: {storage}"
    );
}

#[test]
fn gc_dry_run_includes_storage_accounting_without_deleting() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "gc storage accounting"])
        .assert()
        .success();
    std::fs::write(repo.path().join("feature.txt"), "gc storage\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();

    let report = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    let storage = &report["data"]["storage"];
    assert_eq!(report["data"]["dry_run"], true);
    assert!(
        storage["total_bytes"].as_u64().unwrap() > 0,
        "gc dry-run should include storage accounting: {report}"
    );
    assert!(
        storage["loose_objects"]["files"].as_u64().unwrap() > 0,
        "gc dry-run should include loose object file count: {report}"
    );
    assert!(report["data"]["deleted"].as_array().unwrap().is_empty());
}
