mod common;

use common::TestRepo;
use forge_content::FORGE_TREE_PREFIX;
use forge_content_native::{NativeObjectStore, ObjectId, ObjectKind};
use serde_json::Value;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const LARGE_BYTES: usize = 1024 * 1024 + 257;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn large_payload(seed: u8) -> Vec<u8> {
    (0..LARGE_BYTES)
        .map(|index| seed.wrapping_add((index % 251) as u8))
        .collect()
}

fn init_native(repo: &TestRepo, intent: &str) {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "start", intent])
        .assert()
        .success();
}

fn root_tree_id(content_ref: &str) -> ObjectId {
    ObjectId::parse(
        content_ref
            .strip_prefix(FORGE_TREE_PREFIX)
            .expect("forge tree content ref"),
    )
    .expect("parse root tree id")
}

fn tree_entry(store: &NativeObjectStore, tree: &ObjectId, name: &str) -> Value {
    let payload = store.read_object(tree).expect("read tree object");
    let tree: Value = serde_json::from_slice(&payload).expect("tree json");
    tree["entries"]
        .as_array()
        .expect("tree entries")
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("missing tree entry {name}: {tree}"))
        .clone()
}

fn forge_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("forge")
}

fn loose_object_path(repo: &Path, id: &ObjectId) -> std::path::PathBuf {
    repo.join(".forge/objects/sha256")
        .join(&id.digest()[..2])
        .join(id.digest())
}

fn run_until_crash(repo: &Path, crash_point: &str, args: &[&str]) {
    let output = Command::new(forge_bin())
        .args(args)
        .env("FORGE_CRASH_POINT", crash_point)
        .current_dir(repo)
        .output()
        .expect("spawn forge");
    assert!(
        !output.status.success(),
        "expected injected crash `{crash_point}` to abort `forge {args:?}`, but it succeeded"
    );
}

#[test]
fn streaming_blob_write_matches_reference_object_id() {
    let repo = TestRepo::new_git();
    let path = repo.path().join("large.bin");
    let payload = large_payload(17);
    fs::write(&path, &payload).expect("write large payload");

    let store = NativeObjectStore::new(repo.path());
    let streamed = store
        .write_blob_from_path(&path)
        .expect("stream large blob into native store");
    let reference = ObjectId::new(ObjectKind::Blob, &payload);

    assert_eq!(streamed, reference);
    assert_eq!(
        store.read_object(&streamed).expect("read streamed blob"),
        payload
    );
}

#[test]
fn streaming_restore_preserves_legacy_raw_large_blob_compatibility() {
    let repo = TestRepo::new_git();
    let payload = large_payload(23);
    let id = ObjectId::new(ObjectKind::Blob, &payload);
    let path = loose_object_path(repo.path(), &id);
    fs::create_dir_all(path.parent().expect("object parent")).expect("create object shard");
    fs::write(&path, &payload).expect("write legacy raw object");

    let store = NativeObjectStore::new(repo.path());
    let mut restored = Vec::new();
    store
        .write_object_payload_to(&id, &mut restored)
        .expect("stream legacy raw payload");

    assert_eq!(restored, payload);
}

#[test]
fn large_blob_restore_round_trips_bytes_and_mode() {
    let repo = TestRepo::new_git();
    init_native(&repo, "large restore");

    let large = repo.path().join("large.bin");
    let first = large_payload(3);
    let second = large_payload(99);
    fs::write(&large, &first).expect("write first large blob");
    #[cfg(unix)]
    fs::set_permissions(&large, fs::Permissions::from_mode(0o755)).expect("make executable");
    let first_save = json_output(repo.forge().args(["--json", "save"]).assert().success());
    let first_snapshot = first_save["data"]["snapshot_id"]
        .as_str()
        .unwrap()
        .to_string();

    fs::write(&large, &second).expect("write second large blob");
    #[cfg(unix)]
    fs::set_permissions(&large, fs::Permissions::from_mode(0o644)).expect("make non-executable");
    repo.forge().args(["--json", "save"]).assert().success();

    repo.forge()
        .args(["--json", "restore", &first_snapshot, "--yes"])
        .assert()
        .success();

    assert_eq!(fs::read(&large).expect("read restored large blob"), first);
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&large)
            .expect("restored metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn secret_risk_large_paths_are_excluded_before_streaming_starts() {
    let repo = TestRepo::new_git();
    init_native(&repo, "large secret exclusion");

    fs::write(repo.path().join(".env"), large_payload(42)).expect("write large secret");
    fs::write(repo.path().join("visible.bin"), large_payload(7)).expect("write visible large blob");
    let saved = json_output(repo.forge().args(["--json", "save"]).assert().success());

    let changed = saved["data"]["changed_paths"]
        .as_array()
        .expect("changed paths");
    assert!(
        changed.iter().all(|path| path.as_str() != Some(".env")),
        "secret-risk path leaked into changed paths: {saved}"
    );

    let store = NativeObjectStore::new(repo.path());
    let root = root_tree_id(saved["data"]["content_ref"].as_str().unwrap());
    let visible = tree_entry(&store, &root, "visible.bin");
    assert_eq!(visible["kind"], "file");
    assert!(
        serde_json::from_slice::<Value>(&store.read_object(&root).expect("read root tree"))
            .expect("root tree json")["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .all(|entry| entry["name"] != ".env"),
        "secret-risk entry leaked into native tree"
    );
}

#[test]
fn crash_after_streaming_large_blob_before_db_commit_leaves_no_referenced_missing_object() {
    let repo = TestRepo::new_git();
    init_native(&repo, "large crash before db");
    fs::write(repo.path().join("large.bin"), large_payload(64)).expect("write large blob");

    run_until_crash(
        repo.path(),
        "after_object_fsync_before_db_commit",
        &["--json", "save"],
    );

    let doctor = json_output(repo.forge().args(["--json", "doctor"]).assert().success());
    assert_eq!(doctor["data"]["ok"], true, "doctor after crash: {doctor}");
    assert!(
        doctor["data"]["dangling_content_refs"]
            .as_array()
            .expect("dangling refs")
            .is_empty(),
        "crash before DB commit must not leave a committed ref to a missing streamed object"
    );
}

#[test]
fn large_regular_file_paths_do_not_use_whole_file_buffers() {
    let source = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../forge-content-native/src/lib.rs"
    ))
    .expect("read native content source");
    let write_tree = source
        .split("fn write_tree(")
        .nth(1)
        .expect("write_tree function")
        .split("fn materialize_tree(")
        .next()
        .expect("write_tree body");
    let materialize_tree = source
        .split("fn materialize_tree(")
        .nth(1)
        .expect("materialize_tree function")
        .split("/// Lexically normalize")
        .next()
        .expect("materialize_tree body");

    assert!(write_tree.contains("write_blob_from_path(&repo_root.join(&file.path))"));
    assert!(materialize_tree.contains("write_object_payload_to(&child, &mut writer)"));
    assert!(
        !write_tree.contains("let bytes = fs::read(repo_root.join(&file.path))?;"),
        "snapshotting regular files regressed to whole-file fs::read"
    );
    assert!(
        !materialize_tree
            .contains("let bytes = store.read_object(&child)?;\n                let full = repo_root.join(&rel);"),
        "restoring regular files regressed to whole-blob Vec buffering"
    );
}
