mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn start_save_and_restore_snapshot() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let started = json_output(
        repo.forge()
            .args(["--json", "start", "make readme useful"])
            .assert()
            .success(),
    );
    assert!(started["data"]["attempt_id"]
        .as_str()
        .unwrap()
        .starts_with("attempt_"));

    std::fs::write(repo.path().join("README.md"), "changed once\n").expect("write readme");
    let first = json_output(repo.forge().args(["--json", "save"]).assert().success());
    assert_eq!(first["data"]["changed_paths"][0], "README.md");
    assert!(first["data"]["parent_snapshot_id"].is_null());

    std::fs::write(repo.path().join("README.md"), "changed twice\n").expect("write readme");
    std::fs::write(repo.path().join("later.txt"), "created later\n").expect("write later file");
    let second = json_output(repo.forge().args(["--json", "save"]).assert().success());
    assert_eq!(
        second["data"]["parent_snapshot_id"],
        first["data"]["snapshot_id"]
    );

    let first_snapshot = first["data"]["snapshot_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "restore", first_snapshot, "--yes"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "changed once\n"
    );
    assert!(!repo.path().join("later.txt").exists());
}

#[test]
fn save_requires_active_attempt() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let output = json_output(repo.forge().args(["--json", "save"]).assert().failure());
    assert_eq!(output["errors"][0]["code"], "NO_ACTIVE_ATTEMPT");
}

#[test]
fn restore_refuses_unsaved_dirty_worktree() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "restore safely"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "saved\n").expect("write readme");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let snapshot_id = saved["data"]["snapshot_id"].as_str().unwrap();

    std::fs::write(repo.path().join("README.md"), "unsaved\n").expect("write unsaved readme");
    let output = json_output(
        repo.forge()
            .args(["--json", "restore", snapshot_id, "--yes"])
            .assert()
            .failure(),
    );

    assert_eq!(output["errors"][0]["code"], "DIRTY_WORKTREE");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "unsaved\n"
    );
}

#[test]
fn duplicate_request_id_replays_without_second_mutation() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let first = json_output(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "req-start-once",
                "start",
                "idempotent",
            ])
            .assert()
            .success(),
    );
    let second = json_output(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "req-start-once",
                "start",
                "idempotent",
            ])
            .assert()
            .success(),
    );

    assert_eq!(second["operation_id"], first["operation_id"]);
    assert_eq!(second["data"]["idempotent_replay"], true);
}

#[test]
fn failed_request_id_replays_original_failure() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let first = json_output(
        repo.forge()
            .args(["--json", "--request-id", "req-failed-save", "save"])
            .assert()
            .failure(),
    );
    let second = json_output(
        repo.forge()
            .args(["--json", "--request-id", "req-failed-save", "save"])
            .assert()
            .failure(),
    );

    assert_eq!(second["operation_id"], first["operation_id"]);
    assert_eq!(second["errors"][0]["code"], "NO_ACTIVE_ATTEMPT");

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let failed_ops: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE request_id = 'req-failed-save'",
            [],
            |row| row.get(0),
        )
        .expect("count failed operations");
    assert_eq!(failed_ops, 1);
}

#[test]
fn request_id_reuse_for_different_command_conflicts() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    repo.forge()
        .args([
            "--json",
            "--request-id",
            "req-command-scope",
            "start",
            "scoped",
        ])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args(["--json", "--request-id", "req-command-scope", "save"])
            .assert()
            .failure(),
    );

    assert_eq!(output["errors"][0]["code"], "REQUEST_ID_CONFLICT");
}

#[test]
fn save_excludes_secret_risk_paths_from_snapshot_and_changed_paths() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "avoid secrets"])
        .assert()
        .success();

    std::fs::write(repo.path().join("README.md"), "safe change\n").expect("write readme");
    std::fs::write(repo.path().join(".env"), "TOKEN=raw-secret\n").expect("write env");
    let output = json_output(repo.forge().args(["--json", "save"]).assert().success());

    let changed_paths = output["data"]["changed_paths"].as_array().unwrap();
    assert!(changed_paths.iter().any(|path| path == "README.md"));
    assert!(!changed_paths.iter().any(|path| path == ".env"));

    let content_ref = output["data"]["content_ref"].as_str().unwrap();
    let tree = content_ref.strip_prefix("git-tree:").unwrap();
    let tree_paths = git(repo.path(), &["ls-tree", "-r", "--name-only", tree]);
    assert!(tree_paths.contains("README.md"));
    assert!(!tree_paths.lines().any(|path| path == ".env"));
}

#[test]
fn save_excludes_tracked_secret_risk_paths_from_snapshot() {
    let repo = TestRepo::new_git();
    std::fs::write(repo.path().join(".env"), "TOKEN=raw-secret\n").expect("write env");
    git(repo.path(), &["add", ".env"]);
    git(repo.path(), &["commit", "-m", "add tracked env"]);

    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "avoid tracked secrets"])
        .assert()
        .success();
    std::fs::write(repo.path().join(".env"), "TOKEN=changed-secret\n").expect("write env");

    let output = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let content_ref = output["data"]["content_ref"].as_str().unwrap();
    let tree = content_ref.strip_prefix("git-tree:").unwrap();
    let tree_paths = git(repo.path(), &["ls-tree", "-r", "--name-only", tree]);
    assert!(!tree_paths.lines().any(|path| path == ".env"));
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
