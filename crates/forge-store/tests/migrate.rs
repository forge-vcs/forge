//! `forge_store::migrate(cwd)` — the transient, self-acquiring schema-upgrade
//! entrypoint (NER-133 U4).
//!
//! These exercise `migrate` directly against hand-built `.forge/forge.db` files in
//! real (git-initialized) temp repos: a behind DB upgrades to head, an at-head DB
//! is a no-op, a forward-versioned DB is refused with `UnknownSchemaVersion`, and
//! an uninitialized repo is a no-op (no panic, no lock).
//!
//! `migrate` reads only `schema_migrations` and applies DDL — it never touches a
//! domain row — so a behind-DB fixture needs only the baseline tables plus a
//! version stamp, not a valid domain state. Critically, a v1 fixture is built from
//! the reverted-001 baseline (which has NEITHER `content_backend` NOR
//! `attached_attempt_id`), not by deleting a row from a v2 DB (which would still
//! carry both columns and make re-applying 002 hit "duplicate column name").

use rusqlite::Connection;
use std::path::Path;

/// The reverted-001 baseline DDL (neither `content_backend` nor
/// `attached_attempt_id`), read from the shipped migration file so the fixture
/// can never drift from the real baseline.
const BASELINE_001: &str = include_str!("../migrations/001_init.sql");
/// The 002 ALTERs (both columns) — used to build an at-head fixture.
const COLUMNS_002: &str = include_str!("../migrations/002_columns.sql");
/// The 003 ALTER (`intents.check_spec_json`) — used to build an at-head fixture.
const CHECK_SPEC_003: &str = include_str!("../migrations/003_check_spec.sql");
/// The 004 integrity/actor migration — used to build an at-head fixture.
const INTEGRITY_004: &str = include_str!("../migrations/004_integrity_and_actor.sql");

/// Initialize a real git repo in a fresh temp dir (so `git rev-parse
/// --show-toplevel`, which `migrate` uses to resolve the root, succeeds).
fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temp dir");
    run_git(dir.path(), &["init"]);
    run_git(dir.path(), &["config", "user.email", "forge@example.test"]);
    run_git(dir.path(), &["config", "user.name", "Forge Test"]);
    dir
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// Create `<root>/.forge/forge.db`, returning the database path.
fn make_forge_db(root: &Path) -> std::path::PathBuf {
    let forge_dir = root.join(".forge");
    std::fs::create_dir_all(&forge_dir).expect("create .forge");
    forge_dir.join("forge.db")
}

/// Open a connection with `foreign_keys=ON` (matching `open_connection`'s pragma,
/// so the 002 `REFERENCES` ALTER behaves the same way in-fixture).
fn open(db: &Path) -> Connection {
    let conn = Connection::open(db).expect("open db");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("enable fks");
    conn
}

/// Stamp a `schema_migrations(version, name, applied_at_ms)` ledger WITHOUT the
/// `checksum` column (a genuine pre-runner v1 ledger), inserting one row per
/// version with a NULL checksum.
fn stamp_versions(conn: &Connection, versions: &[(i64, &str)]) {
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at_ms INTEGER NOT NULL
        );",
    )
    .expect("create schema_migrations");
    for (version, name) in versions {
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, 0)",
            rusqlite::params![version, name],
        )
        .expect("stamp version");
    }
}

/// Whether a table has a named column (by `PRAGMA table_info`).
fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table_info");
    let mut rows = statement.query([]).expect("query table_info");
    while let Some(row) = rows.next().expect("row") {
        let name: String = row.get(1).expect("column name");
        if name == column {
            return true;
        }
    }
    false
}

fn max_version(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )
    .expect("max version")
}

#[test]
fn behind_db_upgrades_to_head() {
    let repo = git_repo();
    let db = make_forge_db(repo.path());
    {
        let conn = open(&db);
        conn.execute_batch(BASELINE_001)
            .expect("apply reverted-001 baseline");
        stamp_versions(&conn, &[(1, "001_init")]);
        // Sanity: a genuine v1 fixture has NEITHER column yet.
        assert!(!has_column(&conn, "repositories", "content_backend"));
        assert!(!has_column(&conn, "current_state", "attached_attempt_id"));
    }

    forge_store::migrate(repo.path()).expect("migrate upgrades a behind DB");

    let conn = open(&db);
    assert!(
        has_column(&conn, "repositories", "content_backend"),
        "002 added content_backend"
    );
    assert!(
        has_column(&conn, "current_state", "attached_attempt_id"),
        "002 added attached_attempt_id"
    );
    assert!(
        has_column(&conn, "intents", "check_spec_json"),
        "003 added check_spec_json"
    );
    assert!(
        has_column(&conn, "evidence", "content_hash"),
        "004 added evidence.content_hash"
    );
    assert_eq!(max_version(&conn), 4, "reached HEAD=4");
}

#[test]
fn at_head_db_is_a_noop() {
    let repo = git_repo();
    let db = make_forge_db(repo.path());
    {
        let conn = open(&db);
        conn.execute_batch(BASELINE_001).expect("apply baseline");
        conn.execute_batch(COLUMNS_002).expect("apply 002 ALTERs");
        conn.execute_batch(CHECK_SPEC_003).expect("apply 003 ALTER");
        conn.execute_batch(INTEGRITY_004).expect("apply 004 ALTERs");
        stamp_versions(
            &conn,
            &[
                (1, "001_init"),
                (2, "002_columns"),
                (3, "003_check_spec"),
                (4, "004_integrity_and_actor"),
            ],
        );
        assert_eq!(max_version(&conn), 4);
    }

    forge_store::migrate(repo.path()).expect("at-head migrate is Ok");

    let conn = open(&db);
    assert_eq!(max_version(&conn), 4, "still at HEAD, unchanged");
}

#[test]
fn head_plus_one_is_refused() {
    let repo = git_repo();
    let db = make_forge_db(repo.path());
    {
        let conn = open(&db);
        conn.execute_batch(BASELINE_001).expect("apply baseline");
        conn.execute_batch(COLUMNS_002).expect("apply 002 ALTERs");
        conn.execute_batch(CHECK_SPEC_003).expect("apply 003 ALTER");
        conn.execute_batch(INTEGRITY_004).expect("apply 004 ALTERs");
        stamp_versions(
            &conn,
            &[
                (1, "001_init"),
                (2, "002_columns"),
                (3, "003_check_spec"),
                (4, "004_integrity_and_actor"),
                (5, "future"),
            ],
        );
        assert_eq!(max_version(&conn), 5);
    }

    let error = forge_store::migrate(repo.path()).expect_err("HEAD+1 must be refused");
    match error.downcast_ref::<forge_store::ForgeError>() {
        Some(forge_store::ForgeError::UnknownSchemaVersion {
            db_version,
            supported_head,
        }) => {
            assert_eq!(*db_version, 5);
            assert_eq!(*supported_head, 4);
        }
        other => panic!("expected UnknownSchemaVersion, got {other:?}"),
    }
}

#[test]
fn uninitialized_repo_is_a_noop() {
    // A git repo with no `.forge/forge.db` at all: migrate must return Ok(()) so
    // the command's own logic surfaces NOT_INITIALIZED — no panic, no lock.
    let repo = git_repo();
    assert!(!repo.path().join(".forge/forge.db").exists());
    forge_store::migrate(repo.path()).expect("uninitialized repo is a no-op");
}

#[test]
fn not_a_git_repo_is_a_noop() {
    // Outside any git work tree, `git_root` fails and migrate no-ops (Ok), letting
    // the command surface the not-a-git-repo error itself.
    let dir = tempfile::tempdir().expect("temp dir");
    forge_store::migrate(dir.path()).expect("non-git dir is a no-op");
}
