mod common;

use common::TestRepo;
use forge_content::ContentBackend;
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

#[derive(Debug)]
struct ConflictSetRow {
    id: String,
    context: String,
    paths_json: String,
    base_content_ref: Option<String>,
    ours_content_ref: Option<String>,
    theirs_content_ref: Option<String>,
    generated_by_operation_id: Option<String>,
    resolver_backend: Option<String>,
    status: String,
    content_hash: Option<String>,
}

/// Open the repo's `.forge/forge.db` directly and return every `conflict_sets` row.
fn conflict_set_rows(repo: &TestRepo) -> Vec<ConflictSetRow> {
    let database_path = repo.path().join(".forge/forge.db");
    let connection = rusqlite::Connection::open(&database_path).expect("open forge.db");
    let mut stmt = connection
        .prepare(
            "SELECT id, context, paths_json, base_content_ref, ours_content_ref,
                    theirs_content_ref, generated_by_operation_id, resolver_backend,
                    status, content_hash
             FROM conflict_sets ORDER BY rowid",
        )
        .expect("prepare conflict_sets query");
    let rows = stmt
        .query_map([], |row| {
            Ok(ConflictSetRow {
                id: row.get(0)?,
                context: row.get(1)?,
                paths_json: row.get(2)?,
                base_content_ref: row.get(3)?,
                ours_content_ref: row.get(4)?,
                theirs_content_ref: row.get(5)?,
                generated_by_operation_id: row.get(6)?,
                resolver_backend: row.get(7)?,
                status: row.get(8)?,
                content_hash: row.get(9)?,
            })
        })
        .expect("query conflict_sets")
        .collect::<Result<Vec<ConflictSetRow>, _>>()
        .expect("collect conflict_sets");
    rows
}

fn path_conflict_count(repo: &TestRepo, conflict_set_id: &str) -> i64 {
    let database_path = repo.path().join(".forge/forge.db");
    let connection = rusqlite::Connection::open(&database_path).expect("open forge.db");
    connection
        .query_row(
            "SELECT COUNT(*) FROM path_conflicts WHERE conflict_set_id = ?1",
            [conflict_set_id],
            |row| row.get(0),
        )
        .expect("count path_conflicts")
}

fn native_tree_ref_for_current_worktree(repo: &TestRepo) -> String {
    let backend = forge_content_native::NativeContentBackend;
    backend
        .snapshot_worktree(repo.path())
        .expect("snapshot native worktree")
        .content_ref
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
    // A passing command on the proposed snapshot, so the default-mode check passes
    // and the evidence gate lets `accept` proceed (NER-135).
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
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
    let row = &rows[0];
    assert_eq!(row.context, "stale_base_accept");
    assert_eq!(
        row.generated_by_operation_id.as_deref(),
        output["operation_id"].as_str()
    );
    assert_eq!(row.resolver_backend.as_deref(), Some("stale_base"));
    assert_eq!(row.status, "unresolved");
    assert!(row.base_content_ref.is_some(), "base content ref recorded");
    assert!(row.ours_content_ref.is_some(), "ours content ref recorded");
    assert!(
        row.theirs_content_ref.is_some(),
        "theirs content ref recorded"
    );
    assert!(row.content_hash.is_some(), "conflict hash recorded");
    assert_eq!(path_conflict_count(&repo, &row.id), 1);
    let paths: Value = serde_json::from_str(&row.paths_json).expect("parse paths_json");
    assert!(
        paths.get("expected_head").is_some(),
        "expected_head key missing: {}",
        row.paths_json
    );
    assert!(
        paths.get("actual_head").is_some(),
        "actual_head key missing: {}",
        row.paths_json
    );

    let before_resolve = std::fs::read_to_string(repo.path().join("README.md")).unwrap();
    let resolution_ref = native_tree_ref_for_current_worktree(&repo);
    let rejected = json_output(
        repo.forge()
            .args([
                "--json",
                "conflict",
                "resolve",
                &row.id,
                "--tree",
                &resolution_ref,
            ])
            .assert()
            .failure(),
    );
    assert_eq!(rejected["errors"][0]["code"], "UNSUPPORTED_CONTENT_BACKEND");
    assert_eq!(
        rejected["errors"][0]["details"]["command"],
        "conflict resolve"
    );
    assert_eq!(rejected["errors"][0]["details"]["required"], "native_merge");
    assert_eq!(rejected["errors"][0]["details"]["actual"], "stale_base");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        before_resolve
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
    let row = &rows[0];
    assert_eq!(row.context, "stale_base_export");
    assert_eq!(
        row.generated_by_operation_id.as_deref(),
        output["operation_id"].as_str()
    );
    assert_eq!(row.resolver_backend.as_deref(), Some("stale_base"));
    assert_eq!(row.status, "unresolved");
    assert!(row.base_content_ref.is_some(), "base content ref recorded");
    assert!(row.ours_content_ref.is_some(), "ours content ref recorded");
    assert!(
        row.theirs_content_ref.is_some(),
        "theirs content ref recorded"
    );
    assert!(row.content_hash.is_some(), "conflict hash recorded");
    assert_eq!(path_conflict_count(&repo, &row.id), 1);
    let paths: Value = serde_json::from_str(&row.paths_json).expect("parse paths_json");
    assert!(
        paths.get("expected_head").is_some(),
        "expected_head key missing: {}",
        row.paths_json
    );
    assert!(
        paths.get("actual_head").is_some(),
        "actual_head key missing: {}",
        row.paths_json
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

#[test]
fn conflict_list_and_show_redact_raw_paths() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    move_head(&repo, "stale-before-accept.txt");

    let stale = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(stale["errors"][0]["code"], "STALE_BASE");

    let list = json_output(
        repo.forge()
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    let conflicts = list["data"]["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1);
    let conflict_id = conflicts[0]["conflict_set_id"]
        .as_str()
        .expect("conflict id");
    assert_eq!(conflicts[0]["path_conflict_count"], 1);

    let show = json_output(
        repo.forge()
            .args(["--json", "conflict", "show", conflict_id])
            .assert()
            .success(),
    );
    assert_eq!(show["data"]["conflict"]["conflict_set_id"], conflict_id);
    assert_eq!(
        show["data"]["path_conflicts"]
            .as_array()
            .expect("path conflicts")
            .len(),
        1
    );
    let rendered = serde_json::to_string(&show).expect("render show json");
    assert!(
        !rendered.contains("README.md"),
        "conflict JSON must not emit raw changed paths: {rendered}"
    );
    assert!(
        !rendered.contains("changed\n"),
        "conflict JSON must not emit inline blob content: {rendered}"
    );

    let suggested = json_output(
        repo.forge()
            .args(["--json", "conflict", "show", conflict_id, "--suggest"])
            .assert()
            .success(),
    );
    assert!(
        suggested["data"].get("suggestions").is_none(),
        "stale-base conflicts must not emit native auto-resolution suggestions: {suggested}"
    );
}

#[test]
fn conflict_show_missing_id_is_typed() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let output = json_output(
        repo.forge()
            .args(["--json", "conflict", "show", "conflict_missing"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "CONFLICT_SET_NOT_FOUND");
    assert_eq!(
        output["errors"][0]["details"]["selector_present"], true,
        "missing selector is acknowledged without echoing it"
    );
    let path_like = json_output(
        repo.forge()
            .args(["--json", "conflict", "show", "secrets/.env"])
            .assert()
            .failure(),
    );
    assert_eq!(path_like["errors"][0]["code"], "CONFLICT_SET_NOT_FOUND");
    let rendered = serde_json::to_string(&output).expect("render error json");
    assert!(
        !rendered.contains('/'),
        "missing-conflict error should not carry paths: {rendered}"
    );
    let path_like_rendered = serde_json::to_string(&path_like).expect("render error json");
    assert!(
        !path_like_rendered.contains("secrets/.env"),
        "missing-conflict error must not echo path-like selectors: {path_like_rendered}"
    );
}
