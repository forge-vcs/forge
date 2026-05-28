mod common;

use common::TestRepo;
use forge_content_native::{NativeObjectStore, ObjectKind};
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn doctor_passes_healthy_repo_and_reports_dangling_temp_files() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let healthy = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(healthy["data"]["ok"], true);

    let tmp = repo.path().join(".forge/tmp");
    std::fs::create_dir_all(&tmp).expect("create tmp");
    std::fs::write(tmp.join("interrupted"), "partial").expect("write temp marker");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue == "dangling temporary files"));
}

#[test]
fn gc_dry_run_reports_without_deleting() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let report = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    assert_eq!(report["data"]["dry_run"], true);
    assert!(report["data"]["deleted"].as_array().unwrap().is_empty());
}

#[test]
fn doctor_reports_corrupt_native_content_and_gc_reports_unreachable_objects() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "native doctor"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "native doctor\n").expect("write readme");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let content_ref = saved["data"]["content_ref"].as_str().unwrap();
    let digest = content_ref.rsplit(':').next().unwrap();
    let object_path = repo
        .path()
        .join(".forge/objects/sha256")
        .join(&digest[..2])
        .join(digest);
    std::fs::write(&object_path, b"corrupt").expect("corrupt root tree");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue.as_str().unwrap().contains("hash mismatch")));

    std::fs::remove_file(&object_path).expect("remove corrupt object");
    let missing = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert!(missing["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue
            .as_str()
            .unwrap()
            .contains("missing native content object")));

    let store = NativeObjectStore::new(repo.path());
    let orphan = store
        .write_object(ObjectKind::Blob, b"unreachable")
        .expect("write orphan");
    let gc = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    assert!(gc["data"]["unreachable_native_objects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == &orphan.to_string()));
}

#[test]
fn doctor_reports_malformed_native_tree_without_panicking() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "malformed native tree"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "native\n").expect("write readme");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let snapshot_id = saved["data"]["snapshot_id"].as_str().unwrap();
    let store = NativeObjectStore::new(repo.path());
    let malformed = store
        .write_object(ObjectKind::Tree, b"not json")
        .expect("write malformed tree");
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    connection
        .execute(
            "UPDATE snapshots SET content_ref = ?1 WHERE id = ?2",
            [format!("forge-tree:{malformed}"), snapshot_id.to_string()],
        )
        .expect("point snapshot at malformed tree");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue
            .as_str()
            .unwrap()
            .contains("malformed native tree object")));
}

#[test]
fn native_restore_verifies_reachable_objects_before_mutating_worktree() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "preflight restore"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "first saved\n").expect("write readme");
    let first = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let snapshot_id = first["data"]["snapshot_id"].as_str().unwrap();
    std::fs::write(repo.path().join("README.md"), "latest saved\n").expect("write current");
    repo.forge().args(["--json", "save"]).assert().success();

    let first_blob = object_path_containing(repo.path(), "first saved\n").expect("first blob");
    std::fs::remove_file(first_blob).expect("remove reachable blob");
    let output = json_output(
        repo.forge()
            .args(["--json", "restore", snapshot_id, "--yes"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "COMMAND_FAILED");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "latest saved\n"
    );
}

fn object_path_containing(repo_path: &std::path::Path, needle: &str) -> Option<std::path::PathBuf> {
    let objects = repo_path.join(".forge/objects/sha256");
    for prefix in std::fs::read_dir(objects).ok()? {
        let prefix = prefix.ok()?;
        for object in std::fs::read_dir(prefix.path()).ok()? {
            let object = object.ok()?;
            let bytes = std::fs::read(object.path()).ok()?;
            if String::from_utf8_lossy(&bytes).contains(needle) {
                return Some(object.path());
            }
        }
    }
    None
}

#[test]
fn doctor_reports_mismatched_current_view() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open forge db");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("disable foreign keys for corruption fixture");
    connection
        .execute(
            "UPDATE operations SET resulting_view_id = 'view_missing'",
            [],
        )
        .expect("corrupt current operation");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue == "invalid current operation/view"));
}

#[test]
fn doctor_reports_foreign_key_violations() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open forge db");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("disable foreign keys for corruption fixture");
    connection
        .execute(
            "INSERT INTO attempts (id, repo_id, intent_id, base_head, status, created_at_ms)
             VALUES ('attempt_dangling', 'repo_missing', 'intent_missing', 'HEAD', 'active', 1)",
            [],
        )
        .expect("insert dangling attempt");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue.as_str().unwrap().contains("foreign key violation")));
}
