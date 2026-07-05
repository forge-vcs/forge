use super::*;

fn compare_row(
    attempt_id: &str,
    has_proposal: bool,
    integrity: &str,
    check_status: Option<&str>,
    tests_failed: Option<u64>,
    tests_passed: Option<u64>,
) -> AttemptCompareRow {
    compare_row_with_changes(
        attempt_id,
        has_proposal,
        integrity,
        check_status,
        tests_failed,
        tests_passed,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn compare_row_with_changes(
    attempt_id: &str,
    has_proposal: bool,
    integrity: &str,
    check_status: Option<&str>,
    tests_failed: Option<u64>,
    tests_passed: Option<u64>,
    changed_count: usize,
) -> AttemptCompareRow {
    AttemptCompareRow {
        attempt_id: attempt_id.to_string(),
        status: "active".to_string(),
        proposal: has_proposal.then(|| ComparedProposal {
            proposal_id: format!("prop_{attempt_id}"),
            proposal_revision_id: format!("rev_{attempt_id}"),
            snapshot_id: format!("snap_{attempt_id}"),
            base_head: "base".to_string(),
            content_ref: "git-tree:deadbeef".to_string(),
        }),
        changed_paths: Vec::new(),
        changed_count,
        gates: Vec::new(),
        check_status: check_status.map(str::to_string),
        metrics: StructuredMetrics {
            tests_passed,
            tests_failed,
            tests_ignored: None,
            clippy_findings: None,
        },
        integrity: integrity.to_string(),
        decision_status: None,
        publication_status: None,
        rank: None,
        rank_reason: String::new(),
    }
}

#[test]
fn review_changed_paths_redact_local_private_labels() {
    let paths = vec![
        "src/public.rs".to_string(),
        "src/private.rs".to_string(),
        "docs/notes.md".to_string(),
    ];
    let labels = vec![LocalPrivatePathLabel {
        work_package_kind: "proposal".to_string(),
        work_package_id: "proposal_1".to_string(),
        path: "src/private.rs".to_string(),
        path_label_id: "path_label_1".to_string(),
        path_hash: "sha256:private".to_string(),
        visibility: "private".to_string(),
    }];

    let sanitized = crate::proposals::sanitize_review_changed_paths(&paths, &labels);

    assert_eq!(sanitized[0].path, "src/public.rs");
    assert_eq!(sanitized[0].status, "changed");
    assert_eq!(sanitized[1].path, "[restricted private path]");
    assert_eq!(sanitized[1].status, "restricted");
    assert_eq!(sanitized[2].path, "docs/notes.md");
}

#[test]
fn rank_passing_attempt_above_failing() {
    let mut rows = vec![
        compare_row(
            "a",
            true,
            INTEGRITY_VERIFIED,
            Some("failed"),
            Some(2),
            Some(48),
        ),
        compare_row(
            "b",
            true,
            INTEGRITY_VERIFIED,
            Some("passed"),
            Some(0),
            Some(50),
        ),
    ];
    rank_compare_rows(&mut rows);
    // The passing attempt ranks first regardless of input order.
    assert_eq!(rows[0].attempt_id, "b");
    assert_eq!(rows[0].rank, Some(1));
    assert_eq!(rows[1].attempt_id, "a");
    assert_eq!(rows[1].rank, Some(2));
}

#[test]
fn rank_fewer_failures_first_within_tier() {
    let mut rows = vec![
        compare_row(
            "a",
            true,
            INTEGRITY_VERIFIED,
            Some("passed"),
            Some(0),
            Some(48),
        ),
        compare_row(
            "b",
            true,
            INTEGRITY_VERIFIED,
            Some("passed"),
            Some(0),
            Some(50),
        ),
    ];
    rank_compare_rows(&mut rows);
    // Same failures (0), so more passing wins.
    assert_eq!(rows[0].attempt_id, "b");
}

#[test]
fn tampered_attempt_is_unranked_and_placed_last() {
    let mut rows = vec![
        // The would-be winner by exit code, but tampered.
        compare_row(
            "a",
            true,
            INTEGRITY_TAMPERED,
            Some("passed"),
            Some(0),
            Some(99),
        ),
        // An honest but failing attempt.
        compare_row(
            "b",
            true,
            INTEGRITY_VERIFIED,
            Some("failed"),
            Some(1),
            Some(10),
        ),
    ];
    rank_compare_rows(&mut rows);
    // The honest attempt is the rank-1 winner; the tampered one is unranked & last.
    assert_eq!(rows[0].attempt_id, "b");
    assert_eq!(rows[0].rank, Some(1));
    assert_eq!(rows[1].attempt_id, "a");
    assert_eq!(rows[1].rank, None);
    assert!(rows[1].rank_reason.contains("tampered"));
}

#[test]
fn all_tampered_group_yields_no_rank_one() {
    let mut rows = vec![
        compare_row(
            "a",
            true,
            INTEGRITY_TAMPERED,
            Some("passed"),
            Some(0),
            Some(1),
        ),
        compare_row(
            "b",
            true,
            INTEGRITY_TAMPERED,
            Some("passed"),
            Some(0),
            Some(2),
        ),
    ];
    rank_compare_rows(&mut rows);
    // A numeric-min consumer cannot select a tampered attempt: no row has rank 1.
    assert!(rows.iter().all(|row| row.rank.is_none()));
}

#[test]
fn no_proposal_attempt_is_unranked() {
    let mut rows = vec![
        compare_row(
            "a",
            true,
            INTEGRITY_VERIFIED,
            Some("passed"),
            Some(0),
            Some(5),
        ),
        compare_row("b", false, INTEGRITY_NO_EVIDENCE, None, None, None),
    ];
    rank_compare_rows(&mut rows);
    assert_eq!(rows[0].attempt_id, "a");
    assert_eq!(rows[0].rank, Some(1));
    assert_eq!(rows[1].rank, None);
    assert!(rows[1].rank_reason.contains("no proposal"));
}

#[test]
fn ranking_is_a_stable_total_order() {
    // Two identical-metric rows keep input (created) order — deterministic.
    let build = || {
        vec![
            compare_row(
                "a",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(10),
            ),
            compare_row(
                "b",
                true,
                INTEGRITY_VERIFIED,
                Some("passed"),
                Some(0),
                Some(10),
            ),
        ]
    };
    let mut first = build();
    rank_compare_rows(&mut first);
    let mut second = build();
    rank_compare_rows(&mut second);
    let ids_first: Vec<_> = first.iter().map(|r| r.attempt_id.clone()).collect();
    let ids_second: Vec<_> = second.iter().map(|r| r.attempt_id.clone()).collect();
    assert_eq!(ids_first, ids_second);
    assert_eq!(ids_first, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn rank_verified_nonempty_above_no_evidence_empty_on_tied_gates() {
    // NER-256: when the gate/check status ties at non-"passed" (here both "missing"),
    // a zero-change, no_evidence attempt must NOT outrank a verified attempt with a
    // real diff just because it was created first. `a` is placed FIRST in input
    // (earlier created order) but is empty + no_evidence; `b` is verified + non-empty.
    // Before the tie-break the stable sort kept `a` first; now `b` ranks 1.
    let mut rows = vec![
        compare_row_with_changes(
            "a",
            true,
            INTEGRITY_NO_EVIDENCE,
            Some("missing"),
            None,
            None,
            0,
        ),
        compare_row_with_changes(
            "b",
            true,
            INTEGRITY_VERIFIED,
            Some("missing"),
            None,
            None,
            3,
        ),
    ];
    rank_compare_rows(&mut rows);
    assert_eq!(rows[0].attempt_id, "b");
    assert_eq!(rows[0].rank, Some(1));
    assert_eq!(rows[1].attempt_id, "a");
    assert_eq!(rows[1].rank, Some(2));
    // The rank_reason must name the tie-break that ACTUALLY applied. Integrity is the
    // first field that differs (verified vs no_evidence), so it — not the diff — is the
    // deciding discriminator the reason should cite (NER-256 adversarial review: the old
    // code always claimed "verified evidence + non-empty diff" even when only one of
    // those fields differentiated the rows).
    assert!(
        rows[0].rank_reason.contains("gates tie"),
        "winner rank_reason should mention the gate tie-break: {}",
        rows[0].rank_reason
    );
    assert!(
        rows[0].rank_reason.contains("verified evidence"),
        "winner rank_reason should cite its verified-evidence advantage: {}",
        rows[0].rank_reason
    );
    // The loser's reason states why IT landed below: its no-evidence integrity, the
    // first field on which it lost to the verified winner.
    assert!(
        rows[1].rank_reason.contains("gates tie")
            && rows[1].rank_reason.contains("no-evidence integrity"),
        "loser rank_reason should state its no-evidence integrity: {}",
        rows[1].rank_reason
    );
}

#[test]
fn rank_reason_names_diff_discriminator_when_integrity_ties_on_gate_tie() {
    // NER-256 adversarial review: when integrity AND gates tie, the diff size is the
    // real discriminator. The reason must cite the diff — NOT a fixed integrity label.
    let mut rows = vec![
        compare_row_with_changes(
            "a",
            true,
            INTEGRITY_VERIFIED,
            Some("missing"),
            None,
            None,
            0,
        ),
        compare_row_with_changes(
            "b",
            true,
            INTEGRITY_VERIFIED,
            Some("missing"),
            None,
            None,
            4,
        ),
    ];
    rank_compare_rows(&mut rows);
    assert_eq!(rows[0].attempt_id, "b");
    assert_eq!(rows[1].attempt_id, "a");
    assert!(
        rows[0].rank_reason.contains("non-empty diff"),
        "winner reason should cite its non-empty diff: {}",
        rows[0].rank_reason
    );
    assert!(
        rows[1].rank_reason.contains("empty diff"),
        "loser reason should cite its empty diff: {}",
        rows[1].rank_reason
    );
}

#[test]
fn rank_reason_names_test_count_discriminator_when_integrity_and_diff_tie() {
    // NER-256 adversarial review: three non-passing attempts, all verified + non-empty,
    // differing ONLY in tests_failed. The discriminator is tests_failed — the reason
    // must say so, not the (identical, non-differentiating) integrity + diff labels.
    let mut rows = vec![
        compare_row_with_changes(
            "x",
            true,
            INTEGRITY_VERIFIED,
            Some("missing"),
            Some(0),
            Some(5),
            2,
        ),
        compare_row_with_changes(
            "y",
            true,
            INTEGRITY_VERIFIED,
            Some("missing"),
            Some(3),
            Some(5),
            2,
        ),
        compare_row_with_changes(
            "z",
            true,
            INTEGRITY_VERIFIED,
            Some("missing"),
            Some(10),
            Some(5),
            2,
        ),
    ];
    rank_compare_rows(&mut rows);
    assert_eq!(rows[0].attempt_id, "x");
    assert_eq!(rows[1].attempt_id, "y");
    assert_eq!(rows[2].attempt_id, "z");
    // The middle row lost to x on failing-test count, not integrity or diff.
    assert!(
        rows[1].rank_reason.contains("more failing tests"),
        "y's reason should cite the failing-test discriminator, not integrity/diff: {}",
        rows[1].rank_reason
    );
    assert!(
        rows[2].rank_reason.contains("more failing tests"),
        "z's reason should cite the failing-test discriminator: {}",
        rows[2].rank_reason
    );
    // The winner reads positively: it has the fewest failing tests.
    assert!(
        rows[0].rank_reason.contains("fewer failing tests"),
        "x's reason should cite its fewer-failing-tests advantage: {}",
        rows[0].rank_reason
    );
}

#[test]
fn rank_reason_says_gates_not_satisfied_when_a_passing_attempt_outranks() {
    // NER-256 correctness review: when one attempt passes its gates, the non-passing
    // remainder did NOT tie on gates — they lost. Their reason must say "required gates
    // not satisfied", not "gates tie" (which would falsely claim equal gate status).
    let mut rows = vec![
        compare_row(
            "a",
            true,
            INTEGRITY_VERIFIED,
            Some("passed"),
            Some(0),
            Some(5),
        ),
        compare_row(
            "b",
            true,
            INTEGRITY_VERIFIED,
            Some("failed"),
            Some(2),
            Some(3),
        ),
        compare_row(
            "c",
            true,
            INTEGRITY_NO_EVIDENCE,
            Some("missing"),
            None,
            None,
        ),
    ];
    rank_compare_rows(&mut rows);
    assert_eq!(rows[0].attempt_id, "a");
    assert!(
        rows[0].rank_reason.contains("all required gates passing"),
        "passing attempt reason: {}",
        rows[0].rank_reason
    );
    for loser in &rows[1..] {
        assert!(
            loser.rank_reason.contains("required gates not satisfied"),
            "non-passing attempt must NOT claim a gate tie when a passing attempt exists: {}",
            loser.rank_reason
        );
        assert!(
            !loser.rank_reason.contains("gates tie"),
            "non-passing attempt must not claim a gate tie here: {}",
            loser.rank_reason
        );
    }
}

#[test]
fn rank_reason_legacy_caveat_on_gate_tie() {
    // NER-256 testing review: a legacy_unverified attempt is rankable but its deciding
    // evidence was never hash-verified. On a gate tie it must (a) rank between verified
    // and no_evidence and (b) carry the legacy caveat in its rank_reason.
    let mut rows = vec![
        compare_row_with_changes(
            "v",
            true,
            INTEGRITY_VERIFIED,
            Some("missing"),
            None,
            None,
            1,
        ),
        compare_row_with_changes("l", true, INTEGRITY_LEGACY, Some("missing"), None, None, 1),
        compare_row_with_changes(
            "n",
            true,
            INTEGRITY_NO_EVIDENCE,
            Some("missing"),
            None,
            None,
            1,
        ),
    ];
    rank_compare_rows(&mut rows);
    assert_eq!(rows[0].attempt_id, "v");
    assert_eq!(rows[1].attempt_id, "l");
    assert_eq!(rows[2].attempt_id, "n");
    assert!(
        rows[1].rank_reason.contains("legacy_unverified"),
        "legacy attempt reason must carry the legacy caveat: {}",
        rows[1].rank_reason
    );
}

#[test]
fn rank_tie_break_does_not_override_passing_gate() {
    // NER-256 guardrail: gates_passing stays the FIRST sort key, so a passing-gate
    // attempt outranks a non-passing one EVEN IF the non-passing one has stronger
    // integrity and a non-empty diff. Here `b` passes but is empty + (still verified),
    // while `a` is non-passing, verified, non-empty — `b` must still rank 1.
    let mut rows = vec![
        compare_row_with_changes(
            "a",
            true,
            INTEGRITY_VERIFIED,
            Some("failed"),
            Some(1),
            Some(9),
            5,
        ),
        compare_row_with_changes(
            "b",
            true,
            INTEGRITY_VERIFIED,
            Some("passed"),
            Some(0),
            Some(9),
            0,
        ),
    ];
    rank_compare_rows(&mut rows);
    assert_eq!(rows[0].attempt_id, "b");
    assert_eq!(rows[0].rank, Some(1));
    assert_eq!(rows[1].attempt_id, "a");
}

#[test]
fn latest_selector_breaks_same_ms_ties_by_rowid() {
    // The nine "latest" selectors append `, rowid DESC` so rows sharing a
    // created_at_ms are returned in deterministic insertion order (highest rowid =
    // most recently inserted). Proven directly against SQLite, independent of the
    // multi-process coverage deferred to Phase 1b.
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "CREATE TABLE snapshots (id TEXT PRIMARY KEY, attempt_id TEXT, created_at_ms INTEGER);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO snapshots (id, attempt_id, created_at_ms) VALUES ('first', 'a', 100)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO snapshots (id, attempt_id, created_at_ms) VALUES ('second', 'a', 100)",
            [],
        )
        .unwrap();
    let latest: String = connection
            .query_row(
                "SELECT id FROM snapshots WHERE attempt_id = ?1 ORDER BY created_at_ms DESC, rowid DESC LIMIT 1",
                params!["a"],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(latest, "second");
}

#[test]
fn busy_classification_retries_only_busy_class_errors() {
    use rusqlite::ffi::Error as FfiError;

    // SQLITE_BUSY (5) and SQLITE_BUSY_SNAPSHOT (517) are the retryable cases.
    let busy = anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(5), None));
    assert!(is_retryable_busy(&busy), "plain SQLITE_BUSY must retry");

    let busy_snapshot =
        anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(517), None));
    assert!(
        is_retryable_busy(&busy_snapshot),
        "SQLITE_BUSY_SNAPSHOT (517) must retry"
    );

    // Detected even when wrapped further up the anyhow source chain.
    let wrapped = anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(517), None))
        .context("while committing writer txn");
    assert!(
        is_retryable_busy(&wrapped),
        "chain walk must find a busy cause"
    );

    // A constraint violation (e.g. the request-id unique index) is NOT retryable.
    let constraint = anyhow::Error::from(rusqlite::Error::SqliteFailure(FfiError::new(19), None));
    assert!(
        !is_retryable_busy(&constraint),
        "SQLITE_CONSTRAINT must not retry"
    );

    // Non-SQLite errors, including the replay sentinel, are not retryable.
    assert!(!is_retryable_busy(&anyhow!("plain failure")));
    assert!(!is_retryable_busy(
        &RequestIdReplay {
            operation: RequestIdOperation {
                operation_id: "op_x".to_string(),
                command: "save".to_string(),
                status: "succeeded".to_string(),
                error_json: None,
                kind: None,
                view_id: None,
                state: None,
            },
        }
        .into()
    ));
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// FIX D: `insert_operation_view`'s optimistic singleton CAS, when its captured
/// parent operation no longer matches live `current_state`, raises the typed,
/// retryable `ForgeError::CurrentStateChanged` (code `CONFLICT`) — NOT a plain
/// `anyhow!`. This is what the CLI's `is_transient_error` keys on to skip
/// recording the failure under the `--request-id`. Driven as a focused in-crate
/// unit test because `insert_operation_view` is private and the production fns
/// re-read `current_state` per call (so a stale parent can't be pinned through
/// the public API alone).
#[test]
fn insert_operation_view_stale_parent_raises_current_state_changed() {
    let mut connection = Connection::open_in_memory().expect("open in-memory db");
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .expect("enable fks");
    connection
        .execute_batch(include_str!("../migrations/001_init.sql"))
        .expect("baseline schema");
    // Phase 5 adds operations.content_hash, which insert_operation_view now writes.
    connection
        .execute_batch(include_str!("../migrations/004_integrity_and_actor.sql"))
        .expect("phase 5 integrity columns");

    // Seed: a repo, a genesis operation+view that `current_state` points at, and
    // a SECOND operation row (`op_stale`) that does NOT match current_state.
    connection
        .execute_batch(
            "INSERT INTO repositories (id, root_path, created_at_ms)
                     VALUES ('repo_1', '/tmp/repo', 0);
                 INSERT INTO operations (id, repo_id, command, status, kind, created_at_ms)
                     VALUES ('op_genesis', 'repo_1', 'init', 'succeeded', 'init', 0);
                 INSERT INTO views (id, repo_id, operation_id, kind, state_json, created_at_ms)
                     VALUES ('view_genesis', 'repo_1', 'op_genesis', 'initialized', '{}', 0);
                 INSERT INTO current_state
                     (singleton, repo_id, current_operation_id, current_view_id, updated_at_ms)
                     VALUES (1, 'repo_1', 'op_genesis', 'view_genesis', 0);
                 INSERT INTO operations (id, repo_id, command, status, kind, created_at_ms)
                     VALUES ('op_stale', 'repo_1', 'save', 'succeeded', 'save', 0);",
        )
        .expect("seed rows");

    let error = with_immediate_retry(&mut connection, |tx| {
        // Pass `op_stale` as the parent: it exists (satisfies the FK) but does
        // NOT equal current_state.current_operation_id (`op_genesis`), so the
        // CAS `WHERE current_operation_id = 'op_stale'` updates zero rows.
        insert_operation_view(
            tx,
            "repo_1",
            Some("op_stale"),
            OperationViewInput {
                request_id: None,
                command: "save".to_string(),
                kind: "snapshot_saved".to_string(),
                view_kind: ViewKind::Initialized,
                state: json!({ "lifecycle": "test" }),
            },
        )
    })
    .expect_err("a stale parent must lose the CAS");

    let forge_error = error
        .downcast_ref::<ForgeError>()
        .expect("the CAS failure is a typed ForgeError");
    assert_eq!(*forge_error, ForgeError::CurrentStateChanged);
    assert_eq!(forge_error.code(), "CONFLICT");
    assert!(forge_error.retryable());
    assert_eq!(forge_error.after_ms(), Some(50));
}

#[test]
fn record_conflict_set_redacts_secret_paths() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "forge@example.test"]);
    run_git(root, &["config", "user.name", "Forge Test"]);
    fs::write(root.join("README.md"), "hello\n").expect("write readme");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-m", "initial"]);

    init_repository(root, None, "git".to_string()).expect("init repository");

    let paths = vec![
        "src/main.rs".to_string(),
        ".env".to_string(),
        "k.pem".to_string(),
    ];
    let id = record_conflict_set(root, "stale_base_accept", "HEAD0", "HEAD1", &paths)
        .expect("record conflict set");
    assert!(id.starts_with("conflict_"), "unexpected id: {id}");

    let database_path = root.join(".forge/forge.db");
    let connection = Connection::open(&database_path).expect("open db");
    let (context, paths_json): (String, String) = connection
        .query_row(
            "SELECT context, paths_json FROM conflict_sets WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query conflict row");

    assert_eq!(context, "stale_base_accept");
    let value: Value = serde_json::from_str(&paths_json).expect("parse paths_json");
    assert_eq!(value["expected_head"], "HEAD0");
    assert_eq!(value["actual_head"], "HEAD1");
    assert_eq!(value["redacted_count"], 2);
    assert!(
        paths_json.contains("src/main.rs"),
        "non-secret path must be kept: {paths_json}"
    );
    assert!(
        !paths_json.contains(".env"),
        "secret-risk path leaked: {paths_json}"
    );
    assert!(
        !paths_json.contains("k.pem"),
        "secret-risk path leaked: {paths_json}"
    );
}

/// NER-257: `intent_detail` parses the declared gate spec (program, args, and the
/// `require_structured_pass` → `structured` flag) and links the started attempt id;
/// an unknown id raises the typed `UnknownIntent`.
#[test]
fn intent_detail_returns_gates_and_attempts() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "forge@example.test"]);
    run_git(root, &["config", "user.name", "Forge Test"]);
    fs::write(root.join("README.md"), "hello\n").expect("write readme");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-m", "initial"]);
    init_repository(root, None, "git".to_string()).expect("init repository");

    let check_spec_json =
        check_spec_json_from_requires(&["cargo test".to_string()], &["cargo clippy".to_string()]);
    let started = start_attempt(
        root,
        None,
        "two gates".to_string(),
        "HEAD0".to_string(),
        check_spec_json,
    )
    .expect("start attempt");

    let detail = intent_detail(root, &started.intent_id).expect("intent detail");
    assert_eq!(detail.intent_id, started.intent_id);
    assert_eq!(detail.title, "two gates");
    assert_eq!(detail.status, "open");
    assert_eq!(detail.gates.len(), 2);

    let plain = detail
        .gates
        .iter()
        .find(|gate| gate.args == ["test"])
        .expect("plain gate");
    assert_eq!(plain.program, "cargo");
    assert!(!plain.structured);
    let structured = detail
        .gates
        .iter()
        .find(|gate| gate.args == ["clippy"])
        .expect("structured gate");
    assert!(structured.structured);

    assert_eq!(detail.attempt_ids, vec![started.attempt_id.clone()]);

    let listed = intents_list(root).expect("intents list");
    assert!(listed
        .iter()
        .any(|intent| intent.intent_id == started.intent_id));

    let err = intent_detail(root, "intent_missing").expect_err("unknown intent errors");
    let typed = err.downcast_ref::<ForgeError>().expect("typed ForgeError");
    assert_eq!(typed.code(), "UNKNOWN_INTENT");
}

/// NER-257 secret-safety: a secret-like `key=value` gate token in the stored
/// `check_spec_json` is redacted before `intent_detail` egress (the stored spec is
/// raw), mirroring the check-surface `redact_gate_result` pass.
#[test]
fn intent_detail_redacts_secret_like_gate_tokens() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "forge@example.test"]);
    run_git(root, &["config", "user.name", "Forge Test"]);
    fs::write(root.join("README.md"), "hello\n").expect("write readme");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-m", "initial"]);
    init_repository(root, None, "git".to_string()).expect("init repository");

    let check_spec_json =
        check_spec_json_from_requires(&["deploy --token=ghp_supersecret".to_string()], &[]);
    let started = start_attempt(
        root,
        None,
        "secret gate".to_string(),
        "HEAD0".to_string(),
        check_spec_json,
    )
    .expect("start attempt");

    let detail = intent_detail(root, &started.intent_id).expect("intent detail");
    let serialized = serde_json::to_string(&detail.gates).expect("serialize gates");
    assert!(
        !serialized.contains("ghp_supersecret"),
        "secret-like gate token must be redacted: {serialized}"
    );
    assert!(serialized.contains("[REDACTED]"));
}

#[test]
fn visibility_defaults_grants_revocation_and_projection_decisions() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "forge@example.test"]);
    run_git(root, &["config", "user.name", "Forge Test"]);
    fs::write(root.join("README.md"), "hello\n").expect("write readme");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-m", "initial"]);
    init_repository(root, None, "git".to_string()).expect("init repository");

    let policy = visibility_policy(root).expect("visibility policy");
    assert_eq!(policy.default_work_package_visibility, "public");
    assert!(policy
        .supported_capabilities
        .contains(&"sync_materialize".to_string()));

    let started = start_attempt(
        root,
        None,
        "private extension".to_string(),
        "HEAD0".to_string(),
        None,
    )
    .expect("start attempt");

    let public = projection_decision(
        root,
        "attempt",
        &started.attempt_id,
        "reviewer@example.test",
        "sync_materialize",
    )
    .expect("public projection decision");
    assert!(public.allowed);
    assert_eq!(public.disclosure, "full");

    set_work_package_visibility(
        root,
        "attempt",
        &started.attempt_id,
        "private",
        "maintainer",
        Some("invite-only review"),
    )
    .expect("set private");

    let hidden = projection_decision(
        root,
        "attempt",
        &started.attempt_id,
        "reviewer@example.test",
        "sync_materialize",
    )
    .expect("hidden projection decision");
    assert!(!hidden.allowed);
    assert_eq!(hidden.visibility, "private");
    assert_eq!(hidden.disclosure, "hidden");

    grant_visibility_capability(
        root,
        "attempt",
        &started.attempt_id,
        "reviewer@example.test",
        "see_stub",
        "maintainer",
        Some("coordination"),
    )
    .expect("grant stub");
    let stub = projection_decision(
        root,
        "attempt",
        &started.attempt_id,
        "reviewer@example.test",
        "sync_materialize",
    )
    .expect("stub projection decision");
    assert!(!stub.allowed);
    assert_eq!(stub.disclosure, "stub");

    grant_visibility_capability(
        root,
        "attempt",
        &started.attempt_id,
        "reviewer@example.test",
        "sync_materialize",
        "maintainer",
        Some("private review"),
    )
    .expect("grant materialize");
    let allowed = projection_decision(
        root,
        "attempt",
        &started.attempt_id,
        "reviewer@example.test",
        "sync_materialize",
    )
    .expect("allowed projection decision");
    assert!(allowed.allowed);
    assert_eq!(allowed.disclosure, "full");

    revoke_visibility_capability(
        root,
        "attempt",
        &started.attempt_id,
        "reviewer@example.test",
        "sync_materialize",
        "maintainer",
        Some("review complete"),
    )
    .expect("revoke materialize");
    let revoked = projection_decision(
        root,
        "attempt",
        &started.attempt_id,
        "reviewer@example.test",
        "sync_materialize",
    )
    .expect("revoked projection decision");
    assert!(!revoked.allowed);
    assert_eq!(revoked.disclosure, "stub");

    let database_path = root.join(".forge/forge.db");
    let connection = Connection::open(database_path).expect("open db");
    let audit_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM visibility_audit", [], |row| {
            row.get(0)
        })
        .expect("audit count");
    assert_eq!(audit_count, 4);
}

#[test]
fn attempts_from_private_intents_and_proposals_inherit_visibility() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "forge@example.test"]);
    run_git(root, &["config", "user.name", "Forge Test"]);
    fs::write(root.join("README.md"), "hello\n").expect("write readme");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-m", "initial"]);
    init_repository(root, None, "git".to_string()).expect("init repository");

    let first = start_attempt(
        root,
        None,
        "private parent".to_string(),
        "HEAD0".to_string(),
        None,
    )
    .expect("start first attempt");
    set_work_package_visibility(
        root,
        "intent",
        &first.intent_id,
        "private",
        "maintainer",
        Some("private line of work"),
    )
    .expect("set intent private");

    let second = start_attempt_for_intent(root, None, first.intent_id.clone(), "HEAD0".to_string())
        .expect("start attempt for private intent");
    let second_decision = projection_decision(
        root,
        "attempt",
        &second.attempt_id,
        "outsider@example.test",
        "inspect_content",
    )
    .expect("second attempt decision");
    assert_eq!(second_decision.visibility, "private");
    assert!(!second_decision.allowed);

    attach_attempt(root, None, &second.attempt_id, "git-tree:test-private")
        .expect("attach second attempt");
    save_snapshot(
        root,
        None,
        Some(&second.attempt_id),
        "git-tree:test-private".to_string(),
        vec!["README.md".to_string()],
    )
    .expect("save snapshot");
    let proposal = propose(root, None, Some(&second.attempt_id), None).expect("propose");
    let proposal_decision = projection_decision(
        root,
        "proposal",
        &proposal.proposal_id,
        "outsider@example.test",
        "inspect_content",
    )
    .expect("proposal decision");
    assert_eq!(proposal_decision.visibility, "private");
    assert!(!proposal_decision.allowed);
}

#[test]
fn private_decrypt_authority_requires_grant_and_active_encryption_key() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "forge@example.test"]);
    run_git(root, &["config", "user.name", "Forge Test"]);
    fs::write(root.join("README.md"), "hello\n").expect("write readme");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-m", "initial"]);
    init_repository(root, None, "git".to_string()).expect("init repository");

    let org =
        init_org_governance(root, None, "maintainer", Some("bootstrap org")).expect("init org");
    let started = start_attempt(
        root,
        None,
        "private extension".to_string(),
        "HEAD0".to_string(),
        None,
    )
    .expect("start attempt");
    set_work_package_visibility(
        root,
        "attempt",
        &started.attempt_id,
        "private",
        &org.owner_actor_id,
        Some("private review"),
    )
    .expect("set private");

    let missing_grant =
        private_decrypt_authority(root, "attempt", &started.attempt_id, &org.owner_actor_id)
            .expect_err("grant is required");
    assert_eq!(
        missing_grant
            .downcast_ref::<ForgeError>()
            .expect("typed error")
            .code(),
        "PRIVATE_DECRYPT_AUTHORITY_MISSING"
    );

    grant_visibility_capability(
        root,
        "attempt",
        &started.attempt_id,
        &org.owner_actor_id,
        "sync_materialize",
        &org.owner_actor_id,
        Some("review private path"),
    )
    .expect("grant materialize");
    let missing_key =
        private_decrypt_authority(root, "attempt", &started.attempt_id, &org.owner_actor_id)
            .expect_err("encryption key is required");
    assert_eq!(
        missing_key
            .downcast_ref::<ForgeError>()
            .expect("typed error")
            .details()["reason"],
        "missing_active_encryption_key"
    );

    let identity = forge_private::EncryptionIdentity::generate();
    let binding = bind_org_encryption_key(
        root,
        &org.owner_actor_id,
        identity.recipient().as_str(),
        &org.owner_actor_id,
        Some("bind owner encryption key"),
    )
    .expect("bind encryption key");
    let authority =
        private_decrypt_authority(root, "attempt", &started.attempt_id, &org.owner_actor_id)
            .expect("decrypt authority");
    assert_eq!(authority.principal_id, org.owner_actor_id);
    assert_eq!(authority.key_fingerprint, binding.key_fingerprint);

    let database_path = root.join(".forge/forge.db");
    let connection = Connection::open(database_path).expect("open db");
    connection
        .execute(
            "UPDATE org_encryption_key_bindings
                 SET state = 'revoked', revocation_reason = 'rotated'
                 WHERE key_fingerprint = ?1",
            params![binding.key_fingerprint],
        )
        .expect("revoke key");
    let revoked =
        private_decrypt_authority(root, "attempt", &started.attempt_id, &org.owner_actor_id)
            .expect_err("revoked key fails closed");
    assert_eq!(
        revoked
            .downcast_ref::<ForgeError>()
            .expect("typed error")
            .details()["reason"],
        "missing_active_encryption_key"
    );
}

#[test]
fn private_overlay_rows_do_not_store_plaintext_path_names() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    run_git(root, &["init"]);
    run_git(root, &["config", "user.email", "forge@example.test"]);
    run_git(root, &["config", "user.name", "Forge Test"]);
    fs::write(root.join("README.md"), "hello\n").expect("write readme");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-m", "initial"]);
    let repo = init_repository(root, None, "git".to_string()).expect("init repository");
    let started = start_attempt(
        root,
        None,
        "private extension".to_string(),
        "HEAD0".to_string(),
        None,
    )
    .expect("start attempt");

    let private_path = "src/private_ext.rs";
    let path_hash = scoped_private_path_hash(
        &repo.repository_id,
        "attempt",
        &started.attempt_id,
        private_path,
    );
    let label = record_private_path_label(
        root,
        "attempt",
        &started.attempt_id,
        &path_hash,
        "age-envelope-for-display-path",
        "private",
    )
    .expect("record label");
    assert_ne!(label.path_hash, private_path);
    assert!(!label.encrypted_display_path.contains(private_path));

    let payload = record_encrypted_private_payload(
        root,
        EncryptedPrivatePayloadInput {
            work_package_kind: "attempt".to_string(),
            work_package_id: started.attempt_id.clone(),
            snapshot_id: None,
            path_label_id: label.path_label_id.clone(),
            path_hash: path_hash.clone(),
            envelope_format: forge_private::ENVELOPE_FORMAT_AGE_X25519_V1.to_string(),
            recipient_fingerprint: "age-x25519:recipient".to_string(),
            ciphertext_digest: "a".repeat(64),
            private_object_path: ".forge/private/objects/sha256/aa".to_string(),
            encrypted_metadata_json: "{\"encrypted\":true}".to_string(),
        },
    )
    .expect("record payload");
    assert_eq!(payload.path_hash, path_hash);

    let database_path = root.join(".forge/forge.db");
    let connection = Connection::open(database_path).expect("open db");
    let rows_json: String = connection
        .query_row(
            "SELECT json_group_array(json_object(
                    'path_hash', path_hash,
                    'encrypted_display_path', encrypted_display_path
                )) FROM private_path_labels",
            [],
            |row| row.get(0),
        )
        .expect("query labels");
    let payloads_json: String = connection
        .query_row(
            "SELECT json_group_array(json_object(
                    'path_hash', path_hash,
                    'private_object_path', private_object_path,
                    'encrypted_metadata_json', encrypted_metadata_json
                )) FROM encrypted_private_payloads",
            [],
            |row| row.get(0),
        )
        .expect("query payloads");
    assert!(
        !rows_json.contains(private_path),
        "private label row leaked path: {rows_json}"
    );
    assert!(
        !payloads_json.contains(private_path),
        "private payload row leaked path: {payloads_json}"
    );
}
