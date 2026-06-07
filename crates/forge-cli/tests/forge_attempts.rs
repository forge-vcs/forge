mod common;

use common::{forge_in, TestRepo};
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn clear_attached_attempt(repo: &TestRepo) {
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    connection
        .execute("UPDATE current_state SET attached_attempt_id = NULL", [])
        .expect("clear attachment");
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

#[test]
fn start_attaches_created_attempt() {
    // The "migrate an existing DB" half of this test used to DELETE the version-2
    // row while leaving its inline column in place — an artificial one-column state
    // that the version-gated migration runner (NER-133 U3) would try to re-ALTER
    // and fail with "duplicate column name". Genuine v1->v2 upgrade convergence is
    // now covered by `forge-store`'s `migrations::tests` (genesis case B), so this
    // test focuses on the CLI attach behavior against a normal at-HEAD DB.
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let started = json_output(
        repo.forge()
            .args(["--json", "start", "first attempt"])
            .assert()
            .success(),
    );
    let attempts = json_output(
        repo.forge()
            .args(["--json", "attempt", "list"])
            .assert()
            .success(),
    );

    assert_eq!(started["data"]["attached"], true);
    assert_eq!(attempts["data"]["attempts"][0]["attached"], true);
    assert_eq!(
        attempts["data"]["attempts"][0]["attempt_id"],
        started["data"]["attempt_id"]
    );
}

#[test]
fn attempt_start_lists_and_shows_competing_attempts() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );

    assert_eq!(second["data"]["intent_id"], intent_id);
    let listed = json_output(
        repo.forge()
            .args(["--json", "attempt", "list"])
            .assert()
            .success(),
    );
    assert_eq!(listed["data"]["attempts"].as_array().unwrap().len(), 2);

    let shown = json_output(
        repo.forge()
            .args([
                "--json",
                "attempt",
                "show",
                second["data"]["attempt_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    assert_eq!(shown["data"]["attempt"]["intent_id"], intent_id);
    assert!(shown["data"]["proposals"].as_array().unwrap().is_empty());
}

#[test]
fn ambiguous_attempt_requires_explicit_selector() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "attempt", "start", "--intent", intent_id])
        .assert()
        .success();
    clear_attached_attempt(&repo);

    std::fs::write(repo.path().join("README.md"), "ambiguous\n").expect("write readme");
    let output = json_output(repo.forge().args(["--json", "save"]).assert().failure());
    assert_eq!(output["errors"][0]["code"], "AMBIGUOUS_ATTEMPT");

    let saved = json_output(
        repo.forge()
            .args([
                "--json",
                "save",
                "--attempt",
                first["data"]["attempt_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    assert_eq!(saved["data"]["attempt_id"], first["data"]["attempt_id"]);

    let shown = json_output(repo.forge().args(["--json", "show"]).assert().failure());
    assert_eq!(shown["errors"][0]["code"], "AMBIGUOUS_ATTEMPT");
}

#[test]
fn unknown_attempt_selector_is_typed() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "known"])
        .assert()
        .success();

    let output = json_output(
        repo.forge()
            .args(["--json", "save", "--attempt", "attempt_missing"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "UNKNOWN_ATTEMPT");
}

#[test]
fn attach_materializes_snapshot_and_refuses_dirty_work() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let first_attempt = first["data"]["attempt_id"].as_str().unwrap();
    std::fs::write(repo.path().join("README.md"), "attempt one\n").expect("write first");
    repo.forge().args(["--json", "save"]).assert().success();

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_attempt = second["data"]["attempt_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "attempt", "attach", second_attempt])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "hello\n"
    );

    std::fs::write(repo.path().join("README.md"), "unsaved\n").expect("write dirty");
    let dirty = json_output(
        repo.forge()
            .args(["--json", "attempt", "attach", first_attempt])
            .assert()
            .failure(),
    );
    assert_eq!(dirty["errors"][0]["code"], "DIRTY_WORKTREE");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "unsaved\n"
    );
}

#[test]
fn attach_materializes_native_base_via_forge_tree_ref() {
    // NER-138 Phase 7 slice 2: a native repo's base_head is an f1:commit: id (not a git
    // SHA). Attaching a fresh competing attempt materializes its base through
    // base_content_ref -> forge-tree: -> the native backend (prefix dispatch), with no git
    // — proving native base anchoring round-trips through `attempt attach`.
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    assert!(
        first["data"]["base_head"]
            .as_str()
            .unwrap()
            .starts_with("f1:commit:sha256:"),
        "native base_head must be a commit id: {}",
        first["data"]["base_head"]
    );
    // Modify + save under the first attempt; the genesis base remains README = "hello".
    std::fs::write(repo.path().join("README.md"), "attempt one\n").expect("write first");
    repo.forge().args(["--json", "save"]).assert().success();

    // A competing attempt under the same intent; attaching it (no snapshot yet)
    // materializes the native BASE tree via the forge-tree: ref.
    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_attempt = second["data"]["attempt_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "attempt", "attach", second_attempt])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "hello\n",
        "attach must materialize the native base via the forge-tree: ref"
    );
}

#[test]
fn native_attempts_surface_and_materialize_workspace_paths() {
    let repo = TestRepo::new_git();
    std::fs::write(repo.path().join(".env"), "TOKEN=committed\n").expect("write env");
    git(repo.path(), &["add", ".env"]);
    git(repo.path(), &["commit", "-m", "track env"]);

    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json_output(
        repo.forge()
            .args(["--json", "start", "workspace"])
            .assert()
            .success(),
    );
    let workspace_path = started["data"]["workspace_path"].as_str().unwrap();
    assert!(workspace_path.starts_with(".forge/worktrees/"));
    let workspace = repo.path().join(workspace_path);
    assert!(workspace
        .join(forge_content::WORKSPACE_MARKER_FILE)
        .exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join("README.md")).unwrap(),
        "hello\n"
    );
    assert!(
        !workspace.join(".env").exists(),
        "secret-risk paths must not materialize into attempt workspaces"
    );

    let listed = json_output(
        repo.forge()
            .args(["--json", "attempt", "list"])
            .assert()
            .success(),
    );
    assert_eq!(
        listed["data"]["attempts"][0]["workspace_path"],
        started["data"]["workspace_path"]
    );
    let shown = json_output(
        repo.forge()
            .args([
                "--json",
                "attempt",
                "show",
                started["data"]["attempt_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        shown["data"]["attempt"]["workspace_path"],
        started["data"]["workspace_path"]
    );
}

#[test]
fn native_attempt_workspaces_are_isolated_and_bind_saves() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "parallel"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let first_workspace = repo
        .path()
        .join(first["data"]["workspace_path"].as_str().unwrap());

    std::fs::write(first_workspace.join("README.md"), "attempt one\n").expect("write first");
    let first_save = json_output(
        forge_in(&first_workspace)
            .args(["--json", "save"])
            .assert()
            .success(),
    );
    assert_eq!(
        first_save["data"]["attempt_id"],
        first["data"]["attempt_id"]
    );

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_workspace = repo
        .path()
        .join(second["data"]["workspace_path"].as_str().unwrap());

    assert_eq!(
        std::fs::read_to_string(first_workspace.join("README.md")).unwrap(),
        "attempt one\n"
    );
    assert_eq!(
        std::fs::read_to_string(second_workspace.join("README.md")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "hello\n",
        "workspace saves must not mutate the repo-root checkout"
    );

    std::fs::write(second_workspace.join("README.md"), "attempt two\n").expect("write second");
    let second_save = json_output(
        forge_in(&second_workspace)
            .args(["--json", "save"])
            .assert()
            .success(),
    );
    assert_eq!(
        second_save["data"]["attempt_id"],
        second["data"]["attempt_id"]
    );
    assert_eq!(
        std::fs::read_to_string(first_workspace.join("README.md")).unwrap(),
        "attempt one\n"
    );
}

#[test]
fn workspace_save_does_not_poison_repo_root_dirty_baseline() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "parallel merge"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let first_attempt = first["data"]["attempt_id"].as_str().unwrap();
    let first_workspace = repo
        .path()
        .join(first["data"]["workspace_path"].as_str().unwrap());
    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_attempt = second["data"]["attempt_id"].as_str().unwrap();
    let second_workspace = repo
        .path()
        .join(second["data"]["workspace_path"].as_str().unwrap());

    std::fs::write(first_workspace.join("README.md"), "attempt one\n").expect("write first");
    forge_in(&first_workspace)
        .args(["--json", "save"])
        .assert()
        .success();
    forge_in(&first_workspace)
        .args(["--json", "run", "--", "true"])
        .assert()
        .success();
    let first_proposal = json_output(
        forge_in(&first_workspace)
            .args(["--json", "propose"])
            .assert()
            .success(),
    );

    std::fs::write(second_workspace.join("OTHER.md"), "attempt two\n").expect("write second");
    forge_in(&second_workspace)
        .args(["--json", "save"])
        .assert()
        .success();
    forge_in(&second_workspace)
        .args(["--json", "run", "--", "true"])
        .assert()
        .success();
    let second_proposal = json_output(
        forge_in(&second_workspace)
            .args(["--json", "propose"])
            .assert()
            .success(),
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "hello\n",
        "workspace saves must leave the repo root at its own baseline"
    );

    repo.forge()
        .args([
            "--json",
            "check",
            "--attempt",
            first_attempt,
            "--proposal",
            first_proposal["data"]["proposal_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "accept",
            "--attempt",
            first_attempt,
            "--proposal",
            first_proposal["data"]["proposal_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "hello\n",
        "accept records the decision but does not materialize into the repo root"
    );

    let merged = json_output(
        repo.forge()
            .args([
                "--json",
                "merge",
                "--proposal",
                second_proposal["data"]["proposal_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    assert_eq!(merged["data"]["merged"], true);
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "attempt one\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("OTHER.md")).unwrap(),
        "attempt two\n"
    );
    assert_eq!(
        second_attempt,
        second_proposal["data"]["attempt_id"].as_str().unwrap()
    );
}

#[test]
fn attach_base_revision_preserves_tracked_secret_risk_paths() {
    let repo = TestRepo::new_git();
    std::fs::write(repo.path().join(".env"), "TOKEN=committed\n").expect("write env");
    git(repo.path(), &["add", ".env"]);
    git(repo.path(), &["commit", "-m", "track env"]);

    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "secrets"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    std::fs::write(repo.path().join("README.md"), "saved\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    std::fs::write(repo.path().join(".env"), "TOKEN=local\n").expect("write local env");

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    repo.forge()
        .args([
            "--json",
            "attempt",
            "attach",
            second["data"]["attempt_id"].as_str().unwrap(),
        ])
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(repo.path().join(".env")).unwrap(),
        "TOKEN=local\n"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "hello\n"
    );
}

#[test]
fn ambiguous_proposal_requires_explicit_selector() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "choose proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "proposal one\n").expect("write one");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();
    std::fs::write(repo.path().join("README.md"), "proposal two\n").expect("write two");
    repo.forge().args(["--json", "save"]).assert().success();
    // A passing command on proposal two's snapshot so the evidence gate lets the
    // explicit `accept --proposal <two>` proceed (NER-135).
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let second = json_output(repo.forge().args(["--json", "propose"]).assert().success());

    let ambiguous = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(ambiguous["errors"][0]["code"], "AMBIGUOUS_PROPOSAL");

    let accepted = json_output(
        repo.forge()
            .args([
                "--json",
                "accept",
                "--proposal",
                second["data"]["proposal_id"].as_str().unwrap(),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        accepted["data"]["proposal_id"],
        second["data"]["proposal_id"]
    );
    assert_eq!(
        accepted["data"]["proposal_revision_id"],
        second["data"]["proposal_revision_id"]
    );
}

#[test]
fn competing_attempt_loop_exports_selected_proposal() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let first_attempt = first["data"]["attempt_id"].as_str().unwrap();

    std::fs::write(repo.path().join("README.md"), "attempt one\n").expect("write first");
    repo.forge()
        .args(["--json", "save", "--attempt", first_attempt])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "run",
            "--attempt",
            first_attempt,
            "--",
            "sh",
            "-c",
            "true",
        ])
        .assert()
        .success();
    let first_proposal = json_output(
        repo.forge()
            .args(["--json", "propose", "--attempt", first_attempt])
            .assert()
            .success(),
    );

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_attempt = second["data"]["attempt_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "attempt", "attach", second_attempt])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "attempt two\n").expect("write second");
    repo.forge()
        .args(["--json", "save", "--attempt", second_attempt])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "run",
            "--attempt",
            second_attempt,
            "--",
            "sh",
            "-c",
            "true",
        ])
        .assert()
        .success();
    let second_proposal = json_output(
        repo.forge()
            .args(["--json", "propose", "--attempt", second_attempt])
            .assert()
            .success(),
    );

    let proposals = json_output(
        repo.forge()
            .args(["--json", "proposal", "list", "--attempt", second_attempt])
            .assert()
            .success(),
    );
    assert_eq!(proposals["data"]["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(
        proposals["data"]["proposals"][0]["proposal_id"],
        second_proposal["data"]["proposal_id"]
    );

    repo.forge()
        .args([
            "--json",
            "check",
            "--attempt",
            second_attempt,
            "--proposal",
            second_proposal["data"]["proposal_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "accept",
            "--attempt",
            second_attempt,
            "--proposal",
            second_proposal["data"]["proposal_id"].as_str().unwrap(),
        ])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "export",
            "branch",
            "--attempt",
            second_attempt,
            "--proposal",
            second_proposal["data"]["proposal_id"].as_str().unwrap(),
            "forge/selected-attempt",
        ])
        .assert()
        .success();

    assert_eq!(
        git(repo.path(), &["show", "forge/selected-attempt:README.md"]),
        "attempt two\n"
    );
    assert_ne!(
        first_proposal["data"]["proposal_id"],
        second_proposal["data"]["proposal_id"]
    );
}

#[test]
fn save_records_target_attempt_not_materialized_attempt() {
    // NER-134 exit criterion: `save --attempt X` must NOT record a different attempt's
    // content when X is not the attempt the worktree is materialized for. Reproduces the
    // footgun directly — `attempt start` neither materializes nor attaches, so after
    // creating A2 the worktree still holds A1's content and `attached_attempt_id` still
    // points at A1.
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let first_attempt = first["data"]["attempt_id"].as_str().unwrap();

    std::fs::write(repo.path().join("README.md"), "attempt one\n").expect("write first");
    repo.forge()
        .args(["--json", "save", "--attempt", first_attempt])
        .assert()
        .success();

    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_attempt = second["data"]["attempt_id"].as_str().unwrap();

    // Worktree still holds A1's content and attached == A1. Saving to A2 must be refused
    // with the typed mismatch error, carrying both opaque ids and a non-retryable verdict.
    let mismatch = json_output(
        repo.forge()
            .args(["--json", "save", "--attempt", second_attempt])
            .assert()
            .failure(),
    );
    assert_eq!(mismatch["errors"][0]["code"], "ATTEMPT_WORKTREE_MISMATCH");
    assert_eq!(
        mismatch["errors"][0]["details"]["requested_attempt"],
        second_attempt
    );
    assert_eq!(
        mismatch["errors"][0]["details"]["attached_attempt"],
        first_attempt
    );
    assert_eq!(mismatch["retry"]["retryable"], false);

    // Nothing was recorded under A2.
    let shown_a2 = json_output(
        repo.forge()
            .args(["--json", "attempt", "show", second_attempt])
            .assert()
            .success(),
    );
    assert!(
        shown_a2["data"]["latest_snapshot"].is_null(),
        "no snapshot may exist for A2 after the refused save"
    );

    // The fix is to attach A2 first (re-materializes its base, re-binds the worktree);
    // then `save --attempt A2` records A2's own content.
    repo.forge()
        .args(["--json", "attempt", "attach", second_attempt])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "attempt two\n").expect("write second");
    let saved = json_output(
        repo.forge()
            .args(["--json", "save", "--attempt", second_attempt])
            .assert()
            .success(),
    );
    assert_eq!(saved["data"]["attempt_id"], second_attempt);
    let shown_after = json_output(
        repo.forge()
            .args(["--json", "attempt", "show", second_attempt])
            .assert()
            .success(),
    );
    assert!(
        !shown_after["data"]["latest_snapshot"].is_null(),
        "A2 now has its own snapshot"
    );
}

#[test]
fn restore_rejects_cross_attempt_snapshot() {
    // NER-134 Piece 1b: `restore <snapshot>` must refuse a snapshot owned by an attempt
    // other than the one the worktree is bound to — otherwise restore is a second
    // cross-attempt contamination vector (it would clobber the worktree with another
    // attempt's content while `attached_attempt_id` stays put).
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    let first = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = first["data"]["intent_id"].as_str().unwrap();
    let first_attempt = first["data"]["attempt_id"].as_str().unwrap();

    std::fs::write(repo.path().join("README.md"), "attempt one\n").expect("write first");
    let snap_one = json_output(
        repo.forge()
            .args(["--json", "save", "--attempt", first_attempt])
            .assert()
            .success(),
    );
    let snap_one_id = snap_one["data"]["snapshot_id"].as_str().unwrap();

    // Create + attach A2, then snapshot it so it has its own snapshot.
    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", intent_id])
            .assert()
            .success(),
    );
    let second_attempt = second["data"]["attempt_id"].as_str().unwrap();
    repo.forge()
        .args(["--json", "attempt", "attach", second_attempt])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "attempt two\n").expect("write two");
    let snap_two = json_output(
        repo.forge()
            .args(["--json", "save", "--attempt", second_attempt])
            .assert()
            .success(),
    );
    let snap_two_id = snap_two["data"]["snapshot_id"].as_str().unwrap();

    // Worktree is bound to A2 and clean vs A2's latest. Restoring A1's snapshot must be
    // refused with the typed mismatch error and must leave the worktree untouched.
    let mismatch = json_output(
        repo.forge()
            .args(["--json", "restore", snap_one_id, "--yes"])
            .assert()
            .failure(),
    );
    assert_eq!(mismatch["errors"][0]["code"], "ATTEMPT_WORKTREE_MISMATCH");
    assert_eq!(
        mismatch["errors"][0]["details"]["requested_attempt"],
        first_attempt
    );
    assert_eq!(
        mismatch["errors"][0]["details"]["attached_attempt"],
        second_attempt
    );
    // The mismatch is deterministic on both the save and restore surfaces — pin the
    // contract so a refactor can't silently flip restore's classification (review T1).
    assert_eq!(mismatch["retry"]["retryable"], false);
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "attempt two\n",
        "a refused restore must not clobber the worktree"
    );

    // Restoring A2's OWN snapshot is still allowed (worktree matches A2's latest).
    repo.forge()
        .args(["--json", "restore", snap_two_id, "--yes"])
        .assert()
        .success();
}
