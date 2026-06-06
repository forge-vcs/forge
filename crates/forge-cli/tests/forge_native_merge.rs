mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn forge_ok(repo: &TestRepo, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    json_output(repo.forge().args(&full).assert().success())
}

fn forge_fail(repo: &TestRepo, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    json_output(repo.forge().args(&full).assert().failure())
}

fn db(repo: &TestRepo) -> Connection {
    Connection::open(repo.path().join(".forge/forge.db")).expect("open forge db")
}

fn init_two_native_attempts(repo: &TestRepo) -> (String, String, String) {
    std::fs::write(repo.path().join("README.md"), "one\ntwo\nthree\nfour\n").unwrap();
    forge_ok(repo, &["init", "--content-backend", "native"]);
    let first = forge_ok(repo, &["start", "native merge"]);
    let intent_id = first["data"]["intent_id"].as_str().unwrap().to_string();
    let attempt_a = first["data"]["attempt_id"].as_str().unwrap().to_string();
    let second = forge_ok(repo, &["attempt", "start", "--intent", &intent_id]);
    let attempt_b = second["data"]["attempt_id"].as_str().unwrap().to_string();
    (intent_id, attempt_a, attempt_b)
}

fn propose_attempt(repo: &TestRepo, attempt_id: &str, body: &str) -> String {
    forge_ok(repo, &["attempt", "attach", attempt_id]);
    std::fs::write(repo.path().join("README.md"), body).unwrap();
    forge_ok(repo, &["save", "--attempt", attempt_id]);
    forge_ok(
        repo,
        &["run", "--attempt", attempt_id, "--", "sh", "-c", "true"],
    );
    let proposed = forge_ok(repo, &["propose", "--attempt", attempt_id]);
    forge_ok(repo, &["check", "--attempt", attempt_id]);
    proposed["data"]["proposal_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn merge_in_git_backed_repo_returns_typed_unsupported_backend() {
    let repo = TestRepo::new_git();
    forge_ok(&repo, &["init"]);
    let started = forge_ok(&repo, &["start", "git merge unsupported"]);
    let attempt = started["data"]["attempt_id"].as_str().unwrap();
    std::fs::write(repo.path().join("README.md"), "git backed\n").unwrap();
    forge_ok(&repo, &["save", "--attempt", attempt]);
    forge_ok(
        &repo,
        &["run", "--attempt", attempt, "--", "sh", "-c", "true"],
    );
    let proposed = forge_ok(&repo, &["propose", "--attempt", attempt]);
    let proposal = proposed["data"]["proposal_id"].as_str().unwrap();

    let out = forge_fail(&repo, &["merge", "--proposal", proposal]);

    assert_eq!(out["errors"][0]["code"], "UNSUPPORTED_CONTENT_BACKEND");
    assert_eq!(out["errors"][0]["details"]["command"], "merge");
    assert_eq!(out["errors"][0]["details"]["required"], "native");
    assert_eq!(out["errors"][0]["details"]["actual"], "git");
    assert_eq!(out["retry"]["retryable"], false);
}

#[test]
fn native_merge_clean_non_overlapping_changes_returns_merged_tree() {
    let repo = TestRepo::new_git();
    let (_intent_id, attempt_a, attempt_b) = init_two_native_attempts(&repo);
    let proposal_a = propose_attempt(&repo, &attempt_a, "ONE\ntwo\nthree\nfour\n");
    let proposal_b = propose_attempt(&repo, &attempt_b, "one\ntwo\nthree\nFOUR\n");
    forge_ok(
        &repo,
        &["accept", "--attempt", &attempt_a, "--proposal", &proposal_a],
    );

    let out = forge_ok(&repo, &["merge", "--proposal", &proposal_b]);

    assert_eq!(out["data"]["merged"], true);
    assert!(out["data"]["merged_content_ref"]
        .as_str()
        .unwrap()
        .starts_with("forge-tree:"));
    assert_eq!(out["data"]["proposal_id"], proposal_b);
    let merged_content_ref = out["data"]["merged_content_ref"].as_str().unwrap();
    let merged_revision = out["data"]["proposal_revision_id"].as_str().unwrap();

    let premature_accept = forge_fail(
        &repo,
        &["accept", "--attempt", &attempt_b, "--proposal", &proposal_b],
    );
    assert_eq!(premature_accept["errors"][0]["code"], "CHECK_NOT_PASSED");

    forge_ok(
        &repo,
        &["run", "--attempt", &attempt_b, "--", "sh", "-c", "true"],
    );
    let checked = forge_ok(&repo, &["check", "--attempt", &attempt_b]);
    assert_eq!(checked["data"]["proposal_revision_id"], merged_revision);
    assert_eq!(checked["data"]["status"], "passed");
    forge_ok(
        &repo,
        &["accept", "--attempt", &attempt_b, "--proposal", &proposal_b],
    );
    let log = forge_ok(&repo, &["log"]);
    let commits = log["data"]["commits"].as_array().unwrap();
    let merge_commit = commits
        .iter()
        .find(|commit| commit["proposal_revision_id"] == merged_revision)
        .expect("clean merge commit is logged");
    assert_eq!(
        merge_commit["tree"],
        merged_content_ref.trim_start_matches("forge-tree:")
    );
    assert_eq!(merge_commit["parents"].as_array().unwrap().len(), 2);
    let doctor = forge_ok(&repo, &["doctor"]);
    assert!(doctor["data"]["issues"].as_array().unwrap().is_empty());

    let connection = db(&repo);
    let state_json: String = connection
        .query_row(
            "SELECT state_json FROM views WHERE kind = 'merge_clean' ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("merge_clean view exists");
    let mut state: Value = serde_json::from_str(&state_json).expect("view json");
    state["ours_head"] = Value::String(
        "f1:commit:sha256:0000000000000000000000000000000000000000000000000000000000000000"
            .to_string(),
    );
    connection
        .execute(
            "UPDATE views SET state_json = ?1 WHERE kind = 'merge_clean'",
            [serde_json::to_string(&state).unwrap()],
        )
        .expect("tamper merge lineage");
    let tampered = forge_ok(&repo, &["doctor"]);
    assert!(!tampered["data"]["issues"].as_array().unwrap().is_empty());
}

#[test]
fn native_merge_overlapping_changes_persists_conflict_set() {
    let repo = TestRepo::new_git();
    let (_intent_id, attempt_a, attempt_b) = init_two_native_attempts(&repo);
    let proposal_a = propose_attempt(&repo, &attempt_a, "one\nOURS\nthree\nfour\n");
    let proposal_b = propose_attempt(&repo, &attempt_b, "one\nTHEIRS\nthree\nfour\n");
    forge_ok(
        &repo,
        &["accept", "--attempt", &attempt_a, "--proposal", &proposal_a],
    );

    let out = forge_ok(&repo, &["merge", "--proposal", &proposal_b]);

    assert_eq!(out["data"]["merged"], false);
    let conflict_set_id = out["data"]["conflict_set_id"].as_str().unwrap();
    let shown = forge_ok(&repo, &["conflict", "show", conflict_set_id]);
    assert_eq!(
        shown["data"]["conflict"]["resolver_backend"],
        "native_merge"
    );
    assert_eq!(shown["data"]["path_conflicts"][0]["kind"], "content");
    let body = serde_json::to_string(&shown).unwrap();
    assert!(
        !body.contains("README.md"),
        "raw paths must stay redacted: {body}"
    );

    let resolution_ref = shown["data"]["conflict"]["ours_content_ref"]
        .as_str()
        .unwrap()
        .to_string();
    let before_failed_resolve = std::fs::read_to_string(repo.path().join("README.md")).unwrap();
    let failed_resolve = forge_fail(
        &repo,
        &[
            "conflict",
            "resolve",
            "conflict_missing",
            "--tree",
            &resolution_ref,
        ],
    );
    assert_eq!(
        failed_resolve["errors"][0]["code"],
        "CONFLICT_SET_NOT_FOUND"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        before_failed_resolve
    );
    let resolved = forge_ok(
        &repo,
        &[
            "conflict",
            "resolve",
            conflict_set_id,
            "--tree",
            &resolution_ref,
        ],
    );
    assert_eq!(resolved["data"]["conflict_set_id"], conflict_set_id);
    assert_eq!(resolved["data"]["resolution_ref"], resolution_ref);

    let shown_after = forge_ok(&repo, &["conflict", "show", conflict_set_id]);
    assert_eq!(shown_after["data"]["conflict"]["status"], "resolved");
    assert_eq!(
        shown_after["data"]["path_conflicts"][0]["status"],
        "resolved"
    );
    assert_eq!(
        shown_after["data"]["path_conflicts"][0]["resolution_ref"],
        resolution_ref
    );

    let premature_accept = forge_fail(
        &repo,
        &["accept", "--attempt", &attempt_b, "--proposal", &proposal_b],
    );
    assert_eq!(premature_accept["errors"][0]["code"], "CHECK_NOT_PASSED");
    let stale_check = forge_ok(&repo, &["check", "--attempt", &attempt_b]);
    assert_ne!(stale_check["data"]["status"], "passed");
    forge_ok(
        &repo,
        &["run", "--attempt", &attempt_b, "--", "sh", "-c", "true"],
    );
    let checked = forge_ok(&repo, &["check", "--attempt", &attempt_b]);
    assert_eq!(
        checked["data"]["proposal_revision_id"],
        resolved["data"]["proposal_revision_id"]
    );
    assert_eq!(checked["data"]["status"], "passed");
    forge_ok(
        &repo,
        &["accept", "--attempt", &attempt_b, "--proposal", &proposal_b],
    );
    let log = forge_ok(&repo, &["log"]);
    let commits = log["data"]["commits"].as_array().unwrap();
    let merge_commit = commits
        .iter()
        .find(|commit| commit["proposal_revision_id"] == resolved["data"]["proposal_revision_id"])
        .expect("resolved merge commit is logged");
    assert_eq!(merge_commit["parents"].as_array().unwrap().len(), 2);
    let doctor = forge_ok(&repo, &["doctor"]);
    assert!(doctor["data"]["issues"].as_array().unwrap().is_empty());
}
