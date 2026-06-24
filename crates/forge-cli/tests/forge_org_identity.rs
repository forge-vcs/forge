mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json envelope")
}

fn open_db(repo: &TestRepo) -> Connection {
    Connection::open(repo.path().join(".forge/forge.db")).expect("open forge.db")
}

#[test]
fn org_status_is_disabled_after_init_and_legacy_visibility_still_works() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let out = json(
        repo.forge()
            .args(["--json", "org", "status"])
            .assert()
            .success(),
    );
    assert_eq!(out["command"], "org status");
    assert_eq!(out["data"]["enabled"], false);
    assert_eq!(out["data"]["policy_revision"], 0);
    assert_eq!(out["data"]["recovery_status"], "normal");
    assert_eq!(out["data"]["principal_count"], 0);
    assert_eq!(out["data"]["key_binding_count"], 0);
    assert_eq!(out["data"]["role_binding_count"], 0);
    assert!(out["data"].get("org_id").is_none());

    let visibility = json(
        repo.forge()
            .args(["--json", "visibility", "policy"])
            .assert()
            .success(),
    );
    assert_eq!(
        visibility["data"]["default_work_package_visibility"],
        "public"
    );
}

#[test]
fn org_init_bootstraps_owner_and_local_key() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    let out = json(
        repo.forge()
            .args([
                "--json",
                "org",
                "init",
                "--actor",
                "alice",
                "--reason",
                "bootstrap org",
            ])
            .assert()
            .success(),
    );
    assert_eq!(out["command"], "org init");
    let data = &out["data"];
    assert_eq!(data["enabled"], true);
    assert_eq!(data["policy_revision"], 1);
    assert_eq!(data["owner_alias"], "alice");
    assert_eq!(data["role"], "owner");
    assert!(data["operation_id"].as_str().unwrap().starts_with("op_"));
    assert_eq!(out["operation_id"], data["operation_id"]);
    assert!(data["org_id"].as_str().unwrap().starts_with("org_"));
    assert!(data["owner_actor_id"]
        .as_str()
        .unwrap()
        .starts_with("actor_"));
    assert_eq!(data["key_fingerprint"].as_str().unwrap().len(), 32);
    assert_eq!(data["public_key"].as_str().unwrap().len(), 64);

    let conn = open_db(&repo);
    let profile: (i64, i64, String, String, String) = conn
        .query_row(
            "SELECT enabled, policy_revision, org_id, bootstrap_actor_id,
                    bootstrap_key_fingerprint
             FROM org_authority_profile WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .expect("query org profile");
    assert_eq!(profile.0, 1);
    assert_eq!(profile.1, 1);
    assert_eq!(profile.2, data["org_id"].as_str().unwrap());
    assert_eq!(profile.3, data["owner_actor_id"].as_str().unwrap());
    assert_eq!(profile.4, data["key_fingerprint"].as_str().unwrap());

    let principal: (String, String) = conn
        .query_row(
            "SELECT kind, state FROM org_principals WHERE repo_id = (SELECT id FROM repositories)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query bootstrap principal");
    assert_eq!(principal, ("human".to_string(), "active".to_string()));
    let key_binding: (String, String) = conn
        .query_row(
            "SELECT key_fingerprint, state FROM org_key_bindings
             WHERE repo_id = (SELECT id FROM repositories)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query bootstrap key binding");
    assert_eq!(key_binding.0, data["key_fingerprint"].as_str().unwrap());
    assert_eq!(key_binding.1, "active");
    let role_binding: (String, String) = conn
        .query_row(
            "SELECT role, state FROM org_role_bindings
             WHERE repo_id = (SELECT id FROM repositories)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query bootstrap role binding");
    assert_eq!(role_binding, ("owner".to_string(), "active".to_string()));
    let audit: (i64, String) = conn
        .query_row(
            "SELECT COUNT(*), MAX(reason) FROM org_policy_audit
             WHERE action = 'org_init'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query bootstrap audit");
    assert_eq!(audit, (1, "bootstrap org".to_string()));

    let status = json(
        repo.forge()
            .args(["--json", "org", "status"])
            .assert()
            .success(),
    );
    assert_eq!(status["data"]["enabled"], true);
    assert_eq!(status["data"]["principal_count"], 1);
    assert_eq!(status["data"]["key_binding_count"], 1);
    assert_eq!(status["data"]["role_binding_count"], 1);
    assert_eq!(
        status["data"]["bootstrap_key_fingerprint"],
        data["key_fingerprint"]
    );
}

#[test]
fn org_init_replays_success_for_same_request_id() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let first = json(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "org-init-retry",
                "org",
                "init",
                "--actor",
                "alice",
            ])
            .assert()
            .success(),
    );
    let replay = json(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "org-init-retry",
                "org",
                "init",
                "--actor",
                "alice",
            ])
            .assert()
            .success(),
    );
    let replay_with_different_args = json(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "org-init-retry",
                "org",
                "init",
                "--actor",
                "bob",
                "--reason",
                "must not replace original",
            ])
            .assert()
            .success(),
    );

    assert_eq!(replay["command"], "org init");
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["request_id"], "org-init-retry");
    for key in [
        "operation_id",
        "enabled",
        "org_id",
        "policy_revision",
        "owner_actor_id",
        "owner_alias",
        "key_fingerprint",
        "public_key",
        "role",
        "audit_id",
    ] {
        assert_eq!(
            replay["data"][key], first["data"][key],
            "replay must preserve {key}"
        );
    }
    assert_eq!(
        replay_with_different_args["data"], replay["data"],
        "same request id must replay the original bootstrap regardless of retry args"
    );

    let conn = open_db(&repo);
    let audit_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM org_policy_audit", [], |row| {
            row.get(0)
        })
        .expect("count audit rows");
    assert_eq!(audit_count, 1, "replay must not bootstrap twice");
}

#[test]
fn org_init_refuses_second_bootstrap() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json(
        repo.forge()
            .args(["--json", "org", "init", "--actor", "alice"])
            .assert()
            .success(),
    );

    let second = json(
        repo.forge()
            .args(["--json", "org", "init", "--actor", "bob"])
            .assert()
            .failure(),
    );
    assert_eq!(second["errors"][0]["code"], "ORG_ALREADY_ENABLED");
    assert_eq!(
        second["errors"][0]["details"]["org_id"],
        first["data"]["org_id"]
    );
}

#[test]
fn org_init_rejects_blank_actor_without_bootstrap_side_effects() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let out = json(
        repo.forge()
            .args(["--json", "org", "init", "--actor", "   "])
            .assert()
            .failure(),
    );
    assert_eq!(out["command"], "org init");
    assert_eq!(out["errors"][0]["code"], "ORG_AUTHORITY_REQUIRED");
    assert_eq!(out["errors"][0]["details"]["action"], "org_init");
    assert_eq!(
        out["errors"][0]["details"]["required_role"],
        "non_empty_actor"
    );

    let conn = open_db(&repo);
    let profile: (i64, i64) = conn
        .query_row(
            "SELECT enabled, policy_revision FROM org_authority_profile WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query org profile");
    assert_eq!(profile, (0, 0));
    for table in [
        "org_principals",
        "org_principal_aliases",
        "org_key_bindings",
        "org_role_bindings",
        "org_policy_audit",
    ] {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = conn
            .query_row(&sql, [], |row| row.get(0))
            .unwrap_or_else(|error| panic!("count {table}: {error}"));
        assert_eq!(count, 0, "{table} must remain empty after blank actor");
    }
}

#[test]
fn org_encryption_binding_enables_private_decrypt_authority_after_grant() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    let org = json(
        repo.forge()
            .args(["--json", "org", "init", "--actor", "alice"])
            .assert()
            .success(),
    );
    let principal_id = org["data"]["owner_actor_id"]
        .as_str()
        .expect("owner actor id");

    let attempt = json(
        repo.forge()
            .args(["--json", "start", "private overlay"])
            .assert()
            .success(),
    );
    let attempt_id = attempt["data"]["attempt_id"].as_str().expect("attempt id");

    let missing_before_key = json(
        repo.forge()
            .args([
                "--json",
                "org",
                "decrypt-authority",
                "--kind",
                "attempt",
                "--id",
                attempt_id,
                "--principal-id",
                principal_id,
            ])
            .assert()
            .failure(),
    );
    assert_eq!(
        missing_before_key["errors"][0]["code"],
        "PRIVATE_DECRYPT_AUTHORITY_MISSING"
    );

    let binding = json(
        repo.forge()
            .args([
                "--json",
                "org",
                "encryption",
                "bind-local",
                "--principal-id",
                principal_id,
                "--reason",
                "bind local recipient",
            ])
            .assert()
            .success(),
    );
    assert_eq!(binding["command"], "org encryption bind-local");
    assert_eq!(binding["data"]["principal_id"], principal_id);
    assert!(binding["data"]["public_key"]
        .as_str()
        .expect("age recipient")
        .starts_with("age1"));
    assert!(binding["data"]["key_fingerprint"]
        .as_str()
        .expect("fingerprint")
        .starts_with("age-x25519:"));

    let missing_before_grant = json(
        repo.forge()
            .args([
                "--json",
                "org",
                "decrypt-authority",
                "--kind",
                "attempt",
                "--id",
                attempt_id,
                "--principal-id",
                principal_id,
            ])
            .assert()
            .failure(),
    );
    assert_eq!(
        missing_before_grant["errors"][0]["code"],
        "PRIVATE_DECRYPT_AUTHORITY_MISSING"
    );
    assert!(missing_before_grant["errors"][0]["details"]["reason"]
        .as_str()
        .expect("reason")
        .starts_with("missing_visibility_grant:"));

    repo.forge()
        .args([
            "--json",
            "visibility",
            "grant",
            "--kind",
            "attempt",
            "--id",
            attempt_id,
            "--recipient",
            principal_id,
            "--capability",
            "sync_materialize",
        ])
        .assert()
        .success();

    let authority = json(
        repo.forge()
            .args([
                "--json",
                "org",
                "decrypt-authority",
                "--kind",
                "attempt",
                "--id",
                attempt_id,
                "--principal-id",
                principal_id,
            ])
            .assert()
            .success(),
    );
    assert_eq!(authority["command"], "org decrypt-authority");
    assert_eq!(authority["data"]["principal_id"], principal_id);
    assert_eq!(
        authority["data"]["public_key"],
        binding["data"]["public_key"]
    );
    assert_eq!(
        authority["data"]["recipient_fingerprint"],
        binding["data"]["key_fingerprint"]
    );
}
