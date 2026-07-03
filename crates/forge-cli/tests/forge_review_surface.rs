mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn prepare_checked_proposal(repo: &TestRepo, intent: &str) -> String {
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", intent])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "reviewed\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_id = proposed["data"]["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    repo.forge()
        .args(["--json", "check", "--proposal", &proposal_id])
        .assert()
        .success();
    proposal_id
}

fn operation_count(repo: &TestRepo) -> i64 {
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    connection
        .query_row("SELECT COUNT(*) FROM operations", [], |row| row.get(0))
        .expect("operation count")
}

#[test]
fn review_show_returns_ready_read_only_aggregate() {
    let repo = TestRepo::new_git();
    let proposal_id = prepare_checked_proposal(&repo, "review ready proposal");
    let before = operation_count(&repo);

    let reviewed = json_output(
        repo.forge()
            .args(["--json", "review", "show", "--proposal", &proposal_id])
            .assert()
            .success(),
    );

    assert_eq!(reviewed["operation_id"], Value::Null);
    assert_eq!(operation_count(&repo), before);
    assert_eq!(reviewed["data"]["proposal"]["proposal_id"], proposal_id);
    assert_eq!(reviewed["data"]["readiness"]["status"], "ready");
    assert_eq!(reviewed["data"]["lifecycle"]["check_status"], "passed");
    assert!(reviewed["data"]["terminal_handoffs"]
        .as_array()
        .expect("handoffs")
        .iter()
        .any(|handoff| handoff["command"]
            .as_str()
            .unwrap()
            .starts_with("forge accept --proposal")));
}

#[test]
fn review_show_blocks_missing_check_and_unknown_proposal_is_typed() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "review blocked proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "unchecked\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    let proposed = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_id = proposed["data"]["proposal_id"].as_str().unwrap();

    let reviewed = json_output(
        repo.forge()
            .args(["--json", "review", "show", "--proposal", proposal_id])
            .assert()
            .success(),
    );
    assert_eq!(reviewed["operation_id"], Value::Null);
    assert_eq!(reviewed["data"]["readiness"]["status"], "blocked");
    assert!(reviewed["data"]["readiness"]["deciding_factors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|factor| factor["code"] == "missing_check"));

    let unknown = json_output(
        repo.forge()
            .args(["--json", "review", "show", "--proposal", "proposal_missing"])
            .assert()
            .failure(),
    );
    assert_eq!(unknown["errors"][0]["code"], "UNKNOWN_PROPOSAL");
}

#[test]
fn review_export_escapes_html_and_contains_no_mutating_controls() {
    let repo = TestRepo::new_git();
    let proposal_id = prepare_checked_proposal(&repo, "review <script>alert(1)</script>");
    let output = repo.path().join("target/review.html");

    let exported = json_output(
        repo.forge()
            .args([
                "--json",
                "review",
                "export",
                "--proposal",
                &proposal_id,
                "--output",
                output.to_str().unwrap(),
            ])
            .assert()
            .success(),
    );

    assert_eq!(exported["operation_id"], Value::Null);
    assert_eq!(exported["data"]["readiness"], "ready");
    let html = std::fs::read_to_string(output).expect("read html");
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script>alert(1)</script>"));
    assert!(!html.contains("<form"));
    assert!(!html.contains("type=\"submit\""));
    assert!(html.contains("forge accept --proposal"));
}

#[test]
fn review_sanitizes_private_paths_in_json_and_html() {
    let repo = TestRepo::new_git();
    std::fs::create_dir_all(repo.path().join("private")).expect("private dir");
    std::fs::write(
        repo.path().join("private/SECRET_REVIEW_SENTINEL.txt"),
        "initial private content\n",
    )
    .expect("write initial private file");
    let status = std::process::Command::new("git")
        .args(["add", "private/SECRET_REVIEW_SENTINEL.txt"])
        .current_dir(repo.path())
        .status()
        .expect("git add initial private sentinel");
    assert!(status.success(), "git add initial private sentinel failed");
    let status = std::process::Command::new("git")
        .args(["commit", "-m", "add private sentinel"])
        .current_dir(repo.path())
        .status()
        .expect("git commit initial private sentinel");
    assert!(
        status.success(),
        "git commit initial private sentinel failed"
    );
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "review private path"])
        .assert()
        .success();
    std::fs::write(
        repo.path().join("private/SECRET_REVIEW_SENTINEL.txt"),
        "SECRET_REVIEW_PAYLOAD\n",
    )
    .expect("write private file");
    let status = std::process::Command::new("git")
        .args(["add", "private/SECRET_REVIEW_SENTINEL.txt"])
        .current_dir(repo.path())
        .status()
        .expect("git add modified private sentinel");
    assert!(status.success(), "git add modified private sentinel failed");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_id = proposed["data"]["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    repo.forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--path",
            "private/SECRET_REVIEW_SENTINEL.txt",
            "--visibility",
            "private",
        ])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "check", "--proposal", &proposal_id])
        .assert()
        .success();

    let reviewed = json_output(
        repo.forge()
            .args(["--json", "review", "show", "--proposal", &proposal_id])
            .assert()
            .success(),
    );
    let json = serde_json::to_string(&reviewed).expect("serialize");
    assert!(!json.contains("SECRET_REVIEW_SENTINEL"));
    assert!(!json.contains("SECRET_REVIEW_PAYLOAD"));
    assert_eq!(
        reviewed["data"]["visibility"]["private_path_detail"],
        "restricted_count_only"
    );
    assert!(reviewed["data"]["readiness"]["deciding_factors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|factor| factor["code"] == "restricted_content"));

    let output = repo.path().join("target/private-review.html");
    repo.forge()
        .args([
            "--json",
            "review",
            "export",
            "--proposal",
            &proposal_id,
            "--output",
            output.to_str().unwrap(),
        ])
        .assert()
        .success();
    let html = std::fs::read_to_string(output).expect("read html");
    assert!(!html.contains("SECRET_REVIEW_SENTINEL"));
    assert!(!html.contains("SECRET_REVIEW_PAYLOAD"));
    assert!(html.contains("restricted_count_only"));
}

#[test]
fn review_open_exports_without_browser_for_automation() {
    let repo = TestRepo::new_git();
    let proposal_id = prepare_checked_proposal(&repo, "review open proposal");
    let output = repo.path().join("target/open-review.html");

    let opened = json_output(
        repo.forge()
            .args([
                "--json",
                "review",
                "open",
                "--proposal",
                &proposal_id,
                "--output",
                output.to_str().unwrap(),
                "--no-browser",
            ])
            .assert()
            .success(),
    );

    assert_eq!(opened["operation_id"], Value::Null);
    assert_eq!(opened["data"]["opened"], false);
    assert!(opened["warnings"][0]
        .as_str()
        .unwrap()
        .contains("--no-browser"));
    assert!(output.is_file());
}
