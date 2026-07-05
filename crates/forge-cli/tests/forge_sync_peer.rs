//! Native sync peer transport, divergence, merge, and conflict-set integration tests.

mod common;
#[path = "support/sync.rs"]
mod sync_support;

use common::{forge_in, TestRepo};
use rusqlite::Connection;
use sync_support::*;

#[test]
fn sync_fetch_clean_divergence_records_merge_commit_without_materializing() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clean-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source disjoint", "source-only.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer disjoint", "peer-only.txt", "peer\n");
    let peer_before = export_native_head(peer.path(), "clean-peer-before.json");

    let first = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-divergence",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["command"], "sync fetch");
    assert_eq!(first["status"], "success");
    assert_eq!(first["data"]["merged"], true);
    assert_eq!(first["data"]["materialized"], false);
    assert!(first["data"]["merge_commit_id"]
        .as_str()
        .expect("merge commit id")
        .starts_with("f1:commit:"));
    assert!(first["data"]["merged_content_ref"]
        .as_str()
        .expect("merged content ref")
        .starts_with("forge-tree:"));
    let first_operation_id = first["operation_id"]
        .as_str()
        .expect("clean merge operation id")
        .to_string();
    assert_eq!(
        operation_count_for_request_id(peer.path(), "clean-divergence"),
        1,
        "clean-divergence merge should replay deterministically by request-id"
    );
    assert_eq!(
        conflict_count(peer.path()),
        0,
        "clean divergent merge must not record a path conflict"
    );

    let peer_after = export_native_head(peer.path(), "clean-peer-after.json");
    assert_ne!(
        peer_after["data"]["native_head"], peer_before["data"]["native_head"],
        "clean fetch divergence must advance the local native head to the merge commit"
    );
    assert_eq!(
        peer_after["data"]["native_head"],
        first["data"]["merge_commit_id"]
    );
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
    let peer_after_doctor = export_native_head(peer.path(), "clean-peer-after-doctor.json");
    assert_eq!(
        peer_after_doctor["data"]["native_head"], first["data"]["merge_commit_id"],
        "reconcile must keep the clean fetch merge commit as the native tip"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "clean fetch divergence must not materialize over local worktree content"
    );
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "fetch should not materialize the merged source-side file"
    );

    let replay = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-divergence",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["status"], "success");
    assert_eq!(replay["operation_id"], first_operation_id);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["merged"], true);
    assert_eq!(
        replay["data"]["protocol_version"],
        first["data"]["protocol_version"]
    );
    assert_eq!(
        replay["data"]["merge_commit_id"],
        first["data"]["merge_commit_id"]
    );
    assert_eq!(
        replay["data"]["merged_content_ref"],
        first["data"]["merged_content_ref"]
    );
    assert_eq!(replay["data"]["materialized"], false);
}

#[test]
fn sync_fetch_clean_divergence_ignores_future_peer_decision_timestamp() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clean-skew-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean skew base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source future clock",
        "source-only.txt",
        "source\n",
    );
    skew_latest_decision_timestamp_for_test(source.path(), 4_102_444_800_000);
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer local clock", "peer-only.txt", "peer\n");

    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(fetched["status"], "success");
    assert_eq!(fetched["data"]["merged"], true);
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
    let after_doctor = export_native_head(peer.path(), "clean-skew-after-doctor.json");
    assert_eq!(
        after_doctor["data"]["native_head"], fetched["data"]["merge_commit_id"],
        "future peer decision timestamps must not outrank the receiver's merge commit"
    );
}

#[test]
fn sync_fetch_clean_divergence_skips_dangling_future_decision_tip() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-dangling-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean dangling base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source dangling tip",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer dangling tip", "peer-only.txt", "peer\n");

    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(fetched["data"]["merged"], true);

    poison_latest_decision_commit_for_test(
        peer.path(),
        "f1:commit:sha256:0000000000000000000000000000000000000000000000000000000000000000",
        4_102_444_800_000,
    );

    let exported = export_native_head(peer.path(), "clean-dangling-after-poison.json");
    assert_eq!(
        exported["data"]["native_head"], fetched["data"]["merge_commit_id"],
        "dangling future decisions must not outrank the valid sync merge tip"
    );
    let doctor = json(
        forge_in(peer.path())
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_eq!(doctor["data"]["ok"], false);
    assert!(doctor["data"]["native_history_issues"]
        .as_array()
        .expect("native history findings")
        .iter()
        .any(|finding| finding["kind"] == "dangling_commit_id"));
}

#[test]
fn sync_pull_after_clean_fetch_materializes_existing_merge_commit() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-fetch-pull-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 clean fetch-pull base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source clean fetch-pull",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        peer.path(),
        "peer clean fetch-pull",
        "peer-only.txt",
        "peer\n",
    );

    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(fetched["data"]["merged"], true);
    assert_eq!(fetched["data"]["materialized"], false);
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "fetch should leave the merged source-side file unmaterialized"
    );

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "pull-after-clean-fetch",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pulled["command"], "sync pull");
    assert_eq!(pulled["status"], "success");
    assert_eq!(pulled["data"]["up_to_date"], true);
    assert_eq!(pulled["data"]["materialized"], true);
    assert_eq!(
        pulled["data"]["materialized_content_ref"],
        fetched["data"]["merged_content_ref"]
    );
    let pulled_operation_id = pulled["operation_id"]
        .as_str()
        .expect("pull materialization operation id")
        .to_string();
    assert_eq!(
        operation_count_for_request_id_and_kind(
            peer.path(),
            "pull-after-clean-fetch",
            "sync_pull_materialized"
        ),
        1,
        "up-to-date sync pull must claim request-id under sync pull"
    );
    let replay = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "pull-after-clean-fetch",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["status"], "success");
    assert_eq!(replay["operation_id"], pulled_operation_id);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["up_to_date"], true);
    assert_eq!(replay["data"]["materialized"], true);
    assert_eq!(
        replay["data"]["materialized_content_ref"],
        pulled["data"]["materialized_content_ref"]
    );
    assert_eq!(
        replay["data"]["materialized_operation_id"],
        pulled["data"]["materialized_operation_id"]
    );
    assert_eq!(
        replay["data"]["materialized_view_id"],
        pulled["data"]["materialized_view_id"]
    );
    native_accept_file_change_in(
        peer.path(),
        "later clean replay state",
        "later-clean.txt",
        "later\n",
    );
    let later_head = export_native_head(peer.path(), "clean-fetch-pull-later-head.json");
    let replay_after_later_clean = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "pull-after-clean-fetch",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay_after_later_clean["status"], "success");
    assert_eq!(
        replay_after_later_clean["operation_id"],
        pulled_operation_id
    );
    assert_eq!(replay_after_later_clean["data"]["idempotent_replay"], true);
    assert_eq!(
        std::fs::read_to_string(peer.path().join("later-clean.txt")).expect("later clean content"),
        "later\n",
        "idempotent replay must not restore an older tree over a newer clean state"
    );
    let after_later_replay_head =
        export_native_head(peer.path(), "clean-fetch-pull-after-later-replay-head.json");
    assert_eq!(
        after_later_replay_head["data"]["native_head"], later_head["data"]["native_head"],
        "idempotent replay must not rewind a newer native head"
    );
    std::fs::write(peer.path().join("after-replay-edit.txt"), "edited\n")
        .expect("write post-success edit");
    let replay_after_edit = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "pull-after-clean-fetch",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay_after_edit["status"], "success");
    assert_eq!(replay_after_edit["operation_id"], pulled_operation_id);
    assert_eq!(replay_after_edit["data"]["idempotent_replay"], true);
    assert_eq!(
        std::fs::read_to_string(peer.path().join("after-replay-edit.txt"))
            .expect("post-success edit content"),
        "edited\n",
        "idempotent replay must not revalidate or overwrite later user edits"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("source-only.txt")).expect("source-only content"),
        "source\n",
        "pull after fetch must materialize the source-side merged content"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "pull after fetch must preserve receiver-side merged content"
    );
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
}

#[test]
fn sync_pull_fast_forward_refuses_dirty_worktree_before_materializing() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-ff-dirty-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 ff dirty base path"),
        ])
        .assert()
        .success();

    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change(&source, "source ff dirty", "source-only.txt", "source\n");
    std::fs::write(peer.path().join("local-dirty.txt"), "dirty\n").expect("write dirty file");
    let peer_before = export_native_head(peer.path(), "target/ff-dirty-peer-before.json");

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "ff-dirty-pull",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(pulled["status"], "error");
    assert_eq!(pulled["errors"][0]["code"], "DIRTY_WORKTREE");
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "dirty fast-forward pull must not materialize remote content"
    );
    let peer_after = export_native_head(peer.path(), "target/ff-dirty-peer-after.json");
    assert_eq!(
        peer_after["data"]["native_head"], peer_before["data"]["native_head"],
        "dirty fast-forward pull must not advance native HEAD before refusing"
    );
}

#[test]
fn sync_pull_clean_divergence_records_merge_commit_and_materializes() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clean-pull-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean pull base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source clean pull", "source-only.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer clean pull", "peer-only.txt", "peer\n");
    let peer_before = checkout_current_native_head(peer.path(), "clean-pull-before.json");
    let peer_before_expected =
        expected_content_ref_for_test(peer.path()).expect("pre-merge peer expected content ref");

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-pull-merge-replay",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pulled["command"], "sync pull");
    assert_eq!(pulled["status"], "success");
    assert_eq!(pulled["data"]["merged"], true);
    assert_eq!(pulled["data"]["materialized"], true);
    let peer_after = export_native_head(peer.path(), "clean-pull-after.json");
    assert_ne!(
        peer_after["data"]["native_head"], peer_before["data"]["native_head"],
        "clean pull divergence must advance the local native head"
    );
    assert_eq!(
        peer_after["data"]["native_head"],
        pulled["data"]["merge_commit_id"]
    );
    forge_content_native::restore_content_ref_to_worktree(
        peer.path(),
        peer.path(),
        &peer_before_expected,
    )
    .expect("restore exact pre-merge content before replay");
    set_expected_content_ref_for_test(peer.path(), &peer_before_expected);
    std::fs::write(
        peer.path().join(".forge/refs/HEAD"),
        peer_before["data"]["native_head"]
            .as_str()
            .expect("prior head"),
    )
    .expect("rewind native HEAD for replay recovery test");
    let replay = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-pull-merge-replay",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(
        replay["data"]["merge_commit_id"], pulled["data"]["merge_commit_id"],
        "sync merge replay must preserve the merge commit field"
    );
    assert_eq!(
        replay["data"]["merged_content_ref"], pulled["data"]["merged_content_ref"],
        "sync merge replay must preserve the merged content field"
    );
    assert_eq!(replay["data"]["merged"], true);
    assert_eq!(replay["data"]["materialized"], true);
    assert_eq!(
        replay["data"]["source_native_head"], pulled["data"]["source_native_head"],
        "sync merge replay must preserve source head"
    );
    assert_eq!(
        replay["data"]["receiver_native_head"], pulled["data"]["receiver_native_head"],
        "sync merge replay must preserve receiver head"
    );
    assert_eq!(
        replay["data"]["common_ancestor_native_head"],
        pulled["data"]["common_ancestor_native_head"],
        "sync merge replay must preserve common ancestor"
    );
    let replay_head = export_native_head(peer.path(), "clean-pull-replay-head.json");
    assert_eq!(
        replay_head["data"]["native_head"], pulled["data"]["merge_commit_id"],
        "same-request-id replay must reconcile a stale native sync merge HEAD"
    );
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
    let peer_after_doctor = export_native_head(peer.path(), "clean-pull-after-doctor.json");
    assert_eq!(
        peer_after_doctor["data"]["native_head"], pulled["data"]["merge_commit_id"],
        "reconcile must keep the clean pull merge commit as the native tip"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "clean pull divergence must preserve receiver-side content"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("source-only.txt")).expect("source-only content"),
        "source\n",
        "clean pull divergence must materialize source-side content"
    );
    assert_eq!(conflict_count(peer.path()), 0);
}

#[test]
fn sync_pull_clean_divergence_plain_retry_heals_torn_materialization() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-pull-plain-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 clean pull plain base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source clean plain pull",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        peer.path(),
        "peer clean plain pull",
        "peer-only.txt",
        "peer\n",
    );
    let peer_before_expected =
        expected_content_ref_for_test(peer.path()).expect("pre-merge peer expected content ref");

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pulled["data"]["merged"], true);
    assert_eq!(pulled["data"]["materialized"], true);
    let merge_commit_id = pulled["data"]["merge_commit_id"]
        .as_str()
        .expect("merge commit id");

    forge_content_native::restore_content_ref_to_worktree(
        peer.path(),
        peer.path(),
        &peer_before_expected,
    )
    .expect("restore exact pre-merge content before plain retry");
    set_expected_content_ref_for_test(peer.path(), &peer_before_expected);
    let retry_after_unrestored = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(retry_after_unrestored["status"], "success");
    assert_eq!(retry_after_unrestored["data"]["materialized"], true);
    assert_eq!(
        std::fs::read_to_string(peer.path().join("source-only.txt")).expect("source-only content"),
        "source\n",
        "plain retry must restore source-side content after a torn clean sync merge"
    );
    assert_eq!(
        std::fs::read_to_string(peer.path().join("peer-only.txt")).expect("peer-only content"),
        "peer\n",
        "plain retry must preserve receiver-side content after a torn clean sync merge"
    );

    set_expected_content_ref_for_test(peer.path(), &peer_before_expected);
    let retry_after_expected_lag = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(retry_after_expected_lag["status"], "success");
    assert_eq!(retry_after_expected_lag["data"]["materialized"], true);
    let final_head = export_native_head(peer.path(), "clean-pull-plain-retry-head.json");
    assert_eq!(
        final_head["data"]["native_head"], merge_commit_id,
        "plain retry must not fork or rewind the clean sync merge commit"
    );
    forge_in(peer.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
}

#[test]
fn sync_pull_clean_divergence_refuses_dirty_worktree_before_recording_merge() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-pull-dirty-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 clean pull dirty base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source dirty pull", "source-only.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer dirty pull", "peer-only.txt", "peer\n");
    std::fs::write(peer.path().join("local-dirty.txt"), "dirty\n").expect("write dirty file");

    let pulled = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-dirty-pull",
                "sync",
                "pull",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(pulled["status"], "error");
    assert_eq!(pulled["errors"][0]["code"], "DIRTY_WORKTREE");
    assert_eq!(
        operation_count_for_request_id_and_kind(
            peer.path(),
            "clean-dirty-pull",
            "sync_pull_merged"
        ),
        0,
        "dirty clean pull must fail before recording a merge operation"
    );
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "dirty clean pull must not materialize source-side content"
    );
    assert_eq!(conflict_count(peer.path()), 0);
}

#[test]
fn doctor_reports_missing_sync_merge_commit_signature() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-missing-merge-signature-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 missing signature base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source missing merge signature",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        peer.path(),
        "peer missing merge signature",
        "peer-only.txt",
        "peer\n",
    );
    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    let merge_commit_id = fetched["data"]["merge_commit_id"]
        .as_str()
        .expect("merge commit id");
    Connection::open(peer.path().join(".forge/forge.db"))
        .expect("open forge db")
        .execute(
            "DELETE FROM ledger_signatures
             WHERE subject_kind = 'sync_merge_commit' AND subject_id = ?1",
            [merge_commit_id],
        )
        .expect("delete sync merge signature");

    let report = json(
        forge_in(peer.path())
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_eq!(report["data"]["ok"], false);
    let issues = report["data"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(issues.iter().any(|issue| {
        issue["kind"] == "missing_signature"
            && issue["subject_kind"] == "sync_merge_commit"
            && issue["subject_id"] == merge_commit_id
    }));
}

#[test]
fn doctor_accepts_imported_peer_sync_merge_commit_signature() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-import-peer-merge-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 imported peer merge base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source imported peer merge",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(
        peer.path(),
        "peer imported sync merge",
        "peer-only.txt",
        "peer\n",
    );
    let fetched = json(
        forge_in(peer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    let merge_commit_id = fetched["data"]["merge_commit_id"]
        .as_str()
        .expect("peer merge commit id")
        .to_string();

    let importer = cloned_peer_from_bundle(&bundle_path);
    let imported = json(
        forge_in(importer.path())
            .args([
                "--json",
                "sync",
                "fetch",
                peer.path().to_str().expect("utf8 peer path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(
        imported["data"]["remote_native_head"].as_str(),
        Some(merge_commit_id.as_str())
    );

    let report = json(
        forge_in(importer.path())
            .args(["--json", "doctor"])
            .assert()
            .success(),
    );
    assert_eq!(report["data"]["ok"], true);
    let issues = report["data"]["signature_issues"]
        .as_array()
        .expect("signature issues");
    assert!(
        issues.is_empty(),
        "imported peer sync merge signatures must remain peer-verifiable without local-only noise: {report}"
    );
}

#[test]
fn sync_push_clean_divergence_records_remote_merge_commit_without_materializing() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-clean-push-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean push base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source clean push", "source-only.txt", "source\n");
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer clean push", "peer-only.txt", "peer\n");
    let source_before = export_native_head(source.path(), "target/clean-push-before.json");

    let pushed = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-push-divergence",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(pushed["command"], "sync push");
    assert_eq!(pushed["status"], "success");
    assert_eq!(pushed["data"]["merged"], true);
    assert_eq!(pushed["data"]["materialized"], false);
    let pushed_operation_id = pushed["operation_id"]
        .as_str()
        .expect("clean push operation id")
        .to_string();
    let source_after = export_native_head(source.path(), "target/clean-push-after.json");
    assert_ne!(
        source_after["data"]["native_head"], source_before["data"]["native_head"],
        "clean push divergence must advance the remote native head"
    );
    assert_eq!(
        source_after["data"]["native_head"],
        pushed["data"]["merge_commit_id"]
    );
    forge_in(source.path())
        .args(["--json", "doctor"])
        .assert()
        .success();
    let source_after_doctor =
        export_native_head(source.path(), "target/clean-push-after-doctor.json");
    assert_eq!(
        source_after_doctor["data"]["native_head"], pushed["data"]["merge_commit_id"],
        "reconcile must keep the clean push merge commit as the remote native tip"
    );
    assert_eq!(
        std::fs::read_to_string(source.path().join("source-only.txt"))
            .expect("source-only content"),
        "source\n",
        "clean push divergence must not materialize over remote worktree content"
    );
    assert!(
        !source.path().join("peer-only.txt").exists(),
        "push should not materialize the peer-side file in the remote worktree"
    );
    assert_eq!(conflict_count(source.path()), 0);

    let replayed = json(
        forge_in(peer.path())
            .args([
                "--json",
                "--request-id",
                "clean-push-divergence",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replayed["operation_id"], pushed_operation_id);
    assert_eq!(replayed["data"]["idempotent_replay"], true);
    assert_eq!(replayed["data"]["merged"], true);
    assert_eq!(replayed["data"]["materialized"], false);
    assert_eq!(
        replayed["data"]["merge_commit_id"],
        pushed["data"]["merge_commit_id"]
    );
    assert_eq!(
        replayed["data"]["remote_operation_id"],
        pushed["data"]["remote_operation_id"]
    );
}

#[test]
fn sync_fetch_clean_divergence_from_subdirectory_records_merge_commit() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-clean-subdir-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 clean subdir base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(
        &source,
        "source clean subdir",
        "source-only.txt",
        "source\n",
    );
    let peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(peer.path(), "peer clean subdir", "peer-only.txt", "peer\n");
    let nested = peer.path().join("nested/leaf");
    std::fs::create_dir_all(&nested).expect("nested peer cwd");

    let fetched = json(
        forge_in(&nested)
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(fetched["status"], "success");
    assert_eq!(fetched["data"]["merged"], true);
    assert_eq!(fetched["data"]["materialized"], false);
    assert!(fetched["data"]["merge_commit_id"]
        .as_str()
        .expect("merge commit id")
        .starts_with("f1:commit:"));
    let peer_after = export_native_head(peer.path(), "clean-subdir-after.json");
    assert_eq!(
        peer_after["data"]["native_head"],
        fetched["data"]["merge_commit_id"]
    );
    assert!(
        !peer.path().join("source-only.txt").exists(),
        "subdirectory fetch should not materialize the source-side file"
    );
    assert_eq!(conflict_count(peer.path()), 0);
}

#[test]
fn sync_push_divergence_request_id_replays_without_duplicate_remote_conflict() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-push-request-id-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 push request id base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source side", "side.txt", "source\n");
    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push peer side", "side.txt", "peer\n");

    let first = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-divergence",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["merged"], false);
    let remote_operation_id = first["data"]["remote_operation_id"]
        .as_str()
        .expect("remote operation id")
        .to_string();
    assert_ne!(
        first["operation_id"], first["data"]["remote_operation_id"],
        "top-level push operation should be the local request-id marker"
    );
    assert_eq!(conflict_count(source.path()), 1);
    assert_eq!(
        operation_count_for_request_id(push_peer.path(), "push-divergence"),
        1,
        "push must claim the request-id in the initiating repo"
    );
    assert_eq!(
        operation_count_for_request_id(source.path(), "push-divergence"),
        0,
        "push divergence must not claim the initiator's request-id in the remote repo"
    );
    forge_in(source.path())
        .args([
            "--json",
            "--request-id",
            "push-divergence",
            "start",
            "remote namespace reuse",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    let conflict_refs =
        single_native_sync_conflict_content_refs(source.path(), "sync_push_divergence");
    assert_gc_keeps_content_refs_reachable(source.path(), &conflict_refs);

    let replay = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-divergence",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(
        conflict_count(source.path()),
        1,
        "local request-id replay must not call into the remote again"
    );
    let reused_for_save = json(
        forge_in(push_peer.path())
            .args(["--json", "--request-id", "push-divergence", "save"])
            .assert()
            .failure(),
    );
    assert_eq!(reused_for_save["errors"][0]["code"], "REQUEST_ID_CONFLICT");
    assert_eq!(
        first["data"]["remote_operation_id"], remote_operation_id,
        "remote operation id should remain available in the first push response"
    );
    assert_eq!(
        conflict_count(source.path()),
        1,
        "request-id replay must not create a duplicate remote conflict set"
    );
}

#[test]
fn sync_push_divergence_without_request_id_dedups_remote_conflict() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-push-no-request-id-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 push no request id base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source side", "side.txt", "source\n");
    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push peer side", "side.txt", "peer\n");

    for _ in 0..2 {
        let pushed = json(
            forge_in(push_peer.path())
                .args([
                    "--json",
                    "sync",
                    "push",
                    source.path().to_str().expect("utf8 source path"),
                ])
                .assert()
                .success(),
        );
        assert_eq!(pushed["data"]["merged"], false);
    }

    assert_eq!(
        conflict_count(source.path()),
        1,
        "repeated divergent push without request-id must reuse the remote conflict"
    );
}

#[test]
fn sync_push_divergence_without_request_id_dedups_resolved_remote_conflict() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-push-resolved-dedup-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 push resolved dedup base path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source side", "side.txt", "source\n");
    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push peer side", "side.txt", "peer\n");

    json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    let conflict_id = single_conflict_id(source.path(), "sync_push_divergence");
    mark_conflict_resolved_for_test(source.path(), &conflict_id);

    let replayed = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replayed["data"]["conflict_set_id"], conflict_id);
    assert_eq!(
        conflict_count(source.path()),
        1,
        "unrequested same-triple push should not open a second conflict after resolution"
    );
}

#[test]
fn sync_push_fast_forward_request_id_replays_locally() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source
        .path()
        .join("target/forge-sync-push-fast-forward-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path
                .to_str()
                .expect("utf8 push fast-forward base path"),
        ])
        .assert()
        .success();

    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(push_peer.path(), "push ff", "from-peer.txt", "peer\n");

    let first = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-fast-forward",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["direction"], "push");
    assert_eq!(first["data"]["materialized"], false);
    assert!(first["operation_id"].as_str().is_some());
    assert_eq!(
        operation_count_for_request_id(push_peer.path(), "push-fast-forward"),
        1,
        "fast-forward push must claim the request-id in the initiating repo"
    );
    assert_eq!(
        operation_count_for_request_id(source.path(), "push-fast-forward"),
        0,
        "fast-forward import has no remote request-id row"
    );

    let replay = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-fast-forward",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
}

#[test]
fn sync_fetch_noop_request_id_replays_locally() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-fetch-noop-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 fetch noop base path"),
        ])
        .assert()
        .success();

    let fetch_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change_in(fetch_peer.path(), "fetch noop", "peer.txt", "peer\n");

    let first = json(
        forge_in(fetch_peer.path())
            .args([
                "--json",
                "--request-id",
                "fetch-noop",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["direction"], "fetch");
    assert_eq!(first["data"]["up_to_date"], true);
    assert!(first["operation_id"].as_str().is_some());
    assert_eq!(
        operation_count_for_request_id(fetch_peer.path(), "fetch-noop"),
        1,
        "no-op fetch must still claim the request-id in the initiating repo"
    );

    let replay = json(
        forge_in(fetch_peer.path())
            .args([
                "--json",
                "--request-id",
                "fetch-noop",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["direction"], first["data"]["direction"]);
    assert_eq!(replay["data"]["remote_path"], first["data"]["remote_path"]);
    assert_eq!(
        replay["data"]["base_native_head"], first["data"]["base_native_head"],
        "sync fetch replay must preserve the base head"
    );
    assert_eq!(
        replay["data"]["remote_native_head"], first["data"]["remote_native_head"],
        "sync fetch replay must preserve the remote head"
    );
    assert_eq!(
        replay["data"]["imported_native_objects"], first["data"]["imported_native_objects"],
        "sync fetch replay must preserve import counts"
    );
    assert_eq!(
        replay["data"]["imported_ledger_rows"], first["data"]["imported_ledger_rows"],
        "sync fetch replay must preserve ledger counts"
    );
    assert_eq!(
        replay["data"]["materialized"],
        first["data"]["materialized"]
    );
    assert_eq!(replay["data"]["up_to_date"], first["data"]["up_to_date"]);

    let reused_for_save = json(
        forge_in(fetch_peer.path())
            .args(["--json", "--request-id", "fetch-noop", "save"])
            .assert()
            .failure(),
    );
    assert_eq!(reused_for_save["errors"][0]["code"], "REQUEST_ID_CONFLICT");
}

#[test]
fn sync_push_noop_request_id_replays_locally() {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = source.path().join("target/forge-sync-push-noop-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 push noop base path"),
        ])
        .assert()
        .success();

    let push_peer = cloned_peer_from_bundle(&bundle_path);
    native_accept_file_change(&source, "push noop", "source.txt", "source\n");

    let first = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-noop",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(first["data"]["direction"], "push");
    assert_eq!(first["data"]["up_to_date"], true);
    assert!(first["operation_id"].as_str().is_some());
    assert_eq!(
        operation_count_for_request_id(push_peer.path(), "push-noop"),
        1,
        "no-op push must still claim the request-id in the initiating repo"
    );
    assert_eq!(
        operation_count_for_request_id(source.path(), "push-noop"),
        0,
        "no-op push has no remote request-id row"
    );

    let replay = json(
        forge_in(push_peer.path())
            .args([
                "--json",
                "--request-id",
                "push-noop",
                "sync",
                "push",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(replay["operation_id"], first["operation_id"]);
    assert_eq!(replay["data"]["idempotent_replay"], true);
    assert_eq!(replay["data"]["direction"], "push");
    assert_eq!(replay["data"]["up_to_date"], true);
    assert_eq!(
        replay["data"]["base_native_head"],
        first["data"]["base_native_head"]
    );
    assert_eq!(
        replay["data"]["local_native_head"],
        first["data"]["local_native_head"]
    );
    assert_eq!(replay["data"]["materialized"], false);

    let reused_for_save = json(
        forge_in(push_peer.path())
            .args(["--json", "--request-id", "push-noop", "save"])
            .assert()
            .failure(),
    );
    assert_eq!(reused_for_save["errors"][0]["code"], "REQUEST_ID_CONFLICT");
}

#[test]
fn sync_clone_carries_conflict_sets_from_source_ledger() {
    let source = tempfile::tempdir().expect("source conflict repo");
    record_sync_divergence_conflict(source.path());

    let bundle_dir = tempfile::tempdir().expect("conflict bundle dir");
    let bundle_path = bundle_dir.path().join("conflict-ledger.json");
    forge_in(source.path())
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 conflict bundle"),
        ])
        .assert()
        .success();

    let clone_dir = tempfile::tempdir().expect("conflict clone dir");
    forge_in(clone_dir.path())
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 conflict bundle"),
        ])
        .assert()
        .success();

    assert_single_native_sync_conflict(clone_dir.path(), "sync_fetch_divergence");
}

#[test]
fn sync_clone_carries_native_merge_path_conflicts_from_source_ledger() {
    let source = TestRepo::new_git();
    let conflict_id = record_native_merge_conflict(&source);

    let bundle_dir = tempfile::tempdir().expect("merge conflict bundle dir");
    let bundle_path = bundle_dir.path().join("merge-conflict-ledger.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 merge conflict bundle"),
        ])
        .assert()
        .success();

    let clone_dir = tempfile::tempdir().expect("merge conflict clone dir");
    forge_in(clone_dir.path())
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 merge conflict bundle"),
        ])
        .assert()
        .success();

    let shown = forge_json(clone_dir.path(), &["conflict", "show", &conflict_id]);
    assert_eq!(
        shown["data"]["conflict"]["resolver_backend"],
        "native_merge"
    );
    assert_eq!(
        shown["data"]["path_conflicts"]
            .as_array()
            .expect("path conflicts")
            .len(),
        1
    );
    assert_eq!(shown["data"]["path_conflicts"][0]["kind"], "content");
}

#[test]
fn sync_peer_fetch_pull_and_push_carry_conflict_sets() {
    let (source, _base_dir, base_bundle) = source_with_conflict_after_base_export();

    let fetch_peer = cloned_peer_from_bundle(&base_bundle);
    forge_in(fetch_peer.path())
        .args([
            "--json",
            "sync",
            "fetch",
            source.path().to_str().expect("utf8 source path"),
        ])
        .assert()
        .success();
    assert_single_native_sync_conflict(fetch_peer.path(), "sync_fetch_divergence");

    let pull_peer = cloned_peer_from_bundle(&base_bundle);
    forge_in(pull_peer.path())
        .args([
            "--json",
            "sync",
            "pull",
            source.path().to_str().expect("utf8 source path"),
        ])
        .assert()
        .success();
    assert_single_native_sync_conflict(pull_peer.path(), "sync_fetch_divergence");

    let push_peer = cloned_peer_from_bundle(&base_bundle);
    forge_in(source.path())
        .args([
            "--json",
            "sync",
            "push",
            push_peer.path().to_str().expect("utf8 push peer path"),
        ])
        .assert()
        .success();
    assert_single_native_sync_conflict(push_peer.path(), "sync_fetch_divergence");
}
