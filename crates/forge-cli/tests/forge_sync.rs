//! Phase 9 native sync MVP: export, inspect, and import a versioned sync bundle
//! carrying native object payloads plus ledger rows through the JSON envelope.

mod common;

use common::{forge_in, TestRepo};
use rusqlite::Connection;
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

fn native_checked_proposal(repo: &TestRepo) {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "start",
            "sync trust boundary",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("peer-trust.txt"), "peer trust\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
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

fn assert_single_native_sync_conflict(repo_path: &std::path::Path, context: &str) {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    let conflicts = list["data"]["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "expected one sync conflict: {list}");
    assert_eq!(conflicts[0]["context"], context);
    assert_eq!(conflicts[0]["resolver_backend"], "native_merge");
    assert_eq!(conflicts[0]["status"], "unresolved");
    assert_eq!(conflicts[0]["path_conflict_count"], 1);

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
    assert_eq!(show["data"]["conflict"]["resolver_backend"], "native_merge");
    for field in ["base_content_ref", "ours_content_ref", "theirs_content_ref"] {
        let content_ref = show["data"]["conflict"][field]
            .as_str()
            .unwrap_or_else(|| panic!("{field} must be present"));
        assert!(
            content_ref.starts_with("forge-tree:"),
            "{field} should be a forge-tree content ref: {content_ref}"
        );
    }
    let path_conflicts = show["data"]["path_conflicts"]
        .as_array()
        .expect("path conflicts");
    assert_eq!(path_conflicts.len(), 1);
    assert_eq!(path_conflicts[0]["kind"], "content");
    assert_eq!(path_conflicts[0]["status"], "unresolved");
}

fn single_native_sync_conflict_content_refs(
    repo_path: &std::path::Path,
    context: &str,
) -> Vec<String> {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    let conflicts = list["data"]["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "expected one sync conflict: {list}");
    assert_eq!(conflicts[0]["context"], context);
    let conflict_id = conflicts[0]["conflict_set_id"]
        .as_str()
        .expect("conflict id");
    let show = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "show", conflict_id])
            .assert()
            .success(),
    );
    ["base_content_ref", "ours_content_ref", "theirs_content_ref"]
        .iter()
        .map(|field| {
            show["data"]["conflict"][field]
                .as_str()
                .unwrap_or_else(|| panic!("{field} must be present"))
                .to_string()
        })
        .collect()
}

fn single_conflict_id(repo_path: &std::path::Path, context: &str) -> String {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    let conflicts = list["data"]["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "expected one sync conflict: {list}");
    assert_eq!(conflicts[0]["context"], context);
    conflicts[0]["conflict_set_id"]
        .as_str()
        .expect("conflict id")
        .to_string()
}

fn conflict_count(repo_path: &std::path::Path) -> usize {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    list["data"]["conflicts"]
        .as_array()
        .expect("conflicts")
        .len()
}

fn native_object_count(repo_path: &std::path::Path, file_name: &str) -> u64 {
    export_native_head(repo_path, file_name)["data"]["native_object_count"]
        .as_u64()
        .expect("native object count")
}

fn operation_count_for_request_id(repo_path: &std::path::Path, request_id: &str) -> i64 {
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE request_id = ?1",
            [request_id],
            |row| row.get(0),
        )
        .expect("count request-id operations")
}

fn mark_conflict_resolved_for_test(repo_path: &std::path::Path, conflict_id: &str) {
    // This helper only flips the status fields needed to exercise sync-conflict
    // dedup policy. It intentionally does not simulate a real resolution
    // operation; the original conflict-creation operation and integrity chain
    // remain intact for the dedup query under test.
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .execute(
            "UPDATE conflict_sets SET status = 'resolved' WHERE id = ?1",
            [conflict_id],
        )
        .expect("mark conflict set resolved");
    connection
        .execute(
            "UPDATE path_conflicts SET status = 'resolved' WHERE conflict_set_id = ?1",
            [conflict_id],
        )
        .expect("mark path conflicts resolved");
}

fn assert_gc_keeps_content_refs_reachable(repo_path: &std::path::Path, content_refs: &[String]) {
    let gc = json(
        forge_in(repo_path)
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    let unreachable = gc["data"]["unreachable_native_objects"]
        .as_array()
        .expect("unreachable native objects");
    for content_ref in content_refs {
        let tree_id = content_ref
            .strip_prefix("forge-tree:")
            .unwrap_or_else(|| panic!("native content ref expected: {content_ref}"));
        assert!(
            !unreachable
                .iter()
                .any(|value| value.as_str() == Some(tree_id)),
            "gc must keep conflict content ref reachable: {content_ref}; report: {gc}"
        );
    }
}

fn record_sync_divergence_conflict(repo_path: &std::path::Path) {
    let base = tempfile::tempdir().expect("sync divergence base dir");
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = base.path().join("base.json");
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
        .success();
    forge_in(repo_path)
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 bundle path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source divergence", "diverge.txt", "source\n");
    native_accept_file_change_in(repo_path, "peer divergence", "diverge.txt", "peer\n");
    let conflicted = json(
        forge_in(repo_path)
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
    assert!(conflicted["data"]["conflict_set_id"].as_str().is_some());
    assert_single_native_sync_conflict(repo_path, "sync_fetch_divergence");
}

fn forge_json(repo_path: &std::path::Path, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    json(forge_in(repo_path).args(full).assert().success())
}

fn record_native_merge_conflict(repo: &TestRepo) -> String {
    std::fs::write(repo.path().join("README.md"), "one\ntwo\nthree\n").expect("seed readme");
    forge_json(repo.path(), &["init", "--content-backend", "native"]);
    let started = forge_json(repo.path(), &["start", "sync native merge conflict"]);
    let intent = started["data"]["intent_id"].as_str().expect("intent id");
    let attempt_a = started["data"]["attempt_id"].as_str().expect("attempt a");
    let started_b = forge_json(repo.path(), &["attempt", "start", "--intent", intent]);
    let attempt_b = started_b["data"]["attempt_id"].as_str().expect("attempt b");

    forge_json(repo.path(), &["attempt", "attach", attempt_a]);
    std::fs::write(repo.path().join("README.md"), "one\nOURS\nthree\n").expect("ours");
    forge_json(repo.path(), &["save", "--attempt", attempt_a]);
    forge_json(
        repo.path(),
        &["run", "--attempt", attempt_a, "--", "sh", "-c", "true"],
    );
    let proposed_a = forge_json(repo.path(), &["propose", "--attempt", attempt_a]);
    let proposal_a = proposed_a["data"]["proposal_id"]
        .as_str()
        .expect("proposal a");
    forge_json(repo.path(), &["check", "--attempt", attempt_a]);

    forge_json(repo.path(), &["attempt", "attach", attempt_b]);
    std::fs::write(repo.path().join("README.md"), "one\nTHEIRS\nthree\n").expect("theirs");
    forge_json(repo.path(), &["save", "--attempt", attempt_b]);
    forge_json(
        repo.path(),
        &["run", "--attempt", attempt_b, "--", "sh", "-c", "true"],
    );
    let proposed_b = forge_json(repo.path(), &["propose", "--attempt", attempt_b]);
    let proposal_b = proposed_b["data"]["proposal_id"]
        .as_str()
        .expect("proposal b");
    forge_json(repo.path(), &["check", "--attempt", attempt_b]);

    forge_json(
        repo.path(),
        &["accept", "--attempt", attempt_a, "--proposal", proposal_a],
    );
    let merged = forge_json(repo.path(), &["merge", "--proposal", proposal_b]);
    assert_eq!(merged["data"]["merged"], false);
    let conflict_id = merged["data"]["conflict_set_id"]
        .as_str()
        .expect("conflict id")
        .to_string();
    let shown = forge_json(repo.path(), &["conflict", "show", &conflict_id]);
    assert_eq!(
        shown["data"]["conflict"]["resolver_backend"],
        "native_merge"
    );
    assert_eq!(
        shown["data"]["path_conflicts"]
            .as_array()
            .expect("path conflicts")
            .len(),
        1
    );
    conflict_id
}

fn source_with_conflict_after_base_export() -> (TestRepo, tempfile::TempDir, std::path::PathBuf) {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let base_dir = tempfile::tempdir().expect("peer base dir");
    let base_bundle = base_dir.path().join("peer-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            base_bundle.to_str().expect("utf8 peer base bundle"),
        ])
        .assert()
        .success();

    let divergence_peer = cloned_peer_from_bundle(&base_bundle);
    native_accept_file_change(
        &source,
        "source conflict row",
        "conflict-row.txt",
        "source\n",
    );
    native_accept_file_change_in(
        divergence_peer.path(),
        "peer conflict row",
        "conflict-row.txt",
        "peer\n",
    );
    let conflicted = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "fetch",
                divergence_peer.path().to_str().expect("utf8 peer path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(conflicted["data"]["merged"], false);
    assert_single_native_sync_conflict(source.path(), "sync_fetch_divergence");
    (source, base_dir, base_bundle)
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

#[test]
fn sync_fetch_clean_divergence_is_unsupported_without_advancing_head() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clean-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source disjoint", "source-only.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer disjoint", "peer-only.txt", "peer\n");
    let peer_before = export_native_head(peer.path(), "clean-peer-before.json");
    let peer_object_count_before =
        native_object_count(peer.path(), "clean-peer-objects-before.json");

    let first = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-divergence",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(first["command"], "sync fetch");
    assert_eq!(first["errors"][0]["code"], "SYNC_DIVERGENCE_UNSUPPORTED");
    assert_eq!(first["errors"][0]["details"]["direction"], "fetch");
    assert_eq!(
        first["errors"][0]["details"]["reason"],
        "clean_divergent_merge"
    );
    assert_eq!(first["retry"]["retryable"], false);
    let first_operation_id = first["operation_id"]
        .as_str()
        .expect("unsupported divergence operation id")
        .to_string();
    assert_eq!(
        operation_count_for_request_id(peer.path(), "clean-divergence"),
        1,
        "unsupported clean-divergence failure should replay deterministically by request-id"
    );
    assert_eq!(
        conflict_count(peer.path()),
        0,
        "clean divergent merge is unsupported and must not record a path conflict"
    );
    assert_eq!(
        native_object_count(peer.path(), "clean-peer-objects-after.json"),
        peer_object_count_before,
        "unsupported clean divergence must not leave staged peer objects behind"
    );

    let peer_after = export_native_head(peer.path(), "clean-peer-after.json");
    assert_eq!(
        peer_after["data"]["native_head"], peer_before["data"]["native_head"],
        "unsupported clean divergence must not advance the local native head"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "unsupported clean divergence must not materialize over local worktree content"
    );

    let replay = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-divergence",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(replay["errors"][0]["code"], "SYNC_DIVERGENCE_UNSUPPORTED");
    assert_eq!(replay["operation_id"], first_operation_id);
    assert_eq!(replay["errors"][0]["details"]["direction"], "fetch");
    assert_eq!(
        replay["errors"][0]["details"]["reason"],
        "clean_divergent_merge"
    );
}

#[test]
fn sync_pull_clean_divergence_is_unsupported_without_materializing() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clean-pull-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean pull base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source clean pull", "source-only.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer clean pull", "peer-only.txt", "peer\n");
    let peer_before = checkout_current_native_head(peer.path(), "clean-pull-before.json");
    let object_count_before = native_object_count(peer.path(), "clean-pull-objects-before.json");

    let refused = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused["command"], "sync pull");
    assert_eq!(refused["errors"][0]["code"], "SYNC_DIVERGENCE_UNSUPPORTED");
    assert_eq!(refused["errors"][0]["details"]["direction"], "pull");
    assert_eq!(
        refused["errors"][0]["details"]["reason"],
        "clean_divergent_merge"
    );
    assert_eq!(
        native_object_count(peer.path(), "clean-pull-objects-after.json"),
        object_count_before,
        "unsupported clean pull divergence must not leak staged local objects"
    );
    let peer_after = export_native_head(peer.path(), "clean-pull-after.json");
    assert_eq!(
        peer_after["data"]["native_head"], peer_before["data"]["native_head"],
        "unsupported clean pull divergence must not advance the local native head"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "unsupported clean pull divergence must not materialize over local worktree content"
    );
    assert_eq!(conflict_count(peer.path()), 0);
}

#[test]
fn sync_push_clean_divergence_is_unsupported_without_advancing_remote() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clean-push-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean push base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source clean push", "source-only.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer clean push", "peer-only.txt", "peer\n");
    let source_before = export_native_head(source.path(), "target/clean-push-before.json");
    let source_object_count_before =
        native_object_count(source.path(), "target/clean-push-objects-before.json");

    let refused = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused["command"], "sync push");
    assert_eq!(refused["errors"][0]["code"], "SYNC_DIVERGENCE_UNSUPPORTED");
    assert_eq!(refused["errors"][0]["details"]["direction"], "push");
    assert_eq!(
        refused["errors"][0]["details"]["reason"],
        "clean_divergent_merge"
    );
    assert_eq!(
        native_object_count(source.path(), "target/clean-push-objects-after.json"),
        source_object_count_before,
        "unsupported clean push divergence must not leak staged remote objects"
    );
    let source_after = export_native_head(source.path(), "target/clean-push-after.json");
    assert_eq!(
        source_after["data"]["native_head"], source_before["data"]["native_head"],
        "unsupported clean push divergence must not advance the remote native head"
    );
    assert_eq!(
        std::fs::read_to_string(source.path().join("source-only.txt"))
            .expect("source-only content"),
        "source\n",
        "unsupported clean push divergence must not materialize over remote worktree content"
    );
    assert_eq!(conflict_count(source.path()), 0);
}

#[test]
fn sync_fetch_clean_divergence_from_subdirectory_cleans_staged_objects() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-subdir-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean subdir base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source clean subdir",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer clean subdir", "peer-only.txt", "peer\n");
    let nested = peer.path().join("nested/leaf");
    std::fs::create_dir_all(&nested).expect("nested peer cwd");
    let object_count_before = native_object_count(peer.path(), "clean-subdir-before.json");

    let refused = json(
        forge_in(&nested)
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(refused["errors"][0]["code"], "SYNC_DIVERGENCE_UNSUPPORTED");
    assert_eq!(
        native_object_count(peer.path(), "clean-subdir-after.json"),
        object_count_before,
        "unsupported clean divergence from a subdirectory must not leak staged objects"
    );
    assert_eq!(conflict_count(peer.path()), 0);
}

#[test]
fn sync_push_divergence_request_id_replays_without_duplicate_remote_conflict() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-push-request-id-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 push request id base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source side", "side.txt", "source\n");
    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push peer side", "side.txt", "peer\n");

    let first = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-divergence",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["merged"], false);
    let remote_operation_id = first["data"]["remote_operation_id"]
        .as_str()
        .expect("remote operation id")
        .to_string();
    assert_ne!(
        first["operation_id"], first["data"]["remote_operation_id"],
        "top-level push operation should be the local request-id marker"
    );
    assert_eq!(conflict_count(source.path()), 1);
    assert_eq!(
        operation_count_for_request_id(push_peer.path(), "push-divergence"),
        1,
        "push must claim the request-id in the initiating repo"
    );
    assert_eq!(
        operation_count_for_request_id(source.path(), "push-divergence"),
        0,
        "push divergence must not claim the initiator's request-id in the remote repo"
    );
    forge_in(source.path())
        .args([
            "--json",
            "--request-id",
            "push-divergence",
            "start",
            "remote namespace reuse",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    let conflict_refs =
        single_native_sync_conflict_content_refs(source.path(), "sync_push_divergence");
    assert_gc_keeps_content_refs_reachable(source.path(), &conflict_refs);

    let replay = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-divergence",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(
        conflict_count(source.path()),
        1,
        "local request-id replay must not call into the remote again"
    );
    let reused_for_save = json(
        forge_in(push_peer.path())
            .args(["--json", "--request-id", "push-divergence", "save"])
            .assert()
            .failure(),
    );
    assert_eq!(reused_for_save["errors"][0]["code"], "REQUEST_ID_CONFLICT");
    assert_eq!(
        first["data"]["remote_operation_id"], remote_operation_id,
        "remote operation id should remain available in the first push response"
    );
    assert_eq!(
        conflict_count(source.path()),
        1,
        "request-id replay must not create a duplicate remote conflict set"
    );
}

#[test]
fn sync_push_divergence_without_request_id_dedups_remote_conflict() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-push-no-request-id-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 push no request id base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source side", "side.txt", "source\n");
    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push peer side", "side.txt", "peer\n");

    for _ in 0..2 {
        let pushed = json(
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
        assert_eq!(pushed["data"]["merged"], false);
    }

    assert_eq!(
        conflict_count(source.path()),
        1,
        "repeated divergent push without request-id must reuse the remote conflict"
    );
}

#[test]
fn sync_push_divergence_without_request_id_dedups_resolved_remote_conflict() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-push-resolved-dedup-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 push resolved dedup base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source side", "side.txt", "source\n");
    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push peer side", "side.txt", "peer\n");

    json(
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
    let conflict_id = single_conflict_id(source.path(), "sync_push_divergence");
    mark_conflict_resolved_for_test(source.path(), &conflict_id);

    let replayed = json(
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
    assert_eq!(replayed["data"]["conflict_set_id"], conflict_id);
    assert_eq!(
        conflict_count(source.path()),
        1,
        "unrequested same-triple push should not open a second conflict after resolution"
    );
}

#[test]
fn sync_push_fast_forward_request_id_replays_locally() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-push-fast-forward-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 push fast-forward base path"),
        ])
        .assert()
        .success();

    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push ff", "from-peer.txt", "peer\n");

    let first = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-fast-forward",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["direction"], "push");
    assert_eq!(first["data"]["materialized"], false);
    assert!(first["operation_id"].as_str().is_some());
    assert_eq!(
        operation_count_for_request_id(push_peer.path(), "push-fast-forward"),
        1,
        "fast-forward push must claim the request-id in the initiating repo"
    );
    assert_eq!(
        operation_count_for_request_id(source.path(), "push-fast-forward"),
        0,
        "fast-forward import has no remote request-id row"
    );

    let replay = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-fast-forward",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
}

#[test]
fn sync_fetch_noop_request_id_replays_locally() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-fetch-noop-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 fetch noop base path"),
        ])
        .assert()
        .success();

    let fetch_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(fetch_peer.path(), "fetch noop", "peer.txt", "peer\n");

    let first = json(
        forge_in(fetch_peer.path())
            .args([
                "--json",
                "--request-id",
                "fetch-noop",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["direction"], "fetch");
    assert_eq!(first["data"]["up_to_date"], true);
    assert!(first["operation_id"].as_str().is_some());
    assert_eq!(
        operation_count_for_request_id(fetch_peer.path(), "fetch-noop"),
        1,
        "no-op fetch must still claim the request-id in the initiating repo"
    );

    let replay = json(
        forge_in(fetch_peer.path())
            .args([
                "--json",
                "--request-id",
                "fetch-noop",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);

    let reused_for_save = json(
        forge_in(fetch_peer.path())
            .args(["--json", "--request-id", "fetch-noop", "save"])
            .assert()
            .failure(),
    );
    assert_eq!(reused_for_save["errors"][0]["code"], "REQUEST_ID_CONFLICT");
}

#[test]
fn sync_push_noop_request_id_replays_locally() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-push-noop-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 push noop base path"),
        ])
        .assert()
        .success();

    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change(&source, "push noop", "source.txt", "source\n");

    let first = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-noop",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["direction"], "push");
    assert_eq!(first["data"]["up_to_date"], true);
    assert!(first["operation_id"].as_str().is_some());
    assert_eq!(
        operation_count_for_request_id(push_peer.path(), "push-noop"),
        1,
        "no-op push must still claim the request-id in the initiating repo"
    );
    assert_eq!(
        operation_count_for_request_id(source.path(), "push-noop"),
        0,
        "no-op push has no remote request-id row"
    );

    let replay = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-noop",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);

    let reused_for_save = json(
        forge_in(push_peer.path())
            .args(["--json", "--request-id", "push-noop", "save"])
            .assert()
            .failure(),
    );
    assert_eq!(reused_for_save["errors"][0]["code"], "REQUEST_ID_CONFLICT");
}

#[test]
fn sync_clone_carries_conflict_sets_from_source_ledger() {
    let source = tempfile::tempdir().expect("source conflict repo");
    record_sync_divergence_conflict(source.path());

    let bundle_dir = tempfile::tempdir().expect("conflict bundle dir");
    let bundle_path = bundle_dir.path().join("conflict-ledger.json");
    forge_in(source.path())
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 conflict bundle"),
        ])
        .assert()
        .success();

    let clone_dir = tempfile::tempdir().expect("conflict clone dir");
    forge_in(clone_dir.path())
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 conflict bundle"),
        ])
        .assert()
        .success();

    assert_single_native_sync_conflict(clone_dir.path(), "sync_fetch_divergence");
}

#[test]
fn sync_clone_carries_native_merge_path_conflicts_from_source_ledger() {
    let source = TestRepo::new_git();
    let conflict_id = record_native_merge_conflict(&source);

    let bundle_dir = tempfile::tempdir().expect("merge conflict bundle dir");
    let bundle_path = bundle_dir.path().join("merge-conflict-ledger.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 merge conflict bundle"),
        ])
        .assert()
        .success();

    let clone_dir = tempfile::tempdir().expect("merge conflict clone dir");
    forge_in(clone_dir.path())
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 merge conflict bundle"),
        ])
        .assert()
        .success();

    let shown = forge_json(clone_dir.path(), &["conflict", "show", &conflict_id]);
    assert_eq!(
        shown["data"]["conflict"]["resolver_backend"],
        "native_merge"
    );
    assert_eq!(
        shown["data"]["path_conflicts"]
            .as_array()
            .expect("path conflicts")
            .len(),
        1
    );
    assert_eq!(shown["data"]["path_conflicts"][0]["kind"], "content");
}

#[test]
fn sync_peer_fetch_pull_and_push_carry_conflict_sets() {
    let (source, _base_dir, base_bundle) = source_with_conflict_after_base_export();

    let fetch_peer = cloned_peer_from_bundle(&base_bundle);
    forge_in(fetch_peer.path())
        .args([
            "--json",
            "sync",
            "fetch",
            source.path().to_str().expect("utf8 source path"),
        ])
        .assert()
        .success();
    assert_single_native_sync_conflict(fetch_peer.path(), "sync_fetch_divergence");

    let pull_peer = cloned_peer_from_bundle(&base_bundle);
    forge_in(pull_peer.path())
        .args([
            "--json",
            "sync",
            "pull",
            source.path().to_str().expect("utf8 source path"),
        ])
        .assert()
        .success();
    assert_single_native_sync_conflict(pull_peer.path(), "sync_fetch_divergence");

    let push_peer = cloned_peer_from_bundle(&base_bundle);
    forge_in(source.path())
        .args([
            "--json",
            "sync",
            "push",
            push_peer.path().to_str().expect("utf8 push peer path"),
        ])
        .assert()
        .success();
    assert_single_native_sync_conflict(push_peer.path(), "sync_fetch_divergence");
}
