mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn run_records_successful_command_evidence() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "capture evidence"])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args(["--json", "run", "--", "sh", "-c", "printf ok"])
            .assert()
            .success(),
    );

    assert_eq!(output["data"]["command"], "sh");
    assert_eq!(output["data"]["exit_code"], 0);
    assert_eq!(output["data"]["stdout_excerpt"], "ok");
    assert_eq!(output["data"]["trust"], "locally_observed");
    assert!(output["data"].get("environment").is_none());
}

#[test]
fn run_allows_clean_saved_worktree() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "run clean guard"])
        .assert()
        .success();
    repo.forge().args(["--json", "save"]).assert().success();

    let output = json_output(
        repo.forge()
            .args(["--json", "run", "--", "sh", "-c", "printf ok"])
            .assert()
            .success(),
    );

    assert_eq!(output["data"]["exit_code"], 0);
    assert_eq!(output["data"]["stdout_excerpt"], "ok");
}

#[test]
fn run_refuses_dirty_worktree_before_first_save() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "run base guard"])
        .assert()
        .success();

    std::fs::write(repo.path().join("README.md"), "dirty before first save\n")
        .expect("dirty readme");
    let marker = repo.path().join("run-marker.txt");
    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "run",
                "--",
                "sh",
                "-c",
                "printf executed > run-marker.txt",
            ])
            .assert()
            .failure(),
    );

    assert_eq!(output["errors"][0]["code"], "DIRTY_WORKTREE");
    assert!(
        !marker.exists(),
        "run command must not execute when worktree differs from attempt base"
    );
}

#[test]
fn run_refuses_dirty_worktree_before_executing_command() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "run dirty guard"])
        .assert()
        .success();
    repo.forge().args(["--json", "save"]).assert().success();

    std::fs::write(repo.path().join("README.md"), "dirty before run\n").expect("dirty readme");
    let marker = repo.path().join("run-marker.txt");
    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "run",
                "--",
                "sh",
                "-c",
                "printf executed > run-marker.txt",
            ])
            .assert()
            .failure(),
    );

    assert_eq!(output["errors"][0]["code"], "DIRTY_WORKTREE");
    assert!(
        !marker.exists(),
        "run command must not execute when worktree is dirty"
    );
}

#[test]
fn run_records_failed_command_and_truncates_output() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "capture failure"])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "run",
                "--",
                "sh",
                "-c",
                "python3 - <<'PY'\nprint('x' * 5000)\nraise SystemExit(7)\nPY",
            ])
            .assert()
            .success(),
    );

    assert_eq!(output["data"]["exit_code"], 7);
    assert_eq!(output["data"]["stdout_truncated"], true);
    assert!(output["data"]["stdout_excerpt"].as_str().unwrap().len() <= 4096);
}

#[test]
fn run_redacts_secret_like_output_and_marks_sensitivity() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "capture redacted evidence"])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "run",
                "--",
                "sh",
                "-c",
                "printf 'TOKEN=super-secret\\nplain=visible\\n'",
            ])
            .assert()
            .success(),
    );

    let stdout = output["data"]["stdout_excerpt"].as_str().unwrap();
    assert!(stdout.contains("TOKEN=[REDACTED]"));
    assert!(stdout.contains("plain=visible"));
    assert!(!stdout.contains("super-secret"));
    assert_eq!(output["data"]["sensitivity"], "secret_risk");
}

#[test]
fn run_times_out_and_persists_timed_out_evidence() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "timeout evidence"])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "run",
                "--timeout-ms",
                "50",
                "--",
                "sh",
                "-c",
                "sleep 1",
            ])
            .assert()
            .success(),
    );

    assert_eq!(output["data"]["timed_out"], true);
    assert_eq!(output["data"]["exit_code"], -1);
}

#[test]
fn run_caps_noisy_output_without_buffering_full_log() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "noisy evidence"])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "run",
                "--",
                "sh",
                "-c",
                "python3 - <<'PY'\nprint('x' * 20000)\nPY",
            ])
            .assert()
            .success(),
    );

    assert_eq!(output["data"]["stdout_truncated"], true);
    assert!(output["data"]["stdout_excerpt"].as_str().unwrap().len() <= 4096);
}
