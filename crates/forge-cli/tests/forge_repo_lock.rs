mod common;

use common::TestRepo;
use serde_json::Value;
use std::fs::OpenOptions;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

/// Open and exclusively lock the repo lock file from the *test* process, standing
/// in for a peer `forge` command holding the repo-level advisory write lock. The
/// returned handle holds the lock until it is unlocked or dropped.
fn hold_repo_lock(repo: &TestRepo) -> std::fs::File {
    let lock_path = repo.path().join(".forge/forge.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("open repo lock file");
    file.try_lock().expect("test acquires the repo lock");
    file
}

#[test]
fn mutating_command_times_out_with_lock_timeout_when_lock_held() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "repo lock test"])
        .assert()
        .success();

    let held = hold_repo_lock(&repo);

    // A mutating command contends for the held lock and must surface the typed,
    // retryable LOCK_TIMEOUT — not hang, not corrupt state. A short (clamped)
    // FORGE_LOCK_TIMEOUT_MS keeps the test fast.
    let out = json_output(
        repo.forge()
            .env("FORGE_LOCK_TIMEOUT_MS", "80")
            .args(["--json", "save"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "LOCK_TIMEOUT");
    // LOCK_TIMEOUT is transient: the top-level envelope `retry.retryable` is true
    // (NER-133 R6 / deferred finding #2), and the waited_ms surfaces in the error
    // object's details so a client knows how long it waited. `retry` is on the
    // envelope; `details` is on the error object — assert both placements.
    assert_eq!(out["retry"]["retryable"], true);
    assert!(out["retry"]["after_ms"].is_number());
    assert!(
        out["errors"][0]["details"]["waited_ms"].is_number(),
        "waited_ms must surface in the error details: {out}"
    );

    // After release, the same command acquires the lock and succeeds.
    held.unlock().expect("release the repo lock");
    repo.forge().args(["--json", "save"]).assert().success();
}

#[test]
fn run_and_read_commands_do_not_block_on_a_held_write_lock() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "lock-free paths"])
        .assert()
        .success();

    let _held = hold_repo_lock(&repo);

    // `run` is the PRD §10.6 carve-out (it must not hold the lock while its child
    // executes), and read-only `show`/`doctor` never take the write lock — so all
    // three succeed even while a peer holds it. A low FORGE_LOCK_TIMEOUT_MS would
    // make any accidental lock acquisition fail fast and surface as a failure here.
    repo.forge()
        .env("FORGE_LOCK_TIMEOUT_MS", "80")
        .args(["--json", "run", "--", "true"])
        .assert()
        .success();
    repo.forge()
        .env("FORGE_LOCK_TIMEOUT_MS", "80")
        .args(["--json", "show"])
        .assert()
        .success();
    repo.forge()
        .env("FORGE_LOCK_TIMEOUT_MS", "80")
        .args(["--json", "doctor"])
        .assert()
        .success();
}
