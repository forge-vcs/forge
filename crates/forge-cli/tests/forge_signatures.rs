//! Phase 9 local signing: new evidence, decision, and native commit ledger subjects
//! receive Ed25519 `locally_signed` attestations, and `doctor` verifies them.

mod common;

use common::TestRepo;
use rusqlite::{params, Connection};
use serde_json::Value;

fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json envelope")
}

fn db(repo: &TestRepo) -> Connection {
    Connection::open(repo.path().join(".forge/forge.db")).expect("open forge.db")
}

fn native_accepted_lifecycle(repo: &TestRepo) -> String {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "start",
            "signed lifecycle",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("signed.txt"), "signed\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    let accepted = json(repo.forge().args(["--json", "accept"]).assert().success());
    accepted["data"]["commit_id"]
        .as_str()
        .expect("native commit id")
        .to_string()
}

fn fingerprint_for_subject(repo: &TestRepo, subject_kind: &str) -> String {
    db(repo)
        .query_row(
            "SELECT key_fingerprint FROM ledger_signatures WHERE subject_kind = ?1 ORDER BY rowid DESC LIMIT 1",
            [subject_kind],
            |row| row.get(0),
        )
        .expect("signature fingerprint")
}

#[test]
fn key_status_creates_and_reports_the_local_public_key() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    let status = json(
        repo.forge()
            .args(["--json", "key", "status"])
            .assert()
            .success(),
    );
    assert_eq!(status["data"]["key_path"], ".forge/keys/local-ed25519.pk8");
    assert_eq!(status["data"]["exists_before_command"], false);
    assert_eq!(
        status["data"]["key_fingerprint"]
            .as_str()
            .expect("fingerprint")
            .len(),
        32
    );
    assert_eq!(
        status["data"]["public_key"]
            .as_str()
            .expect("public key")
            .len(),
        64
    );
    assert!(repo.path().join(".forge/keys/local-ed25519.pk8").exists());

    let shown_again = json(
        repo.forge()
            .args(["--json", "key", "status"])
            .assert()
            .success(),
    );
    assert_eq!(shown_again["data"]["exists_before_command"], true);
    assert_eq!(
        shown_again["data"]["key_fingerprint"],
        status["data"]["key_fingerprint"]
    );
}

#[test]
fn key_rotate_preserves_old_signature_verification_and_uses_new_key_for_future_rows() {
    let repo = TestRepo::new_git();
    native_accepted_lifecycle(&repo);
    let old_fingerprint = fingerprint_for_subject(&repo, "decision");

    let rotation = json(
        repo.forge()
            .args(["--json", "key", "rotate"])
            .assert()
            .success(),
    );
    let new_fingerprint = rotation["data"]["key_fingerprint"]
        .as_str()
        .expect("new fingerprint")
        .to_string();
    assert_ne!(new_fingerprint, old_fingerprint);
    assert_eq!(
        rotation["data"]["previous_fingerprint"],
        Value::String(old_fingerprint.clone())
    );
    let backup = rotation["data"]["previous_key_backup_path"]
        .as_str()
        .expect("backup path");
    assert!(repo.path().join(backup).exists(), "backup {backup} missing");

    repo.forge()
        .args([
            "--json",
            "start",
            "rotated lifecycle",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("rotated.txt"), "rotated\n").expect("write rotated feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();

    let connection = db(&repo);
    let old_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM ledger_signatures WHERE key_fingerprint = ?1",
            [&old_fingerprint],
            |row| row.get(0),
        )
        .expect("old signature count");
    let new_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM ledger_signatures WHERE key_fingerprint = ?1",
            [&new_fingerprint],
            |row| row.get(0),
        )
        .expect("new signature count");
    assert!(old_count > 0, "old signatures should remain in the ledger");
    assert!(new_count > 0, "new signatures should use the rotated key");

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], true, "doctor report: {report}");
    assert!(report["data"]["signature_issues"]
        .as_array()
        .expect("signature_issues array")
        .is_empty());
}

#[test]
fn doctor_verifies_local_signatures_for_evidence_decision_and_native_commit() {
    let repo = TestRepo::new_git();
    let commit_id = native_accepted_lifecycle(&repo);

    let connection = db(&repo);
    for subject_kind in ["evidence", "decision", "commit"] {
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM ledger_signatures WHERE subject_kind = ?1",
                [subject_kind],
                |row| row.get(0),
            )
            .expect("count signatures");
        assert_eq!(count, 1, "expected one {subject_kind} signature");
    }
    let signed_commit: String = connection
        .query_row(
            "SELECT signed_digest FROM ledger_signatures WHERE subject_kind = 'commit'",
            [],
            |row| row.get(0),
        )
        .expect("commit signature digest");
    assert_eq!(signed_commit, commit_id);

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], true, "doctor report: {report}");
    assert!(report["data"]["signature_issues"]
        .as_array()
        .expect("signature_issues array")
        .is_empty());
}

#[test]
fn doctor_reports_missing_post_migration_signature() {
    let repo = TestRepo::new_git();
    native_accepted_lifecycle(&repo);
    db(&repo)
        .execute(
            "DELETE FROM ledger_signatures WHERE subject_kind = 'evidence'",
            [],
        )
        .expect("delete evidence signature");

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    let issues = report["data"]["signature_issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature" && issue["subject_kind"] == "evidence"
    }));
}

#[test]
fn doctor_reports_invalid_local_signature_bytes() {
    let repo = TestRepo::new_git();
    native_accepted_lifecycle(&repo);
    db(&repo)
        .execute(
            "UPDATE ledger_signatures SET signature = ?1 WHERE subject_kind = 'decision'",
            params!["00"],
        )
        .expect("corrupt decision signature");

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    let issues = report["data"]["signature_issues"].as_array().unwrap();
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "invalid_signature" && issue["subject_kind"] == "decision"
    }));
}

#[test]
fn doctor_reports_commit_signature_whose_decision_subject_disappeared() {
    let repo = TestRepo::new_git();
    let commit_id = native_accepted_lifecycle(&repo);
    db(&repo)
        .execute(
            "UPDATE decisions SET commit_id = NULL WHERE commit_id = ?1",
            params![commit_id],
        )
        .expect("remove decision commit subject");

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    let issues = report["data"]["signature_issues"].as_array().unwrap();
    assert!(issues
        .iter()
        .any(|issue| { issue["kind"] == "subject_missing" && issue["subject_kind"] == "commit" }));
}
