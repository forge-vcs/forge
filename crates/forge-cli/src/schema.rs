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
            "integrity": "evidence and decision rows carry a SHA-256 content hash chained into the append-only operations spine; check/accept refuse a tampered deciding evidence row (EVIDENCE_TAMPERED, fail-closed, NOT bypassable by --allow-unverified), export refuses a tampered decision before creating the branch, and 'doctor' re-walks the chain. This is tamper-EVIDENT, not tamper-PROOF: an actor with full DB write access can recompute the whole chain; cryptographic signing is Phase 9.",
            "provenance": "an exported commit carries a structured Forge-* trailer including a content-addressed Forge-Provenance-Digest folding the deciding evidence content_hashes + decision digest. 'export verify-branch' recomputes that digest from the LOCAL ledger and confirms the published trailer matches (fail-closed PROVENANCE_MISMATCH). A PASS proves the published trailer is consistent with the current local ledger — it catches a rewritten commit message or a naively-edited ledger row — but it is NOT an authenticity proof: an attacker who rewrites the ledger rows AND re-exports still matches, and the deciding-row check is the cheap per-row check (a fully re-hashed row is caught only by 'doctor'). This is a self-verifying LOCAL trailer; cross-machine provenance/transport and signing are Phase 9."
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
        ("accept", "Accepts a proposal (requires HEAD == base_head AND a passing check by default; --allow-unverified bypasses the check with a warning); data carries the decision + operation_id."),
        ("reject", "Rejects a proposal; data carries the decision + operation_id."),
        ("show", "Shows the active attempt's current state; data carries the attempt view."),
        ("proposal list", "Lists proposals; data carries { proposals: [...] }."),
        ("compare", "Compares competing attempts on verified evidence and ranks them; data carries { intents: [{ intent_id, intent, attempts: [{ attempt_id, gates, metrics, integrity, decision_status, publication_status, rank, rank_reason, ... }] }] }. Read-only and advisory; a cheap-check-tampered attempt is integrity=tampered with rank=null. With --diff <a> <b>, data also carries the file/hunk diff between the two attempts' proposals (via the git adapter)."),
        ("attempt compare", "Alias of `compare` scoped to attempts; same data shape."),
        ("log", "Walks the native commit history tip→genesis via the JSON contract; data carries { commits: [{ commit_id, tree, parents, intent_id, proposal_revision_id, decision_id, actor, authored_time, evidence_digest }] }. Read-only; --intent <id> filters to commits under one intent (\"show every change under this intent\"). Native-backend repos only (a git-backend repo has no native history)."),
        ("doctor", "Reports repository health; data carries the diagnostic checks."),
        ("gc", "Garbage-collection (--dry-run only in v0); data carries the dry-run plan."),
        ("export branch", "Exports an accepted proposal to a new git branch with a structured Forge-* provenance trailer (incl. a content-addressed Forge-Provenance-Digest); secret-risk paths are dropped with a warning."),
        ("export pr-body", "Renders PR-body markdown for an accepted proposal citing the competing attempts against the declared intent; secret-risk paths are omitted with a warning."),
        ("export verify-branch", "Recomputes a published branch's provenance digest from the local ledger and confirms the trailer matches (fail-closed PROVENANCE_MISMATCH); data carries { verified, proposal_id, provenance_digest }. Local consistency, not cross-machine authenticity (see notes.provenance)."),
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
