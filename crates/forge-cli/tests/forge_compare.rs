//! `forge compare` / `forge attempt compare` — the NER-137 Phase 6 compare/rank
//! surface (and, in later sections, the provenance trailer + verify-branch).

mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

/// Drive `forge <args>` as JSON and assert success, returning the parsed envelope.
fn forge_ok(repo: &TestRepo, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    json_output(repo.forge().args(&full).assert().success())
}

/// Set up two competing attempts under one intent, each saving divergent content,
/// running `run_cmd` for evidence, and proposing. Returns
/// `(intent_id, attempt_a, attempt_b, proposal_a, proposal_b)`.
fn two_competing_attempts(
    repo: &TestRepo,
    run_a: &[&str],
    run_b: &[&str],
) -> (String, String, String, String, String) {
    forge_ok(repo, &["init"]);
    let first = forge_ok(repo, &["start", "compete"]);
    let intent_id = first["data"]["intent_id"].as_str().unwrap().to_string();
    let attempt_a = first["data"]["attempt_id"].as_str().unwrap().to_string();

    std::fs::write(repo.path().join("README.md"), "alpha\nshared\n").expect("write a");
    forge_ok(repo, &["save", "--attempt", &attempt_a]);
    let mut run = vec!["run", "--attempt", &attempt_a, "--"];
    run.extend_from_slice(run_a);
    forge_ok(repo, &run);
    let prop_a = forge_ok(repo, &["propose", "--attempt", &attempt_a]);
    let proposal_a = prop_a["data"]["proposal_id"].as_str().unwrap().to_string();

    let second = forge_ok(repo, &["attempt", "start", "--intent", &intent_id]);
    let attempt_b = second["data"]["attempt_id"].as_str().unwrap().to_string();
    forge_ok(repo, &["attempt", "attach", &attempt_b]);
    std::fs::write(repo.path().join("README.md"), "beta\nshared\nextra\n").expect("write b");
    forge_ok(repo, &["save", "--attempt", &attempt_b]);
    let mut run = vec!["run", "--attempt", &attempt_b, "--"];
    run.extend_from_slice(run_b);
    forge_ok(repo, &run);
    let prop_b = forge_ok(repo, &["propose", "--attempt", &attempt_b]);
    let proposal_b = prop_b["data"]["proposal_id"].as_str().unwrap().to_string();

    (intent_id, attempt_a, attempt_b, proposal_a, proposal_b)
}

#[test]
fn compare_groups_competing_attempts_and_ranks_them() {
    let repo = TestRepo::new_git();
    let (intent_id, attempt_a, attempt_b, proposal_a, proposal_b) =
        two_competing_attempts(&repo, &["sh", "-c", "true"], &["sh", "-c", "true"]);

    let out = forge_ok(&repo, &["compare"]);
    let intents = out["data"]["intents"].as_array().unwrap();
    assert_eq!(intents.len(), 1, "one intent group");
    assert_eq!(intents[0]["intent_id"], intent_id);
    let attempts = intents[0]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "both competing attempts present");

    // Both passed the default-mode gate, so both are ranked (1 and 2) and verified.
    let ids: Vec<&str> = attempts
        .iter()
        .map(|a| a["attempt_id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&attempt_a.as_str()));
    assert!(ids.contains(&attempt_b.as_str()));
    for a in attempts {
        assert_eq!(a["integrity"], "verified");
        assert!(a["rank"].is_number(), "verified attempts are ranked");
        // Each row echoes its proposal id so an agent can chain compare -> accept.
        assert!(a["proposal"]["proposal_id"].is_string());
        // Per-gate results + metrics fields are present.
        assert!(a["gates"].is_array());
        assert!(a["metrics"].is_object());
    }
    // The proposal ids are the ones propose returned (chainable).
    let proposal_ids: Vec<&str> = attempts
        .iter()
        .map(|a| a["proposal"]["proposal_id"].as_str().unwrap())
        .collect();
    assert!(proposal_ids.contains(&proposal_a.as_str()));
    assert!(proposal_ids.contains(&proposal_b.as_str()));
}

#[test]
fn compare_ranks_passing_attempt_above_failing_one() {
    let repo = TestRepo::new_git();
    // Attempt A's evidence fails (exit 3); attempt B's passes (exit 0). The default-mode
    // gate makes A's check failed and B's passed, so B ranks first.
    let (_intent, attempt_a, attempt_b, _pa, _pb) =
        two_competing_attempts(&repo, &["sh", "-c", "exit 3"], &["sh", "-c", "true"]);

    let out = forge_ok(&repo, &["compare"]);
    let attempts = out["data"]["intents"][0]["attempts"].as_array().unwrap();
    // Deterministic ranking: rank 1 is the passing attempt.
    let rank1 = attempts.iter().find(|a| a["rank"] == 1).unwrap();
    assert_eq!(rank1["attempt_id"], attempt_b);
    assert_eq!(rank1["check_status"], "passed");
    let rank2 = attempts.iter().find(|a| a["rank"] == 2).unwrap();
    assert_eq!(rank2["attempt_id"], attempt_a);
    assert_eq!(rank2["check_status"], "failed");
}

#[test]
fn attempt_compare_alias_matches_compare_for_one_intent() {
    let repo = TestRepo::new_git();
    let (intent_id, _a, _b, _pa, _pb) =
        two_competing_attempts(&repo, &["sh", "-c", "true"], &["sh", "-c", "true"]);

    let top = forge_ok(&repo, &["compare", "--intent", &intent_id]);
    let alias = forge_ok(&repo, &["attempt", "compare", "--intent", &intent_id]);
    assert_eq!(top["data"], alias["data"], "both forms yield the same data");
}

#[test]
fn compare_diff_emits_file_hunk_diff_between_two_attempts() {
    let repo = TestRepo::new_git();
    let (_intent, attempt_a, attempt_b, _pa, _pb) =
        two_competing_attempts(&repo, &["sh", "-c", "true"], &["sh", "-c", "true"]);

    let out = forge_ok(&repo, &["compare", "--diff", &attempt_a, &attempt_b]);
    let files = out["data"]["diff"]["files"].as_array().unwrap();
    let readme = files
        .iter()
        .find(|f| f["path"] == "README.md")
        .expect("README.md changed between the two proposals");
    assert_eq!(readme["status"], "M");
    assert!(readme["hunk"].as_str().unwrap().contains("beta"));
}

#[test]
fn compare_unknown_intent_is_typed_error() {
    let repo = TestRepo::new_git();
    two_competing_attempts(&repo, &["sh", "-c", "true"], &["sh", "-c", "true"]);

    let out = json_output(
        repo.forge()
            .args(["--json", "compare", "--intent", "intent_does_not_exist"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "UNKNOWN_INTENT");
}

#[test]
fn compare_lists_the_command_in_schema() {
    let repo = TestRepo::new_git();
    let schema = forge_ok(&repo, &["schema"]);
    let names: Vec<&str> = schema["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["command"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"compare"));
    assert!(names.contains(&"attempt compare"));
    assert_eq!(schema["schema_version"], "forge.cli.v0");
}
