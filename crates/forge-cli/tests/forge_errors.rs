//! Typed-error parity + payload coverage (NER-133 U1/U2).
//!
//! Every error code the deleted substring-matched `error_code()` produced must
//! still be produced by the typed `ForgeError` taxonomy, INCLUDING the codes
//! that were raised at the CLI layer (`DIRTY_WORKTREE`, accept-path `STALE_BASE`,
//! `NOT_ACCEPTED`, `REJECTED`, `BRANCH_EXISTS`). These tests pin the codes and the
//! newly-populated `errors[].details` / top-level `retry` envelope fields.

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

/// Drive the lifecycle through `check`, leaving a checked proposal ready for an
/// accept/reject/export decision.
fn prepare_proposal(repo: &TestRepo) {
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "errors test"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "changed\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
}

#[test]
fn not_initialized_code_and_no_retry() {
    let repo = TestRepo::new_git();
    let out = json_output(repo.forge().args(["--json", "show"]).assert().failure());
    assert_eq!(out["errors"][0]["code"], "NOT_INITIALIZED");
    assert_eq!(out["retry"]["retryable"], false);
}

#[test]
fn no_active_attempt_code() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    // `save` requires an active attempt; none has been started.
    let out = json_output(repo.forge().args(["--json", "save"]).assert().failure());
    assert_eq!(out["errors"][0]["code"], "NO_ACTIVE_ATTEMPT");
}

#[test]
fn unknown_attempt_code_carries_selector_detail() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let out = json_output(
        repo.forge()
            .args(["--json", "attempt", "show", "att_does_not_exist"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "UNKNOWN_ATTEMPT");
    assert_eq!(
        out["errors"][0]["details"]["selector"],
        "att_does_not_exist"
    );
}

#[test]
fn unknown_intent_code() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let out = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", "int_missing"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "UNKNOWN_INTENT");
    assert_eq!(out["errors"][0]["details"]["selector"], "int_missing");
}

#[test]
fn no_snapshot_code() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "no snapshot"])
        .assert()
        .success();
    // propose with no save made yet -> no snapshot for the active attempt.
    let out = json_output(repo.forge().args(["--json", "propose"]).assert().failure());
    assert_eq!(out["errors"][0]["code"], "NO_SNAPSHOT");
}

#[test]
fn restore_on_dirty_worktree_is_dirty_worktree_code() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "dirty"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "first\n").expect("write");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let snapshot_id = saved["data"]["snapshot_id"].as_str().unwrap().to_string();

    // Dirty the worktree relative to the latest snapshot, then attempt a restore.
    std::fs::write(repo.path().join("README.md"), "uncommitted edit\n").expect("dirty");
    let out = json_output(
        repo.forge()
            .args(["--json", "restore", &snapshot_id, "--yes"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "DIRTY_WORKTREE");
    assert!(out["errors"][0]["details"]["paths"].is_array());
    assert_eq!(out["retry"]["retryable"], false);
}

#[test]
fn accept_path_stale_base_carries_head_details() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);

    // Move HEAD after check but before accept -> the accept-path stale-base bail.
    std::fs::write(repo.path().join("moved.txt"), "move head\n").expect("write");
    git(repo.path(), &["add", "moved.txt"]);
    git(repo.path(), &["commit", "-m", "move head"]);

    let out = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(out["errors"][0]["code"], "STALE_BASE");
    assert!(out["errors"][0]["details"]["expected_head"].is_string());
    assert!(out["errors"][0]["details"]["actual_head"].is_string());
    assert_eq!(out["retry"]["retryable"], false);
}

#[test]
fn rejected_and_not_accepted_codes() {
    // REJECTED: reject then export.
    let rejected_repo = TestRepo::new_git();
    prepare_proposal(&rejected_repo);
    rejected_repo
        .forge()
        .args(["--json", "reject"])
        .assert()
        .success();
    let rejected = json_output(
        rejected_repo
            .forge()
            .args(["--json", "export", "branch", "forge/rejected"])
            .assert()
            .failure(),
    );
    assert_eq!(rejected["errors"][0]["code"], "REJECTED");

    // NOT_ACCEPTED: export a checked-but-undecided proposal.
    let undecided_repo = TestRepo::new_git();
    prepare_proposal(&undecided_repo);
    let not_accepted = json_output(
        undecided_repo
            .forge()
            .args(["--json", "export", "branch", "forge/undecided"])
            .assert()
            .failure(),
    );
    assert_eq!(not_accepted["errors"][0]["code"], "NOT_ACCEPTED");
}

#[test]
fn branch_exists_code_carries_name_detail() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/dup"])
        .assert()
        .success();

    // Re-export onto the same branch name -> BRANCH_EXISTS with the name in details.
    std::fs::write(repo.path().join("README.md"), "diverge\n").expect("write");
    let out = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/dup"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "BRANCH_EXISTS");
    assert_eq!(out["errors"][0]["details"]["name"], "forge/dup");
}

#[test]
fn deterministic_failure_under_request_id_replays_with_same_code() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);

    // Move HEAD so accept fails deterministically with STALE_BASE.
    std::fs::write(repo.path().join("moved.txt"), "move head\n").expect("write");
    git(repo.path(), &["add", "moved.txt"]);
    git(repo.path(), &["commit", "-m", "move head"]);

    let first = json_output(
        repo.forge()
            .args(["--json", "--request-id", "rq-stale", "accept"])
            .assert()
            .failure(),
    );
    assert_eq!(first["errors"][0]["code"], "STALE_BASE");
    let first_expected = first["errors"][0]["details"]["expected_head"].clone();
    let first_actual = first["errors"][0]["details"]["actual_head"].clone();
    assert!(first_expected.is_string(), "first response has details");
    assert!(first_actual.is_string(), "first response has details");
    assert_eq!(
        first["errors"][0]["details"]["reason"],
        "current repository head has advanced since this proposal was based"
    );
    assert!(first["errors"][0]["details"]["recovery_hint"]
        .as_str()
        .expect("recovery hint")
        .contains("Start a fresh intent or attempt"));
    assert!(first["errors"][0]["details"]["recovery_steps"]
        .as_array()
        .expect("recovery steps")
        .iter()
        .any(|step| step == "forge start \"reapply the change on the current base\""));
    assert!(first["errors"][0]["message"]
        .as_str()
        .expect("message")
        .contains("Start a fresh intent or attempt from the current head"));

    // Replaying the same request id reproduces the SAME code AND the SAME details,
    // read from the stored error_json (not re-derived from the message). The
    // deterministic failure was persisted, so the replay yields the recorded
    // operation id. FIX C: a replayed failure must carry identical details, not an
    // empty object.
    let replay = json_output(
        repo.forge()
            .args(["--json", "--request-id", "rq-stale", "accept"])
            .assert()
            .failure(),
    );
    assert_eq!(replay["errors"][0]["code"], "STALE_BASE");
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(
        replay["errors"][0]["details"]["expected_head"], first_expected,
        "replayed STALE_BASE must carry the same expected_head detail"
    );
    assert_eq!(
        replay["errors"][0]["details"]["actual_head"], first_actual,
        "replayed STALE_BASE must carry the same actual_head detail"
    );
    assert_eq!(
        replay["errors"][0]["details"]["recovery_hint"],
        first["errors"][0]["details"]["recovery_hint"],
        "replayed STALE_BASE must carry the same recovery guidance"
    );
}
