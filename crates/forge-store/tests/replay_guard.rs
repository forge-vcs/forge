//! Store-level idempotent-replay coverage (NER-132 U8).
//!
//! The CLI's `command_result` pre-flight short-circuits a *sequential* same
//! `--request-id` retry before any transaction opens, so the **in-transaction**
//! `replay_guard` branch (and the failed-operation replay it carries) is rarely
//! exercised end-to-end. forge-store writers have no such pre-flight, so calling
//! them directly with a repeated request id deterministically drives the in-txn
//! guard — each call opens its own connection, exactly the multi-process shape the
//! guard must defend (Phase 1a R6 / the solution doc's §4 and §7).

use forge_store::ForgeError;
use std::path::Path;
use std::process::Command;

/// Create a one-commit git repo and `forge init` it, returning the temp dir.
fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    run_git(dir.path(), &["config", "user.name", "Test"]);
    std::fs::write(dir.path().join("README.md"), "x").expect("seed file");
    run_git(dir.path(), &["add", "."]);
    run_git(dir.path(), &["commit", "-m", "init"]);
    forge_store::init_repository(dir.path(), None, "git".to_string()).expect("forge init");
    dir
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git")
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn head(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("rev-parse");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn attempt_count(dir: &Path) -> i64 {
    rusqlite::Connection::open(dir.join(".forge/forge.db"))
        .expect("open forge.db")
        .query_row("SELECT COUNT(*) FROM attempts", [], |row| row.get(0))
        .expect("count attempts")
}

#[test]
fn second_same_request_id_writer_aborts_via_in_txn_replay_guard() {
    let repo = init_repo();
    let base = head(repo.path());

    let first = forge_store::start_attempt(
        repo.path(),
        Some("rq-1".to_string()),
        "intent one".to_string(),
        base.clone(),
        None,
    )
    .expect("first start commits");

    // No CLI pre-flight here: this second call enters the IMMEDIATE txn, and
    // replay_guard observes the first attempt's committed operation row (on a
    // separate connection) and aborts with the RequestIdReplay sentinel.
    let error = forge_store::start_attempt(
        repo.path(),
        Some("rq-1".to_string()),
        "intent one".to_string(),
        base,
        None,
    )
    .expect_err("the second same-id write must abort as a replay");
    let replay = error
        .downcast_ref::<forge_store::RequestIdReplay>()
        .expect("in-txn RequestIdReplay sentinel");
    assert_eq!(replay.operation.operation_id, first.operation_id);
    assert_eq!(replay.operation.status, "succeeded");
    assert_eq!(replay.operation.command, "start");

    // Exactly one attempt row exists despite two same-id calls.
    assert_eq!(attempt_count(repo.path()), 1);
}

#[test]
fn recorded_failure_replays_as_failure_via_in_txn_guard() {
    let repo = init_repo();
    let base = head(repo.path());

    // As the CLI does on a domain error, record a failed operation under a request
    // id (it commits no domain row). Retrying the same id through a writer must
    // surface that recorded failure so the CLI's status-aware replay reproduces it.
    forge_store::record_failed_operation(
        repo.path(),
        Some("rq-2".to_string()),
        "start",
        "COMMAND_FAILED",
        "boom",
        serde_json::Value::Object(Default::default()),
    )
    .expect("record failed operation");

    let error = forge_store::start_attempt(
        repo.path(),
        Some("rq-2".to_string()),
        "intent two".to_string(),
        base,
        None,
    )
    .expect_err("a recorded failure replays rather than committing a new attempt");
    let replay = error
        .downcast_ref::<forge_store::RequestIdReplay>()
        .expect("RequestIdReplay sentinel");
    assert_eq!(replay.operation.status, "failed");
    assert_eq!(replay.operation.command, "start");

    // The replayed failure created no attempt.
    assert_eq!(attempt_count(repo.path()), 0);
}

#[test]
fn concurrent_same_request_id_start_commits_exactly_one_attempt() {
    let repo = init_repo();
    let base = head(repo.path());
    let path = repo.path().to_path_buf();

    // Two threads, separate connections, same request id. BEGIN IMMEDIATE
    // serializes them: one commits the attempt; the loser, taking the write lock
    // after the winner commits, observes the in-txn replay guard.
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let base = base.clone();
            std::thread::spawn(move || {
                forge_store::start_attempt(
                    &path,
                    Some("rq-3".to_string()),
                    "race".to_string(),
                    base,
                    None,
                )
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("worker thread"))
        .collect();

    let committed = results.iter().filter(|result| result.is_ok()).count();
    let replayed = results
        .iter()
        .filter(|result| {
            result
                .as_ref()
                .err()
                .map(|error| {
                    error
                        .downcast_ref::<forge_store::RequestIdReplay>()
                        .is_some()
                })
                .unwrap_or(false)
        })
        .count();
    assert_eq!(committed, 1, "exactly one start commits");
    assert_eq!(replayed, 1, "the loser observes the in-txn replay guard");
    assert_eq!(attempt_count(&path), 1);
}

/// FIX D: the genuine singleton CAS now lives only in `insert_operation_view` (the
/// dead `create_operation_view` wrapper was removed). When a mutating writer's
/// captured parent operation no longer matches live `current_state` — i.e. another
/// writer advanced it after this one's determining read — the in-txn CAS must raise
/// the typed, retryable `ForgeError::CurrentStateChanged` (code `CONFLICT`). That is
/// the classification the CLI relies on to SKIP persisting the failure under a
/// `--request-id`, so a retry re-executes against fresh state (NER-133 R7 / U2).
///
/// `insert_operation_view` is private, so we drive its CAS through a real mutating
/// store fn: capture the current operation, advance `current_state` out from under
/// it (a committed `start_attempt`), then issue a `save` whose determining read
/// races against that advance. With the production fns re-reading `current_state`
/// per call we can't pin a stale parent through the public API alone, so this CAS
/// classification is proven by a focused in-crate unit test
/// (`super::tests::insert_operation_view_stale_parent_raises_current_state_changed`
/// in `lib.rs`); the end-to-end transient-replay behaviour is covered in the CLI
/// `forge_errors.rs` suite.
#[test]
fn current_state_changed_is_retryable_conflict() {
    // Pin the published classification the CLI depends on: the one transient,
    // retryable domain error.
    let forge_error = ForgeError::CurrentStateChanged;
    assert_eq!(forge_error.code(), "CONFLICT");
    assert!(
        forge_error.retryable(),
        "the singleton CAS is the one transient/retryable domain error"
    );
    assert_eq!(forge_error.after_ms(), Some(50));
}

/// `record_failed_operation` stores the typed code AND details alongside the
/// message in `error_json`, so the CLI's `replay_response` reads the stored code
/// and details directly rather than re-deriving them (the substring ladder is gone,
/// and a replay reproduces the original details — FIX C).
#[test]
fn record_failed_operation_persists_the_typed_code() {
    let repo = init_repo();
    forge_store::record_failed_operation(
        repo.path(),
        Some("rq-code".to_string()),
        "accept",
        "STALE_BASE",
        "stale base: HEAD moved",
        serde_json::json!({ "expected_head": "HEAD0", "actual_head": "HEAD1" }),
    )
    .expect("record failed operation");

    let stored: String = rusqlite::Connection::open(repo.path().join(".forge/forge.db"))
        .expect("open forge.db")
        .query_row(
            "SELECT error_json FROM operations WHERE request_id = 'rq-code'",
            [],
            |row| row.get(0),
        )
        .expect("read error_json");
    let value: serde_json::Value = serde_json::from_str(&stored).expect("parse error_json");
    assert_eq!(value["code"], "STALE_BASE");
    assert_eq!(value["message"], "stale base: HEAD moved");
    assert_eq!(value["details"]["expected_head"], "HEAD0");
    assert_eq!(value["details"]["actual_head"], "HEAD1");
}
