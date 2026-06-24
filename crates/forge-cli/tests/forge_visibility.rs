//! Visibility command surface for permissioned Forge projections.

mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json envelope")
}

fn started_attempt_id(output: &Value) -> String {
    output["data"]["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string()
}

#[test]
fn visibility_policy_and_projection_lifecycle() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private extension"])
            .assert()
            .success(),
    );
    let attempt_id = started_attempt_id(&started);

    let policy = json(
        repo.forge()
            .args(["--json", "visibility", "policy"])
            .assert()
            .success(),
    );
    assert_eq!(policy["data"]["default_work_package_visibility"], "public");
    assert!(policy["data"]["supported_visibility_labels"]
        .as_array()
        .expect("labels")
        .iter()
        .any(|label| label == "embargoed"));
    assert!(policy["data"]["supported_capabilities"]
        .as_array()
        .expect("capabilities")
        .iter()
        .any(|capability| capability == "sync_materialize"));

    let public = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "check",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "sync_materialize",
            ])
            .assert()
            .success(),
    );
    assert_eq!(public["data"]["allowed"], true);
    assert_eq!(public["data"]["disclosure"], "full");

    let private = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "set",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--visibility",
                "private",
                "--actor",
                "maintainer",
                "--reason",
                "invite-only review",
            ])
            .assert()
            .success(),
    );
    assert_eq!(private["data"]["work_package_kind"], "attempt");
    assert_eq!(private["data"]["work_package_id"], attempt_id);
    assert_eq!(private["data"]["visibility"], "private");

    let hidden = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "check",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "sync_materialize",
            ])
            .assert()
            .success(),
    );
    assert_eq!(hidden["data"]["allowed"], false);
    assert_eq!(hidden["data"]["visibility"], "private");
    assert_eq!(hidden["data"]["disclosure"], "hidden");

    let stub = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "grant",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "see_stub",
                "--actor",
                "maintainer",
            ])
            .assert()
            .success(),
    );
    assert_eq!(stub["data"]["recipient"], "reviewer@example.test");
    assert_eq!(stub["data"]["capability"], "see_stub");
    assert!(stub["data"]["revoked_at_ms"].is_null());

    let stub_decision = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "check",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "sync_materialize",
            ])
            .assert()
            .success(),
    );
    assert_eq!(stub_decision["data"]["allowed"], false);
    assert_eq!(stub_decision["data"]["disclosure"], "stub");

    json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "grant",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "sync_materialize",
                "--actor",
                "maintainer",
            ])
            .assert()
            .success(),
    );
    let allowed = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "check",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "sync_materialize",
            ])
            .assert()
            .success(),
    );
    assert_eq!(allowed["data"]["allowed"], true);
    assert_eq!(allowed["data"]["disclosure"], "full");

    let revoked = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "revoke",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "sync_materialize",
                "--actor",
                "maintainer",
            ])
            .assert()
            .success(),
    );
    assert!(revoked["data"]["revoked_at_ms"].is_i64());

    let after_revoke = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "check",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "sync_materialize",
            ])
            .assert()
            .success(),
    );
    assert_eq!(after_revoke["data"]["allowed"], false);
    assert_eq!(after_revoke["data"]["disclosure"], "stub");

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let audit_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM visibility_audit", [], |row| {
            row.get(0)
        })
        .expect("audit count");
    assert_eq!(audit_count, 4);
}

#[test]
fn visibility_revoke_missing_grant_returns_typed_error() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "missing grant"])
            .assert()
            .success(),
    );
    let attempt_id = started_attempt_id(&started);

    let out = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "revoke",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--recipient",
                "reviewer@example.test",
                "--capability",
                "sync_materialize",
                "--actor",
                "maintainer",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "VISIBILITY_POLICY_UNMET");
    assert_eq!(
        out["errors"][0]["details"]["operation"],
        "revoke_capability"
    );
    assert_eq!(out["errors"][0]["details"]["work_package_kind"], "attempt");
    assert_eq!(out["errors"][0]["details"]["work_package_id"], attempt_id);
    assert_eq!(
        out["errors"][0]["details"]["capability"],
        "sync_materialize"
    );
    assert_eq!(out["errors"][0]["details"]["disclosure"], "hidden");
    assert_eq!(out["retry"]["retryable"], false);
}
