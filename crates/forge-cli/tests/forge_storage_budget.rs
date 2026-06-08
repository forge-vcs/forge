mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn bytes(storage: &Value, category: &str) -> u64 {
    storage[category]["bytes"].as_u64().unwrap()
}

fn files(storage: &Value, category: &str) -> u64 {
    storage[category]["files"].as_u64().unwrap()
}

fn assert_storage_reconciles(storage: &Value) {
    let category_sum: u64 = [
        "loose_objects",
        "packs",
        "database",
        "temp",
        "worktrees",
        "evidence_outputs",
        "other",
    ]
    .iter()
    .map(|category| bytes(storage, category))
    .sum();
    assert_eq!(
        storage["total_bytes"].as_u64().unwrap(),
        category_sum,
        "storage categories must reconcile to total: {storage}"
    );
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
    assert_storage_reconciles(storage);
}

#[test]
fn doctor_storage_accounting_partitions_controlled_category_files() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    let before = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    let before_storage = &before["data"]["storage"];

    write_forge_file(repo.path(), ".forge/objects/accounting/loose.bin", 11);
    write_forge_file(repo.path(), ".forge/packs/accounting.fpack", 13);
    write_forge_file(repo.path(), ".forge/tmp/accounting.tmp", 17);
    write_forge_file(repo.path(), ".forge/worktrees/accounting/file.txt", 19);
    write_forge_file(repo.path(), ".forge/evidence/accounting.out", 23);
    write_forge_file(repo.path(), ".forge/accounting-other.bin", 29);

    let after = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    let storage = &after["data"]["storage"];

    assert_eq!(
        bytes(storage, "loose_objects") - bytes(before_storage, "loose_objects"),
        11
    );
    assert_eq!(
        files(storage, "loose_objects") - files(before_storage, "loose_objects"),
        1
    );
    assert_eq!(bytes(storage, "packs") - bytes(before_storage, "packs"), 13);
    assert_eq!(files(storage, "packs") - files(before_storage, "packs"), 1);
    assert_eq!(bytes(storage, "temp") - bytes(before_storage, "temp"), 17);
    assert_eq!(files(storage, "temp") - files(before_storage, "temp"), 1);
    assert_eq!(
        bytes(storage, "worktrees") - bytes(before_storage, "worktrees"),
        19
    );
    assert_eq!(
        files(storage, "worktrees") - files(before_storage, "worktrees"),
        1
    );
    assert_eq!(
        bytes(storage, "evidence_outputs") - bytes(before_storage, "evidence_outputs"),
        23
    );
    assert_eq!(
        files(storage, "evidence_outputs") - files(before_storage, "evidence_outputs"),
        1
    );
    assert_eq!(bytes(storage, "other") - bytes(before_storage, "other"), 29);
    assert_eq!(files(storage, "other") - files(before_storage, "other"), 1);
    assert_storage_reconciles(storage);
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
    assert_storage_reconciles(storage);
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
    assert_storage_reconciles(storage);
}

#[test]
fn mutating_command_below_budget_emits_no_storage_warning() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json_output(
        repo.forge()
            .args(["--json", "start", "below budget"])
            .assert()
            .success(),
    );
    assert!(
        !has_storage_budget_warning(&started),
        "unexpected budget warning: {started}"
    );
}

#[test]
fn mutating_command_above_budget_warns_but_succeeds_without_eviction() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    write_forge_file(repo.path(), ".forge/packs/pressure.fpack", 32);
    set_storage_policy(repo.path(), 14, 1);

    let started = json_output(
        repo.forge()
            .args(["--json", "start", "above budget"])
            .assert()
            .success(),
    );
    assert!(
        has_storage_budget_warning(&started),
        "expected budget warning: {started}"
    );
    assert!(
        repo.path().join(".forge/packs/pressure.fpack").exists(),
        "budget pressure must not trigger automatic eviction"
    );
}

#[test]
fn doctor_reports_storage_pressure_without_marking_repo_unhealthy() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    set_storage_policy(repo.path(), 14, 1);

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], true, "doctor report: {report}");
    assert_eq!(report["data"]["storage_budget"]["limit_bytes"], 1);
    assert_eq!(report["data"]["storage_budget"]["over_budget"], true);
    assert!(
        report["data"]["storage_budget"]["over_by_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        report["data"]["storage_policy"]["automatic_eviction"],
        false
    );
}

#[test]
fn gc_dry_run_reports_storage_budget_and_retention_policy() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    set_storage_policy(repo.path(), 14, 1);

    let report = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    assert_eq!(report["data"]["dry_run"], true);
    assert_eq!(report["data"]["protection_window_days"], 14);
    assert_eq!(
        report["data"]["storage_policy"]["protection_window_days"],
        14
    );
    assert_eq!(report["data"]["storage_budget"]["limit_bytes"], 1);
    assert_eq!(report["data"]["storage_budget"]["over_budget"], true);
}

fn write_forge_file(repo: &std::path::Path, relative: &str, len: usize) {
    let path = repo.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).expect("create category dir");
    std::fs::write(path, vec![b'x'; len]).expect("write category file");
}

fn set_storage_policy(repo: &std::path::Path, protection_window_days: u64, bytes: u64) {
    let connection = Connection::open(repo.join(".forge/forge.db")).expect("open forge db");
    connection
        .execute(
            "UPDATE storage_policy
             SET protection_window_days = ?1, storage_budget_bytes = ?2
             WHERE singleton = 1",
            [protection_window_days as i64, bytes as i64],
        )
        .expect("set storage policy");
}

fn has_storage_budget_warning(envelope: &Value) -> bool {
    envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| {
            warning
                .as_str()
                .is_some_and(|warning| warning.contains("storage budget exceeded"))
        })
}
