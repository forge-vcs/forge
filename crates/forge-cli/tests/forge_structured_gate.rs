mod common;

use common::TestRepo;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

/// NER-259: a `--require-tests-pass` (structured) gate whose program has no registered
/// structured parser is rejected up front at `forge start` — the gate would otherwise be
/// structurally unsatisfiable (it reads the parsed `failed` count, which stays `None`).
/// The error names the program and is argv-inspection only (no python3 needed).
#[test]
fn start_rejects_structured_gate_without_a_parser() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "start",
                "x",
                "--require-tests-pass",
                "python3 script.py",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "UNSUPPORTED_STRUCTURED_GATE");
    assert_eq!(output["errors"][0]["details"]["program"], "python3");
    assert_eq!(output["retry"]["retryable"], false);
    let message = output["errors"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("python3"),
        "message should name the program: {message}"
    );
}

/// NER-259 adversarial: when the rejected gate program is a secret-like `key=value` token
/// (a plausible agent misconfiguration, e.g. passing a credential as the first token of
/// `--require-tests-pass`), the secret must NOT leak unredacted into the JSON `message`
/// field. `details.program` was already redacted; the human-readable `message` (which
/// `error_to_object` derives from the error's `Display`) must redact it too.
#[test]
fn rejected_structured_gate_redacts_secret_like_program_in_message() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "start",
                "x",
                "--require-tests-pass",
                "TOKEN=ghp_supersecret script.py",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "UNSUPPORTED_STRUCTURED_GATE");
    assert_eq!(
        output["errors"][0]["details"]["program"],
        "TOKEN=[REDACTED]"
    );
    let message = output["errors"][0]["message"].as_str().unwrap();
    assert!(
        !message.contains("ghp_supersecret"),
        "secret token must not leak into the message: {message}"
    );
    assert!(
        message.contains("TOKEN=[REDACTED]"),
        "message should carry the redacted program token: {message}"
    );
}

/// NER-259: fail-fast means no attempt/intent/worktree is created (a failed-operation row
/// is still recorded by the mutating-error path — this asserts only the attempt side).
#[test]
fn rejected_structured_gate_creates_no_attempt() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();

    repo.forge()
        .args([
            "--json",
            "start",
            "x",
            "--require-tests-pass",
            "deploy --run",
        ])
        .assert()
        .failure();

    let listed = json_output(
        repo.forge()
            .args(["--json", "attempt", "list"])
            .assert()
            .success(),
    );
    assert_eq!(
        listed["data"]["attempts"].as_array().unwrap().len(),
        0,
        "no attempt should be created when the structured gate is rejected"
    );
}

/// NER-259: gates that DO have a registered parser succeed. `cargo test`,
/// `python3 -m unittest m`, and `pytest` are all accepted (argv inspection only — none
/// of these are executed at `start`).
#[test]
fn start_accepts_structured_gates_with_a_parser() {
    for gate in ["cargo test", "python3 -m unittest m", "pytest"] {
        let repo = TestRepo::new_git();
        repo.forge().args(["--json", "init"]).assert().success();
        json_output(
            repo.forge()
                .args(["--json", "start", "ok gate", "--require-tests-pass", gate])
                .assert()
                .success(),
        );
    }
}

/// NER-259: a plain `--require` (exit-code) gate accepts ANY program — it is not
/// validated for a structured parser.
#[test]
fn start_accepts_any_plain_require_gate() {
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    json_output(
        repo.forge()
            .args([
                "--json",
                "start",
                "plain gate",
                "--require",
                "anything goes here",
            ])
            .assert()
            .success(),
    );
}
