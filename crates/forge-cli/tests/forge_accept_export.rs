mod common;

use common::TestRepo;
use rusqlite::Connection;
use serde_json::Value;

fn json_output(assert: assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&assert.get_output().stdout).expect("valid json")
}

fn git(cwd: &std::path::Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout).unwrap()
}

fn prepare_proposal(repo: &TestRepo) {
    prepare_proposal_with_init(repo, &["--json", "init"]);
}

fn prepare_native_proposal(repo: &TestRepo) {
    prepare_proposal_with_init(repo, &["--json", "init", "--content-backend", "native"]);
}

fn prepare_proposal_with_init(repo: &TestRepo, init_args: &[&str]) {
    repo.forge().args(init_args).assert().success();
    repo.forge()
        .args(["--json", "start", "export proposal"])
        .assert()
        .success();
    std::fs::write(repo.path().join("README.md"), "exported\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    repo.forge().args(["--json", "propose"]).assert().success();
    repo.forge().args(["--json", "check"]).assert().success();
}

fn decision_signature_fingerprint(repo: &TestRepo) -> String {
    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    connection
        .query_row(
            "SELECT key_fingerprint FROM ledger_signatures WHERE subject_kind = 'decision'",
            [],
            |row| row.get(0),
        )
        .expect("decision signature fingerprint")
}

#[test]
fn accept_records_decision_and_export_branch_leaves_current_branch() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    let current_branch = git(repo.path(), &["branch", "--show-current"]);

    let accepted = json_output(repo.forge().args(["--json", "accept"]).assert().success());
    assert_eq!(accepted["data"]["decision"], "accepted");
    assert_eq!(
        git(repo.path(), &["branch", "--show-current"]),
        current_branch
    );

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/exported"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/exported");
    assert_eq!(
        git(repo.path(), &["branch", "--show-current"]),
        current_branch
    );
    git(repo.path(), &["rev-parse", "--verify", "forge/exported"]);

    let overwrite = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/exported"])
            .assert()
            .failure(),
    );
    assert_eq!(overwrite["errors"][0]["code"], "BRANCH_EXISTS");
}

#[test]
fn reject_prevents_export() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "reject"]).assert().success();

    let output = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/rejected"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "REJECTED");
}

#[test]
fn export_fails_when_base_is_stale() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();

    std::fs::write(repo.path().join("stale.txt"), "move head\n").expect("write stale file");
    git(repo.path(), &["add", "stale.txt"]);
    git(repo.path(), &["commit", "-m", "move target"]);

    let output = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/stale"])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "STALE_BASE");
    assert!(output["operation_id"].as_str().unwrap().starts_with("op_"));
}

#[test]
fn export_reconciles_existing_branch_when_it_matches_expected_commit() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();
    let show = json_output(repo.forge().args(["--json", "show"]).assert().success());
    let proposal = &show["data"]["latest_proposal"];
    let base_head = proposal["base_head"].as_str().unwrap();
    let tree = proposal["content_ref"]
        .as_str()
        .unwrap()
        .strip_prefix("git-tree:")
        .unwrap();
    let commit = git(
        repo.path(),
        &[
            "commit-tree",
            tree,
            "-p",
            base_head,
            "-m",
            "Forge accepted proposal",
        ],
    );
    let commit = commit.trim();
    git(
        repo.path(),
        &["update-ref", "refs/heads/forge/recovered", commit],
    );

    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/recovered"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/recovered");
    assert_eq!(exported["data"]["commit_id"], commit);
}

#[test]
fn accept_fails_when_base_is_stale() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    std::fs::write(repo.path().join("stale-before-accept.txt"), "move head\n")
        .expect("write stale file");
    git(repo.path(), &["add", "stale-before-accept.txt"]);
    git(repo.path(), &["commit", "-m", "move target before accept"]);

    let output = json_output(repo.forge().args(["--json", "accept"]).assert().failure());
    assert_eq!(output["errors"][0]["code"], "STALE_BASE");
}

#[test]
fn export_requires_acceptance_for_exact_latest_revision() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();

    std::fs::write(repo.path().join("README.md"), "new proposal\n").expect("write readme");
    repo.forge().args(["--json", "save"]).assert().success();
    let latest = json_output(repo.forge().args(["--json", "propose"]).assert().success());

    let output = json_output(
        repo.forge()
            .args([
                "--json",
                "export",
                "branch",
                "--proposal",
                latest["data"]["proposal_id"].as_str().unwrap(),
                "forge/not-accepted-latest",
            ])
            .assert()
            .failure(),
    );
    assert_eq!(output["errors"][0]["code"], "NOT_ACCEPTED");
}

#[test]
fn export_carries_a_structured_provenance_trailer() {
    // NER-137 U5: the published commit replaces the constant "Forge accepted proposal"
    // message with a structured Forge-* trailer; exactly one digest line.
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge()
        .args(["--json", "accept", "--actor", "alice"])
        .assert()
        .success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/with-trailer"])
        .assert()
        .success();

    let message = git(
        repo.path(),
        &["show", "-s", "--format=%B", "forge/with-trailer"],
    );
    assert!(message.contains("Forge-Proposal-Id: "), "{message}");
    assert!(message.contains("Forge-Proposal-Revision-Id: "));
    assert!(message.contains("Forge-Decision-Actor: alice"));
    assert!(message.contains("Forge-Gates: "));
    let fingerprint = decision_signature_fingerprint(&repo);
    assert!(message.contains(&format!("Forge-Local-Signature-Fingerprint: {fingerprint}")));

    let digest_lines: Vec<&str> = message
        .lines()
        .filter(|l| l.starts_with("Forge-Provenance-Digest: "))
        .collect();
    assert_eq!(digest_lines.len(), 1, "exactly one digest line");
    let digest = digest_lines[0]
        .trim_start_matches("Forge-Provenance-Digest: ")
        .trim();
    assert_eq!(digest.len(), 64, "64-hex provenance digest");
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(fingerprint.len(), 32, "32-hex key fingerprint");
    assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));
    // The split that an earlier draft carried must NOT appear.
    assert!(!message.contains("Forge-Evidence-Digest"));
    assert!(!message.contains("Forge-Publication-Digest"));
}

#[test]
fn verify_branch_confirms_a_clean_provenance_trailer() {
    // NER-137 U6 (exit criterion): the published trailer recomputes from the ledger.
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/verified"])
        .assert()
        .success();

    let out = json_output(
        repo.forge()
            .args(["--json", "export", "verify-branch", "forge/verified"])
            .assert()
            .success(),
    );
    assert_eq!(out["data"]["verified"], true);
    let digest = out["data"]["provenance_digest"].as_str().unwrap();
    let fingerprint = out["data"]["local_signature_fingerprint"]
        .as_str()
        .expect("signed export verification returns fingerprint");
    assert_eq!(fingerprint, decision_signature_fingerprint(&repo));
    // The verified digest is the one the commit carries.
    let message = git(
        repo.path(),
        &["show", "-s", "--format=%B", "forge/verified"],
    );
    assert!(message.contains(&format!("Forge-Provenance-Digest: {digest}")));
    assert!(message.contains(&format!("Forge-Local-Signature-Fingerprint: {fingerprint}")));
}

#[test]
fn verify_branch_fails_closed_on_a_tampered_deciding_row() {
    // NER-137 U6: a deciding evidence row tampered after export → EVIDENCE_TAMPERED
    // (verify-branch re-verifies the deciding evidence via build_publication_trailer).
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/tampered"])
        .assert()
        .success();

    let connection = Connection::open(repo.path().join(".forge/forge.db")).expect("open db");
    connection
        .execute(
            "UPDATE evidence SET exit_code = 99 WHERE command = 'sh'",
            [],
        )
        .expect("tamper exit_code");

    let out = json_output(
        repo.forge()
            .args(["--json", "export", "verify-branch", "forge/tampered"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "EVIDENCE_TAMPERED");
}

#[test]
fn verify_branch_on_a_non_forge_commit_is_a_typed_missing_trailer_error() {
    // NER-137 code-review: an agent gating CI must tell "not a Forge artifact" from a
    // mismatch/tamper — a plain git commit (no Forge-* trailer) → MISSING_PROVENANCE_TRAILER,
    // not COMMAND_FAILED.
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    // HEAD is the repo's initial git commit — it carries no Forge provenance trailer.
    let out = json_output(
        repo.forge()
            .args(["--json", "export", "verify-branch", "HEAD"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "MISSING_PROVENANCE_TRAILER");
    assert_eq!(
        out["errors"][0]["details"]["missing_field"],
        "proposal_revision_id"
    );
}

#[test]
fn verify_branch_reports_provenance_mismatch_for_a_rewritten_trailer() {
    // NER-137 U6: a commit whose Forge-Provenance-Digest was rewritten (without a
    // matching ledger) → PROVENANCE_MISMATCH, fail-closed.
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/genuine"])
        .assert()
        .success();

    // Forge a sibling commit with the real revision id but a bogus digest.
    let message = git(repo.path(), &["show", "-s", "--format=%B", "forge/genuine"]);
    let zeros = "0".repeat(64);
    let forged: String = message
        .lines()
        .map(|line| {
            if line.starts_with("Forge-Provenance-Digest: ") {
                format!("Forge-Provenance-Digest: {zeros}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let tree = git(repo.path(), &["show", "-s", "--format=%T", "forge/genuine"]);
    let parent = git(repo.path(), &["show", "-s", "--format=%P", "forge/genuine"]);
    let commit = git(
        repo.path(),
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            parent.trim(),
            "-m",
            &forged,
        ],
    );
    git(
        repo.path(),
        &["update-ref", "refs/heads/forge/forged", commit.trim()],
    );

    let out = json_output(
        repo.forge()
            .args(["--json", "export", "verify-branch", "forge/forged"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "PROVENANCE_MISMATCH");
    assert_eq!(out["errors"][0]["details"]["published_digest"], zeros);
}

#[test]
fn verify_branch_reports_local_signature_mismatch_for_a_rewritten_fingerprint() {
    let repo = TestRepo::new_git();
    prepare_proposal(&repo);
    repo.forge().args(["--json", "accept"]).assert().success();
    repo.forge()
        .args(["--json", "export", "branch", "forge/signed"])
        .assert()
        .success();

    let message = git(repo.path(), &["show", "-s", "--format=%B", "forge/signed"]);
    let bogus = "f".repeat(32);
    let forged: String = message
        .lines()
        .map(|line| {
            if line.starts_with("Forge-Local-Signature-Fingerprint: ") {
                format!("Forge-Local-Signature-Fingerprint: {bogus}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let tree = git(repo.path(), &["show", "-s", "--format=%T", "forge/signed"]);
    let parent = git(repo.path(), &["show", "-s", "--format=%P", "forge/signed"]);
    let commit = git(
        repo.path(),
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            parent.trim(),
            "-m",
            &forged,
        ],
    );
    git(
        repo.path(),
        &["branch", "forge/signed-forged", commit.trim()],
    );

    let out = json_output(
        repo.forge()
            .args(["--json", "export", "verify-branch", "forge/signed-forged"])
            .assert()
            .failure(),
    );
    assert_eq!(out["errors"][0]["code"], "LOCAL_SIGNATURE_MISMATCH");
    assert_eq!(out["errors"][0]["details"]["published_fingerprint"], bogus);
}

#[test]
fn native_accepted_proposal_exports_to_git_branch() {
    let repo = TestRepo::new_git();
    prepare_native_proposal(&repo);
    let show = json_output(repo.forge().args(["--json", "show"]).assert().success());
    assert!(show["data"]["latest_proposal"]["content_ref"]
        .as_str()
        .unwrap()
        .starts_with("forge-tree:"));
    let current_branch = git(repo.path(), &["branch", "--show-current"]);

    repo.forge().args(["--json", "accept"]).assert().success();
    let exported = json_output(
        repo.forge()
            .args(["--json", "export", "branch", "forge/native-exported"])
            .assert()
            .success(),
    );
    assert_eq!(exported["data"]["branch_name"], "forge/native-exported");
    assert_eq!(
        git(repo.path(), &["branch", "--show-current"]),
        current_branch
    );
    assert_eq!(
        git(repo.path(), &["show", "forge/native-exported:README.md"]),
        "exported\n"
    );
}

/// NER-260: `accept --proposal <id>` must resolve the proposal GLOBALLY by id (like the
/// export paths already do) and accept it under its OWNING attempt — even when a
/// DIFFERENT attempt is attached. Previously the explicit-`--proposal` branch of
/// `resolve_proposal` cross-checked the proposal against the caller's resolved
/// (attached) attempt and rejected with UNKNOWN_PROPOSAL, so accepting attempt A's
/// proposal while attempt B was attached failed even though the proposal existed with a
/// passed check. This drives two competing attempts on one intent (native backend),
/// attaches B, then accepts A's proposal by id and asserts HEAD advanced to a commit
/// whose tree materializes A's content (not B's).
#[test]
fn accept_resolves_explicit_proposal_id_across_attached_attempt() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    // Attempt A (attached on start): write distinct content, then propose + check green.
    let started = json_output(
        repo.forge()
            .args(["--json", "start", "compete"])
            .assert()
            .success(),
    );
    let intent_id = started["data"]["intent_id"].as_str().unwrap().to_string();
    let attempt_a = started["data"]["attempt_id"].as_str().unwrap().to_string();
    std::fs::write(repo.path().join("feature_a.txt"), "from-attempt-a\n").expect("write a");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed_a = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_a = proposed_a["data"]["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    let content_ref_a = proposed_a["data"]["content_ref"]
        .as_str()
        .unwrap()
        .to_string();
    let tree_a = content_ref_a
        .strip_prefix("forge-tree:")
        .expect("native proposal content_ref is a forge-tree ref")
        .to_string();
    repo.forge().args(["--json", "check"]).assert().success();

    // Attempt B on the SAME intent. `attempt start` does not attach; `attempt attach`
    // materializes B's base (reverting the worktree off A's content), then B proposes +
    // checks its own distinct content.
    let second = json_output(
        repo.forge()
            .args(["--json", "attempt", "start", "--intent", &intent_id])
            .assert()
            .success(),
    );
    let attempt_b = second["data"]["attempt_id"].as_str().unwrap().to_string();
    assert_ne!(attempt_a, attempt_b);
    repo.forge()
        .args(["--json", "attempt", "attach", &attempt_b])
        .assert()
        .success();
    // Attaching B reverted the worktree to the genesis base, dropping A's file.
    assert!(!repo.path().join("feature_a.txt").exists());
    std::fs::write(repo.path().join("feature_b.txt"), "from-attempt-b\n").expect("write b");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed_b = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_b = proposed_b["data"]["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(proposal_a, proposal_b);
    repo.forge().args(["--json", "check"]).assert().success();

    // HEAD is still the genesis base (no accept yet); B is the attached attempt.
    let genesis = head(repo.path()).expect("genesis HEAD after attach");

    // The core assertion: accept A's proposal BY ID while B is attached. This must
    // succeed (previously returned UNKNOWN_PROPOSAL).
    let accepted = json_output(
        repo.forge()
            .args(["--json", "accept", "--proposal", &proposal_a])
            .assert()
            .success(),
    );
    assert_eq!(accepted["data"]["decision"], "accepted");
    let commit_id = accepted["data"]["commit_id"]
        .as_str()
        .expect("native accept surfaces commit_id")
        .to_string();

    // HEAD advanced off genesis to A's commit, and that commit materializes A's tree —
    // proving accept used A's proposal/owning attempt, not attached B's content.
    assert_ne!(commit_id, genesis);
    assert_eq!(head(repo.path()).as_deref(), Some(commit_id.as_str()));
    let store = forge_content_native::NativeObjectStore::new(repo.path());
    let accepted_commit = store
        .read_commit(&forge_content_native::ObjectId::parse(&commit_id).unwrap())
        .expect("read accepted commit");
    assert_eq!(
        accepted_commit.tree, tree_a,
        "accepted commit must carry attempt A's tree, not attached attempt B's"
    );

    // Regression: a genuinely non-existent proposal id still errors UNKNOWN_PROPOSAL
    // (the global-by-id lookup's None path is preserved).
    let unknown = json_output(
        repo.forge()
            .args(["--json", "accept", "--proposal", "proposal_does_not_exist"])
            .assert()
            .failure(),
    );
    assert_eq!(unknown["errors"][0]["code"], "UNKNOWN_PROPOSAL");
}

/// NER-260 boundary: `accept --proposal <id>` resolves the proposal GLOBALLY by id —
/// across attempts AND intents — exactly like `export pr-body`. The intended contract is
/// that naming a proposal accepts THAT proposal under ITS OWN intent: the deciding gate is
/// the proposal's intent's spec and the native commit is stamped with the proposal's intent
/// id, NOT the caller's currently-attached intent. This test pins that cross-intent
/// behavior (previously unspecified/unverified) so a future regression that re-adds an
/// intent-scope guard — or, worse, silently accepts under the wrong intent — is caught.
#[test]
fn accept_proposal_id_from_unrelated_intent_commits_under_that_intent() {
    let repo = TestRepo::new_git();
    repo.forge()
        .args(["--json", "init", "--content-backend", "native"])
        .assert()
        .success();

    // Intent I1: start (attaches), propose + check green. Leave it UNACCEPTED so HEAD is
    // still genesis — the same base_head I2 will be proposed against.
    let started_one = json_output(
        repo.forge()
            .args(["--json", "start", "intent-one"])
            .assert()
            .success(),
    );
    let intent_one = started_one["data"]["intent_id"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::write(repo.path().join("one.txt"), "from-intent-one\n").expect("write one");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed_one = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_one = proposed_one["data"]["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    let tree_one = proposed_one["data"]["content_ref"]
        .as_str()
        .unwrap()
        .strip_prefix("forge-tree:")
        .expect("native content_ref is a forge-tree ref")
        .to_string();
    repo.forge().args(["--json", "check"]).assert().success();

    // Intent I2: a SEPARATE `start` mints a new intent + attaches a fresh attempt. It is now
    // the CURRENTLY-ATTACHED intent. Propose + check green so I2's proposal also has a
    // passing check and base_head == genesis (== I1's base).
    let started_two = json_output(
        repo.forge()
            .args(["--json", "start", "intent-two"])
            .assert()
            .success(),
    );
    let intent_two = started_two["data"]["intent_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(
        intent_one, intent_two,
        "second start mints a distinct intent"
    );
    // Both attempts were started from genesis (no accept ran), so they share base_head ==
    // genesis — exactly the condition under which the stale-base guard cannot distinguish
    // intents and the cross-intent accept must be governed by the proposal's own intent.
    std::fs::write(repo.path().join("two.txt"), "from-intent-two\n").expect("write two");
    repo.forge().args(["--json", "save"]).assert().success();
    repo.forge()
        .args(["--json", "run", "--", "sh", "-c", "true"])
        .assert()
        .success();
    let proposed_two = json_output(repo.forge().args(["--json", "propose"]).assert().success());
    let proposal_two = proposed_two["data"]["proposal_id"]
        .as_str()
        .unwrap()
        .to_string();
    let tree_two = proposed_two["data"]["content_ref"]
        .as_str()
        .unwrap()
        .strip_prefix("forge-tree:")
        .expect("native content_ref is a forge-tree ref")
        .to_string();
    repo.forge().args(["--json", "check"]).assert().success();

    assert_ne!(
        proposal_one, proposal_two,
        "the two intents' proposals differ"
    );
    assert_ne!(
        tree_one, tree_two,
        "the two intents propose different trees"
    );
    let genesis = head(repo.path()).expect("genesis HEAD before any accept");

    // The cross-intent boundary: I2 is the attached intent, but accept I1's proposal BY ID.
    // I1 and I2 share base_head == genesis, so the stale-base guard does NOT block this. The
    // accept must resolve I1's proposal globally and record the commit under I1's OWN intent
    // — NOT the attached I2 — proving `--proposal` is by-id (per its owning intent), exactly
    // like `export pr-body`, and that no caller silently stamps the attached intent.
    let accepted = json_output(
        repo.forge()
            .args(["--json", "accept", "--proposal", &proposal_one])
            .assert()
            .success(),
    );
    assert_eq!(accepted["data"]["decision"], "accepted");
    assert_eq!(
        accepted["data"]["proposal_id"], proposal_one,
        "accept must echo the by-id proposal it resolved"
    );
    let commit_id = accepted["data"]["commit_id"].as_str().unwrap().to_string();
    assert_ne!(commit_id, genesis);
    assert_eq!(head(repo.path()).as_deref(), Some(commit_id.as_str()));

    // The native commit carries I1's tree AND I1's intent id — proving the gate spec and
    // commit metadata followed the proposal's OWN intent, not the attached I2.
    let store = forge_content_native::NativeObjectStore::new(repo.path());
    let commit = store
        .read_commit(&forge_content_native::ObjectId::parse(&commit_id).unwrap())
        .expect("read accepted commit");
    assert_eq!(
        commit.tree, tree_one,
        "commit must carry I1's tree (the by-id proposal), not attached I2's"
    );
    assert_eq!(
        commit.intent_id.as_deref(),
        Some(intent_one.as_str()),
        "commit must be stamped with I1's intent (the proposal's owner), not attached I2's"
    );
    assert_ne!(
        commit.intent_id.as_deref(),
        Some(intent_two.as_str()),
        "the attached intent must NOT be the one recorded"
    );
}

fn head(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path.join(".forge/refs/HEAD"))
        .ok()
        .map(|raw| raw.trim().to_string())
}
