mod common;

use common::TestRepo;
use forge_content_native::materialize_content_ref;
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
use std::path::Path;

fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json envelope")
}

fn attempt_id(output: &Value) -> String {
    output["data"]["attempt_id"]
        .as_str()
        .expect("attempt id")
        .to_string()
}

fn contains_bytes(root: &Path, needle: &[u8]) -> bool {
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        } else if metadata.is_file() {
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if bytes.windows(needle.len()).any(|window| window == needle) {
                return true;
            }
        }
    }
    false
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git stdout utf8")
}

#[test]
fn private_path_save_omits_public_tree_and_records_encrypted_overlay() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private extension"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);

    fs::create_dir_all(repo.path().join("src")).expect("create src");
    fs::write(repo.path().join("src/public.rs"), "pub fn public() {}\n").expect("write public");
    let private_sentinel = "PRIVATE_SENTINEL_NER_356_DO_NOT_LEAK";
    fs::write(
        repo.path().join("src/private_ext.rs"),
        format!("pub const SECRET: &str = \"{private_sentinel}\";\n"),
    )
    .expect("write private");

    let label = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "path",
                "set",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--path",
                "src/private_ext.rs",
                "--visibility",
                "private",
            ])
            .assert()
            .success(),
    );
    assert_eq!(label["data"]["visibility"], "private");
    assert_ne!(label["data"]["path_hash"], "src/private_ext.rs");
    assert!(label["data"]["path_hash"]
        .as_str()
        .expect("path hash")
        .starts_with("hmac-sha256:"));

    let saved = json(repo.forge().args(["--json", "save"]).assert().success());
    let content_ref = saved["data"]["content_ref"]
        .as_str()
        .expect("saved content ref");

    let materialized = tempfile::tempdir().expect("materialized tempdir");
    materialize_content_ref(repo.path(), materialized.path(), content_ref)
        .expect("materialize public content ref");
    assert!(materialized.path().join("src/public.rs").exists());
    assert!(
        !materialized.path().join("src/private_ext.rs").exists(),
        "public tree must not contain private path"
    );
    assert!(
        !contains_bytes(
            &repo.path().join(".forge/objects"),
            private_sentinel.as_bytes()
        ),
        "public native object store leaked private sentinel"
    );

    let conn = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let payload_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM encrypted_private_payloads",
            [],
            |row| row.get(0),
        )
        .expect("payload count");
    assert_eq!(payload_count, 1);

    let syncable_rows: String = conn
        .query_row(
            "SELECT json_object(
                'labels', (SELECT json_group_array(json_object(
                    'path_hash', path_hash,
                    'encrypted_display_path', encrypted_display_path
                )) FROM private_path_labels),
                'payloads', (SELECT json_group_array(json_object(
                    'path_hash', path_hash,
                    'private_object_path', private_object_path,
                    'encrypted_metadata_json', encrypted_metadata_json
                )) FROM encrypted_private_payloads)
            )",
            [],
            |row| row.get(0),
        )
        .expect("syncable rows");
    assert!(
        !syncable_rows.contains("src/private_ext.rs"),
        "syncable private rows leaked plaintext path: {syncable_rows}"
    );
    assert!(
        !syncable_rows.contains(private_sentinel),
        "syncable private rows leaked plaintext payload: {syncable_rows}"
    );
}

#[test]
fn private_overlay_failure_does_not_commit_public_snapshot() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private extension"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);

    fs::create_dir_all(repo.path().join("src")).expect("create src");
    fs::write(repo.path().join("src/public.rs"), "pub fn public() {}\n").expect("write public");
    fs::write(repo.path().join("src/private_ext.rs"), "private\n").expect("write private");

    repo.forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--path",
            "src/private_ext.rs",
            "--visibility",
            "private",
        ])
        .assert()
        .success();
    fs::remove_file(repo.path().join("src/private_ext.rs")).expect("remove private before save");

    let failed = json(repo.forge().args(["--json", "save"]).assert().failure());
    assert_eq!(failed["errors"][0]["code"], "PRIVATE_CONTENT_INVALID");
    assert_eq!(
        failed["errors"][0]["details"]["reason"],
        "private_path_not_regular_file"
    );

    let conn = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let snapshot_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM snapshots", [], |row| row.get(0))
        .expect("snapshot count");
    assert_eq!(
        snapshot_count, 0,
        "private overlay failure must not leave a public-only snapshot"
    );
    let payload_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM encrypted_private_payloads",
            [],
            |row| row.get(0),
        )
        .expect("payload count");
    assert_eq!(payload_count, 0);
}

#[test]
fn missing_local_private_label_cache_fails_closed() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private extension"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);
    fs::create_dir_all(repo.path().join("src")).expect("create src");
    fs::write(repo.path().join("src/private_ext.rs"), "private\n").expect("write private");
    repo.forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--path",
            "src/private_ext.rs",
            "--visibility",
            "private",
        ])
        .assert()
        .success();
    fs::remove_file(repo.path().join(".forge/private/path-labels.json"))
        .expect("remove local label cache");

    let failed = json(repo.forge().args(["--json", "save"]).assert().failure());
    assert_eq!(failed["errors"][0]["code"], "PRIVATE_CONTENT_INVALID");
    assert_eq!(
        failed["errors"][0]["details"]["reason"],
        "missing_local_private_path_labels"
    );
}

#[test]
fn directory_private_label_fails_before_plaintext_objects_are_written() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private extension"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);
    fs::create_dir_all(repo.path().join("secrets")).expect("create secrets dir");
    let private_sentinel = "PRIVATE_DIRECTORY_SENTINEL_NER_356_DO_NOT_LEAK";
    fs::write(repo.path().join("secrets/private.rs"), private_sentinel).expect("write private");
    repo.forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--path",
            "secrets",
            "--visibility",
            "private",
        ])
        .assert()
        .success();

    let failed = json(repo.forge().args(["--json", "save"]).assert().failure());
    assert_eq!(failed["errors"][0]["code"], "PRIVATE_CONTENT_INVALID");
    assert_eq!(
        failed["errors"][0]["details"]["reason"],
        "private_path_not_regular_file"
    );
    assert!(
        !contains_bytes(
            &repo.path().join(".forge/objects"),
            private_sentinel.as_bytes()
        ),
        "failed directory private save leaked plaintext native objects"
    );
}

#[test]
fn private_path_from_base_is_not_reported_as_public_changed_path() {
    let repo = TestRepo::new_git();
    fs::create_dir_all(repo.path().join("src")).expect("create src");
    fs::write(repo.path().join("src/private_ext.rs"), "base private\n").expect("write private");
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private extension"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);
    repo.forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--path",
            "src/private_ext.rs",
            "--visibility",
            "private",
        ])
        .assert()
        .success();
    let saved = json(repo.forge().args(["--json", "save"]).assert().success());
    let saved_text = serde_json::to_string(&saved).expect("save json");

    assert!(
        !saved_text.contains("src/private_ext.rs"),
        "save response leaked private path as changed path: {saved_text}"
    );
}

#[test]
fn public_private_path_label_is_rejected() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private extension"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);
    let failed = json(
        repo.forge()
            .args([
                "--json",
                "visibility",
                "path",
                "set",
                "--kind",
                "attempt",
                "--id",
                &attempt_id,
                "--path",
                "src/private_ext.rs",
                "--visibility",
                "public",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(failed["errors"][0]["code"], "PRIVATE_CONTENT_INVALID");
    assert_eq!(
        failed["errors"][0]["details"]["reason"],
        "public_private_path_label"
    );
}

#[test]
fn run_fails_closed_when_attempt_has_private_path_labels() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private evidence"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);
    fs::create_dir_all(repo.path().join("src")).expect("create src");
    let private_sentinel = "PRIVATE_EVIDENCE_SENTINEL_NER_356_DO_NOT_LEAK";
    fs::write(repo.path().join("src/private_ext.rs"), private_sentinel).expect("write private");
    repo.forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--path",
            "src/private_ext.rs",
            "--visibility",
            "private",
        ])
        .assert()
        .success();

    let failed = json(
        repo.forge()
            .args([
                "--json",
                "run",
                "--",
                "sh",
                "-c",
                &format!("printf {private_sentinel}"),
            ])
            .assert()
            .failure(),
    );
    assert_eq!(failed["errors"][0]["code"], "PRIVATE_CONTENT_INVALID");
    assert_eq!(
        failed["errors"][0]["details"]["reason"],
        "private_tainted_evidence_unsupported"
    );

    let conn = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    let evidence_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM evidence", [], |row| row.get(0))
        .expect("evidence count");
    assert_eq!(evidence_count, 0);
    assert!(
        !contains_bytes(&repo.path().join(".forge"), private_sentinel.as_bytes()),
        "private sentinel should not be persisted by failed evidence capture"
    );
}

#[test]
fn public_git_export_omits_private_overlay_content() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private extension"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);

    fs::create_dir_all(repo.path().join("src")).expect("create src");
    fs::write(repo.path().join("src/public.rs"), "pub fn public() {}\n").expect("write public");
    let private_sentinel = "PRIVATE_SENTINEL_EXPORT_NER_356_DO_NOT_LEAK";
    fs::write(
        repo.path().join("src/private_ext.rs"),
        format!("pub const SECRET: &str = \"{private_sentinel}\";\n"),
    )
    .expect("write private");
    repo.forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--path",
            "src/private_ext.rs",
            "--visibility",
            "private",
        ])
        .assert()
        .success();
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge()
        .args(["--json", "accept", "--allow-unverified"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/private-public"])
        .assert()
        .success();

    let listing = git(
        repo.path(),
        &["ls-tree", "-r", "--name-only", "forge/private-public"],
    );
    assert!(listing.contains("src/public.rs"));
    assert!(
        !listing.contains("src/private_ext.rs"),
        "exported branch leaked private path: {listing}"
    );
    let grep = std::process::Command::new("git")
        .args(["grep", "-n", private_sentinel, "forge/private-public"])
        .current_dir(repo.path())
        .output()
        .expect("git grep");
    assert!(
        !grep.status.success(),
        "exported branch leaked private sentinel: {}",
        String::from_utf8_lossy(&grep.stdout)
    );
}

#[test]
fn projected_sync_from_private_snapshot_uses_generic_v2_without_private_existence_signal() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let started = json(
        repo.forge()
            .args(["--json", "start", "private projected sync"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);

    fs::create_dir_all(repo.path().join("src")).expect("create src");
    fs::write(repo.path().join("src/public.rs"), "pub fn public() {}\n").expect("write public");
    let private_sentinel = "PRIVATE_SENTINEL_SYNC_NER_356_DO_NOT_LEAK";
    fs::write(
        repo.path().join("src/private_ext.rs"),
        format!("pub const SECRET: &str = \"{private_sentinel}\";\n"),
    )
    .expect("write private");
    repo.forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--path",
            "src/private_ext.rs",
            "--visibility",
            "private",
        ])
        .assert()
        .success();
    repo.forge().args(["--json", "save"]).assert().success();

    let manifest_path = repo.path().join("target/private-projected-sync.json");
    let exported = json(
        repo.forge()
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                manifest_path.to_str().expect("manifest path"),
                "--recipient",
                "reviewer@example.test",
            ])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["protocol_version"], "forge-sync.v2");
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest"))
        .expect("manifest json");
    assert_eq!(manifest["protocol_version"], "forge-sync.v2");
    assert_eq!(manifest["private_content"]["capable"], false);
    assert_eq!(manifest["private_content"]["omitted"], false);
    assert_eq!(manifest["private_content"]["encrypted_payload_count"], 0);

    let manifest_text = serde_json::to_string(&manifest).expect("manifest text");
    assert!(!manifest_text.contains("src/private_ext.rs"));
    assert!(!manifest_text.contains(private_sentinel));
    assert!(!manifest_text.contains(".forge/private/objects"));
}

#[test]
fn authorized_projected_sync_materializes_private_overlay_and_resave_stays_private() {
    let source = TestRepo::new_git();
    source
        .forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let org = json(
        source
            .forge()
            .args(["--json", "org", "init", "--actor", "alice"])
            .assert()
            .success(),
    );
    let principal_id = org["data"]["owner_actor_id"]
        .as_str()
        .expect("owner actor id")
        .to_string();
    source
        .forge()
        .args([
            "--json",
            "org",
            "encryption",
            "bind-local",
            "--principal-id",
            &principal_id,
        ])
        .assert()
        .success();

    let started = json(
        source
            .forge()
            .args(["--json", "start", "authorized private projected sync"])
            .assert()
            .success(),
    );
    let attempt_id = attempt_id(&started);
    fs::create_dir_all(source.path().join("src")).expect("create src");
    fs::write(source.path().join("src/public.rs"), "pub fn public() {}\n").expect("write public");
    let private_sentinel = "PRIVATE_SENTINEL_AUTHORIZED_SYNC_NER_356";
    fs::write(
        source.path().join("src/private_ext.rs"),
        format!("pub const SECRET: &str = \"{private_sentinel}\";\n"),
    )
    .expect("write private");
    source
        .forge()
        .args([
            "--json",
            "visibility",
            "path",
            "set",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--path",
            "src/private_ext.rs",
            "--visibility",
            "private",
        ])
        .assert()
        .success();
    source
        .forge()
        .args([
            "--json",
            "visibility",
            "grant",
            "--kind",
            "attempt",
            "--id",
            &attempt_id,
            "--recipient",
            &principal_id,
            "--capability",
            "sync_materialize",
        ])
        .assert()
        .success();
    source.forge().args(["--json", "save"]).assert().success();
    source
        .forge()
        .args(["--json", "propose"])
        .assert()
        .success();
    source.forge().args(["--json", "check"]).assert().success();
    source
        .forge()
        .args(["--json", "accept", "--allow-unverified"])
        .assert()
        .success();

    let manifest_path = source.path().join("target/private-authorized-sync.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            manifest_path.to_str().expect("manifest path"),
            "--recipient",
            &principal_id,
        ])
        .assert()
        .success();
    let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest"))
        .expect("manifest json");
    assert_eq!(manifest["protocol_version"], "forge-sync.v2");
    assert_eq!(manifest["private_content"]["capable"], true);
    assert_eq!(manifest["private_content"]["encrypted_payload_count"], 1);
    assert_eq!(
        manifest["private_overlays"][0]["path"],
        "src/private_ext.rs"
    );
    let manifest_text = serde_json::to_string(&manifest).expect("manifest text");
    assert!(!manifest_text.contains(private_sentinel));
    assert!(!manifest_text.contains(".forge/private/objects"));

    let target = TestRepo::new_git();
    target
        .forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    let source_key = source.path().join(".forge/keys/local-age-x25519.txt");
    let target_key = target.path().join(".forge/keys/local-age-x25519.txt");
    fs::create_dir_all(target_key.parent().expect("target key parent"))
        .expect("create target key parent");
    fs::copy(&source_key, &target_key).expect("copy recipient private key");

    let imported = json(
        target
            .forge()
            .args([
                "--json",
                "sync",
                "import",
                manifest_path.to_str().expect("manifest path"),
                "--materialize",
            ])
            .assert()
            .success(),
    );
    assert_eq!(imported["data"]["materialized"], true);
    assert_eq!(imported["data"]["materialized_private_overlay_count"], 1);
    assert!(target.path().join("src/public.rs").exists());
    assert_eq!(
        fs::read_to_string(target.path().join("src/private_ext.rs")).expect("read private"),
        format!("pub const SECRET: &str = \"{private_sentinel}\";\n")
    );

    target.forge().args(["--json", "save"]).assert().success();
    let saved = json(target.forge().args(["--json", "save"]).assert().success());
    let content_ref = saved["data"]["content_ref"].as_str().expect("content ref");
    let materialized = tempfile::tempdir().expect("materialized tempdir");
    materialize_content_ref(target.path(), materialized.path(), content_ref)
        .expect("materialize public content ref");
    assert!(materialized.path().join("src/public.rs").exists());
    assert!(
        !materialized.path().join("src/private_ext.rs").exists(),
        "re-saved public tree must not contain private path"
    );
}
