//! Phase 9 trust policy: local signatures stay compatible by default, and an
//! opt-in locally_signed policy fails closed before accept/export.

mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json envelope")
}

fn db(repo: &TestRepo) -> Connection {
    Connection::open(repo.path().join(".forge/forge.db")).expect("open forge.db")
}

fn prepare_checked_native_proposal(repo: &TestRepo) {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "start",
            "trust policy lifecycle",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("policy.txt"), "policy\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
}

#[test]
fn trust_policy_shows_and_updates_minimum_levels() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    let shown = json(
        repo.forge()
            .args(["--json", "trust", "policy"])
            .assert()
            .success(),
    );
    assert_eq!(shown["data"]["min_accept_trust"], "self_reported");
    assert_eq!(shown["data"]["min_export_trust"], "self_reported");
    assert!(shown["data"]["supported_trust_levels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|level| level == "locally_signed"));

    let updated = json(
        repo.forge()
            .args([
                "--json",
                "trust",
                "policy",
                "--accept",
                "locally_signed",
                "--export",
                "locally_signed",
            ])
            .assert()
            .success(),
    );
    assert_eq!(updated["data"]["min_accept_trust"], "locally_signed");
    assert_eq!(updated["data"]["min_export_trust"], "locally_signed");
}

#[test]
fn default_policy_preserves_accept_compatibility_when_signature_is_missing() {
    let repo = TestRepo::new_git();
    prepare_checked_native_proposal(&repo);
    db(&repo)
        .execute(
            "DELETE FROM ledger_signatures WHERE subject_kind = 'evidence'",
            [],
        )
        .expect("delete evidence signature");

    let accepted = json(repo.forge().args(["--json", "accept"]).assert().success());
    assert_eq!(accepted["data"]["decision"], "accepted");
}

#[test]
fn locally_signed_accept_policy_rejects_unsigned_evidence() {
    let repo = TestRepo::new_git();
    prepare_checked_native_proposal(&repo);
    db(&repo)
        .execute(
            "DELETE FROM ledger_signatures WHERE subject_kind = 'evidence'",
            [],
        )
        .expect("delete evidence signature");
    repo.forge()
        .args(["--json", "trust", "policy", "--accept", "locally_signed"])
        .assert()
        .success();

    let blocked = json(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");
    assert_eq!(blocked["errors"][0]["details"]["action"], "accept");
    assert_eq!(
        blocked["errors"][0]["details"]["required_trust"],
        "locally_signed"
    );
    let issues = blocked["errors"][0]["details"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature" && issue["subject_kind"] == "evidence"
    }));
}

#[test]
fn locally_signed_accept_policy_rejects_no_evidence_proposal() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "no evidence policy"])
        .assert()
        .success();
    std::fs::write(repo.path().join("no-evidence.txt"), "no evidence\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge()
        .args(["--json", "trust", "policy", "--accept", "locally_signed"])
        .assert()
        .success();

    let blocked = json(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");
    let issues = blocked["errors"][0]["details"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature" && issue["subject_kind"] == "proposal_revision"
    }));
}

#[test]
fn locally_signed_export_policy_rejects_unsigned_accepted_decision_before_branch() {
    let repo = TestRepo::new_git();
    prepare_checked_native_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();
    db(&repo)
        .execute(
            "DELETE FROM ledger_signatures WHERE subject_kind = 'decision'",
            [],
        )
        .expect("delete decision signature");
    repo.forge()
        .args(["--json", "trust", "policy", "--export", "locally_signed"])
        .assert()
        .success();

    let blocked = json(
        repo.forge()
            .args(["--json", "export", "branch", "trust-policy-export"])
            .assert()
            .failure(),
    );
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");
    assert_eq!(blocked["errors"][0]["details"]["action"], "export");
    let issues = blocked["errors"][0]["details"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature" && issue["subject_kind"] == "decision"
    }));
    repo.forge()
        .args(["--json", "export", "verify-branch", "trust-policy-export"])
        .assert()
        .failure();
}
