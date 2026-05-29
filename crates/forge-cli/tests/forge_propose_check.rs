mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

#[test]
fn propose_show_and_check_pass_with_successful_evidence() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "ship proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "proposal\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();

    let proposed = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    assert!(proposed["data"]["proposal_id"]
        .as_str()
        .unwrap()
        .starts_with("proposal_"));

    let shown = json_output(repo.forge().args(["--json", "show"]).assert().success());
    assert_eq!(
        shown["data"]["latest_proposal"]["proposal_id"],
        proposed["data"]["proposal_id"]
    );

    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "passed");
}

#[test]
fn propose_requires_snapshot_and_check_reports_missing_evidence() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "no evidence yet"])
        .assert()
        .success();

    let no_snapshot = json_output(repo.forge().args(["--json", "propose"]).assert().failure());
    assert_eq!(no_snapshot["errors"][0]["code"], "NO_SNAPSHOT");
    assert!(no_snapshot["operation_id"]
        .as_str()
        .unwrap()
        .starts_with("op_"));

    std::fs::write(repo.path().join("README.md"), "proposal\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();
    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "missing");
}

#[test]
fn check_marks_evidence_stale_when_snapshot_changes_after_run() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "stale evidence"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "first\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "second\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();

    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "stale");
    assert!(checked["data"]["reason"]
        .as_str()
        .unwrap()
        .contains("does not match proposal revision"));
}

/// Helpers for the NER-135 declarative-gate tests. A declared gate's identity is
/// `(program, args)` whitespace-tokenized from the `--require` value, matched
/// against the `forge run -- <argv>` evidence identity, so gate strings here use
/// single-token args (e.g. `sh -c true`) that round-trip exactly.
fn start_with_gates(repo: &TestRepo, intent: &str, gates: &[&str]) {
    let mut args = vec![
        "--json".to_string(),
        "start".to_string(),
        intent.to_string(),
    ];
    for gate in gates {
        args.push("--require".to_string());
        args.push((*gate).to_string());
    }
    repo.forge().args(&args).assert().success();
}

fn run_cmd(repo: &TestRepo, argv: &[&str]) {
    let mut args = vec!["--json", "run", "--"];
    args.extend_from_slice(argv);
    // `false` exits non-zero; `forge run` still succeeds (it captured evidence).
    repo.forge().args(&args).assert().success();
}

#[test]
fn run_true_cannot_satisfy_a_declared_gate() {
    // The `run -- true` bypass: a declared gate names `cargo test`, but only a
    // trivial command ran on the proposed snapshot — the gate stays unmet.
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    start_with_gates(&repo, "needs cargo test", &["cargo test"]);
    std::fs::write(repo.path().join("README.md"), "x\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    run_cmd(&repo, &["sh", "-c", "true"]);
    repo.forge().args(["--json", "propose"]).assert().success();

    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_ne!(checked["data"]["status"], "passed");
    assert_eq!(checked["data"]["status"], "missing");
    let gates = checked["data"]["gates"].as_array().expect("gates array");
    assert_eq!(gates.len(), 1);
    assert_eq!(gates[0]["program"], "cargo");
    assert_eq!(gates[0]["verdict"], "missing");
}

#[test]
fn failing_gate_then_passing_other_command_stays_failed() {
    // The footgun: a newer trivial success on a DIFFERENT identity must not flip a
    // failing named gate green.
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    start_with_gates(&repo, "verify", &["sh -c false"]);
    std::fs::write(repo.path().join("README.md"), "x\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    run_cmd(&repo, &["sh", "-c", "false"]); // the gate command, fails (exit 1)
    run_cmd(&repo, &["sh", "-c", "true"]); // a newer, different, passing command
    repo.forge().args(["--json", "propose"]).assert().success();

    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "failed");
    let gates = checked["data"]["gates"].as_array().expect("gates array");
    assert_eq!(gates[0]["verdict"], "failed");
}

#[test]
fn declared_gate_is_stale_when_evidence_is_on_an_earlier_snapshot() {
    // The declared-gate `stale` verdict end-to-end: the gate ran, but only on a
    // prior snapshot, so it does not describe the proposed tree (code-review F3).
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    start_with_gates(&repo, "stale gate", &["sh -c true"]);
    std::fs::write(repo.path().join("README.md"), "first\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    run_cmd(&repo, &["sh", "-c", "true"]); // evidence binds snapshot A
    std::fs::write(repo.path().join("README.md"), "second\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success(); // snapshot B
    repo.forge().args(["--json", "propose"]).assert().success(); // proposes B
    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "stale");
    let gates = checked["data"]["gates"].as_array().expect("gates array");
    assert_eq!(gates[0]["verdict"], "stale");
}

#[test]
fn two_declared_gates_pass_only_when_all_pass() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    start_with_gates(&repo, "two gates", &["true", "sh -c true"]);
    std::fs::write(repo.path().join("README.md"), "x\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    run_cmd(&repo, &["true"]);
    run_cmd(&repo, &["sh", "-c", "true"]);
    repo.forge().args(["--json", "propose"]).assert().success();

    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "passed");
    let gates = checked["data"]["gates"].as_array().expect("gates array");
    assert_eq!(gates.len(), 2);
    assert!(gates.iter().all(|g| g["verdict"] == "passed"));
}

#[test]
fn default_mode_failing_then_passing_other_command_fails() {
    // R9: aggregate-over-snapshot closes the footgun even for an intent with NO
    // declared gates.
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "no gates"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "x\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    run_cmd(&repo, &["sh", "-c", "false"]);
    run_cmd(&repo, &["sh", "-c", "true"]);
    repo.forge().args(["--json", "propose"]).assert().success();

    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "failed");
}

#[test]
fn accept_requires_a_passing_check_by_default_and_allow_unverified_bypasses() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    start_with_gates(&repo, "needs cargo test", &["cargo test"]);
    std::fs::write(repo.path().join("README.md"), "x\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    run_cmd(&repo, &["sh", "-c", "true"]); // does not satisfy the cargo test gate
    repo.forge().args(["--json", "propose"]).assert().success();

    // Default accept is blocked by the unmet gate.
    let blocked = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(blocked["errors"][0]["code"], "CHECK_NOT_PASSED");
    assert_eq!(blocked["errors"][0]["details"]["status"], "missing");
    // The unmet list names the gate the agent must satisfy (machine-actionable).
    let unmet = blocked["errors"][0]["details"]["unmet"]
        .as_array()
        .expect("unmet array");
    assert!(
        unmet.iter().any(|u| u == "cargo test"),
        "unmet must name the cargo test gate, got {unmet:?}"
    );

    // --allow-unverified bypasses with a warning.
    let bypassed = json_output(
        repo.forge()
            .args(["--json", "accept", "--allow-unverified"])
            .assert()
            .success(),
    );
    let warnings = bypassed["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("--allow-unverified")),
        "expected an --allow-unverified warning, got {warnings:?}"
    );
}

#[test]
fn accept_reevaluates_and_overrides_a_stale_passing_check() {
    // `accept` re-evaluates in-txn and is authoritative: a check row that was green
    // does not let a proposal through once newer failing evidence lands on the same
    // snapshot (adversarial review). Default mode (no declared gates).
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "diverge"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "x\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    run_cmd(&repo, &["sh", "-c", "true"]);
    repo.forge().args(["--json", "propose"]).assert().success();
    let checked = json_output(repo.forge().args(["--json", "check"]).assert().success());
    assert_eq!(checked["data"]["status"], "passed");

    // Newer failing evidence on the SAME snapshot (no save in between).
    run_cmd(&repo, &["sh", "-c", "false"]);

    let blocked = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(blocked["errors"][0]["code"], "CHECK_NOT_PASSED");
}
