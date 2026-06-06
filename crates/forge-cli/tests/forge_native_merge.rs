mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn forge_ok(repo: &TestRepo, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    json_output(repo.forge().args(&full).assert().success())
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

    forge_ok(
        &repo,
        &[
            "accept",
            "--attempt",
            &attempt_b,
            "--proposal",
            &proposal_b,
            "--allow-unverified",
        ],
    );
    let log = forge_ok(&repo, &["log"]);
    let commits = log["data"]["commits"].as_array().unwrap();
    let merge_commit = commits
        .iter()
        .find(|commit| commit["proposal_revision_id"] == resolved["data"]["proposal_revision_id"])
        .expect("resolved merge commit is logged");
    assert_eq!(merge_commit["parents"].as_array().unwrap().len(), 2);
}
