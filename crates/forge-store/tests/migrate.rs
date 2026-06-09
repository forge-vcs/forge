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
/// The 005 native-history migration (NER-138 Phase 7 slice 2) — used to build an
/// at-head fixture.
const NATIVE_HISTORY_005: &str = include_str!("../migrations/005_native_history.sql");
/// The 006 commit-id migration (NER-138 Phase 7 slice 3) — used to build an
/// at-head fixture now that HEAD is 7.
const NATIVE_HISTORY_COMMIT_ID_006: &str =
    include_str!("../migrations/006_native_history_commit_id.sql");
/// The 007 expected-content-ref migration (NER-143 R1) — used to build an at-head fixture.
const EXPECTED_CONTENT_REF_007: &str = include_str!("../migrations/007_expected_content_ref.sql");
/// The 008 conflict-data migration (NER-139 Phase 8 S2a).
const CONFLICT_DATA_008: &str = include_str!("../migrations/008_conflict_data.sql");
/// The 009 attempt-workspaces migration (NER-139 Phase 8 S4).
const ATTEMPT_WORKSPACES_009: &str = include_str!("../migrations/009_attempt_workspaces.sql");
/// The 010 storage-policy migration (Phase 8 S5).
const STORAGE_POLICY_010: &str = include_str!("../migrations/010_storage_policy.sql");
/// The 011 local-signatures migration (Phase 9 S1).
const LOCAL_SIGNATURES_011: &str = include_str!("../migrations/011_local_signatures.sql");
/// The 012 trust-policy migration (Phase 9 S2).
const TRUST_POLICY_012: &str = include_str!("../migrations/012_trust_policy.sql");
/// The 013 signing-key-origin migration (Phase 9 remote key trust).
const SIGNING_KEY_ORIGINS_013: &str = include_str!("../migrations/013_signing_key_origins.sql");

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

fn apply_through_012(conn: &Connection) {
    conn.execute_batch(BASELINE_001).expect("apply baseline");
    conn.execute_batch(COLUMNS_002).expect("apply 002 ALTERs");
    conn.execute_batch(CHECK_SPEC_003).expect("apply 003 ALTER");
    conn.execute_batch(INTEGRITY_004).expect("apply 004 ALTERs");
    conn.execute_batch(NATIVE_HISTORY_005)
        .expect("apply 005 native-history");
    conn.execute_batch(NATIVE_HISTORY_COMMIT_ID_006)
        .expect("apply 006 commit-id");
    conn.execute_batch(EXPECTED_CONTENT_REF_007)
        .expect("apply 007 expected-content-ref");
    conn.execute_batch(CONFLICT_DATA_008)
        .expect("apply 008 conflict-data");
    conn.execute_batch(ATTEMPT_WORKSPACES_009)
        .expect("apply 009 attempt-workspaces");
    conn.execute_batch(STORAGE_POLICY_010)
        .expect("apply 010 storage-policy");
    conn.execute_batch(LOCAL_SIGNATURES_011)
        .expect("apply 011 local-signatures");
    conn.execute_batch(TRUST_POLICY_012)
        .expect("apply 012 trust-policy");
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
fn signing_key_origin_backfill_labels_existing_signature_keys_as_peer() {
    let conn = Connection::open_in_memory().expect("open memory db");
    conn.pragma_update(None, "foreign_keys", "ON")
        .expect("enable fks");
    apply_through_012(&conn);
    conn.execute(
        "INSERT INTO repositories (id, root_path, git_head, content_backend, created_at_ms)
         VALUES ('repo_test', '/tmp/forge-test', NULL, 'native', 0)",
        [],
    )
    .expect("insert repository");
    conn.execute(
        "INSERT INTO ledger_signatures (
            id, repo_id, subject_kind, subject_id, signed_digest, signature_alg,
            public_key, key_fingerprint, signature, trust_level, created_at_ms
         ) VALUES (
            'sig_peer', 'repo_test', 'evidence', 'ev_peer', 'digest',
            'ed25519', 'peer_public_key', 'peer_fingerprint', 'peer_signature',
            'locally_signed', 7
         )",
        [],
    )
    .expect("insert pre-existing signature");

    conn.execute_batch(SIGNING_KEY_ORIGINS_013)
        .expect("apply 013 signing-key-origins");

    let trust_origin: String = conn
        .query_row(
            "SELECT trust_origin FROM signing_keys
             WHERE repo_id = 'repo_test' AND key_fingerprint = 'peer_fingerprint'",
            [],
            |row| row.get(0),
        )
        .expect("query signing key origin");
    assert_eq!(
        trust_origin, "peer",
        "migration must fail closed for pre-existing signature rows"
    );
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
    assert!(
        has_column(&conn, "native_object_format", "format_tag"),
        "005 created native_object_format"
    );
    assert!(
        has_column(&conn, "decisions", "commit_id"),
        "006 added decisions.commit_id"
    );
    assert!(
        has_column(&conn, "native_object_format", "object_format_version"),
        "006 added object_format_version"
    );
    assert!(
        has_column(&conn, "current_state", "expected_content_ref"),
        "007 added expected_content_ref"
    );
    assert!(
        has_column(&conn, "conflict_sets", "content_hash"),
        "008 added conflict_sets.content_hash"
    );
    assert!(
        has_column(&conn, "path_conflicts", "path_fingerprint"),
        "008 created path_conflicts"
    );
    assert!(
        has_column(&conn, "attempt_workspaces", "workspace_rel_path"),
        "009 created attempt_workspaces"
    );
    assert_eq!(max_version(&conn), 13, "reached HEAD=13");
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
        conn.execute_batch(NATIVE_HISTORY_005)
            .expect("apply 005 native-history");
        conn.execute_batch(NATIVE_HISTORY_COMMIT_ID_006)
            .expect("apply 006 commit-id");
        conn.execute_batch(EXPECTED_CONTENT_REF_007)
            .expect("apply 007 expected-content-ref");
        conn.execute_batch(CONFLICT_DATA_008)
            .expect("apply 008 conflict-data");
        conn.execute_batch(ATTEMPT_WORKSPACES_009)
            .expect("apply 009 attempt-workspaces");
        conn.execute_batch(STORAGE_POLICY_010)
            .expect("apply 010 storage-policy");
        conn.execute_batch(LOCAL_SIGNATURES_011)
            .expect("apply 011 local-signatures");
        conn.execute_batch(TRUST_POLICY_012)
            .expect("apply 012 trust-policy");
        conn.execute_batch(SIGNING_KEY_ORIGINS_013)
            .expect("apply 013 signing-key-origins");
        stamp_versions(
            &conn,
            &[
                (1, "001_init"),
                (2, "002_columns"),
                (3, "003_check_spec"),
                (4, "004_integrity_and_actor"),
                (5, "005_native_history"),
                (6, "006_native_history_commit_id"),
                (7, "007_expected_content_ref"),
                (8, "008_conflict_data"),
                (9, "009_attempt_workspaces"),
                (10, "010_storage_policy"),
                (11, "011_local_signatures"),
                (12, "012_trust_policy"),
                (13, "013_signing_key_origins"),
            ],
        );
        assert_eq!(max_version(&conn), 13);
    }

    forge_store::migrate(repo.path()).expect("at-head migrate is Ok");

    let conn = open(&db);
    assert_eq!(max_version(&conn), 13, "still at HEAD, unchanged");
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
        conn.execute_batch(NATIVE_HISTORY_005)
            .expect("apply 005 native-history");
        conn.execute_batch(NATIVE_HISTORY_COMMIT_ID_006)
            .expect("apply 006 commit-id");
        conn.execute_batch(EXPECTED_CONTENT_REF_007)
            .expect("apply 007 expected-content-ref");
        conn.execute_batch(CONFLICT_DATA_008)
            .expect("apply 008 conflict-data");
        conn.execute_batch(ATTEMPT_WORKSPACES_009)
            .expect("apply 009 attempt-workspaces");
        conn.execute_batch(STORAGE_POLICY_010)
            .expect("apply 010 storage-policy");
        conn.execute_batch(LOCAL_SIGNATURES_011)
            .expect("apply 011 local-signatures");
        conn.execute_batch(TRUST_POLICY_012)
            .expect("apply 012 trust-policy");
        conn.execute_batch(SIGNING_KEY_ORIGINS_013)
            .expect("apply 013 signing-key-origins");
        // HEAD is now 13, so the genuinely-ahead stamp is 14.
        stamp_versions(
            &conn,
            &[
                (1, "001_init"),
                (2, "002_columns"),
                (3, "003_check_spec"),
                (4, "004_integrity_and_actor"),
                (5, "005_native_history"),
                (6, "006_native_history_commit_id"),
                (7, "007_expected_content_ref"),
                (8, "008_conflict_data"),
                (9, "009_attempt_workspaces"),
                (10, "010_storage_policy"),
                (11, "011_local_signatures"),
                (12, "012_trust_policy"),
                (13, "013_signing_key_origins"),
                (14, "future"),
            ],
        );
        assert_eq!(max_version(&conn), 14);
    }

    let error = forge_store::migrate(repo.path()).expect_err("HEAD+1 must be refused");
    match error.downcast_ref::<forge_store::ForgeError>() {
        Some(forge_store::ForgeError::UnknownSchemaVersion {
            db_version,
            supported_head,
        }) => {
            assert_eq!(*db_version, 14);
            assert_eq!(*supported_head, 13);
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
