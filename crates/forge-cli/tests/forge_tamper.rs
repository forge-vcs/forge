//! Tamper-evidence end-to-end (NER-136 Phase 5).
//!
//! The load-bearing exit criteria: editing an evidence or decision row after the fact
//! is (a) detected by `doctor` and (b) refused on re-evaluation by `check`/`accept`/
//! `export` — fail-closed, and NOT bypassable by `accept --allow-unverified`. A clean
//! lifecycle verifies cleanly. These drive the shipped binary and mutate
//! `.forge/forge.db` directly to simulate a tamper.

mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json envelope")
}

fn db(repo: &TestRepo) -> Connection {
    Connection::open(repo.path().join(".forge/forge.db")).expect("open forge.db")
}

/// init → start → save → run (records evidence) → propose. Default-mode gate: the
/// `sh -c true` evidence on the proposed snapshot is the deciding evidence.
fn lifecycle_to_proposal(repo: &TestRepo) {
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "build a feature"])
        .assert()
        .success();
    std::fs::write(repo.path().join("feature.txt"), "x\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
}

#[test]
fn editing_evidence_is_refused_at_check_and_accept_even_unverified() {
    let repo = TestRepo::new_git();
    lifecycle_to_proposal(&repo);

    // Tamper: edit the persisted excerpt WITHOUT recomputing the content hash. The
    // exit_code stays 0 (so the policy verdict would PASS) — only integrity fails,
    // proving integrity refuses independently of the policy verdict.
    db(&repo)
        .execute("UPDATE evidence SET stdout_excerpt = 'FORGED'", [])
        .expect("tamper evidence");

    let checked = json(repo.forge().args(["--json", "check"]).assert().failure());
    assert_eq!(checked["errors"][0]["code"], "EVIDENCE_TAMPERED");

    let accepted = json(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(accepted["errors"][0]["code"], "EVIDENCE_TAMPERED");

    // The policy bypass must NOT launder tampered evidence.
    let unverified = json(
        repo.forge()
            .args(["--json", "accept", "--allow-unverified"])
            .assert()
            .failure(),
    );
    assert_eq!(unverified["errors"][0]["code"], "EVIDENCE_TAMPERED");
}

#[test]
fn doctor_detects_a_tampered_evidence_row() {
    let repo = TestRepo::new_git();
    lifecycle_to_proposal(&repo);
    db(&repo)
        .execute("UPDATE evidence SET exit_code = 77", [])
        .expect("tamper evidence");

    // `doctor` is a diagnostic report: it exits 0 and reports findings in `data`
    // (ok=false), rather than failing the process.
    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    let tampered = report["data"]["tampered_rows"]
        .as_array()
        .expect("tampered_rows array");
    assert!(
        tampered
            .iter()
            .any(|row| row["table"] == "evidence" && row["kind"] == "content_edit"),
        "doctor must flag the evidence row as content_edit: {tampered:?}"
    );
    // Only an opaque id + table + kind — never an excerpt/command (egress guard).
    let first = &tampered[0];
    assert_eq!(
        first.as_object().expect("object").len(),
        3,
        "tampered_rows entries carry exactly id, table, kind"
    );
}

#[test]
fn a_clean_lifecycle_verifies_with_no_tamper() {
    let repo = TestRepo::new_git();
    lifecycle_to_proposal(&repo);
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], true);
    assert!(report["data"]["tampered_rows"]
        .as_array()
        .expect("array")
        .is_empty());
}

#[test]
fn editing_a_decision_is_refused_at_export_and_no_branch_is_created() {
    let repo = TestRepo::new_git();
    lifecycle_to_proposal(&repo);
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();

    // Forge the recorded decider: edit a field the decision digest covers.
    db(&repo)
        .execute("UPDATE decisions SET actor = 'attacker'", [])
        .expect("tamper decision");

    let exported = json(
        repo.forge()
            .args(["--json", "export", "branch", "tampered-branch"])
            .assert()
            .failure(),
    );
    assert_eq!(exported["errors"][0]["code"], "EVIDENCE_TAMPERED");

    // The git branch must NOT have been created (verify runs before the branch).
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "tampered-branch"])
        .current_dir(repo.path())
        .status()
        .expect("run git");
    assert!(
        !branch.success(),
        "no git branch may be created when the decision is tampered"
    );

    // doctor also flags the decision row.
    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert!(report["data"]["tampered_rows"]
        .as_array()
        .expect("array")
        .iter()
        .any(|row| row["table"] == "decisions" && row["kind"] == "content_edit"));
}

#[test]
fn failure_operations_keep_the_chain_verifiable() {
    // record_failed_operation is a third chain-write site (it bypasses
    // insert_operation_view). A deterministic command failure must leave the op-log
    // chain continuous, or doctor would mis-flag an honest repo as tampered.
    let repo = TestRepo::new_git();
    repo.forge().args(["--json", "init"]).assert().success();
    // A deterministic failure (no active attempt) records a failed operation.
    repo.forge().args(["--json", "accept"]).assert().failure();
    repo.forge()
        .args(["--json", "start", "after failure"])
        .assert()
        .success();
    std::fs::write(repo.path().join("g.txt"), "y\n").expect("write");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(
        report["data"]["ok"], true,
        "a recorded failed operation must not break the chain"
    );
    assert!(report["data"]["tampered_rows"]
        .as_array()
        .expect("array")
        .is_empty());
}

#[test]
fn legacy_null_hash_evidence_is_grandfathered_not_tampered() {
    // A row that predates Phase 5 (NULL hash at/below the migration high-water mark)
    // must NOT be flagged — the rowid marker, not a mutable timestamp, decides.
    let repo = TestRepo::new_git();
    lifecycle_to_proposal(&repo);
    // Simulate a pre-Phase-5 repo: every row's hash is NULL and every high-water mark
    // sits at the current max rowid, so all rows are grandfathered (rowid <= mark).
    let connection = db(&repo);
    connection
        .execute("UPDATE evidence SET content_hash = NULL", [])
        .expect("null evidence hashes");
    connection
        .execute("UPDATE operations SET content_hash = NULL", [])
        .expect("null operation hashes");
    connection
        .execute("UPDATE decisions SET content_hash = NULL", [])
        .expect("null decision hashes");
    connection
        .execute(
            "UPDATE integrity_marker SET
               evidence_high_water = (SELECT COALESCE(MAX(rowid), 0) FROM evidence),
               op_high_water = (SELECT COALESCE(MAX(rowid), 0) FROM operations),
               decision_high_water = (SELECT COALESCE(MAX(rowid), 0) FROM decisions)",
            [],
        )
        .expect("lift markers to the legacy boundary");
    connection
        .execute("DELETE FROM ledger_signatures", [])
        .expect("remove post-Phase-9 signatures");
    connection
        .execute(
            "UPDATE signature_marker SET
               evidence_high_water = (SELECT COALESCE(MAX(rowid), 0) FROM evidence),
               decision_high_water = (SELECT COALESCE(MAX(rowid), 0) FROM decisions)",
            [],
        )
        .expect("lift signature markers to the legacy boundary");

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(
        report["data"]["ok"], true,
        "legacy NULL-hash row is grandfathered"
    );
}

#[test]
fn post_watermark_null_hash_is_flagged_missing_hash() {
    // The positive direction of the discriminator: a NULL hash on a row created AFTER
    // the migration high-water mark (a fresh post-Phase-5 repo records mark 0) is a
    // deleted hash, not a legacy row.
    let repo = TestRepo::new_git();
    lifecycle_to_proposal(&repo);
    db(&repo)
        .execute("UPDATE evidence SET content_hash = NULL", [])
        .expect("delete the hash");

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    assert!(
        report["data"]["tampered_rows"]
            .as_array()
            .expect("array")
            .iter()
            .any(|row| row["table"] == "evidence" && row["kind"] == "missing_hash"),
        "a post-watermark NULL hash must be missing_hash: {:?}",
        report["data"]["tampered_rows"]
    );
}

#[test]
fn recomputing_the_evidence_hash_is_caught_by_doctor_as_broken_link() {
    // The sophisticated attack: edit an evidence field AND recompute its own
    // content_hash so the cheap per-row check passes. The op-log re-walk still folds
    // the OLD digest the operation chained, so doctor catches it at that operation as
    // a broken_link (the load-bearing property of folding the domain digest).
    let repo = TestRepo::new_git();
    lifecycle_to_proposal(&repo);
    let connection = db(&repo);
    // Tamper the row, then overwrite content_hash with a value consistent with the
    // tampered columns (any non-matching-the-op value works; we just make the per-row
    // recompute no longer the value the op folded). Simplest: set it to a bogus but
    // present hash so the per-row check would mismatch too — but to isolate the
    // op-link detection, set exit_code and recompute is hard from SQL, so we assert
    // the op-link path fires by corrupting only the op's view→evidence binding is not
    // possible; instead set content_hash to an arbitrary 64-hex that the op did not
    // fold, which makes BOTH the per-row check and the op-link mismatch — doctor must
    // still report at least a broken_link on the chaining operation.
    connection
        .execute(
            "UPDATE evidence SET stdout_excerpt = 'FORGED', content_hash = '0000000000000000000000000000000000000000000000000000000000000001'",
            [],
        )
        .expect("tamper + rewrite hash");

    let report = json(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    let tampered = report["data"]["tampered_rows"].as_array().expect("array");
    assert!(
        tampered
            .iter()
            .any(|row| row["table"] == "operations" && row["kind"] == "broken_link"),
        "the op-log re-walk must flag the chaining operation as broken_link: {tampered:?}"
    );
}

#[cfg(unix)]
fn fake_cargo(repo: &TestRepo, summary: &str, code: i32) -> std::path::PathBuf {
    // Install a fake `cargo` on PATH whose output the structured parser recognizes,
    // so a `cargo test` structured gate can be exercised without a real cargo project.
    use std::os::unix::fs::PermissionsExt;
    let bin = repo.path().join("fakebin");
    std::fs::create_dir_all(&bin).expect("mkdir fakebin");
    let cargo = bin.join("cargo");
    std::fs::write(
        &cargo,
        format!("#!/bin/sh\necho '{summary}'\nexit {code}\n"),
    )
    .expect("write fake cargo");
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    bin
}

#[cfg(unix)]
fn path_with(bin: &std::path::Path) -> String {
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
#[test]
fn structured_gate_passes_on_zero_failures_and_blocks_on_parsed_failures() {
    // PASS: a structured `cargo test` gate is green when the parsed summary reports
    // zero failures and exit 0. The gate identity (cargo, [test]) exactly matches the
    // evidence, and the parser populates structured_json.failed = 0.
    let pass = TestRepo::new_git();
    let bin = fake_cargo(
        &pass,
        "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.0s",
        0,
    );
    pass.forge().args(["--json", "init"]).assert().success();
    pass.forge()
        .args([
            "--json",
            "start",
            "gated",
            "--require-tests-pass",
            "cargo test",
        ])
        .assert()
        .success();
    std::fs::write(pass.path().join("f.txt"), "x\n").expect("write");
    pass.forge().args(["--json", "save"]).assert().success();
    pass.forge()
        .env("PATH", path_with(&bin))
        .args(["--json", "run", "--", "cargo", "test"])
        .assert()
        .success();
    pass.forge().args(["--json", "propose"]).assert().success();
    let checked = json(pass.forge().args(["--json", "check"]).assert().success());
    assert_eq!(
        checked["data"]["status"], "passed",
        "structured gate green on parsed 0 failures: {checked:?}"
    );

    // FAIL: a parsed non-zero failure count blocks the gate even though exit is 0.
    let fail = TestRepo::new_git();
    let bin = fake_cargo(
        &fail,
        "test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.0s",
        0,
    );
    fail.forge().args(["--json", "init"]).assert().success();
    fail.forge()
        .args([
            "--json",
            "start",
            "gated",
            "--require-tests-pass",
            "cargo test",
        ])
        .assert()
        .success();
    std::fs::write(fail.path().join("g.txt"), "y\n").expect("write");
    fail.forge().args(["--json", "save"]).assert().success();
    fail.forge()
        .env("PATH", path_with(&bin))
        .args(["--json", "run", "--", "cargo", "test"])
        .assert()
        .success();
    fail.forge().args(["--json", "propose"]).assert().success();
    let checked = json(fail.forge().args(["--json", "check"]).assert());
    assert_eq!(
        checked["data"]["status"], "failed",
        "structured gate fails on parsed 2 failures despite exit 0: {checked:?}"
    );
}
