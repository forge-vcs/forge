//! The `forge schema` versioned machine contract (NER-133 U5).
//!
//! Emits a single `serde_json::Value` document describing the `forge.cli.v0`
//! contract: the envelope shape, the dispatched command set, and the FULL
//! error-code registry. The registry is *derived* from
//! [`forge_store::error_registry`] (which the store's drift-guard test pins to the
//! `ForgeError` enum), so a newly-added error variant automatically appears here
//! and cannot drift out of the published contract. The CLI-level codes that never
//! pass through `ForgeError` (clap/parse errors, `LOCK_TIMEOUT`, `COMMAND_FAILED`,
//! `NOT_A_GIT_REPOSITORY`) are hand-appended.
//!
//! No `schemars` / JSON-Schema dependency: the document is hand-authored as a
//! `Value`. It is static — `forge schema` works without a repo (no migrate, no
//! lock, no cwd dependency).

use forge_protocol::{RETRY_BACKOFF_MS, SCHEMA_VERSION};
use serde_json::{json, Value};

/// Build the published `forge.cli.v0` machine contract.
pub fn contract() -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "envelope": envelope_shape(),
        "commands": command_shapes(),
        "errors": error_registry(),
        "notes": {
            "retryable": "advisory; the client bounds retries (server sets after_ms only)",
            "retry_side_effects": "retrying a CONFLICT re-executes the command; for 'run' this re-executes the child process",
            "secret_protection": "captured 'run' output is hardened before persistence (NER-136): line-oriented key=value secrets, bare high-entropy tokens, JSON-embedded secrets, PEM private-key blocks, and scheme://user:pass@host credential-URL passwords are redacted, each surfaced as a warnings[] entry. KNOWN RESIDUALS: a bare 7/8/40/64-char pure-hex token or a UUID is exempted (to avoid redacting Forge's own git SHAs and content hashes), so a secret of exactly that shape is a false negative; secret-alphabet tokens shorter than 20 chars are below the entropy gate. Command argv strings (--require gate specs, CHECK_NOT_PASSED.unmet) are still redacted only for key=value patterns. Export secret-deny is path-name-level (.forge/.env/keys).",
            "integrity": "evidence and decision rows carry a SHA-256 content hash chained into the append-only operations spine; check/accept refuse a tampered deciding evidence row (EVIDENCE_TAMPERED, fail-closed, NOT bypassable by --allow-unverified), export refuses a tampered decision before creating the branch, and 'doctor' re-walks the chain. New post-Phase-9-local-signing evidence rows, decision rows, and native accepted commit ids also carry local Ed25519 `locally_signed` attestations that 'doctor' verifies. `forge trust policy` can require locally_signed subjects before accept/export (TRUST_POLICY_UNMET). `forge key status` exposes the local public key fingerprint and `forge key rotate` changes the future signing key while old ledger signatures remain verifiable from their stored public keys. This is local authenticity, not distributed attestation: hosted-runner trust, third-party attestation, and cross-machine ledger sync remain Phase 9 follow-ons.",
            "provenance": "an exported commit carries a structured Forge-* trailer including a content-addressed Forge-Provenance-Digest folding the deciding evidence content_hashes + decision digest. Post-Phase-9 signed exports also carry Forge-Local-Signature-Fingerprint for the locally verified accepted-decision signature. 'export verify-branch' recomputes the digest from the LOCAL ledger and, when the published commit carries a local signature fingerprint, confirms that fingerprint matches the locally verified decision signature (fail-closed PROVENANCE_MISMATCH / LOCAL_SIGNATURE_MISMATCH / MISSING_PROVENANCE_TRAILER). A PASS proves the published trailer is consistent with the current local ledger and, for signed exports, the local signing key — it catches a rewritten commit message, a naively-edited ledger row, or a mismatched local signature fingerprint — but it is NOT a cross-machine authenticity proof. `forge sync` now exports/imports versioned native bundles and path-peer clone/fetch/pull/push deltas with ledger rows, including clean divergent path-peer merge commits; hosted provenance, third-party trust, and ssh/https transport are Phase 9 follow-ons."
        }
    })
}

/// The field list of `ResponseEnvelope` — names + brief types — so consumers know
/// the wrapper shape. Notes that `retry` is top-level and `details` is per-error.
fn envelope_shape() -> Value {
    json!({
        "schema_version": "string (always \"forge.cli.v0\")",
        "command": "string",
        "request_id": "string | null",
        "operation_id": "string | null",
        "status": "string (\"success\" | \"error\")",
        "data": "object (command-specific; see commands[])",
        "warnings": "array<string>",
        "errors": "array<{ code: string, message: string, details: object }>",
        "retry": "{ retryable: bool, after_ms: number | null } (TOP-LEVEL, not per-error)"
    })
}

/// The dispatched command set, each with a one-line `data` summary. Hand-authored;
/// a one-liner per command is sufficient — the envelope/error shapes carry the
/// machine-checkable contract.
fn command_shapes() -> Value {
    let commands = [
        ("init", "Initializes a .forge repository; data carries root_path and the genesis operation."),
        ("start", "Starts an intent + its first attempt; accepts repeatable --require <command> gates and --require-tests-pass <command> structured gates (which also require zero parsed failures) persisted on the intent, and an optional --actor; data carries the started attempt + operation_id."),
        ("attempt start", "Starts a new attempt for an existing intent; data carries the attempt + operation_id."),
        ("attempt list", "Lists attempts; data carries { attempts: [...] }."),
        ("attempt show", "Shows one attempt; data carries the attempt detail."),
        ("attempt attach", "Attaches the active view to an attempt; data carries attempt_id, content_ref, current_view_id."),
        ("save", "Snapshots the worktree; data carries the saved snapshot + operation_id."),
        ("restore", "Restores a snapshot into the worktree (requires --yes); data carries the restore result."),
        ("run", "Runs a command and captures evidence; data carries the run record + operation_id."),
        ("propose", "Creates a proposal from the latest snapshot; data carries the proposal + operation_id."),
        ("check", "Evaluates the declarative multi-gate check against a proposal's snapshot; data carries the overall verdict + per-gate results (passed/failed/missing/stale) + operation_id."),
        ("accept", "Accepts a proposal (requires HEAD == base_head AND a passing check by default; --allow-unverified bypasses the check with a warning; `forge trust policy --accept locally_signed` additionally requires the proposal's trust-bearing subjects to have valid local signatures); data carries the decision + operation_id."),
        ("reject", "Rejects a proposal; data carries the decision + operation_id."),
        ("show", "Shows the active attempt's current state; data carries the attempt view."),
        ("proposal list", "Lists proposals; data carries { proposals: [...] }."),
        ("compare", "Compares competing attempts on verified evidence and ranks them; data carries { intents: [{ intent_id, intent, attempts: [{ attempt_id, gates, metrics, integrity, decision_status, publication_status, rank, rank_reason, ... }] }] }. Read-only and advisory; a cheap-check-tampered attempt is integrity=tampered with rank=null. With --diff <a> <b>, data also carries the file/hunk diff between the two attempts' proposals; native content refs use the native diff engine, git content refs use the git interop adapter. Diff files carry { path, status, old_path?, similarity?, insertions, deletions, binary, hunk, hunks: [{ old_start, old_lines, new_start, new_lines, lines: [{ tag: context|delete|insert, content }] }], truncated }."),
        ("diff", "Diffs content refs directly: --from <content-ref> --to <content-ref>, or --working --to <forge-tree-ref> for current worktree vs snapshot. Native refs use structured native hunks and rename detection; git refs use the git interop adapter. Flags: --find-renames[=<threshold>] and --no-renames. Data carries { files, dropped_secret_paths, warnings? } with the same file shape as compare --diff."),
        ("merge", "Merges --proposal <proposal-id> against the current native head. Clean native merges return { merged: true, merged_content_ref, base_content_ref, ours_content_ref, theirs_content_ref, operation_id }; true conflicts return { merged: false, conflict_set_id, operation_id } and persist native_merge conflict-as-data rows. Auto-resolution suggestions are never applied by merge."),
        ("conflict list", "Lists persisted conflict-as-data records. Data carries { conflicts: [{ conflict_set_id, context, base_content_ref, ours_content_ref, theirs_content_ref, generated_by_operation_id, resolver_backend, status, path_conflict_count, redacted_count, warnings }] }. Raw paths and inline blob excerpts are never emitted."),
        ("conflict show", "Shows one persisted conflict set plus redacted path-conflict summaries. Data carries { conflict, path_conflicts: [{ path_conflict_id, path_fingerprint, kind, base_ref, ours_ref, theirs_ref, side status/mode fields, resolution_ref, status }] }. With --suggest on unresolved native_merge conflicts, data also carries ranked advisory suggestions with provenance and requires_explicit_resolve=true. Raw paths and inline blob excerpts are never emitted."),
        ("conflict resolve", "Resolves a persisted conflict set with --tree <forge-tree-ref>. Data carries { conflict_set_id, proposal_id, proposal_revision_id, snapshot_id, evidence_id, resolution_ref, operation_id, view_id }. The resolution creates a new snapshot, proposal revision, and tamper-evident evidence row; agents should run evidence and check again before accept."),
        ("attempt compare", "Alias of `compare` scoped to attempts; same data shape."),
        ("log", "Walks the native commit history tip→genesis via the JSON contract; data carries { commits: [{ commit_id, tree, parents, intent_id, proposal_revision_id, decision_id, actor, authored_time, evidence_digest }] }. Read-only; --intent <id> filters to commits under one intent (\"show every change under this intent\"). Native-backend repos only (a git-backend repo has no native history)."),
        ("checkout", "Materializes a past commit's tree into the worktree (refuses a dirty worktree with DIRTY_WORKTREE; an unknown commit is rejected, a ledger-referenced-but-missing one is NATIVE_HISTORY_CORRUPT); data carries { commit_id, content_ref, base_unchanged: true, current_view_id }. Materialize-only: does NOT move the base anchor (a save afterward still diffs against the unchanged base HEAD), and is recorded in the op-log so forge undo can reverse it. Native-backend repos only."),
        ("undo", "Undoes the last save, restoring the worktree to the prior snapshot (the latest snapshot's parent) and recording the undo as a forward op-log operation; refuses a dirty worktree with DIRTY_WORKTREE; data carries { undone_operation_id, restored_snapshot_id, content_ref, current_view_id }. Append-only — never deletes a decision or op-log row. \"nothing to undo\" when there is no earlier snapshot."),
        ("trust policy", "Shows or updates the local trust policy. With no flags, data carries { min_accept_trust, min_export_trust, supported_trust_levels }. With --accept/--export, updates the configured minimum trust for accept/export. Supported levels are self_reported, locally_observed, locally_signed."),
        ("key status", "Shows the local Ed25519 signing key. Data carries { key_fingerprint, public_key, key_path, exists_before_command, signature_count, local_key_count, peer_key_count }. If the key is absent, a local key is created using the same private path as signing."),
        ("key rotate", "Rotates the local Ed25519 signing key for future signatures. Data carries { previous_fingerprint?, previous_key_backup_path?, key_fingerprint, public_key, key_path, signature_count, local_key_count, peer_key_count }. Existing ledger signatures remain verifiable because every signature row stores its public key and fingerprint."),
        ("doctor", "Reports repository health; data carries the diagnostic checks plus storage accounting by category, storage_policy, storage_budget { limit_bytes, used_bytes, over_budget, over_by_bytes }, signature_issues, and signature_key_summary { local_key_fingerprints, peer_key_fingerprints } for Ed25519 ledger attestations. Peer signatures are cryptographically verified but do not satisfy locally_signed policy. Storage budget overflow is informational and does not make doctor unhealthy by itself."),
        ("gc", "Garbage-collection. --dry-run returns a plan with plan_digest/protection_window_days, storage accounting by category, storage_policy, storage_budget, unreachable/protected native object ids, pack_candidate_native_objects, loose_duplicate_native_objects, and deletable_native_packs. Real deletion requires --yes --plan-digest <digest> from a prior dry-run; it writes packs first, deletes only verified loose duplicates, and deletes whole packs only when every indexed object is unreachable and outside retention. Mutating commands emit a non-blocking top-level storage-budget warning when .forge exceeds the configured budget; no command auto-evicts only because the budget is exceeded."),
        ("sync export", "Exports a versioned Forge sync v1 bundle to --output <path>. With --since <prior-bundle>, emits only native objects and allowlisted ledger rows absent from that prior same-repo bundle while keeping the current native HEAD/current_state metadata. Data carries { protocol_version, output_path, content_backend, incremental, since_path?, native_object_count, native_payload_count, ledger_table_count, ledger_row_count, native_head?, local_key_fingerprint? }. Native repos include object payloads plus allowlisted ledger rows; git-backed repos stay compatible with an empty native object set."),
        ("sync inspect", "Inspects a previously exported Forge sync v1 bundle without requiring a repository. Data carries { protocol_version, content_backend, native_object_count, native_payload_count, ledger_table_count, ledger_row_count, native_head?, local_key_fingerprint? }."),
        ("sync import", "Imports a Forge sync v1 native bundle into an initialized native repository. Native object payloads are verified by content id before storage, allowlisted ledger rows are inserted idempotently with the local repo id, and current_state/HEAD advance to the imported tip. With --materialize, the imported native HEAD tree is restored into a clean worktree and recorded as a local sync_import_materialized op. Data carries { protocol_version, content_backend, imported_native_objects, imported_ledger_rows, native_head?, current_operation_id, current_view_id, local_key_fingerprint?, materialized, materialized_content_ref?, materialized_operation_id?, materialized_view_id? }."),
        ("sync clone", "Clones a Forge sync v1 native bundle into an empty directory without minting a local genesis. The source repo_id and ledger rows are preserved, native object payloads are verified by content id, native HEAD/current_state are installed, and the imported HEAD tree is materialized. Data carries { protocol_version, repository_id, root_path, content_backend, imported_native_objects, imported_ledger_rows, native_head?, current_operation_id, current_view_id, local_key_fingerprint?, materialized, materialized_content_ref }."),
        ("sync fetch", "Fetches a native delta from another local Forge repository path, file:// URL, or ssh://host/absolute/path without materializing the worktree. SSH fetch runs the remote `forge --json sync serve export` endpoint and imports the returned envelope data locally. Fast-forward peers import object payloads plus ledger rows. Divergent peers stage source objects, run the native 3-way merge analyzer against the receiver, record a native merge commit when clean, and persist native_merge conflict-as-data when true conflicts exist. Data carries fast-forward fields { protocol_version, direction, remote_path, base_native_head?, remote_native_head?, exported_native_objects, exported_native_payloads, exported_ledger_rows, imported_native_objects, imported_ledger_rows, local_key_fingerprint?, materialized, up_to_date? }, clean-merge fields { protocol_version, direction, remote_path, merged: true, operation_id, merge_commit_id, base_native_head, receiver_native_head, common_ancestor_native_head, source_native_head, merged_content_ref, imported_native_objects, imported_ledger_rows, materialized }, or conflict fields { merged: false, conflict_set_id, operation_id, source_native_head?, imported_native_objects, imported_ledger_rows }. In clean-merge responses, base_native_head is kept as the receiver pre-merge head for compatibility; common_ancestor_native_head is the actual merge base. For fetch, materialized is false on clean merges; checkout/restore the merge_commit_id to reflect it in the worktree."),
        ("sync pull", "Fetches a native delta from another local Forge repository path, file:// URL, or ssh://host/absolute/path and materializes the fetched native HEAD into a clean worktree on fast-forward or the merged tree on a clean divergent merge. SSH pull runs the remote `forge --json sync serve export` endpoint and imports the returned envelope data locally. Divergent peers use native_merge conflict-as-data for true conflicts and do not overwrite the worktree. Data carries sync fetch fields plus { materialized_content_ref?, materialized_operation_id?, materialized_view_id?, up_to_date? } for fast-forwards; clean divergent merges return { protocol_version, direction, remote_path, merged: true, operation_id, merge_commit_id, base_native_head, receiver_native_head, common_ancestor_native_head, source_native_head, merged_content_ref, imported_native_objects, imported_ledger_rows, materialized: true }; true conflicts return { merged: false, conflict_set_id, operation_id, source_native_head?, imported_native_objects, imported_ledger_rows }. Materialized fields are null or omitted when the command does not materialize a worktree."),
        ("sync push", "Pushes a native delta into another local Forge repository path or file:// URL without materializing the peer worktree. SSH push is reserved until the remote receive/merge endpoint exists. Fast-forward local/file peers import object payloads plus ledger rows. Divergent peers record a native merge commit in the receiver repository when clean, or native_merge conflict-as-data when true conflicts exist. Data carries fast-forward fields { protocol_version, direction, remote_path, base_native_head?, local_native_head?, exported_native_objects, exported_native_payloads, exported_ledger_rows, imported_native_objects, imported_ledger_rows, local_key_fingerprint?, materialized, up_to_date? }, clean-merge fields { protocol_version, direction, remote_path, merged: true, operation_id, merge_commit_id, base_native_head, receiver_native_head, common_ancestor_native_head, source_native_head, merged_content_ref, imported_native_objects, imported_ledger_rows, materialized: false }, or conflict fields { merged: false, conflict_set_id, operation_id, source_native_head?, imported_native_objects, imported_ledger_rows }. Clean push merge commits advance native history only; they do not materialize the peer worktree."),
        ("export branch", "Exports an accepted proposal to a new git branch with a structured Forge-* provenance trailer (incl. Forge-Provenance-Digest and, for locally signed decisions, Forge-Local-Signature-Fingerprint); secret-risk paths are dropped with a warning; `forge trust policy --export locally_signed` verifies the proposal evidence, accepted decision, and native commit signature before creating the branch."),
        ("export pr-body", "Renders PR-body markdown for an accepted proposal citing the competing attempts against the declared intent; secret-risk paths are omitted with a warning."),
        ("export verify-branch", "Recomputes a published branch's provenance digest and, when present, local decision-signature fingerprint from the local ledger and confirms the trailers match (fail-closed PROVENANCE_MISMATCH / LOCAL_SIGNATURE_MISMATCH / MISSING_PROVENANCE_TRAILER); data carries { verified, proposal_id, provenance_digest, local_signature_fingerprint? }. Local consistency, not cross-machine authenticity (see notes.provenance)."),
        ("schema", "Emits this versioned machine contract; needs no repository."),
    ];
    Value::Array(
        commands
            .iter()
            .map(|(name, summary)| json!({ "command": name, "data": summary }))
            .collect(),
    )
}

/// The full error-code registry: every `ForgeError` code (derived from the enum
/// via `forge_store::error_registry`) plus the CLI-level codes that never pass
/// through `ForgeError`.
fn error_registry() -> Value {
    let mut entries: Vec<Value> = forge_store::error_registry()
        .iter()
        .map(|spec| {
            json!({
                "code": spec.code,
                "retryable": spec.retryable,
                "after_ms": spec.after_ms,
                "details_keys": spec.details_keys,
            })
        })
        .collect();

    // CLI-level codes that are constructed in main.rs (never via ForgeError).
    entries.push(cli_error(
        "LOCK_TIMEOUT",
        true,
        Some(RETRY_BACKOFF_MS),
        &["waited_ms"],
    ));
    entries.push(cli_error("COMMAND_FAILED", false, None, &[]));
    entries.push(cli_error("NOT_A_GIT_REPOSITORY", false, None, &[]));
    entries.push(cli_error("UNKNOWN_ARGUMENT", false, None, &["kind"]));
    entries.push(cli_error("MISSING_ARGUMENT", false, None, &["kind"]));
    entries.push(cli_error("USAGE_ERROR", false, None, &["kind"]));
    entries.push(cli_error("CONFIRMATION_REQUIRED", false, None, &[]));

    Value::Array(entries)
}

fn cli_error(code: &str, retryable: bool, after_ms: Option<u64>, details_keys: &[&str]) -> Value {
    json!({
        "code": code,
        "retryable": retryable,
        "after_ms": after_ms,
        "details_keys": details_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_carries_schema_version_and_notes() {
        let doc = contract();
        assert_eq!(doc["schema_version"], SCHEMA_VERSION);
        assert!(doc["notes"]["retryable"].is_string());
        assert!(doc["notes"]["secret_protection"].is_string());
    }

    #[test]
    fn registry_includes_every_forge_error_code_plus_lock_timeout() {
        let doc = contract();
        let codes: Vec<&str> = doc["errors"]
            .as_array()
            .expect("errors array")
            .iter()
            .map(|entry| entry["code"].as_str().expect("code string"))
            .collect();

        for spec in forge_store::error_registry() {
            assert!(
                codes.contains(&spec.code),
                "contract is missing ForgeError code {}",
                spec.code
            );
        }
        assert!(codes.contains(&"LOCK_TIMEOUT"));
    }
}
