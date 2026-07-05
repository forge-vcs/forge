#![allow(dead_code)]

use crate::common::{forge_in, TestRepo};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Stdio;
use std::thread;

pub(crate) fn json(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json envelope")
}

pub(crate) fn assert_no_structural_doctor_findings(doctor: &Value, context: &str) {
    for field in [
        "native_history_issues",
        "tampered_rows",
        "ledger_view_issues",
    ] {
        assert!(
            doctor["data"][field]
                .as_array()
                .unwrap_or_else(|| panic!("{field} array"))
                .is_empty(),
            "{context} must not report {field}: {doctor}"
        );
    }
}

pub(crate) fn native_accepted_lifecycle(repo: &TestRepo) {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "start",
            "sync manifest lifecycle",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("sync.txt"), "sync\n").expect("write sync feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
    repo.forge().args(["--json", "accept"]).assert().success();
}

pub(crate) fn native_checked_proposal(repo: &TestRepo) {
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();
    repo.forge()
        .args([
            "--json",
            "start",
            "sync trust boundary",
            "--require",
            "sh -c true",
        ])
        .assert()
        .success();
    std::fs::write(repo.path().join("peer-trust.txt"), "peer trust\n").expect("write feature");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
}

pub(crate) fn native_accept_file_change(repo: &TestRepo, intent: &str, path: &str, contents: &str) {
    native_accept_file_change_in(repo.path(), intent, path, contents);
}

pub(crate) fn native_accept_file_change_in(
    repo_path: &std::path::Path,
    intent: &str,
    path: &str,
    contents: &str,
) {
    forge_in(repo_path)
        .args(["--json", "start", intent, "--require", "sh -c true"])
        .assert()
        .success();
    std::fs::write(repo_path.join(path), contents).expect("write native change");
    forge_in(repo_path)
        .args(["--json", "save"])
        .assert()
        .success();
    forge_in(repo_path)
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    forge_in(repo_path)
        .args(["--json", "propose"])
        .assert()
        .success();
    forge_in(repo_path)
        .args(["--json", "check"])
        .assert()
        .success();
    forge_in(repo_path)
        .args(["--json", "accept"])
        .assert()
        .success();
}

pub(crate) fn export_native_head(repo_path: &std::path::Path, file_name: &str) -> Value {
    let output_dir = tempfile::tempdir().expect("native head export dir");
    let output_path = output_dir.path().join(file_name);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).expect("native head export parent");
    }
    json(
        forge_in(repo_path)
            .args([
                "--json",
                "sync",
                "export",
                "--output",
                output_path.to_str().expect("utf8 export path"),
            ])
            .assert()
            .success(),
    )
}

pub(crate) fn checkout_current_native_head(repo_path: &std::path::Path, file_name: &str) -> Value {
    let exported = export_native_head(repo_path, file_name);
    let head = exported["data"]["native_head"]
        .as_str()
        .expect("native head");
    forge_in(repo_path)
        .args(["--json", "checkout", head])
        .assert()
        .success();
    exported
}

pub(crate) fn expected_content_ref_for_test(repo_path: &std::path::Path) -> Option<String> {
    let conn = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    conn.query_row(
        "SELECT expected_content_ref FROM current_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )
    .expect("current state row")
}

pub(crate) fn set_expected_content_ref_for_test(repo_path: &std::path::Path, content_ref: &str) {
    let conn = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    conn.execute(
        "UPDATE current_state SET expected_content_ref = ?1 WHERE singleton = 1",
        params![content_ref],
    )
    .expect("set expected content ref");
}

pub(crate) fn manifest_table_rows<'a>(manifest: &'a Value, table: &str) -> &'a Vec<Value> {
    manifest["ledger_rows"]
        .as_array()
        .expect("ledger rows")
        .iter()
        .find(|rows| rows["table"] == table)
        .unwrap_or_else(|| panic!("manifest table {table}"))
        .get("rows")
        .and_then(Value::as_array)
        .expect("table rows")
}

pub(crate) fn manifest_ledger_count(manifest: &Value, table: &str) -> i64 {
    manifest["ledger_counts"]
        .as_array()
        .expect("ledger counts")
        .iter()
        .find(|count| count["table"] == table)
        .unwrap_or_else(|| panic!("manifest count {table}"))
        .get("rows")
        .and_then(Value::as_i64)
        .expect("count rows")
}

pub(crate) fn decode_hex_for_test(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "hex payload must have even length");
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("payload hex byte"))
        .collect()
}

pub(crate) fn decoded_native_payloads(manifest: &Value) -> Vec<String> {
    manifest["native_payloads"]
        .as_array()
        .expect("native payloads")
        .iter()
        .map(|payload| {
            let bytes = decode_hex_for_test(payload["payload_hex"].as_str().expect("payload hex"));
            String::from_utf8_lossy(&bytes).into_owned()
        })
        .collect()
}

pub(crate) fn file_url_for_test(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

pub(crate) fn ssh_url_for_test(path: &std::path::Path) -> String {
    format!("ssh://example.test{}", path.display())
}

pub(crate) fn fake_ssh_command() -> tempfile::TempDir {
    let bin = tempfile::tempdir().expect("fake ssh dir");
    let path = bin.path().join("ssh");
    std::fs::write(
        &path,
        "#!/bin/sh\nset -eu\n_host=\"$1\"\nshift\nexec sh -lc \"$1\"\n",
    )
    .expect("write fake ssh");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .expect("fake ssh metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake ssh");
    }
    bin
}

pub(crate) struct TestHttpSyncServer {
    pub(crate) url: String,
    _handle: thread::JoinHandle<()>,
}

impl TestHttpSyncServer {
    pub(crate) fn start(repo_path: &Path, request_count: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind sync http server");
        let url = format!("http://{}", listener.local_addr().expect("sync http addr"));
        let repo_path = repo_path.to_path_buf();
        let forge_bin = assert_cmd::cargo::cargo_bin("forge");
        let handle = thread::spawn(move || {
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().expect("accept sync http request");
                handle_sync_http_request(&mut stream, &repo_path, &forge_bin);
            }
        });
        Self {
            url,
            _handle: handle,
        }
    }
}

pub(crate) fn handle_sync_http_request(stream: &mut TcpStream, repo_path: &Path, forge_bin: &Path) {
    let (path, body) = read_http_request(stream);
    let body_json: Value = serde_json::from_slice(&body).expect("sync http request json");
    let output = match path.as_str() {
        "/sync/serve/export" => {
            let since_manifest = body_json
                .get("since_manifest")
                .filter(|value| !value.is_null())
                .map(|value| serde_json::to_vec(value).expect("since manifest json"));
            run_forge_sync_serve(
                repo_path,
                forge_bin,
                if since_manifest.is_some() {
                    &["--json", "sync", "serve", "export", "--stdin-since"][..]
                } else {
                    &["--json", "sync", "serve", "export"][..]
                },
                since_manifest.as_deref(),
            )
        }
        "/sync/serve/receive" => {
            let manifest = serde_json::to_vec(
                body_json
                    .get("manifest")
                    .expect("sync http receive manifest"),
            )
            .expect("manifest json");
            let remote_label = body_json
                .get("remote_label")
                .and_then(Value::as_str)
                .unwrap_or("<http-test>");
            run_forge_sync_serve(
                repo_path,
                forge_bin,
                &[
                    "--json",
                    "sync",
                    "serve",
                    "receive",
                    "--stdin-manifest",
                    "--remote-label",
                    remote_label,
                ],
                Some(&manifest),
            )
        }
        other => panic!("unexpected sync http path {other}"),
    };
    let body = if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .expect("write sync http response headers");
    stream
        .write_all(&body)
        .expect("write sync http response body");
}

pub(crate) fn run_forge_sync_serve(
    repo_path: &Path,
    forge_bin: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> std::process::Output {
    let mut child = std::process::Command::new(forge_bin)
        .current_dir(repo_path)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn forge sync serve");
    if let Some(stdin) = stdin {
        child
            .stdin
            .as_mut()
            .expect("forge sync serve stdin")
            .write_all(stdin)
            .expect("write forge sync serve stdin");
    }
    drop(child.stdin.take());
    child.wait_with_output().expect("wait forge sync serve")
}

pub(crate) fn read_http_request(stream: &mut TcpStream) -> (String, Vec<u8>) {
    let mut headers = Vec::new();
    let mut byte = [0u8; 1];
    while !headers.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("read http header");
        headers.push(byte[0]);
    }
    let header_text = String::from_utf8(headers).expect("http headers utf8");
    let mut lines = header_text.lines();
    let request_line = lines.next().expect("http request line");
    let path = request_line
        .split_whitespace()
        .nth(1)
        .expect("http request path")
        .to_string();
    let content_length = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content-length usize"))
        })
        .unwrap_or(0);
    let mut body = vec![0; content_length];
    stream.read_exact(&mut body).expect("read http body");
    (path, body)
}

pub(crate) fn cloned_peer_from_bundle(bundle_path: &std::path::Path) -> tempfile::TempDir {
    let peer_dir = tempfile::tempdir().expect("peer dir");
    forge_in(peer_dir.path())
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 bundle path"),
        ])
        .assert()
        .success();
    peer_dir
}

pub(crate) fn assert_single_native_sync_conflict(repo_path: &std::path::Path, context: &str) {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    let conflicts = list["data"]["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "expected one sync conflict: {list}");
    assert_eq!(conflicts[0]["context"], context);
    assert_eq!(conflicts[0]["resolver_backend"], "native_merge");
    assert_eq!(conflicts[0]["status"], "unresolved");
    assert_eq!(conflicts[0]["path_conflict_count"], 1);

    let conflict_id = conflicts[0]["conflict_set_id"]
        .as_str()
        .expect("conflict id");
    let show = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "show", conflict_id])
            .assert()
            .success(),
    );
    assert_eq!(show["data"]["conflict"]["context"], context);
    assert_eq!(show["data"]["conflict"]["resolver_backend"], "native_merge");
    for field in ["base_content_ref", "ours_content_ref", "theirs_content_ref"] {
        let content_ref = show["data"]["conflict"][field]
            .as_str()
            .unwrap_or_else(|| panic!("{field} must be present"));
        assert!(
            content_ref.starts_with("forge-tree:"),
            "{field} should be a forge-tree content ref: {content_ref}"
        );
    }
    let path_conflicts = show["data"]["path_conflicts"]
        .as_array()
        .expect("path conflicts");
    assert_eq!(path_conflicts.len(), 1);
    assert_eq!(path_conflicts[0]["kind"], "content");
    assert_eq!(path_conflicts[0]["status"], "unresolved");
}

pub(crate) fn single_native_sync_conflict_content_refs(
    repo_path: &std::path::Path,
    context: &str,
) -> Vec<String> {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    let conflicts = list["data"]["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "expected one sync conflict: {list}");
    assert_eq!(conflicts[0]["context"], context);
    let conflict_id = conflicts[0]["conflict_set_id"]
        .as_str()
        .expect("conflict id");
    let show = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "show", conflict_id])
            .assert()
            .success(),
    );
    ["base_content_ref", "ours_content_ref", "theirs_content_ref"]
        .iter()
        .map(|field| {
            show["data"]["conflict"][field]
                .as_str()
                .unwrap_or_else(|| panic!("{field} must be present"))
                .to_string()
        })
        .collect()
}

pub(crate) fn single_conflict_id(repo_path: &std::path::Path, context: &str) -> String {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    let conflicts = list["data"]["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1, "expected one sync conflict: {list}");
    assert_eq!(conflicts[0]["context"], context);
    conflicts[0]["conflict_set_id"]
        .as_str()
        .expect("conflict id")
        .to_string()
}

pub(crate) fn conflict_count(repo_path: &std::path::Path) -> usize {
    let list = json(
        forge_in(repo_path)
            .args(["--json", "conflict", "list"])
            .assert()
            .success(),
    );
    list["data"]["conflicts"]
        .as_array()
        .expect("conflicts")
        .len()
}

pub(crate) fn operation_count_for_request_id(repo_path: &std::path::Path, request_id: &str) -> i64 {
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE request_id = ?1",
            [request_id],
            |row| row.get(0),
        )
        .expect("count request-id operations")
}

pub(crate) fn operation_count_for_request_id_and_kind(
    repo_path: &std::path::Path,
    request_id: &str,
    kind: &str,
) -> i64 {
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .query_row(
            "SELECT COUNT(*) FROM operations WHERE request_id = ?1 AND kind = ?2",
            params![request_id, kind],
            |row| row.get(0),
        )
        .expect("count request-id operations by kind")
}

pub(crate) fn skew_latest_decision_timestamp_for_test(
    repo_path: &std::path::Path,
    created_at_ms: i64,
) {
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .execute(
            "UPDATE decisions
             SET created_at_ms = ?1
             WHERE rowid = (SELECT rowid FROM decisions ORDER BY rowid DESC LIMIT 1)",
            [created_at_ms],
        )
        .expect("skew latest decision timestamp");
}

pub(crate) fn poison_latest_decision_commit_for_test(
    repo_path: &std::path::Path,
    commit_id: &str,
    created_at_ms: i64,
) {
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .execute(
            "UPDATE decisions
             SET commit_id = ?1, created_at_ms = ?2
             WHERE rowid = (SELECT rowid FROM decisions ORDER BY rowid DESC LIMIT 1)",
            params![commit_id, created_at_ms],
        )
        .expect("poison latest decision commit");
}

pub(crate) fn mark_conflict_resolved_for_test(repo_path: &std::path::Path, conflict_id: &str) {
    // This helper only flips the status fields needed to exercise sync-conflict
    // dedup policy. It intentionally does not simulate a real resolution
    // operation; the original conflict-creation operation and integrity chain
    // remain intact for the dedup query under test.
    let connection = Connection::open(repo_path.join(".forge/forge.db")).expect("open forge db");
    connection
        .execute(
            "UPDATE conflict_sets SET status = 'resolved' WHERE id = ?1",
            [conflict_id],
        )
        .expect("mark conflict set resolved");
    connection
        .execute(
            "UPDATE path_conflicts SET status = 'resolved' WHERE conflict_set_id = ?1",
            [conflict_id],
        )
        .expect("mark path conflicts resolved");
}

pub(crate) fn assert_gc_keeps_content_refs_reachable(
    repo_path: &std::path::Path,
    content_refs: &[String],
) {
    let gc = json(
        forge_in(repo_path)
            .args(["--json", "gc", "--dry-run"])
            .assert()
            .success(),
    );
    let unreachable = gc["data"]["unreachable_native_objects"]
        .as_array()
        .expect("unreachable native objects");
    for content_ref in content_refs {
        let tree_id = content_ref
            .strip_prefix("forge-tree:")
            .unwrap_or_else(|| panic!("native content ref expected: {content_ref}"));
        assert!(
            !unreachable
                .iter()
                .any(|value| value.as_str() == Some(tree_id)),
            "gc must keep conflict content ref reachable: {content_ref}; report: {gc}"
        );
    }
}

pub(crate) fn record_sync_divergence_conflict(repo_path: &std::path::Path) {
    let base = tempfile::tempdir().expect("sync divergence base dir");
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let bundle_path = base.path().join("base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            bundle_path.to_str().expect("utf8 bundle path"),
        ])
        .assert()
        .success();
    forge_in(repo_path)
        .args([
            "--json",
            "sync",
            "clone",
            bundle_path.to_str().expect("utf8 bundle path"),
        ])
        .assert()
        .success();

    native_accept_file_change(&source, "source divergence", "diverge.txt", "source\n");
    native_accept_file_change_in(repo_path, "peer divergence", "diverge.txt", "peer\n");
    let conflicted = json(
        forge_in(repo_path)
            .args([
                "--json",
                "sync",
                "fetch",
                source.path().to_str().expect("utf8 source path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(conflicted["data"]["merged"], false);
    assert!(conflicted["data"]["conflict_set_id"].as_str().is_some());
    assert_single_native_sync_conflict(repo_path, "sync_fetch_divergence");
}

pub(crate) fn forge_json(repo_path: &std::path::Path, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    json(forge_in(repo_path).args(full).assert().success())
}

pub(crate) fn record_native_merge_conflict(repo: &TestRepo) -> String {
    std::fs::write(repo.path().join("README.md"), "one\ntwo\nthree\n").expect("seed readme");
    forge_json(repo.path(), &["init", "--content-backend", "native"]);
    let started = forge_json(repo.path(), &["start", "sync native merge conflict"]);
    let intent = started["data"]["intent_id"].as_str().expect("intent id");
    let attempt_a = started["data"]["attempt_id"].as_str().expect("attempt a");
    let started_b = forge_json(repo.path(), &["attempt", "start", "--intent", intent]);
    let attempt_b = started_b["data"]["attempt_id"].as_str().expect("attempt b");

    forge_json(repo.path(), &["attempt", "attach", attempt_a]);
    std::fs::write(repo.path().join("README.md"), "one\nOURS\nthree\n").expect("ours");
    forge_json(repo.path(), &["save", "--attempt", attempt_a]);
    forge_json(
        repo.path(),
        &["run", "--attempt", attempt_a, "--", "sh", "-c", "true"],
    );
    let proposed_a = forge_json(repo.path(), &["propose", "--attempt", attempt_a]);
    let proposal_a = proposed_a["data"]["proposal_id"]
        .as_str()
        .expect("proposal a");
    forge_json(repo.path(), &["check", "--attempt", attempt_a]);

    forge_json(repo.path(), &["attempt", "attach", attempt_b]);
    std::fs::write(repo.path().join("README.md"), "one\nTHEIRS\nthree\n").expect("theirs");
    forge_json(repo.path(), &["save", "--attempt", attempt_b]);
    forge_json(
        repo.path(),
        &["run", "--attempt", attempt_b, "--", "sh", "-c", "true"],
    );
    let proposed_b = forge_json(repo.path(), &["propose", "--attempt", attempt_b]);
    let proposal_b = proposed_b["data"]["proposal_id"]
        .as_str()
        .expect("proposal b");
    forge_json(repo.path(), &["check", "--attempt", attempt_b]);

    forge_json(
        repo.path(),
        &["accept", "--attempt", attempt_a, "--proposal", proposal_a],
    );
    let merged = forge_json(repo.path(), &["merge", "--proposal", proposal_b]);
    assert_eq!(merged["data"]["merged"], false);
    let conflict_id = merged["data"]["conflict_set_id"]
        .as_str()
        .expect("conflict id")
        .to_string();
    let shown = forge_json(repo.path(), &["conflict", "show", &conflict_id]);
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
    conflict_id
}

pub(crate) fn source_with_conflict_after_base_export(
) -> (TestRepo, tempfile::TempDir, std::path::PathBuf) {
    let source = TestRepo::new_git();
    native_accepted_lifecycle(&source);
    let base_dir = tempfile::tempdir().expect("peer base dir");
    let base_bundle = base_dir.path().join("peer-base.json");
    source
        .forge()
        .args([
            "--json",
            "sync",
            "export",
            "--output",
            base_bundle.to_str().expect("utf8 peer base bundle"),
        ])
        .assert()
        .success();

    let divergence_peer = cloned_peer_from_bundle(&base_bundle);
    native_accept_file_change(
        &source,
        "source conflict row",
        "conflict-row.txt",
        "source\n",
    );
    native_accept_file_change_in(
        divergence_peer.path(),
        "peer conflict row",
        "conflict-row.txt",
        "peer\n",
    );
    let conflicted = json(
        source
            .forge()
            .args([
                "--json",
                "sync",
                "fetch",
                divergence_peer.path().to_str().expect("utf8 peer path"),
            ])
            .assert()
            .success(),
    );
    assert_eq!(conflicted["data"]["merged"], false);
    assert_single_native_sync_conflict(source.path(), "sync_fetch_divergence");
    (source, base_dir, base_bundle)
}
