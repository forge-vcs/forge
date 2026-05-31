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
fn doctor_reports_half_applied_worktree_from_leftover_restore_temp() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    // Healthy repo: the NER-132 categories are present and empty.
    let healthy = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(healthy["data"]["ok"], true);
    assert!(healthy["data"]["half_applied_worktrees"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(healthy["data"]["dangling_content_refs"]
        .as_array()
        .unwrap()
        .is_empty());

    // Simulate a restore killed mid-flight: a `.forge-restore-*` temp left in a
    // worktree subdirectory (tempfile's Drop does not run on a hard kill). doctor
    // scans the worktree — not just `.forge/tmp` — so it must flag this.
    let nested = repo.path().join("src");
    std::fs::create_dir_all(&nested).expect("create nested worktree dir");
    std::fs::write(nested.join(".forge-restore-abc123"), "partial").expect("plant restore temp");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(report["data"]["half_applied_worktrees"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path.as_str().unwrap().contains(".forge-restore-abc123")));
    assert!(report["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue.as_str().unwrap().contains("half-applied worktree")));
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
fn gc_dry_run_does_not_flag_native_base_commit_as_unreachable() {
    // NER-138 Phase 7 slice 2: native commit objects now enter all_object_ids, so gc
    // reachability must seed from the ref-store HEAD. Otherwise the genesis commit — which
    // every attempt's base_head points at — is reported as garbage (a misleading report
    // today and a latent data-loss trap before slice-3 gc deletion).
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json_output(
        repo.forge()
            .args(["--json", "start", "native gc"])
            .assert()
            .success(),
    );
    let base_head = started["data"]["base_head"].as_str().unwrap().to_string();
    assert!(
        base_head.starts_with("f1:commit:"),
        "native base: {base_head}"
    );
    std::fs::write(repo.path().join("a.txt"), "x\n").expect("write file");
    repo.forge().args(["--json", "save"]).assert().success();

    let gc = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    let unreachable: Vec<String> = gc["data"]["unreachable_native_objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert!(
        !unreachable.contains(&base_head),
        "the live base commit must not be reported as unreachable: {unreachable:?}"
    );
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
    // The failure is also surfaced under the dedicated machine-checkable category
    // (NER-132 U7), not only the generic issues list.
    assert!(missing["data"]["dangling_content_refs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|reference| reference
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

/// Drive a native repo through `init → start → save → run → propose → check → accept`
/// and a `checkout` of the accepted commit, so a `commit_checked_out` view row exists —
/// a reachability root that is reachable ONLY through the op-log (a checkout writes no
/// `decisions` row). Returns the accepted commit id.
fn native_repo_with_a_checkout(repo: &TestRepo) -> String {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "gc fail closed"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "native\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"]
        .as_str()
        .expect("accept returns a native commit id")
        .to_string();
    // Check out that commit so a `commit_checked_out` view row (a root reachable only
    // through the op-log) exists for the corruption tests to target.
    repo.forge()
        .args(["--json", "checkout", &commit_id])
        .assert()
        .success();
    commit_id
}

/// NER-143 R6: `gc --dry-run` must FAIL CLOSED when a ledger row that determines a
/// reachability root is corrupt — a malformed `views.state_json` could hide a live
/// `checkout`-target commit (reachable only through the op-log), and silently
/// under-counting roots would mark a live object for deletion once Phase 8 grants
/// real mark-sweep deletion. Corrupt ONLY the `commit_checked_out` (root-determining)
/// view row, so the test proves the failure is attributable to a row that actually
/// determines a root — and would survive even a future scan narrowed to commit-bearing
/// views. The failure must be path-free (S1).
#[test]
fn gc_fails_closed_on_a_corrupt_ledger_view_row() {
    let repo = TestRepo::new_git();
    native_repo_with_a_checkout(&repo);

    // A clean gc dry-run succeeds before the corruption.
    repo.forge()
        .args(["--json", "gc", "--dry-run"])
        .assert()
        .success();

    // Corrupt ONLY the checkout view row's state_json (the one that names a root reachable
    // only through the op-log), so the root-enumeration scan cannot parse it.
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let corrupted = connection
        .execute(
            "UPDATE views SET state_json = ?1 WHERE state_json LIKE '%commit_checked_out%'",
            ["{ not json"],
        )
        .expect("corrupt the checkout view state_json");
    assert_eq!(
        corrupted, 1,
        "exactly one checkout view row must be corrupted"
    );

    let output = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .failure(),
    );
    assert_eq!(output["status"], "error");
    assert_eq!(output["errors"][0]["code"], "COMMAND_FAILED");
    // S1: the failure must not leak a filesystem path in any envelope string.
    let needle = repo.path().to_string_lossy();
    let rendered = serde_json::to_string(&output).expect("re-serialize envelope");
    assert!(
        !rendered.contains(&*needle),
        "gc fail-closed leaked a path: {rendered}"
    );
}

/// NER-143 R6 (second fail-closed branch): a view row that is VALID json but names an
/// unparseable reachability root (a `commit_id` that is not a real `f1:` object id) must
/// also fail gc closed — the root set is untrustworthy. Exercises the `ObjectId::parse`
/// guard distinctly from the corrupt-json guard above. Path-free (S1).
#[test]
fn gc_fails_closed_on_an_unparseable_reachability_root() {
    let repo = TestRepo::new_git();
    native_repo_with_a_checkout(&repo);

    // Replace the checkout view's commit_id with valid JSON carrying a garbage id: it parses
    // as JSON (so it clears the first guard) but fails ObjectId::parse in the root loop.
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let corrupted = connection
        .execute(
            "UPDATE views SET state_json = ?1 WHERE state_json LIKE '%commit_checked_out%'",
            [r#"{"lifecycle":"commit_checked_out","commit_id":"not-an-object-id"}"#],
        )
        .expect("plant an unparseable root id");
    assert_eq!(
        corrupted, 1,
        "exactly one checkout view row must be rewritten"
    );

    let output = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .failure(),
    );
    assert_eq!(output["status"], "error");
    assert_eq!(output["errors"][0]["code"], "COMMAND_FAILED");
    let needle = repo.path().to_string_lossy();
    let rendered = serde_json::to_string(&output).expect("re-serialize envelope");
    assert!(
        !rendered.contains(&*needle),
        "gc fail-closed leaked a path: {rendered}"
    );
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
