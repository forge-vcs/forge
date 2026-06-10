mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn start_save_and_restore_snapshot() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let started = json_output(
        repo.forge()
            .args(["--json", "start", "make readme useful"])
            .assert()
            .success(),
    );
    assert!(started["data"]["attempt_id"]
        .as_str()
        .unwrap()
        .starts_with("attempt_"));

    std::fs::write(repo.path().join("README.md"), "changed once\n").expect("write readme");
    let first = json_output(repo.forge().args(["--json", "save"]).assert().success());
    assert_eq!(first["data"]["changed_paths"][0], "README.md");
    assert!(first["data"]["parent_snapshot_id"].is_null());

    std::fs::write(repo.path().join("README.md"), "changed twice\n").expect("write readme");
    std::fs::write(repo.path().join("later.txt"), "created later\n").expect("write later file");
    let second = json_output(repo.forge().args(["--json", "save"]).assert().success());
    assert_eq!(
        second["data"]["parent_snapshot_id"],
        first["data"]["snapshot_id"]
    );

    let first_snapshot = first["data"]["snapshot_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "restore", first_snapshot, "--yes"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "changed once\n"
    );
    assert!(!repo.path().join("later.txt").exists());
}

#[test]
fn save_requires_active_attempt() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let output = json_output(repo.forge().args(["--json", "save"]).assert().failure());
    assert_eq!(output["errors"][0]["code"], "NO_ACTIVE_ATTEMPT");
}

#[test]
fn restore_refuses_unsaved_dirty_worktree() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "restore safely"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "saved\n").expect("write readme");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let snapshot_id = saved["data"]["snapshot_id"].as_str().unwrap();

    std::fs::write(repo.path().join("README.md"), "unsaved\n").expect("write unsaved readme");
    let output = json_output(
        repo.forge()
            .args(["--json", "restore", snapshot_id, "--yes"])
            .assert()
            .failure(),
    );

    assert_eq!(output["errors"][0]["code"], "DIRTY_WORKTREE");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "unsaved\n"
    );
}

#[test]
fn duplicate_request_id_replays_without_second_mutation() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let first = json_output(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "req-start-once",
                "start",
                "idempotent",
            ])
            .assert()
            .success(),
    );
    let second = json_output(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "req-start-once",
                "start",
                "idempotent",
            ])
            .assert()
            .success(),
    );

    assert_eq!(second["operation_id"], first["operation_id"]);
    assert_eq!(second["data"]["idempotent_replay"], true);
}

#[test]
fn failed_request_id_replays_original_failure() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let first = json_output(
        repo.forge()
            .args(["--json", "--request-id", "req-failed-save", "save"])
            .assert()
            .failure(),
    );
    let second = json_output(
        repo.forge()
            .args(["--json", "--request-id", "req-failed-save", "save"])
            .assert()
            .failure(),
    );

    assert_eq!(second["operation_id"], first["operation_id"]);
    assert_eq!(second["errors"][0]["code"], "NO_ACTIVE_ATTEMPT");

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let failed_ops: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE request_id = 'req-failed-save'",
            [],
            |row| row.get(0),
        )
        .expect("count failed operations");
    assert_eq!(failed_ops, 1);
}

#[test]
fn request_id_reuse_for_different_command_conflicts() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    repo.forge()
        .args([
            "--json",
            "--request-id",
            "req-command-scope",
            "start",
            "scoped",
        ])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args(["--json", "--request-id", "req-command-scope", "save"])
            .assert()
            .failure(),
    );

    assert_eq!(output["errors"][0]["code"], "REQUEST_ID_CONFLICT");
}

#[test]
fn save_excludes_secret_risk_paths_from_snapshot_and_changed_paths() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "avoid secrets"])
        .assert()
        .success();

    std::fs::write(repo.path().join("README.md"), "safe change\n").expect("write readme");
    std::fs::write(repo.path().join(".env"), "TOKEN=raw-secret\n").expect("write env");
    let output = json_output(repo.forge().args(["--json", "save"]).assert().success());

    let changed_paths = output["data"]["changed_paths"].as_array().unwrap();
    assert!(changed_paths.iter().any(|path| path == "README.md"));
    assert!(!changed_paths.iter().any(|path| path == ".env"));

    let content_ref = output["data"]["content_ref"].as_str().unwrap();
    let tree = content_ref.strip_prefix("git-tree:").unwrap();
    let tree_paths = git(repo.path(), &["ls-tree", "-r", "--name-only", tree]);
    assert!(tree_paths.contains("README.md"));
    assert!(!tree_paths.lines().any(|path| path == ".env"));
}

#[test]
fn save_excludes_tracked_secret_risk_paths_from_snapshot() {
    let repo = TestRepo::new_git();
    std::fs::write(repo.path().join(".env"), "TOKEN=raw-secret\n").expect("write env");
    git(repo.path(), &["add", ".env"]);
    git(repo.path(), &["commit", "-m", "add tracked env"]);

    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "avoid tracked secrets"])
        .assert()
        .success();
    std::fs::write(repo.path().join(".env"), "TOKEN=changed-secret\n").expect("write env");

    let output = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let content_ref = output["data"]["content_ref"].as_str().unwrap();
    let tree = content_ref.strip_prefix("git-tree:").unwrap();
    let tree_paths = git(repo.path(), &["ls-tree", "-r", "--name-only", tree]);
    assert!(!tree_paths.lines().any(|path| path == ".env"));
}

#[test]
fn native_save_and_restore_materializes_exact_tree() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "native restore"])
        .assert()
        .success();

    std::fs::write(repo.path().join("README.md"), "native once\n").expect("write readme");
    std::fs::write(repo.path().join("safe.txt"), "safe\n").expect("write safe");
    std::fs::write(repo.path().join(".env"), "TOKEN=secret\n").expect("write env");
    std::fs::write(repo.path().join(".gitignore"), "ignored.log\n").expect("write gitignore");
    std::fs::write(repo.path().join("ignored.log"), "ignored build\n").expect("write ignored");
    let first = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let first_ref = first["data"]["content_ref"].as_str().unwrap();
    assert!(first_ref.starts_with("forge-tree:f1:tree:sha256:"));
    let changed_paths = first["data"]["changed_paths"].as_array().unwrap();
    assert!(changed_paths.iter().any(|path| path == "README.md"));
    assert!(changed_paths.iter().any(|path| path == "safe.txt"));
    assert!(!changed_paths.iter().any(|path| path == ".env"));
    assert!(!changed_paths.iter().any(|path| path == "ignored.log"));
    assert_native_objects_do_not_contain(repo.path(), "TOKEN=secret");
    assert_native_objects_do_not_contain(repo.path(), ".env");
    assert_native_objects_do_not_contain(repo.path(), "ignored build");

    std::fs::write(repo.path().join("README.md"), "native twice\n").expect("write readme");
    std::fs::write(repo.path().join("later.txt"), "later\n").expect("write later");
    repo.forge().args(["--json", "save"]).assert().success();

    let first_snapshot = first["data"]["snapshot_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "restore", first_snapshot, "--yes"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "native once\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("safe.txt")).unwrap(),
        "safe\n"
    );
    assert!(!repo.path().join("later.txt").exists());
    assert_eq!(
        std::fs::read_to_string(repo.path().join(".env")).unwrap(),
        "TOKEN=secret\n"
    );
}

#[test]
fn native_restore_refuses_unsaved_dirty_worktree() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "native dirty restore"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "saved\n").expect("write readme");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let snapshot_id = saved["data"]["snapshot_id"].as_str().unwrap();

    std::fs::write(repo.path().join("README.md"), "unsaved\n").expect("write readme");
    std::fs::write(repo.path().join("unsaved.txt"), "unsaved new\n").expect("write unsaved");
    let output = json_output(
        repo.forge()
            .args(["--json", "restore", snapshot_id, "--yes"])
            .assert()
            .failure(),
    );

    assert_eq!(output["errors"][0]["code"], "DIRTY_WORKTREE");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "unsaved\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("unsaved.txt")).unwrap(),
        "unsaved new\n"
    );
}

fn assert_native_objects_do_not_contain(repo_path: &std::path::Path, needle: &str) {
    let objects = repo_path.join(".forge/objects/sha256");
    for prefix in std::fs::read_dir(objects).expect("object prefixes") {
        let prefix = prefix.expect("object prefix");
        for object in std::fs::read_dir(prefix.path()).expect("objects") {
            let object = object.expect("object");
            let bytes = std::fs::read(object.path()).expect("read object");
            let text = String::from_utf8_lossy(&bytes);
            assert!(
                !text.contains(needle),
                "native object {} contains {needle:?}",
                object.path().display()
            );
        }
    }
}

/// NER-255: an idempotent `save` replay must return the ORIGINAL result payload (the
/// snapshot_id / content_ref / changed_paths the agent needs for crash recovery), not
/// just {idempotent_replay, request_id}. The first response's `data` is persisted into
/// the operation view state under `replay_data` (in the same txn that records the op);
/// the replay merges it back plus the original operation_id.
#[test]
fn save_request_id_replay_returns_original_payload() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "idempotent save"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "saved once\n").expect("write readme");

    let first = json_output(
        repo.forge()
            .args(["--json", "--request-id", "save-once", "save"])
            .assert()
            .success(),
    );
    let snapshot_id = first["data"]["snapshot_id"]
        .as_str()
        .expect("first save surfaces snapshot_id")
        .to_string();
    assert_eq!(first["data"]["changed_paths"][0], "README.md");

    let replay = json_output(
        repo.forge()
            .args(["--json", "--request-id", "save-once", "save"])
            .assert()
            .success(),
    );
    // The replay is flagged AND carries the original ids byte-faithfully.
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["snapshot_id"], snapshot_id);
    assert_eq!(replay["data"]["content_ref"], first["data"]["content_ref"]);
    assert_eq!(
        replay["data"]["changed_paths"],
        first["data"]["changed_paths"]
    );
    assert_eq!(
        replay["data"]["parent_snapshot_id"],
        first["data"]["parent_snapshot_id"]
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["operation_id"], first["operation_id"]);

    // The replay did NOT write a second snapshot.
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let snapshot_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
        .expect("count snapshots");
    assert_eq!(
        snapshot_count, 1,
        "replay must not create a second snapshot"
    );

    // Contract preserved: reusing the same request id for a DIFFERENT command still
    // conflicts (the command-scope check runs before the replay-payload merge).
    let conflict = json_output(
        repo.forge()
            .args(["--json", "--request-id", "save-once", "propose"])
            .assert()
            .failure(),
    );
    assert_eq!(conflict["errors"][0]["code"], "REQUEST_ID_CONFLICT");
}

/// NER-255 (doc-review amendment): the `start` replay must also carry its original
/// payload — closing the coverage gap where the pre-existing
/// `duplicate_request_id_replays_without_second_mutation` only asserts operation_id /
/// idempotent_replay and would not catch a wrong field name in the stored replay_data.
#[test]
fn start_request_id_replay_returns_original_payload() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let first = json_output(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "start-once",
                "start",
                "recoverable",
            ])
            .assert()
            .success(),
    );
    let attempt_id = first["data"]["attempt_id"]
        .as_str()
        .expect("first start surfaces attempt_id")
        .to_string();
    let intent_id = first["data"]["intent_id"]
        .as_str()
        .expect("first start surfaces intent_id")
        .to_string();

    let replay = json_output(
        repo.forge()
            .args([
                "--json",
                "--request-id",
                "start-once",
                "start",
                "recoverable",
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["attempt_id"], attempt_id);
    assert_eq!(replay["data"]["intent_id"], intent_id);
    assert_eq!(replay["data"]["base_head"], first["data"]["base_head"]);
    assert_eq!(replay["data"]["attached"], first["data"]["attached"]);
    assert_eq!(
        replay["data"]["workspace_path"],
        first["data"]["workspace_path"]
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
}

/// NER-255 (code-review amendment): the `propose` replay must also carry its original
/// payload. `propose` is listed among the covered lifecycle commands but had no test, so a
/// typo in a stored field name (e.g. `revision_id` vs `proposal_revision_id`) would have
/// been silently invisible. This pins every id an agent needs to recover from a crash
/// between `propose` and reading its response.
#[test]
fn propose_request_id_replay_returns_original_payload() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "idempotent propose"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "proposed once\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();

    let first = json_output(
        repo.forge()
            .args(["--json", "--request-id", "propose-once", "propose"])
            .assert()
            .success(),
    );
    let proposal_id = first["data"]["proposal_id"]
        .as_str()
        .expect("first propose surfaces proposal_id")
        .to_string();

    let replay = json_output(
        repo.forge()
            .args(["--json", "--request-id", "propose-once", "propose"])
            .assert()
            .success(),
    );
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["proposal_id"], proposal_id);
    // Every id the implementer summary claims is covered, asserted byte-faithfully so a
    // wrong stored field name is caught.
    for field in [
        "proposal_revision_id",
        "attempt_id",
        "snapshot_id",
        "base_head",
        "content_ref",
        "changed_paths",
    ] {
        assert_eq!(
            replay["data"][field], first["data"][field],
            "replayed propose must preserve `{field}`"
        );
    }
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["operation_id"], first["operation_id"]);

    // The replay did NOT write a second proposal.
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let proposal_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM proposals", [], |row| row.get(0))
        .expect("count proposals");
    assert_eq!(
        proposal_count, 1,
        "replay must not create a second proposal"
    );

    // Reusing the same request id for a DIFFERENT command still conflicts.
    let conflict = json_output(
        repo.forge()
            .args(["--json", "--request-id", "propose-once", "save"])
            .assert()
            .failure(),
    );
    assert_eq!(conflict["errors"][0]["code"], "REQUEST_ID_CONFLICT");
}

fn git(cwd: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
