mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

/// NER-257: start an intent with two gates (one plain `--require`, one structured
/// `--require-tests-pass`), then `intent show <id>` returns both gates with the correct
/// program/args/structured flag plus the linked attempt id, and the envelope `command`
/// is the compound `intent show`.
#[test]
fn intent_show_returns_gate_spec_and_attempt() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let started = json_output(
        repo.forge()
            .args([
                "--json",
                "start",
                "two gates",
                "--require",
                "cargo test",
                "--require-tests-pass",
                "cargo clippy",
            ])
            .assert()
            .success(),
    );
    let intent_id = started["data"]["intent_id"].as_str().unwrap().to_string();
    let attempt_id = started["data"]["attempt_id"].as_str().unwrap().to_string();

    let shown = json_output(
        repo.forge()
            .args(["--json", "intent", "show", &intent_id])
            .assert()
            .success(),
    );
    assert_eq!(shown["command"], "intent show");
    assert_eq!(shown["data"]["intent_id"], intent_id);
    assert_eq!(shown["data"]["title"], "two gates");

    let gates = shown["data"]["gates"].as_array().expect("gates array");
    assert_eq!(gates.len(), 2, "both declared gates surface");
    // Plain --require gate: structured=false.
    let plain = gates
        .iter()
        .find(|gate| gate["args"][0] == "test")
        .expect("plain cargo test gate present");
    assert_eq!(plain["program"], "cargo");
    assert_eq!(plain["args"][0], "test");
    assert_eq!(plain["structured"], false);
    // Structured --require-tests-pass gate: structured=true.
    let structured = gates
        .iter()
        .find(|gate| gate["args"][0] == "clippy")
        .expect("structured cargo clippy gate present");
    assert_eq!(structured["program"], "cargo");
    assert_eq!(structured["args"][0], "clippy");
    assert_eq!(structured["structured"], true);

    let attempt_ids = shown["data"]["attempt_ids"]
        .as_array()
        .expect("attempt_ids array");
    assert!(
        attempt_ids
            .iter()
            .any(|id| id.as_str() == Some(attempt_id.as_str())),
        "linked attempt id is present"
    );
}

/// NER-257: `intent list` includes the started intent (id + title).
#[test]
fn intent_list_includes_started_intent() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let started = json_output(
        repo.forge()
            .args(["--json", "start", "listed intent"])
            .assert()
            .success(),
    );
    let intent_id = started["data"]["intent_id"].as_str().unwrap().to_string();

    let listed = json_output(
        repo.forge()
            .args(["--json", "intent", "list"])
            .assert()
            .success(),
    );
    assert_eq!(listed["command"], "intent list");
    let intents = listed["data"]["intents"].as_array().expect("intents array");
    let found = intents
        .iter()
        .find(|intent| intent["intent_id"].as_str() == Some(intent_id.as_str()))
        .expect("started intent is listed");
    assert_eq!(found["title"], "listed intent");
    assert_eq!(found["status"], "open");
}

/// NER-257: the derived intent status flips from `open` to `accepted` once a linked
/// attempt's proposal is accepted. This drives the full `start → save → run → propose →
/// check → accept` lifecycle through the CLI so the `intent_derived_status` EXISTS JOIN
/// (decisions → proposals → attempts → intent) is exercised end-to-end — not just the
/// `open` fallback every other test asserts (code-review: testing/medium). A broken JOIN
/// or wrong column there would return `open` after a real accept and fail this test.
#[test]
fn intent_status_is_accepted_after_a_linked_proposal_is_accepted() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let started = json_output(
        repo.forge()
            .args(["--json", "start", "accepted intent"])
            .assert()
            .success(),
    );
    let intent_id = started["data"]["intent_id"].as_str().unwrap().to_string();

    // Before any accept, the status is `open`.
    let before = json_output(
        repo.forge()
            .args(["--json", "intent", "show", &intent_id])
            .assert()
            .success(),
    );
    assert_eq!(before["data"]["status"], "open");

    // Drive the attempt to an accepted proposal.
    std::fs::write(repo.path().join("README.md"), "accepted\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    assert_eq!(accepted["data"]["decision"], "accepted");

    // `intent show` now derives `accepted` from the linked attempt's accepted decision.
    let shown = json_output(
        repo.forge()
            .args(["--json", "intent", "show", &intent_id])
            .assert()
            .success(),
    );
    assert_eq!(
        shown["data"]["status"], "accepted",
        "derived status must flip to accepted once a linked proposal is accepted"
    );

    // `intent list` derives the same accepted status for the intent.
    let listed = json_output(
        repo.forge()
            .args(["--json", "intent", "list"])
            .assert()
            .success(),
    );
    let found = listed["data"]["intents"]
        .as_array()
        .expect("intents array")
        .iter()
        .find(|intent| intent["intent_id"].as_str() == Some(intent_id.as_str()))
        .expect("accepted intent is listed");
    assert_eq!(found["status"], "accepted");
}

/// NER-257: `intent show` of an unknown id surfaces the typed UNKNOWN_INTENT error.
#[test]
fn intent_show_unknown_id_errors() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let output = json_output(
        repo.forge()
            .args(["--json", "intent", "show", "intent_does_not_exist"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "UNKNOWN_INTENT");
    assert_eq!(output["retry"]["retryable"], false);
}
