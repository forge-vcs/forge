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
    // A never-written commit id is a USER error (COMMAND_FAILED), NOT corruption — pin the
    // actual code positively, not just "anything but NATIVE_HISTORY_CORRUPT".
    assert_eq!(out["errors"][0]["code"], "COMMAND_FAILED");
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

/// A `forge` command builder whose child PATH contains ONLY `sh` (for `run`) and NO git —
/// so any internal `git` spawn fails. The native lifecycle must complete anyway.
#[cfg(unix)]
fn forge_without_git(repo: &TestRepo, git_free_bin: &std::path::Path) -> assert_cmd::Command {
    let mut command = repo.forge();
    command.env("PATH", git_free_bin);
    command
}

#[cfg(unix)]
#[test]
fn native_lifecycle_runs_with_git_removed_from_path() {
    // NER-138 Phase 7 WHOLE-PHASE exit criterion: a native-backend repo completes
    // init → start → save → run → propose → check → accept → restore, walks its history (log),
    // checks out a past commit, and reports healthy (doctor) — ALL with git removed from PATH.
    let repo = TestRepo::new_git(); // setup uses the test's own git; forge runs git-free below
    let bin = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/bin/sh", bin.path().join("sh")).unwrap();
    let no_git = || forge_without_git(&repo, bin.path());

    no_git()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    no_git()
        .args(["--json", "start", "git-free lifecycle"])
        .assert()
        .success();
    std::fs::write(repo.path().join("feature.txt"), "git-free\n").unwrap();
    let saved = json_output(no_git().args(["--json", "save"]).assert().success());
    let snapshot_id = saved["data"]["snapshot_id"].as_str().unwrap().to_string();
    no_git()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    no_git().args(["--json", "propose"]).assert().success();
    no_git().args(["--json", "check"]).assert().success();
    let accepted = json_output(no_git().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();
    assert!(commit_id.starts_with("f1:commit:sha256:"));
    // restore the just-saved snapshot (worktree is clean / equals it) — git-free.
    no_git()
        .args(["--json", "restore", &snapshot_id, "--yes"])
        .assert()
        .success();
    // walk history, check out the genesis (worktree clean == latest snapshot), report health.
    let logged = json_output(no_git().args(["--json", "log"]).assert().success());
    assert!(!logged["data"]["commits"].as_array().unwrap().is_empty());
    let genesis = head(repo.path()).map(|_| ()); // HEAD file exists (no git needed to read it)
    assert!(genesis.is_some());
    // checkout the accepted commit's parent (genesis) — find it from log's last entry.
    let genesis_commit = logged["data"]["commits"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["commit_id"]
        .as_str()
        .unwrap()
        .to_string();
    no_git()
        .args(["--json", "checkout", &genesis_commit])
        .assert()
        .success();
    let doctor = json_output(no_git().args(["--json", "doctor"]).assert().success());
    assert_eq!(doctor["data"]["ok"], true, "healthy native repo, git-free");
}

#[cfg(unix)]
#[test]
fn native_undo_runs_with_git_removed_from_path() {
    let repo = TestRepo::new_git();
    let bin = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/bin/sh", bin.path().join("sh")).unwrap();
    let no_git = || forge_without_git(&repo, bin.path());

    no_git()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    no_git()
        .args(["--json", "start", "git-free undo"])
        .assert()
        .success();
    std::fs::write(repo.path().join("f.txt"), "A\n").unwrap();
    no_git().args(["--json", "save"]).assert().success();
    std::fs::write(repo.path().join("f.txt"), "B\n").unwrap();
    no_git().args(["--json", "save"]).assert().success();
    no_git().args(["--json", "undo"]).assert().success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        "A\n",
        "undo restored the prior snapshot — git-free"
    );
}

#[test]
fn doctor_reports_a_healthy_native_dag() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], true);
    assert!(
        report["data"]["native_history_issues"]
            .as_array()
            .unwrap()
            .is_empty(),
        "a healthy native DAG has no integrity breaks"
    );
}

#[test]
fn doctor_detects_a_dangling_commit_object() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();

    // Delete the accepted commit's object file: the ledger still references it, so doctor's
    // DAG walk + decisions cross-check must REPORT a dangling commit_id (not panic, not raise).
    let digest = commit_id.rsplit(':').next().unwrap();
    let object = repo
        .path()
        .join(format!(".forge/objects/sha256/{}/{}", &digest[..2], digest));
    std::fs::remove_file(&object).expect("delete commit object");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    let issues = report["data"]["native_history_issues"]
        .as_array()
        .expect("native_history_issues array");
    assert!(
        issues
            .iter()
            .any(|i| i["kind"] == "dangling_commit_id" && i["commit_id"] == commit_id),
        "doctor must report the dangling commit_id: {issues:?}"
    );
}

#[test]
fn accept_populates_evidence_digest_from_the_deciding_evidence() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    // A required gate so the accepted check has a DECIDING evidence row.
    repo.forge()
        .args(["--json", "start", "ev digest", "--require", "sh -c true"])
        .assert()
        .success();
    std::fs::write(repo.path().join("f.txt"), "x\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();

    // The justified commit's evidence_digest is the deciding evidence's content_hash (opaque
    // 64-hex), not None — proving R4 is wired, not just the Hex64 type unit-tested.
    let logged = json_output(repo.forge().args(["--json", "log"]).assert().success());
    let digest = logged["data"]["commits"][0]["evidence_digest"]
        .as_str()
        .expect("evidence_digest populated from the deciding gate");
    assert_eq!(digest.len(), 64);
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    let matches: i64 = db(repo.path())
        .query_row(
            "SELECT COUNT(*) FROM evidence WHERE content_hash = ?1",
            [digest],
            |row| row.get(0),
        )
        .expect("count evidence");
    assert!(
        matches >= 1,
        "evidence_digest must equal a real evidence row's content_hash"
    );
}

#[test]
fn doctor_detects_a_dangling_parent() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let genesis = head(repo.path()).expect("genesis HEAD"); // the accepted commit's parent
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();

    // Delete the genesis (parent) commit object: doctor's DAG walk from the tip reads the tip
    // ok, then finds its parent object missing -> DanglingParent (commit_id=tip, related=genesis).
    let g_digest = genesis.rsplit(':').next().unwrap();
    let g_obj = repo.path().join(format!(
        ".forge/objects/sha256/{}/{}",
        &g_digest[..2],
        g_digest
    ));
    std::fs::remove_file(&g_obj).expect("delete genesis object");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    let issues = report["data"]["native_history_issues"].as_array().unwrap();
    assert!(
        issues.iter().any(|i| i["kind"] == "dangling_parent"
            && i["commit_id"] == commit_id
            && i["related_id"] == genesis),
        "doctor must report the dangling parent: {issues:?}"
    );
}

#[test]
fn doctor_detects_a_dangling_tree() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();
    let logged = json_output(repo.forge().args(["--json", "log"]).assert().success());
    let tree = logged["data"]["commits"][0]["tree"]
        .as_str()
        .unwrap()
        .to_string();

    // Delete the tip commit's tree object: doctor's DAG walk verifies the tree is reachable
    // and reports DanglingTree when it is not.
    let t_digest = tree.rsplit(':').next().unwrap();
    let t_obj = repo.path().join(format!(
        ".forge/objects/sha256/{}/{}",
        &t_digest[..2],
        t_digest
    ));
    std::fs::remove_file(&t_obj).expect("delete tree object");

    let report = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(report["data"]["ok"], false);
    let issues = report["data"]["native_history_issues"].as_array().unwrap();
    assert!(
        issues
            .iter()
            .any(|i| i["kind"] == "dangling_tree" && i["commit_id"] == commit_id),
        "doctor must report the dangling tree: {issues:?}"
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

/// NER-143 R1 (the headline bug): a second navigation command without an intervening `save`
/// must not spuriously fail `DIRTY_WORKTREE`. Pre-fix, the dirty-check compared the worktree
/// against the latest SAVED snapshot, so after `undo` restored snapshot A the next nav command
/// saw worktree(A) != latest-saved(B) and bricked. With the expected-content baseline, undo₁ sets
/// expected = A so the second nav passes (worktree == expected).
///
/// Note the deliberate snapshot-chain v0 semantics (UndoTarget docs): `undo` = "restore the
/// parent of the attached attempt's latest *snapshot*". Undo writes no snapshot, so the latest
/// snapshot stays B and repeated undo is idempotent (lands on A each time) — it does NOT walk
/// B→A→base. The op-log-rewind model that would walk multiple steps is deferred. The bug this
/// fixes is the spurious *error*, not the step count.
#[test]
fn undo_twice_without_intervening_save_succeeds() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "chained undo"])
        .assert()
        .success();
    std::fs::write(repo.path().join("file.txt"), "state A\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();
    std::fs::write(repo.path().join("file.txt"), "state B\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();

    // undo B -> A, then undo AGAIN with no save between (the chained case that used to brick).
    let first = json_output(repo.forge().args(["--json", "undo"]).assert().success());
    assert_eq!(first["status"], "success");
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file.txt")).unwrap(),
        "state A\n"
    );
    // Pre-fix this second undo failed DIRTY_WORKTREE (worktree A != latest-saved B). Post-fix it
    // succeeds; snapshot-chain v0 means it idempotently lands on A again (parent of latest
    // snapshot B), which is the point — the command runs instead of bricking.
    let second = json_output(repo.forge().args(["--json", "undo"]).assert().success());
    assert_eq!(
        second["status"], "success",
        "a second undo without an intervening save must not spuriously fail DIRTY_WORKTREE"
    );
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file.txt")).unwrap(),
        "state A\n"
    );
}

/// NER-143 R1 (safety property preserved): a GENUINE unsaved edit between two nav commands must
/// still be refused. After `undo` the expected ref is the restored snapshot; a hand-edit then
/// makes the worktree match NEITHER expected NOR the next target, so the next nav refuses
/// `DIRTY_WORKTREE` — the refuse-before-materialize invariant is not traded away for the fix.
#[test]
fn nav_then_unsaved_edit_still_refuses_dirty_worktree() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "dirty after nav"])
        .assert()
        .success();
    std::fs::write(repo.path().join("file.txt"), "state A\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();
    std::fs::write(repo.path().join("file.txt"), "state B\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();

    // undo B -> A (clean), then make an unsaved edit.
    repo.forge().args(["--json", "undo"]).assert().success();
    std::fs::write(repo.path().join("file.txt"), "unsaved edit\n").unwrap();

    let output = json_output(repo.forge().args(["--json", "undo"]).assert().failure());
    assert_eq!(output["status"], "error");
    assert_eq!(output["errors"][0]["code"], "DIRTY_WORKTREE");
    // The worktree is NOT clobbered on the refuse path.
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file.txt")).unwrap(),
        "unsaved edit\n"
    );
}

/// NER-143 R1 / DR-F1 (crash-safety hinge): if a materialize completes but the record txn does
/// not commit (crash / `CurrentStateChanged` CAS-loss), the worktree holds the target while
/// `expected_content_ref` is still the prior ref. Re-running the same nav command must HEAL via
/// the `worktree == target` clause, not brick on `DIRTY_WORKTREE`. Simulated by rolling
/// `expected_content_ref` back to its prior value while the worktree holds the checkout target.
#[test]
fn interrupted_nav_self_heals_via_target_match() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();

    // Capture the current expected ref (the accepted state), then check out the commit so the
    // worktree holds its tree and expected == that tree.
    let prior_expected: String = db(repo.path())
        .query_row(
            "SELECT expected_content_ref FROM current_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .expect("expected ref after accept");
    repo.forge()
        .args(["--json", "checkout", &commit_id])
        .assert()
        .success();

    // Simulate the crash window: the materialize happened (worktree holds the target) but the
    // record txn that set expected_content_ref was lost — roll it back to the prior value.
    db(repo.path())
        .execute(
            "UPDATE current_state SET expected_content_ref = ?1 WHERE singleton = 1",
            [&prior_expected],
        )
        .expect("simulate lost record txn");

    // Re-running the same checkout must HEAL (worktree already == target), not fail DIRTY_WORKTREE.
    let healed = json_output(
        repo.forge()
            .args(["--json", "checkout", &commit_id])
            .assert()
            .success(),
    );
    assert_eq!(
        healed["status"], "success",
        "re-running an interrupted nav must self-heal via the worktree==target clause"
    );
}

/// NER-143 R3: `undo` must never restore another attempt's content into the attached attempt's
/// worktree. Bound to attempt B (whose worktree holds B's latest), `undo` reverses B's last save
/// — even when a DIFFERENT attempt A holds the repo-wide latest snapshot (saved most recently).
/// Pre-fix, `undo_target` selected the repo-wide latest (A) and would restore A's parent content
/// into B's worktree.
#[test]
fn undo_does_not_restore_another_attempts_content() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json_output(
        repo.forge()
            .args(["--json", "start", "attempt A"])
            .assert()
            .success(),
    );
    let intent = started["data"]["intent_id"].as_str().unwrap().to_string();
    let attempt_a = started["data"]["attempt_id"].as_str().unwrap().to_string();
    // Attempt B's two saves, so B has a parent chain to undo into.
    let b = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", &intent])
            .assert()
            .success(),
    );
    let attempt_b = b["data"]["attempt_id"].as_str().unwrap().to_string();
    repo.forge()
        .args(["--json", "attempt", "attach", &attempt_b])
        .assert()
        .success();
    std::fs::write(repo.path().join("file.txt"), "B one\n").unwrap();
    repo.forge()
        .args(["--json", "save", "--attempt", &attempt_b])
        .assert()
        .success();
    std::fs::write(repo.path().join("file.txt"), "B two\n").unwrap();
    repo.forge()
        .args(["--json", "save", "--attempt", &attempt_b])
        .assert()
        .success();

    // Attempt A saves LAST, so A now holds the repo-wide latest snapshot.
    repo.forge()
        .args(["--json", "attempt", "attach", &attempt_a])
        .assert()
        .success();
    std::fs::write(repo.path().join("file.txt"), "A latest\n").unwrap();
    repo.forge()
        .args(["--json", "save", "--attempt", &attempt_a])
        .assert()
        .success();

    // Re-attach B and restore B's latest so the worktree holds B's expected content, while A
    // remains the repo-wide latest snapshot.
    repo.forge()
        .args(["--json", "attempt", "attach", &attempt_b])
        .assert()
        .success();
    let b_second: String = db(repo.path())
        .query_row(
            "SELECT id FROM snapshots WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
            [&attempt_b],
            |row| row.get(0),
        )
        .expect("B latest snapshot id");
    repo.forge()
        .args(["--json", "restore", &b_second, "--yes"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(repo.path().join("file.txt")).unwrap(),
        "B two\n"
    );

    // undo while attached to B must restore B's FIRST save ("B one"), never A's content.
    let undo = json_output(repo.forge().args(["--json", "undo"]).assert().success());
    assert_eq!(undo["status"], "success");
    let restored = std::fs::read_to_string(repo.path().join("file.txt")).unwrap();
    assert_eq!(
        restored, "B one\n",
        "undo must restore the attached attempt B's prior save, not attempt A's content"
    );
}

/// NER-143 R4: `undone_operation_id` must reference the `save` operation that produced the
/// snapshot being reversed, NOT the op-log head. After a non-save head op (`run`), the head is
/// the run op; undo still reverses the last save, so `undone_operation_id` must be that save's op.
#[test]
fn undo_labels_the_save_operation_not_the_op_log_head() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "undo label"])
        .assert()
        .success();
    std::fs::write(repo.path().join("file.txt"), "one\n").unwrap();
    repo.forge().args(["--json", "save"]).assert().success();
    std::fs::write(repo.path().join("file.txt"), "two\n").unwrap();
    let save_b = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let save_b_op = save_b["operation_id"].as_str().unwrap().to_string();

    // A non-save head op: `run` records an op that becomes the op-log head.
    let run = json_output(
        repo.forge()
            .args(["--json", "run", "--", "sh", "-c", "true"])
            .assert()
            .success(),
    );
    let run_op = run["operation_id"].as_str().unwrap().to_string();
    assert_ne!(save_b_op, run_op);

    let undo = json_output(repo.forge().args(["--json", "undo"]).assert().success());
    assert_eq!(
        undo["data"]["undone_operation_id"], save_b_op,
        "undone_operation_id must be the save that created the reversed snapshot, not the run head"
    );
    assert_ne!(
        undo["data"]["undone_operation_id"], run_op,
        "undone_operation_id must NOT be the op-log head after a non-save head op"
    );
}
