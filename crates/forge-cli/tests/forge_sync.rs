//! Phase 9 native sync MVP: export, inspect, and import a versioned sync bundle
//! carrying native object payloads plus ledger rows through the JSON envelope.

mod common;

use common::{forge_in, TestRepo};
use rusqlite::{params, Connection};
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

fn expected_content_ref_for_test(repo_path: &std::path::Path) -> Option<String> {
    let conn = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    conn.query_row(
        "SELECT expected_content_ref FROM current_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )
    .expect("current state row")
}

fn set_expected_content_ref_for_test(repo_path: &std::path::Path, content_ref: &str) {
    let conn = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    conn.execute(
        "UPDATE current_state SET expected_content_ref = ?1 WHERE singleton = 1",
        params![content_ref],
    )
    .expect("set expected content ref");
}

fn file_url_for_test(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
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

fn operation_count_for_request_id_and_kind(
    repo_path: &std::path::Path,
    request_id: &str,
    kind: &str,
) -> i64 {
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE request_id = ?1 AND kind = ?2",
            params![request_id, kind],
            |row| row.get(0),
        )
        .expect("count request-id operations by kind")
}

fn skew_latest_decision_timestamp_for_test(repo_path: &std::path::Path, created_at_ms: i64) {
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .execute(
            "UPDATE decisions
             SET created_at_ms = ?1
             WHERE rowid = (SELECT rowid FROM decisions ORDER BY rowid DESC LIMIT 1)",
            [created_at_ms],
        )
        .expect("skew latest decision timestamp");
}

fn poison_latest_decision_commit_for_test(
    repo_path: &std::path::Path,
    commit_id: &str,
    created_at_ms: i64,
) {
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .execute(
            "UPDATE decisions
             SET commit_id = ?1, created_at_ms = ?2
             WHERE rowid = (SELECT rowid FROM decisions ORDER BY rowid DESC LIMIT 1)",
            params![commit_id, created_at_ms],
        )
        .expect("poison latest decision commit");
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
fn sync_peer_commands_reject_unsupported_remote_scheme() {
    let repo = TestRepo::new_git();
    native_accepted_lifecycle(&repo);

    let failed = json(
        repo.forge()
            .args(["--json", "sync", "fetch", "ssh://example.test/repo"])
            .assert()
            .failure(),
    );
    assert_eq!(failed["status"], "error");
    assert_eq!(failed["errors"][0]["code"], "COMMAND_FAILED");
    assert!(
        failed["errors"][0]["message"]
            .as_str()
            .expect("error message")
            .contains("unsupported sync remote scheme ssh"),
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

#[test]
fn sync_fetch_clean_divergence_records_merge_commit_without_materializing() {
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
            .success(),
    );
    assert_eq!(first["command"], "sync fetch");
    assert_eq!(first["status"], "success");
    assert_eq!(first["data"]["merged"], true);
    assert_eq!(first["data"]["materialized"], false);
    assert!(first["data"]["merge_commit_id"]
        .as_str()
        .expect("merge commit id")
        .starts_with("f1:commit:"));
    assert!(first["data"]["merged_content_ref"]
        .as_str()
        .expect("merged content ref")
        .starts_with("forge-tree:"));
    let first_operation_id = first["operation_id"]
        .as_str()
        .expect("clean merge operation id")
        .to_string();
    assert_eq!(
        operation_count_for_request_id(peer.path(), "clean-divergence"),
        1,
        "clean-divergence merge should replay deterministically by request-id"
    );
    assert_eq!(
        conflict_count(peer.path()),
        0,
        "clean divergent merge must not record a path conflict"
    );

    let peer_after = export_native_head(peer.path(), "clean-peer-after.json");
    assert_ne!(
        peer_after["data"]["native_head"], peer_before["data"]["native_head"],
        "clean fetch divergence must advance the local native head to the merge commit"
    );
    assert_eq!(
        peer_after["data"]["native_head"],
        first["data"]["merge_commit_id"]
    );
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
    let peer_after_doctor = export_native_head(peer.path(), "clean-peer-after-doctor.json");
    assert_eq!(
        peer_after_doctor["data"]["native_head"], first["data"]["merge_commit_id"],
        "reconcile must keep the clean fetch merge commit as the native tip"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "clean fetch divergence must not materialize over local worktree content"
    );
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "fetch should not materialize the merged source-side file"
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
            .success(),
    );
    assert_eq!(replay["status"], "success");
    assert_eq!(replay["operation_id"], first_operation_id);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["merged"], true);
    assert_eq!(
        replay["data"]["protocol_version"],
        first["data"]["protocol_version"]
    );
    assert_eq!(
        replay["data"]["merge_commit_id"],
        first["data"]["merge_commit_id"]
    );
    assert_eq!(
        replay["data"]["merged_content_ref"],
        first["data"]["merged_content_ref"]
    );
    assert_eq!(replay["data"]["materialized"], false);
}

#[test]
fn sync_fetch_clean_divergence_ignores_future_peer_decision_timestamp() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clean-skew-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean skew base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source future clock",
        "source-only.txt",
        "source\n",
    );
    skew_latest_decision_timestamp_for_test(source.path(), 4_102_444_800_000);
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer local clock", "peer-only.txt", "peer\n");

    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(fetched["status"], "success");
    assert_eq!(fetched["data"]["merged"], true);
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
    let after_doctor = export_native_head(peer.path(), "clean-skew-after-doctor.json");
    assert_eq!(
        after_doctor["data"]["native_head"], fetched["data"]["merge_commit_id"],
        "future peer decision timestamps must not outrank the receiver's merge commit"
    );
}

#[test]
fn sync_fetch_clean_divergence_skips_dangling_future_decision_tip() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-dangling-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean dangling base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source dangling tip",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer dangling tip", "peer-only.txt", "peer\n");

    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(fetched["data"]["merged"], true);

    poison_latest_decision_commit_for_test(
        peer.path(),
        "f1:commit:sha256:0000000000000000000000000000000000000000000000000000000000000000",
        4_102_444_800_000,
    );

    let exported = export_native_head(peer.path(), "clean-dangling-after-poison.json");
    assert_eq!(
        exported["data"]["native_head"], fetched["data"]["merge_commit_id"],
        "dangling future decisions must not outrank the valid sync merge tip"
    );
    let doctor = json(
        forge_in(peer.path())
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_eq!(doctor["data"]["ok"], false);
    assert!(doctor["data"]["native_history_issues"]
        .as_array()
        .expect("native history findings")
        .iter()
        .any(|finding| finding["kind"] == "dangling_commit_id"));
}

#[test]
fn sync_pull_after_clean_fetch_materializes_existing_merge_commit() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-fetch-pull-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 clean fetch-pull base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source clean fetch-pull",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        peer.path(),
        "peer clean fetch-pull",
        "peer-only.txt",
        "peer\n",
    );

    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(fetched["data"]["merged"], true);
    assert_eq!(fetched["data"]["materialized"], false);
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "fetch should leave the merged source-side file unmaterialized"
    );

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "pull-after-clean-fetch",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pulled["command"], "sync pull");
    assert_eq!(pulled["status"], "success");
    assert_eq!(pulled["data"]["up_to_date"], true);
    assert_eq!(pulled["data"]["materialized"], true);
    assert_eq!(
        pulled["data"]["materialized_content_ref"],
        fetched["data"]["merged_content_ref"]
    );
    let pulled_operation_id = pulled["operation_id"]
        .as_str()
        .expect("pull materialization operation id")
        .to_string();
    assert_eq!(
        operation_count_for_request_id_and_kind(
            peer.path(),
            "pull-after-clean-fetch",
            "sync_pull_materialized"
        ),
        1,
        "up-to-date sync pull must claim request-id under sync pull"
    );
    let replay = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "pull-after-clean-fetch",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["status"], "success");
    assert_eq!(replay["operation_id"], pulled_operation_id);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["up_to_date"], true);
    assert_eq!(replay["data"]["materialized"], true);
    assert_eq!(
        replay["data"]["materialized_content_ref"],
        pulled["data"]["materialized_content_ref"]
    );
    assert_eq!(
        replay["data"]["materialized_operation_id"],
        pulled["data"]["materialized_operation_id"]
    );
    assert_eq!(
        replay["data"]["materialized_view_id"],
        pulled["data"]["materialized_view_id"]
    );
    native_accept_file_change_in(
        peer.path(),
        "later clean replay state",
        "later-clean.txt",
        "later\n",
    );
    let later_head = export_native_head(peer.path(), "clean-fetch-pull-later-head.json");
    let replay_after_later_clean = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "pull-after-clean-fetch",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay_after_later_clean["status"], "success");
    assert_eq!(
        replay_after_later_clean["operation_id"],
        pulled_operation_id
    );
    assert_eq!(replay_after_later_clean["data"]["idempotent_replay"], true);
    assert_eq!(
        std::fs::read_to_string(peer.path().join("later-clean.txt")).expect("later clean content"),
        "later\n",
        "idempotent replay must not restore an older tree over a newer clean state"
    );
    let after_later_replay_head =
        export_native_head(peer.path(), "clean-fetch-pull-after-later-replay-head.json");
    assert_eq!(
        after_later_replay_head["data"]["native_head"], later_head["data"]["native_head"],
        "idempotent replay must not rewind a newer native head"
    );
    std::fs::write(peer.path().join("after-replay-edit.txt"), "edited\n")
        .expect("write post-success edit");
    let replay_after_edit = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "pull-after-clean-fetch",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay_after_edit["status"], "success");
    assert_eq!(replay_after_edit["operation_id"], pulled_operation_id);
    assert_eq!(replay_after_edit["data"]["idempotent_replay"], true);
    assert_eq!(
        std::fs::read_to_string(peer.path().join("after-replay-edit.txt"))
            .expect("post-success edit content"),
        "edited\n",
        "idempotent replay must not revalidate or overwrite later user edits"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("source-only.txt")).expect("source-only content"),
        "source\n",
        "pull after fetch must materialize the source-side merged content"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "pull after fetch must preserve receiver-side merged content"
    );
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
}

#[test]
fn sync_pull_fast_forward_refuses_dirty_worktree_before_materializing() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-ff-dirty-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 ff dirty base path"),
        ])
        .assert()
        .success();

    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change(&source, "source ff dirty", "source-only.txt", "source\n");
    std::fs::write(peer.path().join("local-dirty.txt"), "dirty\n").expect("write dirty file");
    let peer_before = export_native_head(peer.path(), "target/ff-dirty-peer-before.json");

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "ff-dirty-pull",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(pulled["status"], "error");
    assert_eq!(pulled["errors"][0]["code"], "DIRTY_WORKTREE");
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "dirty fast-forward pull must not materialize remote content"
    );
    let peer_after = export_native_head(peer.path(), "target/ff-dirty-peer-after.json");
    assert_eq!(
        peer_after["data"]["native_head"], peer_before["data"]["native_head"],
        "dirty fast-forward pull must not advance native HEAD before refusing"
    );
}

#[test]
fn sync_pull_clean_divergence_records_merge_commit_and_materializes() {
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
    let peer_before_expected =
        expected_content_ref_for_test(peer.path()).expect("pre-merge peer expected content ref");

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-pull-merge-replay",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pulled["command"], "sync pull");
    assert_eq!(pulled["status"], "success");
    assert_eq!(pulled["data"]["merged"], true);
    assert_eq!(pulled["data"]["materialized"], true);
    let peer_after = export_native_head(peer.path(), "clean-pull-after.json");
    assert_ne!(
        peer_after["data"]["native_head"], peer_before["data"]["native_head"],
        "clean pull divergence must advance the local native head"
    );
    assert_eq!(
        peer_after["data"]["native_head"],
        pulled["data"]["merge_commit_id"]
    );
    forge_content_native::restore_content_ref_to_worktree(
        peer.path(),
        peer.path(),
        &peer_before_expected,
    )
    .expect("restore exact pre-merge content before replay");
    set_expected_content_ref_for_test(peer.path(), &peer_before_expected);
    std::fs::write(
        peer.path().join(".forge/refs/HEAD"),
        peer_before["data"]["native_head"]
            .as_str()
            .expect("prior head"),
    )
    .expect("rewind native HEAD for replay recovery test");
    let replay = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-pull-merge-replay",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(
        replay["data"]["merge_commit_id"], pulled["data"]["merge_commit_id"],
        "sync merge replay must preserve the merge commit field"
    );
    assert_eq!(
        replay["data"]["merged_content_ref"], pulled["data"]["merged_content_ref"],
        "sync merge replay must preserve the merged content field"
    );
    assert_eq!(replay["data"]["merged"], true);
    assert_eq!(replay["data"]["materialized"], true);
    assert_eq!(
        replay["data"]["source_native_head"], pulled["data"]["source_native_head"],
        "sync merge replay must preserve source head"
    );
    assert_eq!(
        replay["data"]["receiver_native_head"], pulled["data"]["receiver_native_head"],
        "sync merge replay must preserve receiver head"
    );
    assert_eq!(
        replay["data"]["common_ancestor_native_head"],
        pulled["data"]["common_ancestor_native_head"],
        "sync merge replay must preserve common ancestor"
    );
    let replay_head = export_native_head(peer.path(), "clean-pull-replay-head.json");
    assert_eq!(
        replay_head["data"]["native_head"], pulled["data"]["merge_commit_id"],
        "same-request-id replay must reconcile a stale native sync merge HEAD"
    );
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
    let peer_after_doctor = export_native_head(peer.path(), "clean-pull-after-doctor.json");
    assert_eq!(
        peer_after_doctor["data"]["native_head"], pulled["data"]["merge_commit_id"],
        "reconcile must keep the clean pull merge commit as the native tip"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "clean pull divergence must preserve receiver-side content"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("source-only.txt")).expect("source-only content"),
        "source\n",
        "clean pull divergence must materialize source-side content"
    );
    assert_eq!(conflict_count(peer.path()), 0);
}

#[test]
fn sync_pull_clean_divergence_plain_retry_heals_torn_materialization() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-pull-plain-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 clean pull plain base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source clean plain pull",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        peer.path(),
        "peer clean plain pull",
        "peer-only.txt",
        "peer\n",
    );
    let peer_before_expected =
        expected_content_ref_for_test(peer.path()).expect("pre-merge peer expected content ref");

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pulled["data"]["merged"], true);
    assert_eq!(pulled["data"]["materialized"], true);
    let merge_commit_id = pulled["data"]["merge_commit_id"]
        .as_str()
        .expect("merge commit id");

    forge_content_native::restore_content_ref_to_worktree(
        peer.path(),
        peer.path(),
        &peer_before_expected,
    )
    .expect("restore exact pre-merge content before plain retry");
    set_expected_content_ref_for_test(peer.path(), &peer_before_expected);
    let retry_after_unrestored = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(retry_after_unrestored["status"], "success");
    assert_eq!(retry_after_unrestored["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(peer.path().join("source-only.txt")).expect("source-only content"),
        "source\n",
        "plain retry must restore source-side content after a torn clean sync merge"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "plain retry must preserve receiver-side content after a torn clean sync merge"
    );

    set_expected_content_ref_for_test(peer.path(), &peer_before_expected);
    let retry_after_expected_lag = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(retry_after_expected_lag["status"], "success");
    assert_eq!(retry_after_expected_lag["data"]["materialized"], true);
    let final_head = export_native_head(peer.path(), "clean-pull-plain-retry-head.json");
    assert_eq!(
        final_head["data"]["native_head"], merge_commit_id,
        "plain retry must not fork or rewind the clean sync merge commit"
    );
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
}

#[test]
fn sync_pull_clean_divergence_refuses_dirty_worktree_before_recording_merge() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-pull-dirty-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 clean pull dirty base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source dirty pull", "source-only.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer dirty pull", "peer-only.txt", "peer\n");
    std::fs::write(peer.path().join("local-dirty.txt"), "dirty\n").expect("write dirty file");

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-dirty-pull",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(pulled["status"], "error");
    assert_eq!(pulled["errors"][0]["code"], "DIRTY_WORKTREE");
    assert_eq!(
        operation_count_for_request_id_and_kind(
            peer.path(),
            "clean-dirty-pull",
            "sync_pull_merged"
        ),
        0,
        "dirty clean pull must fail before recording a merge operation"
    );
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "dirty clean pull must not materialize source-side content"
    );
    assert_eq!(conflict_count(peer.path()), 0);
}

#[test]
fn doctor_reports_missing_sync_merge_commit_signature() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-missing-merge-signature-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 missing signature base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source missing merge signature",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        peer.path(),
        "peer missing merge signature",
        "peer-only.txt",
        "peer\n",
    );
    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    let merge_commit_id = fetched["data"]["merge_commit_id"]
        .as_str()
        .expect("merge commit id");
    Connection::open(peer.path().join(".forge/forge.db"))
        .expect("open forge db")
        .execute(
            "DELETE FROM ledger_signatures
             WHERE subject_kind = 'sync_merge_commit' AND subject_id = ?1",
            [merge_commit_id],
        )
        .expect("delete sync merge signature");

    let report = json(
        forge_in(peer.path())
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_eq!(report["data"]["ok"], false);
    let issues = report["data"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature"
            && issue["subject_kind"] == "sync_merge_commit"
            && issue["subject_id"] == merge_commit_id
    }));
}

#[test]
fn doctor_accepts_imported_peer_sync_merge_commit_signature() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-import-peer-merge-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 imported peer merge base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source imported peer merge",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        peer.path(),
        "peer imported sync merge",
        "peer-only.txt",
        "peer\n",
    );
    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    let merge_commit_id = fetched["data"]["merge_commit_id"]
        .as_str()
        .expect("peer merge commit id")
        .to_string();

    let importer = cloned_peer_from_bundle(&bundle_path);
    let imported = json(
        forge_in(importer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                peer.path().to_str().expect("utf8 peer path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        imported["data"]["remote_native_head"].as_str(),
        Some(merge_commit_id.as_str())
    );

    let report = json(
        forge_in(importer.path())
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_eq!(report["data"]["ok"], true);
    let issues = report["data"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(
        issues.is_empty(),
        "imported peer sync merge signatures must remain peer-verifiable without local-only noise: {report}"
    );
}

#[test]
fn sync_push_clean_divergence_records_remote_merge_commit_without_materializing() {
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

    let pushed = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-push-divergence",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pushed["command"], "sync push");
    assert_eq!(pushed["status"], "success");
    assert_eq!(pushed["data"]["merged"], true);
    assert_eq!(pushed["data"]["materialized"], false);
    let pushed_operation_id = pushed["operation_id"]
        .as_str()
        .expect("clean push operation id")
        .to_string();
    let source_after = export_native_head(source.path(), "target/clean-push-after.json");
    assert_ne!(
        source_after["data"]["native_head"], source_before["data"]["native_head"],
        "clean push divergence must advance the remote native head"
    );
    assert_eq!(
        source_after["data"]["native_head"],
        pushed["data"]["merge_commit_id"]
    );
    forge_in(source.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
    let source_after_doctor =
        export_native_head(source.path(), "target/clean-push-after-doctor.json");
    assert_eq!(
        source_after_doctor["data"]["native_head"], pushed["data"]["merge_commit_id"],
        "reconcile must keep the clean push merge commit as the remote native tip"
    );
    assert_eq!(
        std::fs::read_to_string(source.path().join("source-only.txt"))
            .expect("source-only content"),
        "source\n",
        "clean push divergence must not materialize over remote worktree content"
    );
    assert!(
        !source.path().join("peer-only.txt").exists(),
        "push should not materialize the peer-side file in the remote worktree"
    );
    assert_eq!(conflict_count(source.path()), 0);

    let replayed = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-push-divergence",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replayed["operation_id"], pushed_operation_id);
    assert_eq!(replayed["data"]["idempotent_replay"], true);
    assert_eq!(replayed["data"]["merged"], true);
    assert_eq!(replayed["data"]["materialized"], false);
    assert_eq!(
        replayed["data"]["merge_commit_id"],
        pushed["data"]["merge_commit_id"]
    );
    assert_eq!(
        replayed["data"]["remote_operation_id"],
        pushed["data"]["remote_operation_id"]
    );
}

#[test]
fn sync_fetch_clean_divergence_from_subdirectory_records_merge_commit() {
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

    let fetched = json(
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
    assert_eq!(fetched["status"], "success");
    assert_eq!(fetched["data"]["merged"], true);
    assert_eq!(fetched["data"]["materialized"], false);
    assert!(fetched["data"]["merge_commit_id"]
        .as_str()
        .expect("merge commit id")
        .starts_with("f1:commit:"));
    let peer_after = export_native_head(peer.path(), "clean-subdir-after.json");
    assert_eq!(
        peer_after["data"]["native_head"],
        fetched["data"]["merge_commit_id"]
    );
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "subdirectory fetch should not materialize the source-side file"
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
    assert_eq!(replay["data"]["direction"], first["data"]["direction"]);
    assert_eq!(replay["data"]["remote_path"], first["data"]["remote_path"]);
    assert_eq!(
        replay["data"]["base_native_head"], first["data"]["base_native_head"],
        "sync fetch replay must preserve the base head"
    );
    assert_eq!(
        replay["data"]["remote_native_head"], first["data"]["remote_native_head"],
        "sync fetch replay must preserve the remote head"
    );
    assert_eq!(
        replay["data"]["imported_native_objects"], first["data"]["imported_native_objects"],
        "sync fetch replay must preserve import counts"
    );
    assert_eq!(
        replay["data"]["imported_ledger_rows"], first["data"]["imported_ledger_rows"],
        "sync fetch replay must preserve ledger counts"
    );
    assert_eq!(
        replay["data"]["materialized"],
        first["data"]["materialized"]
    );
    assert_eq!(replay["data"]["up_to_date"], first["data"]["up_to_date"]);

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
    assert_eq!(replay["data"]["direction"], "push");
    assert_eq!(replay["data"]["up_to_date"], true);
    assert_eq!(
        replay["data"]["base_native_head"],
        first["data"]["base_native_head"]
    );
    assert_eq!(
        replay["data"]["local_native_head"],
        first["data"]["local_native_head"]
    );
    assert_eq!(replay["data"]["materialized"], false);

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
