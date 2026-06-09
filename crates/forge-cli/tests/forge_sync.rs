//! Phase 9 native sync MVP: export, inspect, and import a versioned sync bundle
//! carrying native object payloads plus ledger rows through the JSON envelope.

mod common;

use common::{forge_in, TestRepo};
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

fn native_accept_file_change(repo: &TestRepo, intent: &str, path: &str, contents: &str) {
    native_accept_file_change_in(repo.path(), intent, path, contents);
}

fn native_accept_file_change_in(
    repo_path: &std::path::Path,
    intent: &str,
    path: &str,
    contents: &str,
) {
    forge_in(repo_path)
        .args(["--json", "start", intent, "--require", "sh -c true"])
        .assert()
        .success();
    std::fs::write(repo_path.join(path), contents).expect("write native change");
    forge_in(repo_path)
        .args(["--json", "save"])
        .assert()
        .success();
    forge_in(repo_path)
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    forge_in(repo_path)
        .args(["--json", "propose"])
        .assert()
        .success();
    forge_in(repo_path)
        .args(["--json", "check"])
        .assert()
        .success();
    forge_in(repo_path)
        .args(["--json", "accept"])
        .assert()
        .success();
}

fn export_native_head(repo_path: &std::path::Path, file_name: &str) -> Value {
    let output_dir = tempfile::tempdir().expect("native head export dir");
    let output_path = output_dir.path().join(file_name);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).expect("native head export parent");
    }
    json(
        forge_in(repo_path)
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                output_path.to_str().expect("utf8 export path"),
            ])
            .assert()
            .success(),
    )
}

fn checkout_current_native_head(repo_path: &std::path::Path, file_name: &str) -> Value {
    let exported = export_native_head(repo_path, file_name);
    let head = exported["data"]["native_head"]
        .as_str()
        .expect("native head");
    forge_in(repo_path)
        .args(["--json", "checkout", head])
        .assert()
        .success();
    exported
}

fn cloned_peer_from_bundle(bundle_path: &std::path::Path) -> tempfile::TempDir {
    let peer_dir = tempfile::tempdir().expect("peer dir");
    forge_in(peer_dir.path())
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 bundle path"),
        ])
        .assert()
        .success();
    peer_dir
}

fn assert_single_sync_divergence(repo_path: &std::path::Path, context: &str) {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    let conflicts = list["data"]["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "expected one sync conflict: {list}");
    assert_eq!(conflicts[0]["context"], context);
    assert_eq!(conflicts[0]["resolver_backend"], "stale_base");
    assert_eq!(conflicts[0]["status"], "unresolved");
    assert_eq!(conflicts[0]["path_conflict_count"], 0);
    assert!(conflicts[0]["base_content_ref"]
        .as_str()
        .unwrap()
        .starts_with("forge-tree:"));
    assert!(conflicts[0]["ours_content_ref"]
        .as_str()
        .unwrap()
        .starts_with("forge-tree:"));
    assert!(conflicts[0]["theirs_content_ref"]
        .as_str()
        .unwrap()
        .starts_with("forge-tree:"));

    let conflict_id = conflicts[0]["conflict_set_id"]
        .as_str()
        .expect("conflict id");
    let show = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "show", conflict_id])
            .assert()
            .success(),
    );
    assert_eq!(show["data"]["conflict"]["context"], context);
    assert_eq!(
        show["data"]["path_conflicts"]
            .as_array()
            .expect("path conflicts")
            .len(),
        0
    );
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
fn sync_peer_commands_refuse_divergent_native_heads() {
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
    let refused_fetch = json(
        forge_in(fetch_peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused_fetch["command"], "sync fetch");
    assert_eq!(refused_fetch["status"], "error");
    assert_eq!(refused_fetch["errors"][0]["code"], "STALE_BASE");
    assert_single_sync_divergence(fetch_peer.path(), "sync_fetch_divergence");
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
    let refused_pull = json(
        forge_in(pull_peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused_pull["command"], "sync pull");
    assert_eq!(refused_pull["status"], "error");
    assert_eq!(refused_pull["errors"][0]["code"], "STALE_BASE");
    assert_single_sync_divergence(pull_peer.path(), "sync_pull_divergence");
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
    let refused_push = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused_push["command"], "sync push");
    assert_eq!(refused_push["status"], "error");
    assert_eq!(refused_push["errors"][0]["code"], "STALE_BASE");
    assert_single_sync_divergence(push_peer.path(), "sync_push_divergence");
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
