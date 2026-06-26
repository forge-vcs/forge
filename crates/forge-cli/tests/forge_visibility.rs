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

fn prepare_checked_proposal(repo: &TestRepo) -> String {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "embargoed security fix"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "security fix\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed = json(repo.forge().args(["--json", "propose"]).assert().success());
    repo.forge().args(["--json", "check"]).assert().success();
    proposed["data"]["proposal_id"]
        .as_str()
        .expect("proposal id")
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
fn embargo_workflow_blocks_generic_visibility_and_gates_publication() {
    let repo = TestRepo::new_git();
    let proposal_id = prepare_checked_proposal(&repo);

    let direct_set = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "set",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--visibility",
                "embargoed",
                "--actor",
                "maintainer",
            ])
            .assert()
            .success(),
    );
    assert_eq!(direct_set["data"]["visibility"], "embargoed");

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let workflow_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM embargo_workflows
             WHERE work_package_kind = 'proposal' AND work_package_id = ?1 AND state = 'active'",
            [&proposal_id],
            |row| row.get(0),
        )
        .expect("workflow count");
    assert_eq!(workflow_count, 1);

    let marked = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "mark",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--actor",
                "maintainer",
                "--reason",
                "coordinate fix privately",
            ])
            .assert()
            .success(),
    );
    assert_eq!(marked["data"]["workflow"]["state"], "active");
    assert_eq!(marked["data"]["event"]["action"], "mark");

    let generic_grant = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "grant",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
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
    assert_eq!(
        generic_grant["errors"][0]["code"],
        "EMBARGO_WORKFLOW_REQUIRED"
    );

    let embargo_grant = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "grant",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
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
    assert_eq!(embargo_grant["data"]["event"]["action"], "grant");
    assert_eq!(
        embargo_grant["data"]["event"]["capability"],
        "sync_materialize"
    );

    let accepted = json(
        repo.forge()
            .args(["--json", "accept", "--proposal", &proposal_id])
            .assert()
            .success(),
    );
    assert_eq!(accepted["data"]["decision"], "accepted");

    let blocked_export = json(
        repo.forge()
            .args(["--json", "export", "branch", "embargo-before-publish"])
            .assert()
            .failure(),
    );
    assert_eq!(blocked_export["errors"][0]["code"], "EMBARGO_STATE_INVALID");
    assert_eq!(
        blocked_export["errors"][0]["details"]["state"],
        "accepted_under_embargo"
    );

    let release_path = repo.path().join("embargo-release.json");
    let missing_release_authorization = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "release",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--recipient",
                "reviewer@example.test",
                "--output",
                release_path.to_str().expect("release path"),
                "--actor",
                "maintainer",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(
        missing_release_authorization["errors"][0]["code"],
        "VISIBILITY_POLICY_UNMET"
    );
    assert_eq!(
        missing_release_authorization["errors"][0]["details"]["capability"],
        "publish_reveal"
    );
    assert!(!release_path.exists());

    repo.forge()
        .args([
            "--json",
            "embargo",
            "grant",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--recipient",
            "reviewer@example.test",
            "--capability",
            "publish_reveal",
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();

    let released = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "release",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--recipient",
                "reviewer@example.test",
                "--output",
                release_path.to_str().expect("release path"),
                "--content-class",
                "release_inputs",
                "--content-class",
                "sanitized_provenance",
                "--actor",
                "maintainer",
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        released["data"]["release"]["workflow"]["state"],
        "released_under_embargo"
    );
    assert_eq!(released["data"]["release"]["event"]["action"], "release");
    assert_eq!(
        released["data"]["report"]["projection"]["policy_version"],
        "embargo-release.v1"
    );
    let reported_digest = released["data"]["report"]["projection"]["bundle_digest"]
        .as_str()
        .expect("reported bundle digest");
    assert_eq!(
        released["data"]["release"]["event"]["bundle_digest"],
        reported_digest
    );
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(&release_path).expect("read embargo release manifest"),
    )
    .expect("valid release manifest");
    assert_eq!(manifest["projection"]["mode"], "embargo_release");
    assert_eq!(manifest["projection"]["recipient"], "reviewer@example.test");
    assert_eq!(manifest["projection"]["work_package_id"], proposal_id);
    assert!(
        manifest["projection"]["bundle_digest"]
            .as_str()
            .expect("bundle digest")
            .len()
            >= 64
    );
    assert_eq!(
        manifest["projection"]["revocation_warning"],
        "Revocation applies to future releases and does not claw back already delivered bundles."
    );
    assert_eq!(manifest["native_head"], Value::Null);
    assert_eq!(
        manifest["native_objects"]
            .as_array()
            .expect("objects")
            .len(),
        0
    );
    assert_eq!(
        manifest["native_payloads"]
            .as_array()
            .expect("payloads")
            .len(),
        0
    );
    assert!(manifest["ledger_rows"]
        .as_array()
        .expect("ledger rows")
        .iter()
        .all(|table| table["rows"].as_array().expect("table rows").is_empty()));

    let import_target = TestRepo::new_git();
    import_target
        .forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let refused_import = json(
        import_target
            .forge()
            .args([
                "--json",
                "sync",
                "import",
                release_path.to_str().expect("release path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused_import["errors"][0]["code"], "COMMAND_FAILED");
    assert_eq!(refused_import["errors"][0]["message"], "apply sync bundle");
    let clone_dir = tempfile::tempdir().expect("clone target dir");
    let refused_clone = json(
        common::forge_in(clone_dir.path())
            .args([
                "--json",
                "sync",
                "clone",
                release_path.to_str().expect("release path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused_clone["errors"][0]["code"], "COMMAND_FAILED");
    assert_eq!(refused_clone["errors"][0]["message"], "clone sync bundle");

    let mut tampered_manifest = manifest.clone();
    tampered_manifest["projection"]["recipient"] = Value::String("attacker@example.test".into());
    let tampered_path = repo.path().join("tampered-embargo-release.json");
    std::fs::write(
        &tampered_path,
        serde_json::to_vec_pretty(&tampered_manifest).expect("serialize tampered manifest"),
    )
    .expect("write tampered manifest");
    let tampered = json(
        repo.forge()
            .args([
                "--json",
                "sync",
                "inspect",
                tampered_path.to_str().expect("tampered path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(tampered["errors"][0]["code"], "COMMAND_FAILED");
    assert!(tampered["errors"][0]["message"]
        .as_str()
        .expect("error message")
        .contains("bundle digest mismatch"));

    let revealed = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "reveal",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--mode",
                "full-source",
                "--public-actor-ref",
                "security-team",
                "--actor",
                "maintainer",
            ])
            .assert()
            .success(),
    );
    assert_eq!(revealed["data"]["workflow"]["state"], "revealed");
    assert_eq!(
        revealed["data"]["workflow"]["public_projection_mode"],
        "full_source"
    );

    let published = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "publish",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--actor",
                "maintainer",
            ])
            .assert()
            .success(),
    );
    assert_eq!(published["data"]["workflow"]["state"], "published");
    assert_eq!(published["data"]["event"]["action"], "publish");

    repo.forge()
        .args(["--json", "export", "branch", "embargo-after-publish"])
        .assert()
        .success();
}

#[test]
fn sanitized_embargo_publication_refuses_full_source_branch_export() {
    let repo = TestRepo::new_git();
    let proposal_id = prepare_checked_proposal(&repo);

    repo.forge()
        .args([
            "--json",
            "embargo",
            "mark",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "accept", "--proposal", &proposal_id])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "embargo",
            "reveal",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--mode",
            "sanitized-source",
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "embargo",
            "publish",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();

    let export = json(
        repo.forge()
            .args(["--json", "export", "branch", "sanitized-publish-export"])
            .assert()
            .failure(),
    );
    assert_eq!(export["errors"][0]["code"], "EMBARGO_STATE_INVALID");
    assert_eq!(export["errors"][0]["details"]["state"], "sanitized_source");
    assert_eq!(export["errors"][0]["details"]["required"], "full_source");
}

#[test]
fn embargo_release_before_accept_fails_without_writing_bundle() {
    let repo = TestRepo::new_git();
    let proposal_id = prepare_checked_proposal(&repo);

    repo.forge()
        .args([
            "--json",
            "embargo",
            "mark",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();

    let release_path = repo.path().join("preaccept-release.json");
    let released = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "release",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--recipient",
                "release-bot@example.test",
                "--output",
                release_path.to_str().expect("release path"),
                "--actor",
                "maintainer",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(released["errors"][0]["code"], "EMBARGO_STATE_INVALID");
    assert_eq!(released["errors"][0]["details"]["state"], "active");
    assert!(
        !release_path.exists(),
        "failed release must not write a bundle"
    );
}

#[test]
fn embargo_release_export_failure_does_not_advance_state() {
    let repo = TestRepo::new_git();
    let proposal_id = prepare_checked_proposal(&repo);

    repo.forge()
        .args([
            "--json",
            "embargo",
            "mark",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "embargo",
            "grant",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--recipient",
            "release-bot@example.test",
            "--capability",
            "sync_materialize",
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "embargo",
            "grant",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--recipient",
            "release-bot@example.test",
            "--capability",
            "publish_reveal",
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "accept", "--proposal", &proposal_id])
        .assert()
        .success();

    let occupied_path = repo.path().join("occupied-release.json");
    std::fs::write(&occupied_path, "already here").expect("write occupied output");
    let release = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "release",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--recipient",
                "release-bot@example.test",
                "--output",
                occupied_path.to_str().expect("occupied path"),
                "--actor",
                "maintainer",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(release["errors"][0]["code"], "COMMAND_FAILED");
    assert!(release["errors"][0]["message"]
        .as_str()
        .expect("release output error")
        .contains("sync export output already exists"));
    assert_eq!(
        std::fs::read_to_string(&occupied_path).expect("occupied output content"),
        "already here"
    );
    let pending_outputs = std::fs::read_dir(repo.path())
        .expect("read repo dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains(".occupied-release.json.pending.")
        })
        .count();
    assert_eq!(pending_outputs, 0);

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let state: String = connection
        .query_row(
            "SELECT state FROM embargo_workflows
             WHERE work_package_kind = 'proposal' AND work_package_id = ?1",
            [&proposal_id],
            |row| row.get(0),
        )
        .expect("embargo state");
    assert_eq!(state, "accepted_under_embargo");
    let release_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM embargo_workflow_events
             WHERE work_package_kind = 'proposal' AND work_package_id = ?1 AND action = 'release'",
            [&proposal_id],
            |row| row.get(0),
        )
        .expect("release event count");
    assert_eq!(release_events, 0);
    let release_authorizations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM embargo_release_authorizations
             WHERE work_package_kind = 'proposal' AND work_package_id = ?1 AND recipient = 'release-bot@example.test'",
            [&proposal_id],
            |row| row.get(0),
        )
        .expect("release authorization count");
    assert_eq!(release_authorizations, 0);
}

#[test]
fn closed_embargo_refuses_later_reveal_or_release() {
    let repo = TestRepo::new_git();
    let proposal_id = prepare_checked_proposal(&repo);

    repo.forge()
        .args([
            "--json",
            "embargo",
            "mark",
            "--kind",
            "proposal",
            "--id",
            &proposal_id,
            "--actor",
            "maintainer",
        ])
        .assert()
        .success();

    let closed = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "close",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--actor",
                "maintainer",
                "--reason",
                "superseded privately",
            ])
            .assert()
            .success(),
    );
    assert_eq!(closed["data"]["workflow"]["state"], "closed");
    assert_eq!(closed["data"]["event"]["action"], "close");

    let reveal = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "reveal",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--mode",
                "provenance-only",
                "--actor",
                "maintainer",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(reveal["errors"][0]["code"], "EMBARGO_STATE_INVALID");
    assert_eq!(reveal["errors"][0]["details"]["state"], "closed");

    let release_path = repo.path().join("closed-release.json");
    let release = json(
        repo.forge()
            .args([
                "--json",
                "embargo",
                "release",
                "--kind",
                "proposal",
                "--id",
                &proposal_id,
                "--recipient",
                "release-bot@example.test",
                "--output",
                release_path.to_str().expect("release path"),
                "--actor",
                "maintainer",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(release["errors"][0]["code"], "EMBARGO_STATE_INVALID");
    assert_eq!(release["errors"][0]["details"]["state"], "closed");
    assert!(
        !release_path.exists(),
        "closed release must not write a bundle"
    );
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
