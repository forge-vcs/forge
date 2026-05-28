mod common;

use common::{forge_in, TestRepo};
use predicates::prelude::*;
use rusqlite::Connection;
use serde_json::Value;

#[test]
fn stubbed_command_returns_json_envelope_and_echoes_request_id() {
    let repo = TestRepo::new_git();

    let output = repo
        .forge()
        .args(["--json", "--request-id", "req-u1", "start", "tight scope"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["schema_version"], "forge.cli.v0");
    assert_eq!(json["command"], "start");
    assert_eq!(json["request_id"], "req-u1");
    assert_eq!(json["status"], "error");
    assert!(json["data"].is_object());
    assert!(json["warnings"].as_array().unwrap().is_empty());
    assert_eq!(json["errors"][0]["code"], "NOT_INITIALIZED");
    assert_eq!(json["retry"]["retryable"], false);
}

#[test]
fn confirmation_command_returns_structured_error_in_json_mode() {
    let repo = TestRepo::new_git();

    let output = repo
        .forge()
        .args(["--json", "restore", "snap_123"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["command"], "restore");
    assert_eq!(json["errors"][0]["code"], "CONFIRMATION_REQUIRED");
    assert_eq!(json["errors"][0]["details"]["snapshot_id"], "snap_123");
}

#[test]
fn restore_requires_yes_in_human_mode() {
    let repo = TestRepo::new_git();

    repo.forge()
        .args(["restore", "snap_123"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("restore requires --yes"));
}

#[test]
fn human_stub_output_is_concise() {
    let repo = TestRepo::new_git();

    repo.forge()
        .arg("save")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not initialized"))
        .stdout(predicate::str::is_empty());
}

#[test]
fn init_creates_sqlite_metadata_and_initial_operation_view() {
    let repo = TestRepo::new_git();

    let output = repo
        .forge()
        .args(["--json", "--request-id", "req-init", "init"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["schema_version"], "forge.cli.v0");
    assert_eq!(json["command"], "init");
    assert_eq!(json["request_id"], "req-init");
    assert_eq!(json["status"], "success");
    assert_eq!(json["data"]["already_initialized"], false);
    assert!(json["operation_id"].as_str().unwrap().starts_with("op_"));

    let db_path = repo.path().join(".forge/forge.db");
    assert!(db_path.exists());

    let connection = Connection::open(db_path).expect("open forge db");
    let counts: (i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM repositories),
                (SELECT COUNT(*) FROM operations),
                (SELECT COUNT(*) FROM views)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("count metadata");
    assert_eq!(counts, (1, 1, 1));

    let current: (String, String) = connection
        .query_row(
            "SELECT current_operation_id, current_view_id FROM current_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("current state");
    assert_eq!(current.0, json["data"]["current_operation_id"]);
    assert_eq!(current.1, json["data"]["current_view_id"]);
}

#[test]
fn init_is_idempotent() {
    let repo = TestRepo::new_git();

    repo.forge().args(["--json", "init"]).assert().success();
    let output = repo
        .forge()
        .args(["--json", "init"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["data"]["already_initialized"], true);

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open forge db");
    let repo_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM repositories", [], |row| row.get(0))
        .expect("count repositories");
    assert_eq!(repo_count, 1);
}

#[test]
fn existing_repository_without_content_backend_column_migrates_on_normal_command() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    {
        let connection =
            Connection::open(repo.path().join(".forge/forge.db")).expect("open forge db");
        connection
            .execute("ALTER TABLE repositories DROP COLUMN content_backend", [])
            .expect("simulate older schema");
    }

    let output = repo
        .forge()
        .args(["--json", "doctor"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["data"]["ok"], true);

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open forge db");
    let backend: String = connection
        .query_row("SELECT content_backend FROM repositories", [], |row| {
            row.get(0)
        })
        .expect("content backend default");
    assert_eq!(backend, "git");
}

#[test]
fn init_outside_git_repo_returns_structured_error() {
    let temp_dir = tempfile::tempdir().expect("temp dir");

    let output = forge_in(temp_dir.path())
        .args(["--json", "init"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["command"], "init");
    assert_eq!(json["status"], "error");
    assert_eq!(json["errors"][0]["code"], "NOT_A_GIT_REPOSITORY");
}

#[test]
fn json_mode_reports_missing_subcommand_as_envelope() {
    let repo = TestRepo::new_git();

    let output = repo
        .forge()
        .arg("--json")
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["schema_version"], "forge.cli.v0");
    assert_eq!(json["command"], "forge");
    assert_eq!(json["status"], "error");
    assert_eq!(json["errors"][0]["code"], "MISSING_ARGUMENT");
}

#[test]
fn json_mode_reports_missing_export_branch_name_as_envelope() {
    let repo = TestRepo::new_git();

    let output = repo
        .forge()
        .args(["--json", "export", "branch"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["command"], "export branch");
    assert_eq!(json["errors"][0]["code"], "MISSING_ARGUMENT");
}

#[test]
fn json_mode_reports_unknown_argument_as_envelope() {
    let repo = TestRepo::new_git();

    let output = repo
        .forge()
        .args(["--json", "--unknown", "init"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).expect("valid json");
    assert_eq!(json["command"], "init");
    assert_eq!(json["errors"][0]["code"], "UNKNOWN_ARGUMENT");
}
