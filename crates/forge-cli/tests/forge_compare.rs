//! `forge compare` / `forge attempt compare` — the NER-137 Phase 6 compare/rank
//! surface (and, in later sections, the provenance trailer + verify-branch).

mod common;

use common::TestRepo;
use rusqlite::Connection;
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
        let gates = a["gates"].as_array().expect("gates array");
        assert!(a["metrics"].is_object());
        // NER-254: the additive per-gate verdict_detail reaches compare JSON too.
        for gate in gates {
            assert!(
                gate["verdict_detail"].is_string(),
                "each compare gate carries a verdict_detail string: {gate:?}"
            );
        }
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
fn exit_criterion_compare_export_winner_and_verify_trailer() {
    // NER-137 exit criterion end-to-end: 2 rival attempts (each verified), compare
    // asserts per-attempt diffs + per-gate results + metrics + a deterministic
    // ranking; the ranked winner exports headlessly; verify-branch recomputes the
    // trailer from the ledger.
    let repo = TestRepo::new_git();
    // Attempt A fails its evidence (exit 5); attempt B passes — B is the winner.
    let (_intent, attempt_a, attempt_b, _pa, proposal_b) =
        two_competing_attempts(&repo, &["sh", "-c", "exit 5"], &["sh", "-c", "true"]);

    let out = forge_ok(&repo, &["compare"]);
    let attempts = out["data"]["intents"][0]["attempts"].as_array().unwrap();
    let winner = attempts.iter().find(|a| a["rank"] == 1).unwrap();
    assert_eq!(winner["attempt_id"], attempt_b);
    // Per-attempt diff (changed paths) + per-gate results + metrics are present.
    assert!(winner["changed_paths"].is_array());
    let winner_gates = winner["gates"].as_array().unwrap();
    assert!(!winner_gates.is_empty());
    // NER-254: per-gate verdict_detail is present in the compare winner's gates.
    assert!(winner_gates[0]["verdict_detail"].is_string());
    assert!(winner["metrics"].is_object());
    // The loser is verified-but-failing and ranks second.
    let loser = attempts.iter().find(|a| a["rank"] == 2).unwrap();
    assert_eq!(loser["attempt_id"], attempt_a);

    // Pairwise file/hunk diff between the two competing proposals.
    let diffed = forge_ok(&repo, &["compare", "--diff", &attempt_a, &attempt_b]);
    assert!(!diffed["data"]["diff"]["files"]
        .as_array()
        .unwrap()
        .is_empty());

    // The ranked winner exports headlessly using the echoed ids, then verify-branch
    // recomputes the trailer.
    forge_ok(
        &repo,
        &["accept", "--attempt", &attempt_b, "--proposal", &proposal_b],
    );
    forge_ok(
        &repo,
        &[
            "export",
            "branch",
            "--attempt",
            &attempt_b,
            "--proposal",
            &proposal_b,
            "forge/winner",
        ],
    );
    let verified = forge_ok(&repo, &["export", "verify-branch", "forge/winner"]);
    assert_eq!(verified["data"]["verified"], true);
}

#[test]
fn compare_flags_a_tampered_winner_and_promotes_the_honest_attempt() {
    // Load-bearing (NER-137 R4): tamper the would-be winner's deciding evidence row;
    // compare must surface it as tampered/unranked and make the honest attempt rank 1.
    let repo = TestRepo::new_git();
    let (_intent, attempt_a, attempt_b, _pa, _pb) =
        two_competing_attempts(&repo, &["sh", "-c", "true"], &["sh", "-c", "true"]);

    // Tamper attempt A's deciding evidence row (flip its exit_code without rehashing).
    // Find attempt A's snapshot's evidence row and mutate it.
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let changed = connection
        .execute(
            "UPDATE evidence SET exit_code = 1 WHERE attempt_id = ?1",
            [&attempt_a],
        )
        .expect("tamper attempt A evidence");
    assert!(changed >= 1, "a deciding evidence row was tampered");

    let out = forge_ok(&repo, &["compare"]);
    let attempts = out["data"]["intents"][0]["attempts"].as_array().unwrap();
    let tampered = attempts
        .iter()
        .find(|a| a["attempt_id"] == attempt_a)
        .unwrap();
    assert_eq!(tampered["integrity"], "tampered");
    assert!(tampered["rank"].is_null(), "tampered attempt is unranked");
    // The honest attempt is the rank-1 winner.
    let winner = attempts.iter().find(|a| a["rank"] == 1).unwrap();
    assert_eq!(winner["attempt_id"], attempt_b);
    assert_eq!(winner["integrity"], "verified");
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
