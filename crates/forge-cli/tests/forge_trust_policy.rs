//! Phase 9 trust policy: local signatures stay compatible by default, and an
//! opt-in locally_signed policy fails closed before accept/export.

mod common;

use common::TestRepo;
use ring::rand::SystemRandom;
use ring::signature::Ed25519KeyPair;
use rusqlite::Connection;
use serde_json::Value;
use std::path::PathBuf;

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

fn hosted_runner_key(repo: &TestRepo) -> PathBuf {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("generate hosted runner key");
    let path = repo.path().join("hosted-runner-ed25519.pk8");
    std::fs::write(&path, pkcs8.as_ref()).expect("write hosted runner key");
    path
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
    assert!(shown["data"]["supported_trust_levels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|level| level == "hosted_runner_signed"));
    assert!(shown["data"]["supported_trust_levels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|level| level == "third_party_attested"));

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
fn higher_attestation_policy_is_configurable_but_fails_closed_at_accept() {
    let repo = TestRepo::new_git();
    prepare_checked_native_proposal(&repo);

    let updated = json(
        repo.forge()
            .args([
                "--json",
                "trust",
                "policy",
                "--accept",
                "hosted_runner_signed",
            ])
            .assert()
            .success(),
    );
    assert_eq!(updated["data"]["min_accept_trust"], "hosted_runner_signed");

    let blocked = json(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");
    assert_eq!(blocked["errors"][0]["details"]["action"], "accept");
    assert_eq!(
        blocked["errors"][0]["details"]["required_trust"],
        "hosted_runner_signed"
    );
    let issues = blocked["errors"][0]["details"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature" && issue["subject_kind"] == "evidence"
    }));
}

#[test]
fn hosted_runner_attestation_satisfies_hosted_runner_signed_accept_policy() {
    let repo = TestRepo::new_git();
    prepare_checked_native_proposal(&repo);
    let key = hosted_runner_key(&repo);

    repo.forge()
        .args([
            "--json",
            "trust",
            "policy",
            "--accept",
            "hosted_runner_signed",
        ])
        .assert()
        .success();
    let blocked = json(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");

    let attested = json(
        repo.forge()
            .args([
                "--json",
                "trust",
                "attest",
                "hosted-runner",
                "--key",
                key.to_str().expect("utf8 key path"),
                "--issuer",
                "ci.example/verify",
            ])
            .assert()
            .success(),
    );
    assert_eq!(attested["data"]["trust_level"], "hosted_runner_signed");
    assert_eq!(attested["data"]["issuer"], "ci.example/verify");
    assert_eq!(attested["data"]["signature_count"], 1);
    assert_eq!(attested["data"]["subject_count"], 1);

    let accepted = json(repo.forge().args(["--json", "accept"]).assert().success());
    assert_eq!(accepted["data"]["decision"], "accepted");

    let doctor = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert!(
        doctor["data"]["signature_key_summary"]["hosted_runner_key_fingerprints"]
            .as_array()
            .expect("hosted runner keys")
            .len()
            == 1
    );
}

#[test]
fn hosted_runner_attestation_does_not_satisfy_third_party_policy() {
    let repo = TestRepo::new_git();
    prepare_checked_native_proposal(&repo);
    let key = hosted_runner_key(&repo);

    repo.forge()
        .args([
            "--json",
            "trust",
            "attest",
            "hosted-runner",
            "--key",
            key.to_str().expect("utf8 key path"),
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "trust",
            "policy",
            "--accept",
            "third_party_attested",
        ])
        .assert()
        .success();

    let blocked = json(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");
    let issues = blocked["errors"][0]["details"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature"
            && issue["subject_kind"] == "attestation"
            && issue["subject_id"] == "third_party_attested"
    }));
}

#[test]
fn edited_local_signature_cannot_be_upgraded_to_hosted_runner_trust() {
    let repo = TestRepo::new_git();
    prepare_checked_native_proposal(&repo);
    db(&repo)
        .execute(
            "UPDATE signing_keys SET trust_origin = 'hosted_runner'
             WHERE key_fingerprint IN (
                SELECT key_fingerprint FROM ledger_signatures WHERE subject_kind = 'evidence'
             )",
            [],
        )
        .expect("spoof hosted key origin");
    db(&repo)
        .execute(
            "UPDATE ledger_signatures
             SET trust_level = 'hosted_runner_signed'
             WHERE subject_kind = 'evidence'",
            [],
        )
        .expect("spoof hosted trust level");
    repo.forge()
        .args([
            "--json",
            "trust",
            "policy",
            "--accept",
            "hosted_runner_signed",
        ])
        .assert()
        .success();

    let blocked = json(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");
    let issues = blocked["errors"][0]["details"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "invalid_signature" && issue["subject_kind"] == "evidence"
    }));
}

#[test]
fn third_party_attestation_policy_is_configurable_but_fails_closed_at_export() {
    let repo = TestRepo::new_git();
    prepare_checked_native_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();

    let updated = json(
        repo.forge()
            .args([
                "--json",
                "trust",
                "policy",
                "--export",
                "third_party_attested",
            ])
            .assert()
            .success(),
    );
    assert_eq!(updated["data"]["min_export_trust"], "third_party_attested");

    let blocked = json(
        repo.forge()
            .args(["--json", "export", "branch", "attestation-policy-export"])
            .assert()
            .failure(),
    );
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");
    assert_eq!(blocked["errors"][0]["details"]["action"], "export");
    assert_eq!(
        blocked["errors"][0]["details"]["required_trust"],
        "third_party_attested"
    );
    let issues = blocked["errors"][0]["details"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature"
            && issue["subject_kind"] == "attestation"
            && issue["subject_id"] == "third_party_attested"
    }));
    repo.forge()
        .args([
            "--json",
            "export",
            "verify-branch",
            "attestation-policy-export",
        ])
        .assert()
        .failure();
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
