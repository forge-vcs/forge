//! Crash-injection harness (NER-132 U6).
//!
//! Each test runs a real `forge` child process with `FORGE_CRASH_POINT` set, which
//! makes the binary `std::process::abort()` at an instrumented durability boundary
//! (debug-only; see `forge_content::maybe_crash`). `abort()` models a hard kill —
//! SIGKILL / sandbox teardown / OOM — running no destructors and flushing nothing,
//! exactly the failure agents hit. After the crash we reopen the repo (WAL recovery
//! runs on open) and assert the durability invariant held, plus `doctor` is clean.
//!
//! Proven here (Linux + macOS; `abort()` and the process harness are portable):
//! - object-fsync → DB-commit: a crash between them never commits a ref without its
//!   object (the safe state is object-present / ref-absent).
//! - DB-commit → checkpoint: a committed ref survives WAL recovery with its object.
//! - mid-restore: already-renamed files are whole (never torn) and no temp is
//!   orphaned, so `doctor` reports zero half-applied worktrees.
//!
//! This is crash-*consistency* of the ordering given the OS's fsync guarantees —
//! not block-device power-loss fault injection.

mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn forge_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("forge")
}

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

/// Run `forge <args>` in `repo` with the named crash point armed, and assert the
/// process died from the injected abort rather than completing.
fn run_until_crash(repo: &Path, crash_point: &str, args: &[&str]) {
    let output = Command::new(forge_bin())
        .args(args)
        .env("FORGE_CRASH_POINT", crash_point)
        .current_dir(repo)
        .output()
        .expect("spawn forge");
    assert!(
        !output.status.success(),
        "expected injected crash `{crash_point}` to abort `forge {args:?}`, but it succeeded:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert!(
            output.status.signal().is_some(),
            "expected `{crash_point}` to terminate by signal (abort), got {:?}",
            output.status
        );
    }
}

fn snapshot_count(repo: &Path) -> i64 {
    Connection::open(repo.join(".forge/forge.db"))
        .expect("open forge.db")
        .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
        .expect("count snapshots")
}

/// Run `forge doctor` (reopening the repo, so WAL recovery has run) and return the
/// parsed envelope.
fn doctor(repo: &Path) -> Value {
    let output = Command::new(forge_bin())
        .args(["--json", "doctor"])
        .current_dir(repo)
        .output()
        .expect("spawn forge doctor");
    serde_json::from_slice(&output.stdout).expect("valid doctor json")
}

fn empty(value: &Value, key: &str) -> bool {
    value["data"][key]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(false)
}

#[test]
fn crash_after_object_fsync_before_db_commit_leaves_no_dangling_ref() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "crash before commit"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "to be saved\n").expect("write file");

    assert_eq!(snapshot_count(repo.path()), 0);
    run_until_crash(
        repo.path(),
        "after_object_fsync_before_db_commit",
        &["--json", "save"],
    );

    // Objects were fsynced but the content_ref was never committed: no snapshot row,
    // and doctor finds zero dangling refs — the on-disk objects are merely
    // unreferenced, never a committed-ref-without-object. The forbidden state cannot
    // occur because objects are durable before the commit even begins.
    assert_eq!(
        snapshot_count(repo.path()),
        0,
        "no content_ref may be committed when the crash precedes save_snapshot"
    );
    let report = doctor(repo.path());
    assert!(empty(&report, "dangling_content_refs"));
    assert!(empty(&report, "half_applied_worktrees"));
}

#[test]
fn crash_after_db_commit_keeps_ref_and_object_durable() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "crash after commit"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "committed before crash\n").expect("write file");

    run_until_crash(
        repo.path(),
        "after_db_commit_before_checkpoint",
        &["--json", "save"],
    );

    // The commit is durable (synchronous=NORMAL fsyncs the WAL on commit) and is
    // replayed on reopen: exactly one snapshot row, and doctor verifies its object
    // is present (zero dangling refs) — a committed ref implies a durable object.
    assert_eq!(
        snapshot_count(repo.path()),
        1,
        "the committed snapshot must survive the crash via WAL recovery"
    );
    let report = doctor(repo.path());
    assert_eq!(report["data"]["ok"], true, "doctor: {report}");
    assert!(empty(&report, "dangling_content_refs"));
}

#[test]
fn crash_mid_restore_leaves_whole_files_and_no_orphan_temp() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", "crash mid restore"])
        .assert()
        .success();

    // Multiple files so a mid-restore crash lands after some are renamed.
    std::fs::write(repo.path().join("a.txt"), "v1\n").expect("a v1");
    std::fs::write(repo.path().join("b.txt"), "v1\n").expect("b v1");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let snapshot_id = saved["data"]["snapshot_id"].as_str().unwrap().to_string();

    // Move to v2 and save again so the worktree is clean (matches the latest
    // snapshot) and `restore` will not bail on a dirty worktree.
    std::fs::write(repo.path().join("a.txt"), "v2\n").expect("a v2");
    std::fs::write(repo.path().join("b.txt"), "v2\n").expect("b v2");
    repo.forge().args(["--json", "save"]).assert().success();

    run_until_crash(
        repo.path(),
        "mid_restore",
        &["--json", "restore", &snapshot_id, "--yes"],
    );

    // Per-file atomicity: every file is a complete version (never a torn mix), even
    // though the multi-file restore was interrupted. (Which files were reached
    // depends on iteration order; the invariant is wholeness, not which version.)
    let a = std::fs::read_to_string(repo.path().join("a.txt")).expect("read a");
    let b = std::fs::read_to_string(repo.path().join("b.txt")).expect("read b");
    assert!(
        a == "v1\n" || a == "v2\n",
        "a.txt is a whole version, got {a:?}"
    );
    assert!(
        b == "v1\n" || b == "v2\n",
        "b.txt is a whole version, got {b:?}"
    );

    // No restore temp was orphaned by the crash, so doctor reports a clean worktree.
    let report = doctor(repo.path());
    assert!(
        empty(&report, "half_applied_worktrees"),
        "no orphaned restore temp expected, got {}",
        report["data"]["half_applied_worktrees"]
    );
}
