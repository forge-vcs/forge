//! Phase 9 native sync MVP: export, inspect, and import a versioned sync bundle
//! carrying native object payloads plus ledger rows through the JSON envelope.

mod common;
#[path = "support/sync.rs"]
mod sync_support;

use common::{forge_in, TestRepo};
use serde_json::Value;
use sync_support::*;

#[test]
fn sync_export_writes_a_versioned_native_manifest_and_inspect_reads_it() {
    let repo = TestRepo::new_git();
    native_accepted_lifecycle(&repo);
    let manifest_path = repo.path().join("target/forge-sync-manifest.json");

    let exported = json(
        repo.forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                manifest_path.to_str().expect("utf8 manifest path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["protocol_version"], "forge-sync.v1");
    assert_eq!(exported["data"]["content_backend"], "native");
    assert_eq!(exported["data"]["incremental"], false);
    assert!(exported["data"]["native_head"].as_str().is_some());
    assert!(exported["data"]["native_object_count"].as_u64().unwrap() > 0);
    assert!(exported["data"]["ledger_table_count"].as_u64().unwrap() > 0);
    assert!(exported["data"]["local_key_fingerprint"].as_str().is_some());

    let manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("manifest json");
    assert_eq!(manifest["protocol_version"], "forge-sync.v1");
    assert!(manifest["native_objects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|object| {
            object["kind"] == "commit"
                && object["object_id"]
                    .as_str()
                    .unwrap()
                    .starts_with("f1:commit:")
        }));
    assert!(manifest["native_payloads"]
        .as_array()
        .unwrap()
        .iter()
        .any(|object| {
            object["kind"] == "commit"
                && object["object_id"]
                    .as_str()
                    .unwrap()
                    .starts_with("f1:commit:")
                && object["payload_hex"].as_str().unwrap().len() > 2
        }));
    assert!(manifest["ledger_counts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|count| count["table"] == "ledger_signatures" && count["rows"].as_i64().unwrap() > 0));
    assert!(manifest["ledger_rows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|table| table["table"] == "ledger_signatures"
            && !table["rows"].as_array().unwrap().is_empty()));

    let inspected = json(
        repo.forge()
            .args([
                "--json",
                "sync",
                "inspect",
                manifest_path.to_str().expect("utf8 manifest path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(inspected["data"]["protocol_version"], "forge-sync.v1");
    assert_eq!(inspected["data"]["content_backend"], "native");
    assert_eq!(
        inspected["data"]["native_object_count"],
        exported["data"]["native_object_count"]
    );
    assert_eq!(
        inspected["data"]["native_payload_count"],
        exported["data"]["native_payload_count"]
    );
    assert_eq!(
        inspected["data"]["ledger_table_count"],
        exported["data"]["ledger_table_count"]
    );
    assert_eq!(
        inspected["data"]["ledger_row_count"],
        exported["data"]["ledger_row_count"]
    );
    assert_eq!(
        inspected["data"]["native_head"],
        exported["data"]["native_head"]
    );
    assert_eq!(
        inspected["data"]["local_key_fingerprint"],
        exported["data"]["local_key_fingerprint"]
    );
}

#[test]
fn sync_projected_export_filters_private_work_packages_and_hidden_head() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    let public_start = json(
        repo.forge()
            .args([
                "--json",
                "start",
                "public projected sync",
                "--require",
                "sh -c true",
            ])
            .assert()
            .success(),
    );
    let public_attempt = public_start["data"]["attempt_id"]
        .as_str()
        .expect("public attempt")
        .to_string();
    std::fs::write(repo.path().join("public-sync.txt"), "public\n").expect("write public file");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let public_propose = json(repo.forge().args(["--json", "propose"]).assert().success());
    let public_proposal = public_propose["data"]["proposal_id"]
        .as_str()
        .expect("public proposal")
        .to_string();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();

    let private_start = json(
        repo.forge()
            .args([
                "--json",
                "start",
                "private projected sync",
                "--require",
                "sh -c true",
            ])
            .assert()
            .success(),
    );
    let private_attempt = private_start["data"]["attempt_id"]
        .as_str()
        .expect("private attempt")
        .to_string();
    repo.forge()
        .args([
            "--json",
            "visibility",
            "set",
            "--kind",
            "attempt",
            "--id",
            &private_attempt,
            "--visibility",
            "private",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("private-sync.txt"), "private\n").expect("write private file");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let private_propose = json(repo.forge().args(["--json", "propose"]).assert().success());
    let private_proposal = private_propose["data"]["proposal_id"]
        .as_str()
        .expect("private proposal")
        .to_string();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();

    let manifest_path = repo.path().join("target/projected-sync.json");
    let exported = json(
        repo.forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                manifest_path.to_str().expect("utf8 manifest path"),
                "--recipient",
                "alice@example.test",
            ])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["projection"]["mode"], "recipient");
    assert_eq!(exported["data"]["projection"]["projected"], true);
    assert_eq!(
        exported["data"]["projection"]["capability"],
        "sync_materialize"
    );

    let manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read projected manifest"))
            .expect("projected manifest json");
    assert_eq!(manifest["projection"]["recipient"], "alice@example.test");
    assert!(
        manifest["native_head"].is_null(),
        "hidden latest head is not exported"
    );

    let attempt_ids: Vec<&str> = manifest_table_rows(&manifest, "attempts")
        .iter()
        .map(|row| row["id"].as_str().expect("attempt id"))
        .collect();
    assert!(attempt_ids.contains(&public_attempt.as_str()));
    assert!(!attempt_ids.contains(&private_attempt.as_str()));

    let proposal_ids: Vec<&str> = manifest_table_rows(&manifest, "proposals")
        .iter()
        .map(|row| row["id"].as_str().expect("proposal id"))
        .collect();
    assert!(proposal_ids.contains(&public_proposal.as_str()));
    assert!(!proposal_ids.contains(&private_proposal.as_str()));

    for table in [
        "operations",
        "views",
        "ledger_signatures",
        "conflict_sets",
        "path_conflicts",
        "visibility_policy",
        "work_package_visibility",
        "path_visibility_labels",
        "visibility_grants",
        "visibility_audit",
    ] {
        assert!(
            manifest_table_rows(&manifest, table).is_empty(),
            "projected manifest must not include {table} rows"
        );
        assert_eq!(
            manifest_ledger_count(&manifest, table),
            0,
            "projected manifest count for {table} must be recomputed"
        );
    }

    let payload_texts = decoded_native_payloads(&manifest);
    assert!(
        payload_texts
            .iter()
            .any(|payload| payload.contains("public-sync.txt") || payload == "public\n"),
        "projected manifest should keep public content"
    );
    assert!(
        !payload_texts
            .iter()
            .any(|payload| payload.contains("private-sync.txt") || payload == "private\n"),
        "projected manifest must not include private payload bytes"
    );

    let hidden_target = TestRepo::new_git();
    hidden_target
        .forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let hidden_import = json(
        hidden_target
            .forge()
            .args([
                "--json",
                "sync",
                "import",
                manifest_path.to_str().expect("utf8 hidden bundle path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(hidden_import["data"]["projection"]["mode"], "recipient");
    assert_eq!(hidden_import["data"]["projection"]["projected"], true);
    assert!(hidden_import["data"]["native_head"].is_null());
    assert_eq!(hidden_import["data"]["materialized"], false);
    assert!(
        !hidden_target.path().join("public-sync.txt").exists(),
        "plain hidden projection import must not materialize files"
    );
    let hidden_doctor = json(
        hidden_target
            .forge()
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_no_structural_doctor_findings(&hidden_doctor, "denied projected import");

    let hidden_clone = tempfile::tempdir().expect("hidden projected clone dir");
    forge_in(hidden_clone.path())
        .args([
            "--json",
            "sync",
            "clone",
            manifest_path.to_str().expect("utf8 hidden bundle path"),
        ])
        .assert()
        .failure();

    repo.forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            repo.path()
                .join("target/projected-see-stub.json")
                .to_str()
                .expect("utf8 unsupported capability path"),
            "--recipient",
            "alice@example.test",
            "--capability",
            "see_stub",
        ])
        .assert()
        .failure();

    let full_path = repo.path().join("target/full-sync.json");
    repo.forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            full_path.to_str().expect("utf8 full manifest path"),
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            repo.path()
                .join("target/projected-since-full.json")
                .to_str()
                .expect("utf8 projected since full path"),
            "--since",
            full_path.to_str().expect("utf8 full manifest path"),
            "--recipient",
            "alice@example.test",
        ])
        .assert()
        .failure();
    repo.forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            repo.path()
                .join("target/full-since-projected.json")
                .to_str()
                .expect("utf8 full since projected path"),
            "--since",
            manifest_path
                .to_str()
                .expect("utf8 projected manifest path"),
        ])
        .assert()
        .failure();
    let full_manifest: Value =
        serde_json::from_slice(&std::fs::read(&full_path).expect("read full manifest"))
            .expect("full manifest json");
    let projected_payload_ids: std::collections::HashSet<&str> = manifest["native_payloads"]
        .as_array()
        .expect("projected payloads")
        .iter()
        .map(|payload| payload["object_id"].as_str().expect("payload id"))
        .collect();
    let extra_payload = full_manifest["native_payloads"]
        .as_array()
        .expect("full payloads")
        .iter()
        .find(|payload| {
            !projected_payload_ids.contains(payload["object_id"].as_str().expect("payload id"))
        })
        .expect("extra full payload")
        .clone();
    let extra_object_id = extra_payload["object_id"]
        .as_str()
        .expect("extra object id");
    let extra_object = full_manifest["native_objects"]
        .as_array()
        .expect("full objects")
        .iter()
        .find(|object| object["object_id"].as_str() == Some(extra_object_id))
        .expect("extra object")
        .clone();
    let mut tampered = manifest.clone();
    tampered["native_payloads"]
        .as_array_mut()
        .expect("tampered payloads")
        .push(extra_payload);
    tampered["native_objects"]
        .as_array_mut()
        .expect("tampered objects")
        .push(extra_object);
    let tampered_path = repo.path().join("target/projected-tampered.json");
    std::fs::write(
        &tampered_path,
        serde_json::to_vec_pretty(&tampered).expect("tampered json"),
    )
    .expect("write tampered manifest");
    repo.forge()
        .args([
            "--json",
            "sync",
            "inspect",
            tampered_path.to_str().expect("utf8 tampered path"),
        ])
        .assert()
        .failure();
}

#[test]
fn sync_projected_export_does_not_leak_private_proposal_snapshot() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    repo.forge()
        .args([
            "--json",
            "start",
            "public attempt private proposal",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(
        repo.path().join("proposal-private.txt"),
        "proposal secret\n",
    )
    .expect("write private proposal file");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed = json(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_id = proposed["data"]["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
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
            "private",
        ])
        .assert()
        .success();

    let manifest_path = repo.path().join("target/private-proposal-projected.json");
    repo.forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--recipient",
            "alice@example.test",
        ])
        .assert()
        .success();
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read projected manifest"))
            .expect("projected manifest json");

    let proposal_ids: Vec<&str> = manifest_table_rows(&manifest, "proposals")
        .iter()
        .map(|row| row["id"].as_str().expect("proposal id"))
        .collect();
    assert!(!proposal_ids.contains(&proposal_id.as_str()));
    let payload_texts = decoded_native_payloads(&manifest);
    assert!(
        !payload_texts
            .iter()
            .any(|payload| payload.contains("proposal-private.txt")
                || payload.contains("proposal secret")),
        "private proposal snapshot payloads must not leak"
    );
}

#[test]
fn sync_projected_import_and_clone_materialize_granted_projection() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    repo.forge()
        .args([
            "--json",
            "start",
            "granted projected sync",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("granted-projection.txt"), "draft\n")
        .expect("write granted projection draft file");
    repo.forge().args(["--json", "save"]).assert().success();
    std::fs::write(repo.path().join("granted-projection.txt"), "allowed\n")
        .expect("write granted projection final file");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed = json(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_id = proposed["data"]["proposal_id"]
        .as_str()
        .expect("proposal id")
        .to_string();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();
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
            "private",
        ])
        .assert()
        .success();
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
            "release-auditor@example.test",
            "--capability",
            "sync_materialize",
        ])
        .assert()
        .success();

    let bundle_path = repo.path().join("target/granted-projected-sync.json");
    let exported = json(
        repo.forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                bundle_path.to_str().expect("utf8 bundle path"),
                "--recipient",
                "release-auditor@example.test",
            ])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["projection"]["mode"], "recipient");
    assert_eq!(exported["data"]["projection"]["projected"], true);
    assert!(
        exported["data"]["native_head"].is_string(),
        "granted materialization projection should carry a native head"
    );

    let target = TestRepo::new_git();
    target
        .forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let imported = json(
        target
            .forge()
            .args([
                "--json",
                "sync",
                "import",
                "--materialize",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(imported["data"]["projection"]["mode"], "recipient");
    assert_eq!(imported["data"]["projection"]["projected"], true);
    assert_eq!(imported["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(target.path().join("granted-projection.txt"))
            .expect("read projected import file"),
        "allowed\n"
    );
    let import_doctor = json(target.forge().args(["--json", "doctor"]).assert().success());
    assert_no_structural_doctor_findings(&import_doctor, "granted projected import");

    let clone = tempfile::tempdir().expect("projected clone dir");
    let cloned = json(
        forge_in(clone.path())
            .args([
                "--json",
                "sync",
                "clone",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(cloned["data"]["projection"]["mode"], "recipient");
    assert_eq!(cloned["data"]["projection"]["projected"], true);
    assert_eq!(cloned["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(clone.path().join("granted-projection.txt"))
            .expect("read projected clone file"),
        "allowed\n"
    );
    let clone_doctor = json(
        forge_in(clone.path())
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_no_structural_doctor_findings(&clone_doctor, "granted projected clone");
}

#[test]
fn sync_import_applies_native_bundle_into_fresh_native_repo() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-bundle.json");

    let exported = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );

    let plain_target = TestRepo::new_git();
    plain_target
        .forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let imported = json(
        plain_target
            .forge()
            .args([
                "--json",
                "sync",
                "import",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(imported["data"]["protocol_version"], "forge-sync.v1");
    assert_eq!(imported["data"]["content_backend"], "native");
    assert_eq!(
        imported["data"]["native_head"],
        exported["data"]["native_head"]
    );
    assert_eq!(
        imported["data"]["imported_native_objects"],
        exported["data"]["native_payload_count"]
    );
    assert_eq!(
        imported["data"]["local_key_fingerprint"],
        exported["data"]["local_key_fingerprint"]
    );
    assert_eq!(imported["data"]["materialized"], false);
    assert!(
        !plain_target.path().join("sync.txt").exists(),
        "plain sync import must not rewrite the worktree"
    );

    let target = TestRepo::new_git();
    target
        .forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let materialized = json(
        target
            .forge()
            .args([
                "--json",
                "sync",
                "import",
                "--materialize",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        materialized["data"]["native_head"],
        exported["data"]["native_head"]
    );
    assert_eq!(materialized["data"]["materialized"], true);
    assert!(materialized["data"]["materialized_content_ref"]
        .as_str()
        .unwrap()
        .starts_with("forge-tree:"));
    assert!(materialized["data"]["materialized_operation_id"]
        .as_str()
        .unwrap()
        .starts_with("op_"));
    assert_eq!(
        std::fs::read_to_string(target.path().join("sync.txt")).expect("materialized sync file"),
        "sync\n"
    );

    target.forge().args(["--json", "doctor"]).assert().success();

    let reexport_dir = tempfile::tempdir().expect("reexport temp dir");
    let reexport_path = reexport_dir.path().join("reexported-sync-bundle.json");
    let reexported = json(
        target
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                reexport_path.to_str().expect("utf8 reexport path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        reexported["data"]["native_head"],
        exported["data"]["native_head"]
    );
    assert_eq!(
        reexported["data"]["native_object_count"],
        exported["data"]["native_object_count"]
    );
    assert_eq!(
        reexported["data"]["local_key_fingerprint"],
        exported["data"]["local_key_fingerprint"]
    );

    let imported_again = json(
        target
            .forge()
            .args([
                "--json",
                "sync",
                "import",
                "--materialize",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        imported_again["data"]["native_head"],
        exported["data"]["native_head"]
    );
    assert_eq!(imported_again["data"]["materialized"], true);
}

#[test]
fn sync_clone_bootstraps_empty_directory_without_extra_native_objects() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clone-bundle.json");

    let exported = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(&bundle_path).expect("read bundle"))
            .expect("bundle json");

    let clone_dir = tempfile::tempdir().expect("clone target dir");
    let cloned = json(
        forge_in(clone_dir.path())
            .args([
                "--json",
                "sync",
                "clone",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(cloned["data"]["protocol_version"], "forge-sync.v1");
    assert_eq!(cloned["data"]["repository_id"], manifest["repo_id"]);
    assert_eq!(
        cloned["data"]["native_head"],
        exported["data"]["native_head"]
    );
    assert_eq!(
        cloned["data"]["imported_native_objects"],
        exported["data"]["native_payload_count"]
    );
    assert_eq!(
        cloned["data"]["imported_ledger_rows"],
        exported["data"]["ledger_row_count"]
    );
    assert_eq!(cloned["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(clone_dir.path().join("sync.txt"))
            .expect("cloned worktree materialized"),
        "sync\n"
    );
    forge_in(clone_dir.path())
        .args(["--json", "doctor"])
        .assert()
        .success();

    let reexport_dir = tempfile::tempdir().expect("clone reexport temp dir");
    let reexport_path = reexport_dir.path().join("clone-reexport.json");
    let reexported = json(
        forge_in(clone_dir.path())
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                reexport_path.to_str().expect("utf8 reexport path"),
            ])
            .assert()
            .success(),
    );
    let reexported_manifest: Value =
        serde_json::from_slice(&std::fs::read(&reexport_path).expect("read clone reexport"))
            .expect("clone reexport json");
    assert_eq!(reexported_manifest["repo_id"], manifest["repo_id"]);
    assert_eq!(
        reexported["data"]["ledger_row_count"],
        exported["data"]["ledger_row_count"]
    );
    assert_eq!(
        reexported["data"]["native_head"],
        exported["data"]["native_head"]
    );
    assert_eq!(
        reexported["data"]["native_object_count"], exported["data"]["native_object_count"],
        "fresh sync clone must not mint target-only native genesis objects"
    );
    let mut source_objects: Vec<_> = manifest["native_objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|object| object["object_id"].as_str().unwrap().to_string())
        .collect();
    let mut cloned_objects: Vec<_> = reexported_manifest["native_objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|object| object["object_id"].as_str().unwrap().to_string())
        .collect();
    source_objects.sort();
    cloned_objects.sort();
    assert_eq!(
        cloned_objects, source_objects,
        "fresh sync clone must have the exact source native object ids"
    );
    assert_eq!(
        reexported["data"]["local_key_fingerprint"],
        exported["data"]["local_key_fingerprint"]
    );

    let non_empty = tempfile::tempdir().expect("non-empty target dir");
    std::fs::write(non_empty.path().join("README.md"), "occupied\n").expect("occupy target");
    let refused = json(
        forge_in(non_empty.path())
            .args([
                "--json",
                "sync",
                "clone",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused["command"], "sync clone");
    assert_eq!(refused["status"], "error");
}

#[test]
fn sync_clone_labels_imported_signatures_as_peer_not_local_trust() {
    let source = TestRepo::new_git();
    native_checked_proposal(&source);
    let bundle_dir = tempfile::tempdir().expect("peer trust bundle dir");
    let bundle_path = bundle_dir.path().join("peer-trust.json");
    let exported = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    let source_fingerprint = exported["data"]["local_key_fingerprint"]
        .as_str()
        .expect("source signing fingerprint")
        .to_string();

    let clone_dir = tempfile::tempdir().expect("peer trust clone");
    forge_in(clone_dir.path())
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 bundle path"),
        ])
        .assert()
        .success();

    let doctor = json(
        forge_in(clone_dir.path())
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_eq!(
        doctor["data"]["signature_key_summary"]["peer_key_fingerprints"][0],
        source_fingerprint
    );
    assert!(
        doctor["data"]["signature_key_summary"]["local_key_fingerprints"]
            .as_array()
            .expect("local fingerprints")
            .is_empty()
    );

    let target_key = json(
        forge_in(clone_dir.path())
            .args(["--json", "key", "status"])
            .assert()
            .success(),
    );
    assert_ne!(target_key["data"]["key_fingerprint"], source_fingerprint);
    assert_eq!(target_key["data"]["local_key_count"], 1);
    assert_eq!(target_key["data"]["peer_key_count"], 1);

    forge_in(clone_dir.path())
        .args(["--json", "trust", "policy", "--accept", "locally_signed"])
        .assert()
        .success();
    let blocked = json(
        forge_in(clone_dir.path())
            .args(["--json", "accept"])
            .assert()
            .failure(),
    );
    assert_eq!(blocked["errors"][0]["code"], "TRUST_POLICY_UNMET");
    assert_eq!(blocked["errors"][0]["details"]["action"], "accept");
    let issues = blocked["errors"][0]["details"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(
        issues.iter().any(|issue| {
            issue["kind"] == "missing_signature" && issue["subject_kind"] == "evidence"
        }),
        "peer signatures must not satisfy local-only trust: {blocked}"
    );
}

#[test]
fn sync_import_rejects_signature_row_with_spoofed_local_fingerprint() {
    let source = TestRepo::new_git();
    native_checked_proposal(&source);
    let bundle_dir = tempfile::tempdir().expect("spoof bundle dir");
    let bundle_path = bundle_dir.path().join("spoofed-fingerprint.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 spoof bundle path"),
        ])
        .assert()
        .success();

    let target = TestRepo::new_git();
    target
        .forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let target_key = json(
        target
            .forge()
            .args(["--json", "key", "status"])
            .assert()
            .success(),
    );
    let target_fingerprint = target_key["data"]["key_fingerprint"]
        .as_str()
        .expect("target local fingerprint")
        .to_string();

    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&bundle_path).expect("read bundle"))
            .expect("bundle json");
    let signature_rows = manifest["ledger_rows"]
        .as_array_mut()
        .expect("ledger tables")
        .iter_mut()
        .find(|table| table["table"] == "ledger_signatures")
        .expect("ledger signatures table")["rows"]
        .as_array_mut()
        .expect("signature rows");
    assert!(
        !signature_rows.is_empty(),
        "source export must include signature rows"
    );
    for row in signature_rows {
        row["key_fingerprint"] = Value::String(target_fingerprint.clone());
    }
    std::fs::write(
        &bundle_path,
        serde_json::to_vec_pretty(&manifest).expect("encode spoofed bundle"),
    )
    .expect("write spoofed bundle");

    let imported = json(
        target
            .forge()
            .args([
                "--json",
                "sync",
                "import",
                bundle_path.to_str().expect("utf8 spoofed bundle path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(imported["status"], "error");
    assert_eq!(imported["errors"][0]["code"], "COMMAND_FAILED");
}

#[test]
fn sync_export_since_emits_delta_that_updates_a_cloned_repo() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let full_bundle_path = source.path().join("target/forge-sync-full.json");

    let initial_export = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                full_bundle_path.to_str().expect("utf8 full bundle path"),
            ])
            .assert()
            .success(),
    );

    let clone_dir = tempfile::tempdir().expect("clone target dir");
    json(
        forge_in(clone_dir.path())
            .args([
                "--json",
                "sync",
                "clone",
                full_bundle_path.to_str().expect("utf8 full bundle path"),
            ])
            .assert()
            .success(),
    );

    native_accept_file_change(
        &source,
        "incremental sync lifecycle",
        "sync-next.txt",
        "next\n",
    );
    let source_after_path = source.path().join("target/forge-sync-after.json");
    let source_after = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                source_after_path.to_str().expect("utf8 source after path"),
            ])
            .assert()
            .success(),
    );
    let delta_path = source.path().join("target/forge-sync-delta.json");
    let delta = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--since",
                full_bundle_path.to_str().expect("utf8 full bundle path"),
                "--output",
                delta_path.to_str().expect("utf8 delta path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(delta["data"]["incremental"], true);
    assert_eq!(
        delta["data"]["native_head"],
        source_after["data"]["native_head"]
    );
    assert!(
        delta["data"]["native_payload_count"].as_u64().unwrap()
            < source_after["data"]["native_payload_count"]
                .as_u64()
                .unwrap(),
        "delta bundle should omit objects already advertised by the base bundle"
    );
    assert!(
        delta["data"]["ledger_row_count"].as_u64().unwrap()
            < source_after["data"]["ledger_row_count"].as_u64().unwrap(),
        "delta bundle should omit ledger rows already advertised by the base bundle"
    );

    let materialized = json(
        forge_in(clone_dir.path())
            .args([
                "--json",
                "sync",
                "import",
                "--materialize",
                delta_path.to_str().expect("utf8 delta path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        materialized["data"]["native_head"],
        source_after["data"]["native_head"]
    );
    assert_eq!(materialized["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(clone_dir.path().join("sync-next.txt"))
            .expect("delta materialized next file"),
        "next\n"
    );

    let clone_after_dir = tempfile::tempdir().expect("clone after dir");
    let clone_after_path = clone_after_dir.path().join("clone-after.json");
    let clone_after = json(
        forge_in(clone_dir.path())
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                clone_after_path.to_str().expect("utf8 clone after path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        clone_after["data"]["native_head"],
        source_after["data"]["native_head"]
    );
    assert_eq!(
        clone_after["data"]["native_object_count"],
        source_after["data"]["native_object_count"]
    );

    let source_manifest: Value =
        serde_json::from_slice(&std::fs::read(&source_after_path).expect("read source after"))
            .expect("source after json");
    let clone_manifest: Value =
        serde_json::from_slice(&std::fs::read(&clone_after_path).expect("read clone after"))
            .expect("clone after json");
    let mut source_objects: Vec<_> = source_manifest["native_objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|object| object["object_id"].as_str().unwrap().to_string())
        .collect();
    let mut clone_objects: Vec<_> = clone_manifest["native_objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|object| object["object_id"].as_str().unwrap().to_string())
        .collect();
    source_objects.sort();
    clone_objects.sort();
    assert_eq!(clone_objects, source_objects);
    assert_ne!(
        source_after["data"]["native_head"],
        initial_export["data"]["native_head"]
    );
}

#[test]
fn sync_fetch_pull_and_push_between_local_peer_repos() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-peer-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 peer base path"),
        ])
        .assert()
        .success();

    let peer_dir = tempfile::tempdir().expect("peer dir");
    forge_in(peer_dir.path())
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 peer base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "peer fetch change", "from-source.txt", "source\n");
    let source_after_fetch = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                source
                    .path()
                    .join("target/source-after-fetch.json")
                    .to_str()
                    .expect("utf8 source after fetch path"),
            ])
            .assert()
            .success(),
    );

    let fetched = json(
        forge_in(peer_dir.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(fetched["data"]["direction"], "fetch");
    assert_eq!(fetched["data"]["materialized"], false);
    assert_eq!(
        fetched["data"]["remote_native_head"],
        source_after_fetch["data"]["native_head"]
    );
    assert!(
        !peer_dir.path().join("from-source.txt").exists(),
        "fetch must not materialize the peer worktree"
    );

    let pulled = json(
        forge_in(peer_dir.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pulled["data"]["direction"], "pull");
    assert_eq!(pulled["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(peer_dir.path().join("from-source.txt"))
            .expect("pulled file materialized"),
        "source\n"
    );

    native_accept_file_change_in(
        peer_dir.path(),
        "peer push change",
        "from-peer.txt",
        "peer\n",
    );
    let peer_after_push = json(
        forge_in(peer_dir.path())
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                peer_dir
                    .path()
                    .join("peer-after-push.json")
                    .to_str()
                    .expect("utf8 peer after push path"),
            ])
            .assert()
            .success(),
    );

    let pushed = json(
        forge_in(peer_dir.path())
            .args([
                "--json",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pushed["data"]["direction"], "push");
    assert_eq!(pushed["data"]["materialized"], false);
    assert_eq!(
        pushed["data"]["local_native_head"],
        peer_after_push["data"]["native_head"]
    );

    let source_after_push_path = source.path().join("target/source-after-push.json");
    let source_after_push = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                source_after_push_path
                    .to_str()
                    .expect("utf8 source after push path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        source_after_push["data"]["native_head"],
        peer_after_push["data"]["native_head"]
    );
    assert!(
        !source.path().join("from-peer.txt").exists(),
        "push applies native state to the peer repo without materializing its worktree"
    );
}

#[test]
fn sync_fetch_fast_forward_request_id_replays_locally() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-fetch-ff-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 fetch ff base path"),
        ])
        .assert()
        .success();

    let fetch_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change(&source, "fetch ff one", "one.txt", "one\n");
    let first = json(
        forge_in(fetch_peer.path())
            .args([
                "--json",
                "--request-id",
                "fetch-fast-forward",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["direction"], "fetch");
    assert_eq!(first["data"]["materialized"], false);
    assert!(first["operation_id"].as_str().is_some());
    assert_eq!(
        operation_count_for_request_id(fetch_peer.path(), "fetch-fast-forward"),
        1,
        "fast-forward fetch must claim the request-id in the initiating repo"
    );
    let peer_after_first = export_native_head(fetch_peer.path(), "fetch-ff-after-first.json");
    assert_eq!(
        peer_after_first["data"]["native_head"], first["data"]["remote_native_head"],
        "fast-forward fetch should advance native history without materializing"
    );

    native_accept_file_change(&source, "fetch ff two", "two.txt", "two\n");
    let source_after_second =
        export_native_head(source.path(), "source-fetch-ff-after-second.json");
    assert_ne!(
        source_after_second["data"]["native_head"], first["data"]["remote_native_head"],
        "remote must advance so replay would be observable if it re-executed"
    );
    let replay = json(
        forge_in(fetch_peer.path())
            .args([
                "--json",
                "--request-id",
                "fetch-fast-forward",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(
        replay["data"]["remote_native_head"], first["data"]["remote_native_head"],
        "fast-forward fetch replay must preserve the first response"
    );
    let peer_after_replay = export_native_head(fetch_peer.path(), "fetch-ff-after-replay.json");
    assert_eq!(
        peer_after_replay["data"]["native_head"], first["data"]["remote_native_head"],
        "request-id replay must not import a newer remote head"
    );
    assert!(
        !fetch_peer.path().join("one.txt").exists(),
        "fetch replay must not materialize fetched content"
    );
    assert!(
        !fetch_peer.path().join("two.txt").exists(),
        "fetch replay must not materialize or import the later remote worktree"
    );
}

#[test]
fn sync_peer_commands_accept_file_url_remotes() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-file-url-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 file url base path"),
        ])
        .assert()
        .success();
    let source_url = file_url_for_test(source.path());

    native_accept_file_change(
        &source,
        "file url fetch source",
        "source-ff.txt",
        "source\n",
    );
    let fetch_peer = cloned_peer_from_bundle(&bundle_path);
    let fetched = json(
        forge_in(fetch_peer.path())
            .args(["--json", "sync", "fetch", &source_url])
            .assert()
            .success(),
    );
    assert_eq!(fetched["data"]["direction"], "fetch");
    assert_eq!(fetched["data"]["materialized"], false);
    assert_eq!(
        export_native_head(fetch_peer.path(), "file-url-fetch-head.json")["data"]["native_head"],
        fetched["data"]["remote_native_head"],
        "file-url fetch should advance native history"
    );

    let clean_source = TestRepo::new_git();
    native_accepted_lifecycle(&clean_source);
    let clean_bundle_path = clean_source
        .path()
        .join("target/forge-sync-file-url-clean-base.json");
    clean_source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            clean_bundle_path.to_str().expect("utf8 clean base path"),
        ])
        .assert()
        .success();
    let clean_source_url = file_url_for_test(clean_source.path());
    native_accept_file_change(
        &clean_source,
        "file url pull source",
        "source-only.txt",
        "source\n",
    );

    let pull_peer = cloned_peer_from_bundle(&clean_bundle_path);
    native_accept_file_change_in(
        pull_peer.path(),
        "file url pull peer",
        "peer-only.txt",
        "peer\n",
    );
    let pulled = json(
        forge_in(pull_peer.path())
            .args(["--json", "sync", "pull", &clean_source_url])
            .assert()
            .success(),
    );
    assert_eq!(pulled["data"]["merged"], true);
    assert_eq!(pulled["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(pull_peer.path().join("source-only.txt"))
            .expect("file-url pull source content"),
        "source\n"
    );
    assert_eq!(
        std::fs::read_to_string(pull_peer.path().join("peer-only.txt"))
            .expect("file-url pull peer content"),
        "peer\n"
    );

    let push_peer = cloned_peer_from_bundle(&clean_bundle_path);
    native_accept_file_change_in(
        push_peer.path(),
        "file url push peer",
        "pushed-only.txt",
        "peer\n",
    );
    let pushed = json(
        forge_in(push_peer.path())
            .args(["--json", "sync", "push", &clean_source_url])
            .assert()
            .success(),
    );
    assert_eq!(pushed["data"]["direction"], "push");
    assert_eq!(pushed["data"]["merged"], true);
    assert_eq!(pushed["data"]["materialized"], false);
    assert_eq!(
        export_native_head(clean_source.path(), "file-url-push-head.json")["data"]["native_head"],
        pushed["data"]["merge_commit_id"],
        "file-url push should advance the remote native head to the merge commit"
    );
    assert!(
        !clean_source.path().join("pushed-only.txt").exists(),
        "file-url push should not materialize the remote worktree"
    );
}

#[test]
fn sync_peer_commands_fetch_and_pull_over_fake_ssh() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-ssh-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 ssh base path"),
        ])
        .assert()
        .success();
    native_accept_file_change(&source, "ssh source change", "ssh-source.txt", "source\n");

    let fake_ssh = fake_ssh_command();
    let fake_ssh_path = fake_ssh.path().join("ssh");
    let forge_bin = assert_cmd::cargo::cargo_bin("forge");
    let source_url = ssh_url_for_test(source.path());

    let fetch_peer = cloned_peer_from_bundle(&bundle_path);
    let fetched = json(
        forge_in(fetch_peer.path())
            .env("FORGE_SYNC_SSH_COMMAND", &fake_ssh_path)
            .env("FORGE_SYNC_REMOTE_FORGE", &forge_bin)
            .args(["--json", "sync", "fetch", &source_url])
            .assert()
            .success(),
    );
    assert_eq!(fetched["data"]["direction"], "fetch");
    assert_eq!(fetched["data"]["remote_path"], source_url);
    assert_eq!(fetched["data"]["materialized"], false);
    assert!(
        !fetch_peer.path().join("ssh-source.txt").exists(),
        "ssh fetch must not materialize the fetched tree"
    );
    assert_eq!(
        export_native_head(fetch_peer.path(), "ssh-fetch-head.json")["data"]["native_head"],
        fetched["data"]["remote_native_head"],
        "ssh fetch should advance the local native head"
    );

    let pull_peer = cloned_peer_from_bundle(&bundle_path);
    let pulled = json(
        forge_in(pull_peer.path())
            .env("FORGE_SYNC_SSH_COMMAND", &fake_ssh_path)
            .env("FORGE_SYNC_REMOTE_FORGE", &forge_bin)
            .args(["--json", "sync", "pull", &source_url])
            .assert()
            .success(),
    );
    assert_eq!(pulled["data"]["direction"], "pull");
    assert_eq!(pulled["data"]["remote_path"], source_url);
    assert_eq!(pulled["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(pull_peer.path().join("ssh-source.txt"))
            .expect("ssh pull source content"),
        "source\n"
    );
}

#[test]
fn sync_push_over_fake_ssh_fast_forwards_and_merges_clean_divergence() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-ssh-push-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 ssh push base path"),
        ])
        .assert()
        .success();

    let fake_ssh = fake_ssh_command();
    let fake_ssh_path = fake_ssh.path().join("ssh");
    let forge_bin = assert_cmd::cargo::cargo_bin("forge");
    let source_url = ssh_url_for_test(source.path());

    let fast_forward_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        fast_forward_peer.path(),
        "ssh push fast forward",
        "ssh-pushed.txt",
        "peer\n",
    );
    let pushed = json(
        forge_in(fast_forward_peer.path())
            .env("FORGE_SYNC_SSH_COMMAND", &fake_ssh_path)
            .env("FORGE_SYNC_REMOTE_FORGE", &forge_bin)
            .args(["--json", "sync", "push", &source_url])
            .assert()
            .success(),
    );
    assert_eq!(pushed["data"]["direction"], "push");
    assert_eq!(pushed["data"]["remote_path"], source_url);
    assert_eq!(pushed["data"]["materialized"], false);
    assert_eq!(
        export_native_head(source.path(), "ssh-push-fast-forward-head.json")["data"]["native_head"],
        pushed["data"]["local_native_head"],
        "ssh push should advance the remote native head on fast-forward"
    );
    assert!(
        !source.path().join("ssh-pushed.txt").exists(),
        "ssh push fast-forward must not materialize the remote worktree"
    );

    let clean_source = TestRepo::new_git();
    native_accepted_lifecycle(&clean_source);
    let clean_bundle_path = clean_source
        .path()
        .join("target/forge-sync-ssh-push-clean-base.json");
    clean_source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            clean_bundle_path
                .to_str()
                .expect("utf8 ssh clean push base path"),
        ])
        .assert()
        .success();
    native_accept_file_change(
        &clean_source,
        "ssh push remote side",
        "remote-only.txt",
        "remote\n",
    );
    let clean_source_url = ssh_url_for_test(clean_source.path());
    let clean_peer = cloned_peer_from_bundle(&clean_bundle_path);
    native_accept_file_change_in(
        clean_peer.path(),
        "ssh push peer side",
        "peer-only.txt",
        "peer\n",
    );

    let clean_pushed = json(
        forge_in(clean_peer.path())
            .env("FORGE_SYNC_SSH_COMMAND", &fake_ssh_path)
            .env("FORGE_SYNC_REMOTE_FORGE", &forge_bin)
            .args([
                "--json",
                "--request-id",
                "ssh-clean-push",
                "sync",
                "push",
                &clean_source_url,
            ])
            .assert()
            .success(),
    );
    assert_eq!(clean_pushed["data"]["direction"], "push");
    assert_eq!(clean_pushed["data"]["remote_path"], clean_source_url);
    assert_eq!(clean_pushed["data"]["merged"], true);
    assert_eq!(clean_pushed["data"]["materialized"], false);
    assert!(clean_pushed["data"]["remote_operation_id"].is_string());
    assert_eq!(
        export_native_head(clean_source.path(), "ssh-push-clean-head.json")["data"]["native_head"],
        clean_pushed["data"]["merge_commit_id"],
        "ssh clean push should advance the remote native head to the merge commit"
    );
    assert!(
        !clean_source.path().join("peer-only.txt").exists(),
        "ssh clean push must not materialize the peer-side file remotely"
    );

    let replayed = json(
        forge_in(clean_peer.path())
            .env("FORGE_SYNC_SSH_COMMAND", &fake_ssh_path)
            .env("FORGE_SYNC_REMOTE_FORGE", &forge_bin)
            .args([
                "--json",
                "--request-id",
                "ssh-clean-push",
                "sync",
                "push",
                &clean_source_url,
            ])
            .assert()
            .success(),
    );
    assert_eq!(replayed["operation_id"], clean_pushed["operation_id"]);
    assert_eq!(replayed["data"]["idempotent_replay"], true);
    assert_eq!(
        replayed["data"]["merge_commit_id"],
        clean_pushed["data"]["merge_commit_id"]
    );
}

#[test]
fn sync_push_over_fake_ssh_records_remote_conflicts_as_data() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-ssh-push-conflict-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 ssh conflict push base path"),
        ])
        .assert()
        .success();
    native_accept_file_change(&source, "ssh conflict remote", "side.txt", "remote\n");
    let source_before = export_native_head(source.path(), "ssh-push-conflict-before.json");

    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "ssh conflict peer", "side.txt", "peer\n");
    let fake_ssh = fake_ssh_command();
    let fake_ssh_path = fake_ssh.path().join("ssh");
    let forge_bin = assert_cmd::cargo::cargo_bin("forge");
    let source_url = ssh_url_for_test(source.path());

    let pushed = json(
        forge_in(peer.path())
            .env("FORGE_SYNC_SSH_COMMAND", &fake_ssh_path)
            .env("FORGE_SYNC_REMOTE_FORGE", &forge_bin)
            .args(["--json", "sync", "push", &source_url])
            .assert()
            .success(),
    );
    assert_eq!(pushed["data"]["direction"], "push");
    assert_eq!(pushed["data"]["remote_path"], source_url);
    assert_eq!(pushed["data"]["merged"], false);
    assert_eq!(pushed["data"]["materialized"], false);
    assert!(pushed["data"]["conflict_set_id"].is_string());
    assert_eq!(
        export_native_head(source.path(), "ssh-push-conflict-after.json")["data"]["native_head"],
        source_before["data"]["native_head"],
        "conflicting ssh push must not advance the remote native head"
    );
    assert_single_native_sync_conflict(source.path(), "sync_push_divergence");
}

#[test]
fn sync_peer_commands_round_trip_over_http_transport() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-http-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 http base path"),
        ])
        .assert()
        .success();
    native_accept_file_change(&source, "http source change", "http-source.txt", "source\n");
    let source_server = TestHttpSyncServer::start(source.path(), 4);

    let fetch_peer = cloned_peer_from_bundle(&bundle_path);
    let fetched = json(
        forge_in(fetch_peer.path())
            .env("FORGE_SYNC_ALLOW_INSECURE_HTTP", "1")
            .args(["--json", "sync", "fetch", &source_server.url])
            .assert()
            .success(),
    );
    assert_eq!(fetched["data"]["direction"], "fetch");
    assert_eq!(fetched["data"]["remote_path"], source_server.url);
    assert_eq!(fetched["data"]["materialized"], false);
    assert!(
        !fetch_peer.path().join("http-source.txt").exists(),
        "http fetch must not materialize the fetched tree"
    );

    let pull_peer = cloned_peer_from_bundle(&bundle_path);
    let pulled = json(
        forge_in(pull_peer.path())
            .env("FORGE_SYNC_ALLOW_INSECURE_HTTP", "1")
            .args(["--json", "sync", "pull", &source_server.url])
            .assert()
            .success(),
    );
    assert_eq!(pulled["data"]["direction"], "pull");
    assert_eq!(pulled["data"]["remote_path"], source_server.url);
    assert_eq!(pulled["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(pull_peer.path().join("http-source.txt"))
            .expect("http pull source content"),
        "source\n"
    );

    let push_source = TestRepo::new_git();
    native_accepted_lifecycle(&push_source);
    let push_bundle_path = push_source
        .path()
        .join("target/forge-sync-http-push-base.json");
    push_source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            push_bundle_path.to_str().expect("utf8 http push base path"),
        ])
        .assert()
        .success();
    let push_server = TestHttpSyncServer::start(push_source.path(), 2);
    let push_peer = cloned_peer_from_bundle(&push_bundle_path);
    native_accept_file_change_in(
        push_peer.path(),
        "http push fast forward",
        "http-pushed.txt",
        "peer\n",
    );
    let pushed = json(
        forge_in(push_peer.path())
            .env("FORGE_SYNC_ALLOW_INSECURE_HTTP", "1")
            .args(["--json", "sync", "push", &push_server.url])
            .assert()
            .success(),
    );
    assert_eq!(pushed["data"]["direction"], "push");
    assert_eq!(pushed["data"]["remote_path"], push_server.url);
    assert_eq!(pushed["data"]["materialized"], false);
    assert_eq!(
        export_native_head(push_source.path(), "http-push-head.json")["data"]["native_head"],
        pushed["data"]["local_native_head"],
        "http push should advance the remote native head"
    );
    assert!(
        !push_source.path().join("http-pushed.txt").exists(),
        "http push must not materialize the remote worktree"
    );

    let conflict_source = TestRepo::new_git();
    native_accepted_lifecycle(&conflict_source);
    let conflict_bundle_path = conflict_source
        .path()
        .join("target/forge-sync-http-conflict-base.json");
    conflict_source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            conflict_bundle_path
                .to_str()
                .expect("utf8 http conflict base path"),
        ])
        .assert()
        .success();
    native_accept_file_change(
        &conflict_source,
        "http conflict remote",
        "side.txt",
        "remote\n",
    );
    let conflict_before =
        export_native_head(conflict_source.path(), "http-push-conflict-before.json");
    let conflict_server = TestHttpSyncServer::start(conflict_source.path(), 2);
    let conflict_peer = cloned_peer_from_bundle(&conflict_bundle_path);
    native_accept_file_change_in(
        conflict_peer.path(),
        "http conflict peer",
        "side.txt",
        "peer\n",
    );
    let conflicted = json(
        forge_in(conflict_peer.path())
            .env("FORGE_SYNC_ALLOW_INSECURE_HTTP", "1")
            .args(["--json", "sync", "push", &conflict_server.url])
            .assert()
            .success(),
    );
    assert_eq!(conflicted["data"]["direction"], "push");
    assert_eq!(conflicted["data"]["remote_path"], conflict_server.url);
    assert_eq!(conflicted["data"]["merged"], false);
    assert!(conflicted["data"]["conflict_set_id"].is_string());
    assert_eq!(
        export_native_head(conflict_source.path(), "http-push-conflict-after.json")["data"]
            ["native_head"],
        conflict_before["data"]["native_head"],
        "conflicting http push must not advance remote native head"
    );
    assert_single_native_sync_conflict(conflict_source.path(), "sync_push_divergence");
}

#[test]
fn sync_peer_commands_reject_unsupported_remote_scheme() {
    let repo = TestRepo::new_git();
    native_accepted_lifecycle(&repo);

    let failed = json(
        repo.forge()
            .args(["--json", "sync", "fetch", "ftp://example.test/repo"])
            .assert()
            .failure(),
    );
    assert_eq!(failed["status"], "error");
    assert_eq!(failed["errors"][0]["code"], "COMMAND_FAILED");
    assert!(
        failed["errors"][0]["message"]
            .as_str()
            .expect("error message")
            .contains("unsupported sync remote scheme ftp"),
        "unsupported scheme should fail clearly"
    );
}

#[test]
fn sync_peer_commands_record_divergent_native_heads_as_conflicts() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-diverge-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 diverge base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source side", "side.txt", "source\n");
    let source_before_refused_push =
        export_native_head(source.path(), "target/source-before-push.json");

    let fetch_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(fetch_peer.path(), "fetch peer side", "side.txt", "peer\n");
    let fetch_peer_before = export_native_head(fetch_peer.path(), "fetch-peer-before.json");
    let conflicted_fetch = json(
        forge_in(fetch_peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(conflicted_fetch["command"], "sync fetch");
    assert_eq!(conflicted_fetch["status"], "success");
    assert_eq!(conflicted_fetch["data"]["merged"], false);
    assert_single_native_sync_conflict(fetch_peer.path(), "sync_fetch_divergence");
    let fetch_conflict_refs =
        single_native_sync_conflict_content_refs(fetch_peer.path(), "sync_fetch_divergence");
    assert_gc_keeps_content_refs_reachable(fetch_peer.path(), &fetch_conflict_refs);
    let fetch_peer_after = export_native_head(fetch_peer.path(), "fetch-peer-after.json");
    assert_eq!(
        fetch_peer_after["data"]["native_head"], fetch_peer_before["data"]["native_head"],
        "refused fetch must not advance the local native head"
    );
    assert_eq!(
        std::fs::read_to_string(fetch_peer.path().join("side.txt"))
            .expect("fetch peer worktree side"),
        "peer\n",
        "refused fetch must not materialize over local worktree content"
    );

    let pull_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(pull_peer.path(), "pull peer side", "side.txt", "peer\n");
    let pull_peer_before = checkout_current_native_head(pull_peer.path(), "pull-peer-before.json");
    let conflicted_pull = json(
        forge_in(pull_peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(conflicted_pull["command"], "sync pull");
    assert_eq!(conflicted_pull["status"], "success");
    assert_eq!(conflicted_pull["data"]["merged"], false);
    assert_single_native_sync_conflict(pull_peer.path(), "sync_pull_divergence");
    let pull_peer_after = export_native_head(pull_peer.path(), "pull-peer-after.json");
    assert_eq!(
        pull_peer_after["data"]["native_head"], pull_peer_before["data"]["native_head"],
        "refused pull must not advance the local native head"
    );
    assert_eq!(
        std::fs::read_to_string(pull_peer.path().join("side.txt"))
            .expect("pull peer worktree side"),
        "peer\n",
        "refused pull must not materialize over local worktree content"
    );

    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push peer side", "side.txt", "peer\n");
    let conflicted_push = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(conflicted_push["command"], "sync push");
    assert_eq!(conflicted_push["status"], "success");
    assert_eq!(conflicted_push["data"]["merged"], false);
    assert_single_native_sync_conflict(source.path(), "sync_push_divergence");
    let source_after_refused_push =
        export_native_head(source.path(), "target/source-after-push.json");
    assert_eq!(
        source_after_refused_push["data"]["native_head"],
        source_before_refused_push["data"]["native_head"],
        "refused push must not advance the remote native head"
    );
    assert_eq!(
        std::fs::read_to_string(source.path().join("side.txt")).expect("source worktree side"),
        "source\n",
        "refused push must not materialize over remote worktree content"
    );
}

#[test]
fn sync_fetch_divergence_from_subdirectory_records_conflict_at_repo_root() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-subdir-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 subdir base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source subdir side", "subdir.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer subdir side", "subdir.txt", "peer\n");
    let nested = peer.path().join("nested/leaf");
    std::fs::create_dir_all(&nested).expect("nested peer cwd");

    let conflicted = json(
        forge_in(&nested)
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(conflicted["data"]["merged"], false);
    assert_single_native_sync_conflict(peer.path(), "sync_fetch_divergence");
}

#[test]
fn sync_fetch_divergence_request_id_replays_without_duplicate_local_conflict() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-fetch-request-id-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 fetch request id base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source request-id side", "side.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer request-id side", "side.txt", "peer\n");

    let first = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "fetch-divergence",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["merged"], false);
    assert_single_native_sync_conflict(peer.path(), "sync_fetch_divergence");
    assert_eq!(
        operation_count_for_request_id(peer.path(), "fetch-divergence"),
        1,
        "fetch divergence must claim the request-id in the initiating repo"
    );

    let replay = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "fetch-divergence",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(
        conflict_count(peer.path()),
        1,
        "request-id replay must not create a duplicate local conflict set"
    );
}
