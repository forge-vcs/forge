//! Phase 9 native sync MVP: export, inspect, and import a versioned sync bundle
//! carrying native object payloads plus ledger rows through the JSON envelope.

mod common;

use common::TestRepo;
use serde_json::Value;

fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json envelope")
}

fn native_accepted_lifecycle(repo: &TestRepo) {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "start",
            "sync manifest lifecycle",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("sync.txt"), "sync\n").expect("write sync feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();
}

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

    target.forge().args(["--json", "doctor"]).assert().success();

    let reexport_path = target.path().join("target/reexported-sync-bundle.json");
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
                bundle_path.to_str().expect("utf8 bundle path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        imported_again["data"]["native_head"],
        exported["data"]["native_head"]
    );
}
