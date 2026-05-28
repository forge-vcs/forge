mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn clear_attached_attempt(repo: &TestRepo) {
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    connection
        .execute("UPDATE current_state SET attached_attempt_id = NULL", [])
        .expect("clear attachment");
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

#[test]
fn start_attaches_created_attempt_and_migrates_existing_database() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    {
        let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
        connection
            .execute("DELETE FROM schema_migrations WHERE version = 2", [])
            .expect("remove migration marker");
    }

    let started = json_output(
        repo.forge()
            .args(["--json", "start", "first attempt"])
            .assert()
            .success(),
    );
    let attempts = json_output(
        repo.forge()
            .args(["--json", "attempt", "list"])
            .assert()
            .success(),
    );

    assert_eq!(started["data"]["attached"], true);
    assert_eq!(attempts["data"]["attempts"][0]["attached"], true);
    assert_eq!(
        attempts["data"]["attempts"][0]["attempt_id"],
        started["data"]["attempt_id"]
    );
}

#[test]
fn attempt_start_lists_and_shows_competing_attempts() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );

    assert_eq!(second["data"]["intent_id"], intent_id);
    let listed = json_output(
        repo.forge()
            .args(["--json", "attempt", "list"])
            .assert()
            .success(),
    );
    assert_eq!(listed["data"]["attempts"].as_array().unwrap().len(), 2);

    let shown = json_output(
        repo.forge()
            .args([
                "--json",
                "attempt",
                "show",
                second["data"]["attempt_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    assert_eq!(shown["data"]["attempt"]["intent_id"], intent_id);
    assert!(shown["data"]["proposals"].as_array().unwrap().is_empty());
}

#[test]
fn ambiguous_attempt_requires_explicit_selector() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "attempt", "start", "--intent", intent_id])
        .assert()
        .success();
    clear_attached_attempt(&repo);

    std::fs::write(repo.path().join("README.md"), "ambiguous\n").expect("write readme");
    let output = json_output(repo.forge().args(["--json", "save"]).assert().failure());
    assert_eq!(output["errors"][0]["code"], "AMBIGUOUS_ATTEMPT");

    let saved = json_output(
        repo.forge()
            .args([
                "--json",
                "save",
                "--attempt",
                first["data"]["attempt_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    assert_eq!(saved["data"]["attempt_id"], first["data"]["attempt_id"]);

    let shown = json_output(repo.forge().args(["--json", "show"]).assert().failure());
    assert_eq!(shown["errors"][0]["code"], "AMBIGUOUS_ATTEMPT");
}

#[test]
fn unknown_attempt_selector_is_typed() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "known"])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args(["--json", "save", "--attempt", "attempt_missing"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "UNKNOWN_ATTEMPT");
}

#[test]
fn attach_materializes_snapshot_and_refuses_dirty_work() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let first_attempt = first["data"]["attempt_id"].as_str().unwrap();
    std::fs::write(repo.path().join("README.md"), "attempt one\n").expect("write first");
    repo.forge().args(["--json", "save"]).assert().success();

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_attempt = second["data"]["attempt_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "attempt", "attach", second_attempt])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "hello\n"
    );

    std::fs::write(repo.path().join("README.md"), "unsaved\n").expect("write dirty");
    let dirty = json_output(
        repo.forge()
            .args(["--json", "attempt", "attach", first_attempt])
            .assert()
            .failure(),
    );
    assert_eq!(dirty["errors"][0]["code"], "DIRTY_WORKTREE");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "unsaved\n"
    );
}

#[test]
fn attach_base_revision_preserves_tracked_secret_risk_paths() {
    let repo = TestRepo::new_git();
    std::fs::write(repo.path().join(".env"), "TOKEN=committed\n").expect("write env");
    git(repo.path(), &["add", ".env"]);
    git(repo.path(), &["commit", "-m", "track env"]);

    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "secrets"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    std::fs::write(repo.path().join("README.md"), "saved\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    std::fs::write(repo.path().join(".env"), "TOKEN=local\n").expect("write local env");

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    repo.forge()
        .args([
            "--json",
            "attempt",
            "attach",
            second["data"]["attempt_id"].as_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(repo.path().join(".env")).unwrap(),
        "TOKEN=local\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "hello\n"
    );
}

#[test]
fn ambiguous_proposal_requires_explicit_selector() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "choose proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "proposal one\n").expect("write one");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();
    std::fs::write(repo.path().join("README.md"), "proposal two\n").expect("write two");
    repo.forge().args(["--json", "save"]).assert().success();
    let second = json_output(repo.forge().args(["--json", "propose"]).assert().success());

    let ambiguous = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(ambiguous["errors"][0]["code"], "AMBIGUOUS_PROPOSAL");

    let accepted = json_output(
        repo.forge()
            .args([
                "--json",
                "accept",
                "--proposal",
                second["data"]["proposal_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        accepted["data"]["proposal_id"],
        second["data"]["proposal_id"]
    );
    assert_eq!(
        accepted["data"]["proposal_revision_id"],
        second["data"]["proposal_revision_id"]
    );
}

#[test]
fn competing_attempt_loop_exports_selected_proposal() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let first_attempt = first["data"]["attempt_id"].as_str().unwrap();

    std::fs::write(repo.path().join("README.md"), "attempt one\n").expect("write first");
    repo.forge()
        .args(["--json", "save", "--attempt", first_attempt])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "run",
            "--attempt",
            first_attempt,
            "--",
            "sh",
            "-c",
            "true",
        ])
        .assert()
        .success();
    let first_proposal = json_output(
        repo.forge()
            .args(["--json", "propose", "--attempt", first_attempt])
            .assert()
            .success(),
    );

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_attempt = second["data"]["attempt_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "attempt", "attach", second_attempt])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "attempt two\n").expect("write second");
    repo.forge()
        .args(["--json", "save", "--attempt", second_attempt])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "run",
            "--attempt",
            second_attempt,
            "--",
            "sh",
            "-c",
            "true",
        ])
        .assert()
        .success();
    let second_proposal = json_output(
        repo.forge()
            .args(["--json", "propose", "--attempt", second_attempt])
            .assert()
            .success(),
    );

    let proposals = json_output(
        repo.forge()
            .args(["--json", "proposal", "list", "--attempt", second_attempt])
            .assert()
            .success(),
    );
    assert_eq!(proposals["data"]["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(
        proposals["data"]["proposals"][0]["proposal_id"],
        second_proposal["data"]["proposal_id"]
    );

    repo.forge()
        .args([
            "--json",
            "check",
            "--attempt",
            second_attempt,
            "--proposal",
            second_proposal["data"]["proposal_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "accept",
            "--attempt",
            second_attempt,
            "--proposal",
            second_proposal["data"]["proposal_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "export",
            "branch",
            "--attempt",
            second_attempt,
            "--proposal",
            second_proposal["data"]["proposal_id"].as_str().unwrap(),
            "forge/selected-attempt",
        ])
        .assert()
        .success();

    assert_eq!(
        git(repo.path(), &["show", "forge/selected-attempt:README.md"]),
        "attempt two\n"
    );
    assert_ne!(
        first_proposal["data"]["proposal_id"],
        second_proposal["data"]["proposal_id"]
    );
}
