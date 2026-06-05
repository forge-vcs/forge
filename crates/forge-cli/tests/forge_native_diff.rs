//! NER-139 Phase 8 S1: native structured diff, rename detection, working-vs-snapshot,
//! and the no-git proof for native compare diff.

mod common;

use common::TestRepo;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn forge_ok(repo: &TestRepo, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    json_output(repo.forge().args(&full).assert().success())
}

fn start_native(repo: &TestRepo) -> (String, String) {
    forge_ok(repo, &["init", "--content-backend", "native"]);
    let first = forge_ok(repo, &["start", "native diff"]);
    (
        first["data"]["intent_id"].as_str().unwrap().to_string(),
        first["data"]["attempt_id"].as_str().unwrap().to_string(),
    )
}

fn native_rename_attempts(repo: &TestRepo) -> (String, String) {
    let (intent_id, attempt_a) = start_native(repo);
    std::fs::write(repo.path().join("old.txt"), "one\ntwo\nthree\n").unwrap();
    forge_ok(repo, &["save", "--attempt", &attempt_a]);
    forge_ok(
        repo,
        &["run", "--attempt", &attempt_a, "--", "sh", "-c", "true"],
    );
    forge_ok(repo, &["propose", "--attempt", &attempt_a]);

    let second = forge_ok(repo, &["attempt", "start", "--intent", &intent_id]);
    let attempt_b = second["data"]["attempt_id"].as_str().unwrap().to_string();
    forge_ok(repo, &["attempt", "attach", &attempt_b]);
    let _ = std::fs::remove_file(repo.path().join("old.txt"));
    std::fs::write(repo.path().join("new.txt"), "one\ntwo\nchanged\n").unwrap();
    forge_ok(repo, &["save", "--attempt", &attempt_b]);
    forge_ok(
        repo,
        &["run", "--attempt", &attempt_b, "--", "sh", "-c", "true"],
    );
    forge_ok(repo, &["propose", "--attempt", &attempt_b]);
    (attempt_a, attempt_b)
}

#[test]
fn native_compare_diff_emits_structured_hunks_and_rename() {
    let repo = TestRepo::new_git();
    let (attempt_a, attempt_b) = native_rename_attempts(&repo);

    let out = forge_ok(&repo, &["compare", "--diff", &attempt_a, &attempt_b]);
    let files = out["data"]["diff"]["files"].as_array().unwrap();
    let renamed = files
        .iter()
        .find(|file| file["path"] == "new.txt")
        .expect("renamed file");
    assert!(renamed["status"].as_str().unwrap().starts_with('R'));
    assert_eq!(renamed["old_path"], "old.txt");
    assert!(renamed["similarity"].as_u64().unwrap() >= 50);
    assert!(renamed["hunk"].as_str().unwrap().contains("changed"));
    assert!(!renamed["hunks"].as_array().unwrap().is_empty());
    assert_eq!(renamed["hunks"][0]["lines"][0]["tag"], "context");
}

#[test]
#[cfg(unix)]
fn forge_diff_working_vs_snapshot_is_policy_filtered_and_symlink_aware() {
    let repo = TestRepo::new_git();
    let (_intent_id, attempt_id) = start_native(&repo);
    std::fs::write(repo.path().join("app.txt"), "old\n").unwrap();
    std::fs::write(repo.path().join("gone.txt"), "gone\n").unwrap();
    let saved = forge_ok(&repo, &["save", "--attempt", &attempt_id]);
    let snapshot_ref = saved["data"]["content_ref"].as_str().unwrap().to_string();

    std::fs::write(repo.path().join("app.txt"), "new\n").unwrap();
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    std::fs::write(repo.path().join("added.txt"), "added\n").unwrap();
    std::fs::write(repo.path().join(".env"), "API_TOKEN=secret\n").unwrap();
    std::os::unix::fs::symlink("app.txt", repo.path().join("link")).unwrap();

    let out = forge_ok(&repo, &["diff", "--working", "--to", &snapshot_ref]);
    let files = out["data"]["files"].as_array().unwrap();
    let paths: Vec<&str> = files
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"app.txt"));
    assert!(paths.contains(&"gone.txt"));
    assert!(paths.contains(&"added.txt"));
    assert!(paths.contains(&"link"));
    assert!(!paths.contains(&".env"));
}

#[test]
#[cfg(unix)]
fn forge_diff_working_vs_snapshot_detects_renames_and_emits_hunks() {
    let repo = TestRepo::new_git();
    let (_intent_id, attempt_id) = start_native(&repo);
    std::fs::write(repo.path().join("app.txt"), "hello\nworld\n").unwrap();
    let saved = forge_ok(&repo, &["save", "--attempt", &attempt_id]);
    let snapshot_ref = saved["data"]["content_ref"].as_str().unwrap().to_string();

    std::fs::rename(repo.path().join("app.txt"), repo.path().join("renamed.txt")).unwrap();
    std::fs::write(repo.path().join("renamed.txt"), "hello\nforge\nworld\n").unwrap();

    let out = forge_ok(
        &repo,
        &["diff", "--working", "--to", &snapshot_ref, "--find-renames"],
    );
    let files = out["data"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    let renamed = &files[0];
    assert_eq!(renamed["path"], "renamed.txt");
    assert_eq!(renamed["old_path"], "app.txt");
    assert!(renamed["status"].as_str().unwrap().starts_with('R'));
    assert!(renamed["similarity"].as_u64().unwrap() >= 50);
    assert!(renamed["hunk"].as_str().unwrap().contains("forge"));
    assert!(!renamed["hunks"].as_array().unwrap().is_empty());
}

#[test]
#[cfg(unix)]
fn native_compare_diff_runs_with_git_removed_from_path() {
    let repo = TestRepo::new_git();
    let (attempt_a, attempt_b) = native_rename_attempts(&repo);
    let bin = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("/bin/sh", bin.path().join("sh")).unwrap();

    let out = json_output(
        repo.forge()
            .env("PATH", bin.path())
            .args(["--json", "compare", "--diff", &attempt_a, &attempt_b])
            .assert()
            .success(),
    );
    assert!(out["data"]["diff"]["files"].as_array().unwrap().len() == 1);
}

#[test]
fn native_diff_structurally_matches_git_patience_oracle() {
    let repo = TestRepo::new_git();
    let (_intent_id, attempt_id) = start_native(&repo);
    std::fs::write(repo.path().join("mod.txt"), "old\nshared\n").unwrap();
    std::fs::write(repo.path().join("gone.txt"), "gone\n").unwrap();
    std::fs::write(repo.path().join("bin.dat"), b"a\0b").unwrap();
    let old = forge_ok(&repo, &["save", "--attempt", &attempt_id]);
    let old_ref = old["data"]["content_ref"].as_str().unwrap().to_string();

    std::fs::write(repo.path().join("mod.txt"), "new\nshared\n").unwrap();
    std::fs::remove_file(repo.path().join("gone.txt")).unwrap();
    std::fs::write(repo.path().join("added.txt"), "added\n").unwrap();
    std::fs::write(repo.path().join("bin.dat"), b"a\0c").unwrap();
    let new = forge_ok(&repo, &["save", "--attempt", &attempt_id]);
    let new_ref = new["data"]["content_ref"].as_str().unwrap().to_string();

    let native = forge_ok(
        &repo,
        &["diff", "--from", &old_ref, "--to", &new_ref, "--no-renames"],
    );
    let native_files = structural_files(native["data"]["files"].as_array().unwrap());

    let old_dir = tempfile::tempdir().unwrap();
    let new_dir = tempfile::tempdir().unwrap();
    write_oracle_tree(
        old_dir.path(),
        &[
            ("mod.txt", b"old\nshared\n".as_slice()),
            ("gone.txt", b"gone\n".as_slice()),
            ("bin.dat", b"a\0b".as_slice()),
        ],
    );
    write_oracle_tree(
        new_dir.path(),
        &[
            ("mod.txt", b"new\nshared\n".as_slice()),
            ("added.txt", b"added\n".as_slice()),
            ("bin.dat", b"a\0c".as_slice()),
        ],
    );
    let git_files = git_no_index_structural(old_dir.path(), new_dir.path());

    assert_eq!(native_files, git_files);
}

fn structural_files(files: &[Value]) -> BTreeMap<String, (String, Option<u64>, Option<u64>)> {
    files
        .iter()
        .map(|file| {
            (
                file["path"].as_str().unwrap().to_string(),
                (
                    file["status"].as_str().unwrap().to_string(),
                    file["insertions"].as_u64(),
                    file["deletions"].as_u64(),
                ),
            )
        })
        .collect()
}

fn write_oracle_tree(root: &Path, files: &[(&str, &[u8])]) {
    for (path, bytes) in files {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, bytes).unwrap();
    }
}

fn git_no_index_structural(
    old_dir: &Path,
    new_dir: &Path,
) -> BTreeMap<String, (String, Option<u64>, Option<u64>)> {
    let name_status = git_diff_no_index(old_dir, new_dir, &["--name-status"]);
    let numstat = git_diff_no_index(old_dir, new_dir, &["--numstat"]);
    let mut counts = BTreeMap::new();
    for line in numstat.lines() {
        let mut parts = line.split('\t');
        let (Some(ins), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        counts.insert(
            oracle_path(path),
            (
                if ins == "-" { None } else { ins.parse().ok() },
                if del == "-" { None } else { del.parse().ok() },
            ),
        );
    }

    let mut out = BTreeMap::new();
    for line in name_status.lines() {
        let mut parts = line.split('\t');
        let (Some(status), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        let path = oracle_path(path);
        let (insertions, deletions) = counts.remove(&path).unwrap_or((None, None));
        out.insert(path, (status.to_string(), insertions, deletions));
    }
    out
}

fn git_diff_no_index(old_dir: &Path, new_dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["diff", "--no-index", "--diff-algorithm=patience"])
        .args(args)
        .arg(old_dir)
        .arg(new_dir)
        .output()
        .expect("run git diff --no-index");
    assert!(
        output.status.success() || output.status.code() == Some(1),
        "git diff --no-index failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 git diff output")
}

fn oracle_path(path: &str) -> String {
    let normalized = if let Some((left, right)) = path.split_once("=>") {
        let right = right.trim().trim_end_matches('}');
        if right == "dev/null" {
            left.trim().trim_start_matches('{')
        } else {
            right
        }
    } else {
        path
    };
    Path::new(normalized)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}
