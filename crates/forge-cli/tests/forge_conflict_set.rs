mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
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

/// Open the repo's `.forge/forge.db` directly and return every `conflict_sets`
/// row's `(context, paths_json)` pair.
fn conflict_set_rows(repo: &TestRepo) -> Vec<(String, String)> {
    let database_path = repo.path().join(".forge/forge.db");
    let connection = rusqlite::Connection::open(&database_path).expect("open forge.db");
    let mut stmt = connection
        .prepare("SELECT context, paths_json FROM conflict_sets ORDER BY rowid")
        .expect("prepare conflict_sets query");
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query conflict_sets")
        .collect::<Result<Vec<(String, String)>, _>>()
        .expect("collect conflict_sets");
    rows
}

/// `init` + an attempt with one saved normal file + a proposal, leaving HEAD at
/// the attempt's base. Mirrors `forge_accept_export`'s `prepare_proposal`.
fn prepare_proposal(repo: &TestRepo) {
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "conflict-set proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "changed\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
}

/// Move HEAD with a fresh git commit so the proposal's base becomes stale.
fn move_head(repo: &TestRepo, file: &str) {
    std::fs::write(repo.path().join(file), "move head\n").expect("write head-moving file");
    git(repo.path(), &["add", file]);
    git(repo.path(), &["commit", "-m", "move target"]);
}

#[test]
fn accept_stale_base_persists_conflict_set() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    move_head(&repo, "stale-before-accept.txt");

    let output = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(output["errors"][0]["code"], "STALE_BASE");

    let rows = conflict_set_rows(&repo);
    assert_eq!(rows.len(), 1, "exactly one conflict_sets row expected");
    assert_eq!(rows[0].0, "stale_base_accept");
    let paths: Value = serde_json::from_str(&rows[0].1).expect("parse paths_json");
    assert!(
        paths.get("expected_head").is_some(),
        "expected_head key missing: {}",
        rows[0].1
    );
    assert!(
        paths.get("actual_head").is_some(),
        "actual_head key missing: {}",
        rows[0].1
    );
}

#[test]
fn export_stale_base_persists_conflict_set() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    // Accept while HEAD still matches the base so the decision succeeds...
    repo.forge().args(["--json", "accept"]).assert().success();
    // ...then move HEAD so the export sees a stale base.
    move_head(&repo, "stale-before-export.txt");

    let output = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/stale"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "STALE_BASE");

    let rows = conflict_set_rows(&repo);
    assert_eq!(rows.len(), 1, "exactly one conflict_sets row expected");
    assert_eq!(rows[0].0, "stale_base_export");
    let paths: Value = serde_json::from_str(&rows[0].1).expect("parse paths_json");
    assert!(
        paths.get("expected_head").is_some(),
        "expected_head key missing: {}",
        rows[0].1
    );
    assert!(
        paths.get("actual_head").is_some(),
        "actual_head key missing: {}",
        rows[0].1
    );
}

#[test]
fn happy_path_writes_no_conflict_set() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);

    repo.forge().args(["--json", "accept"]).assert().success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/clean"])
        .assert()
        .success();

    assert!(
        conflict_set_rows(&repo).is_empty(),
        "happy-path accept + export must write zero conflict_sets rows"
    );
}
