//! NER-138 Phase 7 slice 3: navigable native history end-to-end through the CLI —
//! justified commit-on-accept, HEAD advancement, base progression, the HEAD-from-ledger
//! reconcile, and (later units) log / checkout / undo / doctor.

mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn head(path: &Path) -> Option<String> {
    std::fs::read_to_string(path.join(".forge/refs/HEAD"))
        .ok()
        .map(|raw| raw.trim().to_string())
}

fn db(path: &Path) -> Connection {
    Connection::open(path.join(".forge/forge.db")).expect("open forge db")
}

/// Drive a native repo through `init → start → save → run → propose → check`, leaving a
/// checked proposal ready to accept. Returns nothing; the caller reads HEAD / the ledger.
fn prepare_native_proposal(repo: &TestRepo) {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "native history"])
        .assert()
        .success();
    std::fs::write(repo.path().join("feature.txt"), "native\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
}

#[test]
fn native_accept_writes_a_justified_commit_and_advances_head() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);

    // Genesis HEAD is set at `start`, before any accept.
    let genesis = head(repo.path()).expect("genesis HEAD exists after start");
    assert!(genesis.starts_with("f1:commit:sha256:"));

    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"]
        .as_str()
        .expect("native accept surfaces commit_id in JSON data")
        .to_string();
    assert!(commit_id.starts_with("f1:commit:sha256:"));
    assert_ne!(
        commit_id, genesis,
        "accept advances HEAD off the genesis base"
    );

    // HEAD advanced to the new commit, and the ledger records it.
    assert_eq!(head(repo.path()).as_deref(), Some(commit_id.as_str()));
    let ledger_commit: Option<String> = db(repo.path())
        .query_row(
            "SELECT commit_id FROM decisions WHERE decision = 'accepted' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("decision row");
    assert_eq!(ledger_commit.as_deref(), Some(commit_id.as_str()));
}

#[test]
fn git_accept_leaves_commit_id_null_and_no_ref_store() {
    let repo = TestRepo::new_git();
    // Default (git) backend.
    repo.forge().args(["--json", "init"]).assert().success();
    repo.forge()
        .args(["--json", "start", "git history"])
        .assert()
        .success();
    std::fs::write(repo.path().join("feature.txt"), "git\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();

    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    // commit_id is omitted entirely (skip_serializing_if) for a git-backend accept.
    assert!(accepted["data"].get("commit_id").is_none());
    assert!(
        head(repo.path()).is_none(),
        "git repos have no native ref store"
    );
    let ledger_commit: Option<String> = db(repo.path())
        .query_row(
            "SELECT commit_id FROM decisions WHERE decision = 'accepted' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("decision row");
    assert_eq!(ledger_commit, None);
}

#[test]
fn native_accept_replay_same_request_id_writes_no_second_commit() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);

    let first = json_output(
        repo.forge()
            .args(["--json", "accept", "--request-id", "accept-once"])
            .assert()
            .success(),
    );
    let commit_id = first["data"]["commit_id"].as_str().unwrap().to_string();

    // Replaying the SAME request id does NOT run a second `decide` transaction — so no
    // second commit is written and HEAD is unchanged (the load-bearing slice-3 property).
    // Accept records its op-log entry under the decision verb ("accepted"), so the
    // sequential replay surfaces the pre-existing REQUEST_ID_CONFLICT (op command
    // "accepted" vs CLI command "accept") rather than an idempotent stub; either way the
    // decision/commit side effects do not repeat.
    let replay = json_output(
        repo.forge()
            .args(["--json", "accept", "--request-id", "accept-once"])
            .assert()
            .failure(),
    );
    assert_eq!(replay["errors"][0]["code"], "REQUEST_ID_CONFLICT");
    assert_eq!(
        head(repo.path()).as_deref(),
        Some(commit_id.as_str()),
        "replay must not advance HEAD"
    );
    let decision_count: i64 = db(repo.path())
        .query_row("SELECT COUNT(*) FROM decisions", [], |row| row.get(0))
        .expect("count decisions");
    assert_eq!(decision_count, 1, "replay must not write a second decision");
}

#[test]
fn native_reaccept_with_new_request_id_is_stale_after_head_advanced() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    repo.forge()
        .args(["--json", "accept", "--request-id", "first"])
        .assert()
        .success();

    // A fresh accept of the same (now-accepted) proposal: HEAD advanced past the proposal's
    // base, so the stale-base check fires — never a double-commit.
    let second = json_output(
        repo.forge()
            .args(["--json", "accept", "--request-id", "second"])
            .assert()
            .failure(),
    );
    assert_eq!(second["errors"][0]["code"], "STALE_BASE");
}

#[test]
fn reconcile_advances_head_from_a_torn_accept() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let genesis = head(repo.path()).expect("genesis HEAD");

    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();
    assert_eq!(head(repo.path()).as_deref(), Some(commit_id.as_str()));

    // Simulate a crash AFTER the decision committed but BEFORE set_head ran: rewind the
    // ref-store HEAD to the genesis while the ledger still records the accepted commit_id.
    std::fs::write(repo.path().join(".forge/refs/HEAD"), &genesis).expect("rewind HEAD");
    assert_eq!(head(repo.path()).as_deref(), Some(genesis.as_str()));

    // The next lock-holding command runs reconcile_native_head first, healing HEAD forward
    // to the ledger's latest commit_id before the command's own logic runs.
    repo.forge()
        .args(["--json", "start", "after the crash"])
        .assert()
        .success();
    assert_eq!(
        head(repo.path()).as_deref(),
        Some(commit_id.as_str()),
        "reconcile advances HEAD to the ledger tip after a torn accept"
    );
}

#[test]
fn native_log_walks_the_dag_tip_to_genesis() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();

    let logged = json_output(repo.forge().args(["--json", "log"]).assert().success());
    let commits = logged["data"]["commits"].as_array().expect("commits array");
    // The accepted commit is the tip (first), with its full justification; genesis follows.
    assert!(commits.len() >= 2, "tip + genesis at minimum");
    assert_eq!(commits[0]["commit_id"], commit_id);
    assert!(commits[0]["decision_id"].is_string());
    assert!(commits[0]["actor"].is_string());
    assert!(commits[0]["authored_time"].is_i64());
    // intent_id is the opaque intent id (not the intent text).
    assert!(commits[0]["intent_id"]
        .as_str()
        .unwrap()
        .starts_with("intent"));
    // The last entry is the genesis: empty parents, no justification fields.
    let genesis = commits.last().unwrap();
    assert_eq!(genesis["parents"].as_array().unwrap().len(), 0);
    assert!(genesis.get("decision_id").is_none());

    // log is read-only: it took no lock and did not advance HEAD.
    assert_eq!(head(repo.path()).as_deref(), Some(commit_id.as_str()));
}

#[test]
fn native_log_filters_by_intent() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();

    // The intent id is the opaque id the commit recorded — discover it from an unfiltered log.
    let all = json_output(repo.forge().args(["--json", "log"]).assert().success());
    let intent_id = all["data"]["commits"][0]["intent_id"]
        .as_str()
        .expect("the accepted commit carries an intent id")
        .to_string();

    let matching = json_output(
        repo.forge()
            .args(["--json", "log", "--intent", &intent_id])
            .assert()
            .success(),
    );
    assert!(
        !matching["data"]["commits"].as_array().unwrap().is_empty(),
        "the accepted commit is under its intent id"
    );
    let none = json_output(
        repo.forge()
            .args(["--json", "log", "--intent", "no-such-intent"])
            .assert()
            .success(),
    );
    assert!(
        none["data"]["commits"].as_array().unwrap().is_empty(),
        "no commits under an unknown intent"
    );
}

#[test]
fn checkout_materializes_a_past_commit_without_moving_the_base() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let genesis = head(repo.path()).expect("genesis HEAD");
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let tip = accepted["data"]["commit_id"].as_str().unwrap().to_string();

    // The accepted state has feature.txt; the genesis (pre-save) does not.
    assert!(repo.path().join("feature.txt").exists());

    let out = json_output(
        repo.forge()
            .args(["--json", "checkout", &genesis])
            .assert()
            .success(),
    );
    assert_eq!(out["data"]["commit_id"], genesis);
    assert_eq!(out["data"]["base_unchanged"], true);
    // The genesis tree (README.md only) is materialized; feature.txt is gone.
    assert!(!repo.path().join("feature.txt").exists());
    // Checkout did NOT move the base anchor: HEAD is still the accepted tip.
    assert_eq!(head(repo.path()).as_deref(), Some(tip.as_str()));
}

#[test]
fn checkout_refuses_a_dirty_worktree() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let genesis = head(repo.path()).expect("genesis HEAD");
    repo.forge().args(["--json", "accept"]).assert().success();

    // Dirty the worktree with an unsaved edit.
    std::fs::write(repo.path().join("feature.txt"), "uncommitted edit\n").unwrap();
    let out = json_output(
        repo.forge()
            .args(["--json", "checkout", &genesis])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "DIRTY_WORKTREE");
    // The worktree was not clobbered.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("feature.txt")).unwrap(),
        "uncommitted edit\n"
    );
}

#[test]
fn checkout_unknown_commit_is_not_corruption() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();

    // A syntactically-valid but never-written commit id is a user error, NOT corruption.
    let unknown = format!("f1:commit:sha256:{}", "a".repeat(64));
    let out = json_output(
        repo.forge()
            .args(["--json", "checkout", &unknown])
            .assert()
            .failure(),
    );
    assert_ne!(
        out["errors"][0]["code"], "NATIVE_HISTORY_CORRUPT",
        "a typo'd commit id must not inflate the corruption rate"
    );
}

#[test]
fn undo_restores_the_prior_snapshot() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "undo flow"])
        .assert()
        .success();
    // save A
    std::fs::write(repo.path().join("file.txt"), "state A\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();
    // save B
    std::fs::write(repo.path().join("file.txt"), "state B\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file.txt")).unwrap(),
        "state B\n"
    );

    // undo restores the worktree to state A and records the undo as an op.
    let out = json_output(repo.forge().args(["--json", "undo"]).assert().success());
    assert!(out["data"]["restored_snapshot_id"].is_string());
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file.txt")).unwrap(),
        "state A\n",
        "undo restores the prior snapshot's content"
    );
    // The decisions ledger is untouched by undo (no accepts here, but the invariant holds):
    // undo is append-only — it added an "undo" op, not deleted anything.
    let ops: i64 = db(repo.path())
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE command = 'undo'",
            [],
            |row| row.get(0),
        )
        .expect("count undo ops");
    assert_eq!(ops, 1, "undo is recorded as a forward op-log operation");
}

#[test]
fn undo_with_nothing_to_undo_is_a_clear_error_not_a_crash() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "nothing yet"])
        .assert()
        .success();
    // No save yet → nothing to undo. Clear error, not a panic.
    let out = json_output(repo.forge().args(["--json", "undo"]).assert().failure());
    let message = out["errors"][0]["message"].as_str().unwrap();
    assert!(
        message.contains("nothing to undo"),
        "clear message: {message}"
    );
    assert!(!message.contains('/'), "path-free: {message}");
}

#[test]
fn undo_then_gc_dry_run_does_not_flag_an_accepted_commit() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();

    // A second save so there is a prior snapshot to undo to.
    std::fs::write(repo.path().join("feature.txt"), "more\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "undo"]).assert().success();

    // The accepted commit (referenced by decisions.commit_id, which undo never deletes) is
    // NOT reported as unreachable garbage by gc.
    let gc = json_output(
        repo.forge()
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    let unreachable = gc["data"]["unreachable_native_objects"]
        .as_array()
        .expect("unreachable array");
    assert!(
        !unreachable.iter().any(|u| u == &commit_id),
        "an accepted commit must stay reachable after undo"
    );
}

#[test]
fn dangling_ledger_commit_id_surfaces_native_history_corrupt() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let genesis = head(repo.path()).expect("genesis HEAD");
    repo.forge().args(["--json", "accept"]).assert().success();

    // Corrupt the store: point the ledger tip at a commit whose object does not exist, and
    // rewind HEAD so reconcile must walk to it. This is the store-before-DB violation the
    // typed NativeHistoryCorrupt error makes agent-distinguishable from transient IO.
    let missing = format!("f1:commit:sha256:{}", "f".repeat(64));
    db(repo.path())
        .execute(
            "UPDATE decisions SET commit_id = ?1 WHERE decision = 'accepted'",
            [&missing],
        )
        .expect("plant dangling commit_id");
    std::fs::write(repo.path().join(".forge/refs/HEAD"), &genesis).expect("rewind HEAD");

    let output = json_output(
        repo.forge()
            .args(["--json", "start", "after corruption"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "NATIVE_HISTORY_CORRUPT");
    assert_eq!(output["errors"][0]["details"]["kind"], "dangling_commit_id");
    // S1: the error carries only the opaque commit id, no filesystem path.
    let message = output["errors"][0]["message"].as_str().unwrap();
    assert!(
        !message.contains('/'),
        "error message leaked a path: {message}"
    );
}
