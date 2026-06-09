mod schema;

use anyhow::{Context, Result};
use clap::{error::ErrorKind, Args, Parser, Subcommand};
use forge_content::{
    classify_content_ref, ContentBackend, ContentRefKind, SnapshotContent, FORGE_TREE_PREFIX,
};
use forge_protocol::{
    ErrorObject, ResponseEnvelope, ResponseStatus, RetryMetadata, RETRY_BACKOFF_MS,
};
use forge_store::ForgeError;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(name = "forge", version, about = "Local agent change-control loop")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    request_id: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init(InitArgs),
    Start(IntentArgs),
    Attempt(AttemptArgs),
    Save(AttemptScopedArgs),
    Restore(RestoreArgs),
    Run(RunArgs),
    Propose(AttemptScopedArgs),
    Check(ProposalScopedArgs),
    Accept(AcceptArgs),
    Reject(ProposalScopedArgs),
    Show(AttemptScopedArgs),
    Proposal(ProposalArgs),
    /// Compare competing attempts (per intent) on verified evidence + rank them.
    Compare(CompareArgs),
    /// Diff native or git content refs, or the working tree against a native snapshot.
    Diff(DiffArgs),
    /// Merge a proposal against the current native head.
    Merge(MergeArgs),
    /// Inspect persisted conflict-as-data records.
    Conflict(ConflictArgs),
    /// Walk the native commit history (tip→genesis) and the evidence that justified it.
    Log(LogArgs),
    /// Materialize a past commit's tree into the worktree (does not move the base anchor).
    Checkout(CheckoutArgs),
    /// Undo the last save, restoring the prior snapshot (recorded in the op-log).
    Undo,
    /// Inspect or update local trust policy gates.
    Trust(TrustArgs),
    /// Inspect or rotate the local Ed25519 signing key.
    Key(KeyArgs),
    Doctor,
    Gc(GcArgs),
    /// Export or inspect a Forge-native sync protocol bundle manifest.
    Sync(SyncArgs),
    Export(ExportArgs),
    /// Emit the versioned machine contract (schema_version, command + error registry).
    Schema,
}

#[derive(Debug, Args)]
struct CompareArgs {
    /// Compare attempts under this intent only. Omit to compare every intent that has
    /// an attempt (each as its own ranked group).
    #[arg(long)]
    intent: Option<String>,
    /// Compare attempts under this attempt's intent.
    #[arg(long)]
    attempt: Option<String>,
    /// Two attempt ids to additionally produce a file/hunk content diff between their
    /// proposals: `--diff <attempt_a> <attempt_b>`.
    #[arg(long, num_args = 2, value_names = ["ATTEMPT_A", "ATTEMPT_B"])]
    diff: Option<Vec<String>>,
}

#[derive(Debug, Args)]
struct DiffArgs {
    /// Content ref for the old side (`forge-tree:...` or `git-tree:...`). Omit with --working.
    #[arg(long)]
    from: Option<String>,
    /// Content ref for the new/base side (`forge-tree:...` or `git-tree:...`).
    #[arg(long)]
    to: String,
    /// Diff the current working tree against --to.
    #[arg(long)]
    working: bool,
    /// Enable rename detection, optionally overriding the similarity threshold (default 50).
    #[arg(long, num_args = 0..=1, default_missing_value = "50")]
    find_renames: Option<u8>,
    /// Disable rename detection.
    #[arg(long)]
    no_renames: bool,
}

#[derive(Debug, Args)]
struct MergeArgs {
    /// Proposal id whose base/theirs tree should merge with the current repo head.
    #[arg(long)]
    proposal: String,
}

#[derive(Debug, Args)]
struct ConflictArgs {
    #[command(subcommand)]
    command: ConflictCommand,
}

#[derive(Debug, Subcommand)]
enum ConflictCommand {
    List,
    Show {
        conflict_set_id: String,
        /// Emit gated, ranked native resolution suggestions. Suggestions are advisory only;
        /// use `conflict resolve --tree <ref>` to apply one explicitly.
        #[arg(long)]
        suggest: bool,
    },
    Resolve {
        conflict_set_id: String,
        #[arg(long)]
        tree: String,
    },
}

#[derive(Debug, Args)]
struct LogArgs {
    /// Show only commits recorded under this intent ("show every change under this intent").
    #[arg(long)]
    intent: Option<String>,
}

#[derive(Debug, Args)]
struct CheckoutArgs {
    /// The native commit id (`f1:commit:sha256:...`) whose tree to materialize.
    commit_id: String,
}

#[derive(Debug, Args)]
struct InitArgs {
    #[arg(long, value_parser = ["git", "native"])]
    content_backend: Option<String>,
}

#[derive(Debug, Args)]
struct IntentArgs {
    intent: Option<String>,
    /// A required check gate, given as the command that must pass on the proposed
    /// snapshot (e.g. --require "cargo test"). Repeatable; all gates must pass for
    /// `check` to be green and `accept` to proceed (NER-135). The value is
    /// whitespace-tokenized into program + args.
    #[arg(long)]
    require: Vec<String>,
    /// A structured required gate (NER-136): like --require, but the command's parsed
    /// outcome must also report zero failures (e.g. --require-tests-pass "cargo test"
    /// fails the gate if the parsed test-failure count is non-zero, even on exit 0).
    #[arg(long)]
    require_tests_pass: Vec<String>,
}

#[derive(Debug, Args)]
struct AttemptScopedArgs {
    #[arg(long)]
    attempt: Option<String>,
}

#[derive(Debug, Args)]
struct ProposalScopedArgs {
    #[arg(long)]
    attempt: Option<String>,
    #[arg(long)]
    proposal: Option<String>,
    /// Who is making this decision (NER-136 actor model). Falls back to `FORGE_ACTOR`,
    /// then `"unknown"`.
    #[arg(long)]
    actor: Option<String>,
}

#[derive(Debug, Args)]
struct AcceptArgs {
    #[arg(long)]
    attempt: Option<String>,
    #[arg(long)]
    proposal: Option<String>,
    /// Accept even when the proposal's check is not passing (NER-135). Default is to
    /// require a passing check; this bypass emits a warnings[] entry. NOTE: this is a
    /// policy bypass only — it never bypasses an `EVIDENCE_TAMPERED` integrity failure.
    #[arg(long)]
    allow_unverified: bool,
    /// Who is accepting (NER-136 actor model). Falls back to `FORGE_ACTOR`, then
    /// `"unknown"`.
    #[arg(long)]
    actor: Option<String>,
}

#[derive(Debug, Args)]
struct AttemptArgs {
    #[command(subcommand)]
    command: AttemptCommand,
}

#[derive(Debug, Subcommand)]
enum AttemptCommand {
    Start(AttemptStartArgs),
    List,
    Show {
        attempt_id: String,
    },
    Attach {
        attempt_id: String,
    },
    /// Compare competing attempts (per intent) on verified evidence + rank them.
    Compare(CompareArgs),
}

#[derive(Debug, Args)]
struct AttemptStartArgs {
    #[arg(long)]
    intent: String,
}

#[derive(Debug, Args)]
struct ProposalArgs {
    #[command(subcommand)]
    command: ProposalCommand,
}

#[derive(Debug, Subcommand)]
enum ProposalCommand {
    List(AttemptScopedArgs),
}

#[derive(Debug, Args)]
struct RestoreArgs {
    snapshot_id: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long)]
    attempt: Option<String>,
    /// Who is running this command (NER-136 actor model). Falls back to the
    /// `FORGE_ACTOR` env var, then `"unknown"`. Attribution, not authentication.
    #[arg(long)]
    actor: Option<String>,
    #[arg(long, default_value_t = forge_evidence::DEFAULT_TIMEOUT_MS)]
    timeout_ms: u64,
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct GcArgs {
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    plan_digest: Option<String>,
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[command(subcommand)]
    command: ExportCommand,
}

#[derive(Debug, Args)]
struct SyncArgs {
    #[command(subcommand)]
    command: SyncCommand,
}

#[derive(Debug, Subcommand)]
enum SyncCommand {
    /// Export a versioned v1 sync manifest for this repository.
    Export(SyncExportArgs),
    /// Inspect a previously exported sync manifest.
    Inspect(SyncInspectArgs),
    /// Import a previously exported native sync bundle into this repository.
    Import(SyncImportArgs),
    /// Clone a native sync bundle into an empty directory and materialize its HEAD.
    Clone(SyncCloneArgs),
    /// Fetch a fast-forward native delta from another local Forge repository.
    Fetch(SyncPeerArgs),
    /// Fetch and materialize a fast-forward native delta from another local Forge repository.
    Pull(SyncPeerArgs),
    /// Push a fast-forward native delta into another local Forge repository.
    Push(SyncPeerArgs),
}

#[derive(Debug, Args)]
struct SyncExportArgs {
    #[arg(long)]
    output: std::path::PathBuf,
    /// Emit only native objects and ledger rows absent from this prior bundle.
    #[arg(long)]
    since: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
struct SyncInspectArgs {
    path: std::path::PathBuf,
}

#[derive(Debug, Args)]
struct SyncImportArgs {
    path: std::path::PathBuf,
    /// Restore the imported native HEAD tree into the current worktree after applying the bundle.
    #[arg(long)]
    materialize: bool,
}

#[derive(Debug, Args)]
struct SyncCloneArgs {
    path: std::path::PathBuf,
}

#[derive(Debug, Args)]
struct SyncPeerArgs {
    remote: std::path::PathBuf,
}

#[derive(Debug, Subcommand)]
enum ExportCommand {
    Branch(ExportBranchArgs),
    PrBody(ProposalScopedArgs),
    /// Verify a published branch's provenance trailer recomputes from the local ledger.
    VerifyBranch(VerifyBranchArgs),
}

#[derive(Debug, Args)]
struct VerifyBranchArgs {
    name: String,
}

#[derive(Debug, Args)]
struct ExportBranchArgs {
    #[arg(long)]
    attempt: Option<String>,
    #[arg(long)]
    proposal: Option<String>,
    /// Who is publishing (NER-136 actor model). Falls back to `FORGE_ACTOR`, then
    /// `"unknown"`.
    #[arg(long)]
    actor: Option<String>,
    name: String,
}

#[derive(Debug, Args)]
struct TrustArgs {
    #[command(subcommand)]
    command: TrustCommand,
}

#[derive(Debug, Args)]
struct KeyArgs {
    #[command(subcommand)]
    command: KeyCommand,
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
    /// Show the current local signing key fingerprint.
    Status,
    /// Rotate the current local signing key, preserving old public keys in signatures.
    Rotate,
}

#[derive(Debug, Subcommand)]
enum TrustCommand {
    /// Show or update the local minimum trust policy.
    Policy(TrustPolicyArgs),
}

#[derive(Debug, Args)]
struct TrustPolicyArgs {
    /// Minimum trust required for `forge accept`.
    #[arg(long, value_parser = ["self_reported", "locally_observed", "locally_signed"])]
    accept: Option<String>,
    /// Minimum trust required for `forge export branch`.
    #[arg(long, value_parser = ["self_reported", "locally_observed", "locally_signed"])]
    export: Option<String>,
}

fn main() -> ExitCode {
    let raw_args: Vec<String> = env::args().collect();
    let json_mode = raw_args.iter().any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(&raw_args) {
        Ok(cli) => cli,
        Err(error) if json_mode => {
            let response = parser_error_response(&raw_args, error);
            println!("{}", serde_json::to_string_pretty(&response).unwrap());
            return ExitCode::from(2);
        }
        Err(error) => {
            let _ = error.print();
            return ExitCode::from(2);
        }
    };
    let request_id = cli.request_id.clone();
    let response = match cli.command {
        Command::Init(args) => init_response(request_id, args),
        Command::Start(args) => start_response(request_id, args),
        Command::Attempt(args) => attempt_response(request_id, args),
        Command::Save(args) => save_response(request_id, args),
        Command::Restore(args) if !args.yes => structured_error(
            "restore",
            request_id,
            "CONFIRMATION_REQUIRED",
            "restore requires --yes",
            json!({ "snapshot_id": args.snapshot_id }),
        ),
        Command::Restore(args) => restore_response(request_id, args),
        Command::Run(args) => run_response(request_id, args),
        Command::Propose(args) => propose_response(request_id, args),
        Command::Check(args) => check_response(request_id, args),
        Command::Accept(args) => accept_response(request_id, args),
        Command::Reject(args) => reject_response(request_id, args),
        Command::Show(args) => show_response(request_id, args),
        Command::Proposal(args) => proposal_response(request_id, args),
        Command::Compare(args) => compare_response(request_id, "compare", args),
        Command::Diff(args) => diff_response(request_id, args),
        Command::Merge(args) => merge_response(request_id, args),
        Command::Conflict(args) => conflict_response(request_id, args),
        Command::Log(args) => log_response(request_id, args),
        Command::Checkout(args) => checkout_response(request_id, args),
        Command::Undo => undo_response(request_id),
        Command::Trust(args) => trust_response(request_id, args),
        Command::Key(args) => key_response(request_id, args),
        Command::Doctor => doctor_response(request_id),
        Command::Gc(args) if !args.dry_run && (!args.yes || args.plan_digest.is_none()) => {
            structured_error(
                "gc",
                request_id,
                "CONFIRMATION_REQUIRED",
                "gc deletion requires --yes and --plan-digest from a prior dry-run",
                json!({}),
            )
        }
        Command::Gc(args) => gc_response(request_id, args),
        Command::Sync(args) => sync_response(request_id, args),
        Command::Export(args) => export_response(request_id, args),
        Command::Schema => schema_response(request_id),
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&response).unwrap());
    } else {
        print_human(&response);
    }

    if response.status == ResponseStatus::Success {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn parser_error_response(args: &[String], error: clap::Error) -> ResponseEnvelope {
    let code = match error.kind() {
        ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => "UNKNOWN_ARGUMENT",
        ErrorKind::MissingRequiredArgument | ErrorKind::MissingSubcommand => "MISSING_ARGUMENT",
        _ => "USAGE_ERROR",
    };
    structured_error(
        command_from_args(args),
        request_id_from_args(args),
        code,
        error.to_string(),
        json!({ "kind": format!("{:?}", error.kind()) }),
    )
}

fn command_from_args(args: &[String]) -> String {
    let mut positional = Vec::new();
    let mut skip_next = false;
    for arg in args.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "--json" => {}
            "--request-id" => skip_next = true,
            value if value.starts_with("--request-id=") => {}
            value if value.starts_with('-') => {}
            value => positional.push(value.to_string()),
        }
    }

    match positional.as_slice() {
        [] => "forge".to_string(),
        [command] => command.clone(),
        [command, subcommand, ..]
            if matches!(command.as_str(), "export" | "attempt" | "proposal" | "sync") =>
        {
            format!("{command} {subcommand}")
        }
        [command, subcommand, ..] if matches!(command.as_str(), "conflict") => {
            format!("{command} {subcommand}")
        }
        [command, ..] => command.clone(),
    }
}

fn request_id_from_args(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--request-id" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--request-id=") {
            return Some(value.to_string());
        }
    }
    None
}

fn start_response(request_id: Option<String>, args: IntentArgs) -> ResponseEnvelope {
    command_result("start", request_id, |cwd, request_id| {
        let base_head = current_base(&cwd)?;
        // Persist declared check gates on the intent (NER-135); competing attempts
        // under this intent inherit the same bar. None => default mode.
        let check_spec_json =
            forge_store::check_spec_json_from_requires(&args.require, &args.require_tests_pass);
        let started = forge_store::start_attempt(
            &cwd,
            request_id,
            args.intent
                .unwrap_or_else(|| "local agent attempt".to_string()),
            base_head.clone(),
            check_spec_json,
        )?;
        let content_ref = owner_base_content_ref(&cwd, &base_head)?;
        materialize_attempt_workspace(&cwd, &started.attempt_id, &content_ref)?;
        Ok((
            Some(started.operation_id.clone()),
            serde_json::to_value(started)?,
            Vec::new(),
        ))
    })
}

fn attempt_response(request_id: Option<String>, args: AttemptArgs) -> ResponseEnvelope {
    match args.command {
        AttemptCommand::Start(args) => {
            command_result("attempt start", request_id, |cwd, request_id| {
                let base_head = current_base(&cwd)?;
                let started = forge_store::start_attempt_for_intent(
                    &cwd,
                    request_id,
                    args.intent,
                    base_head.clone(),
                )?;
                let content_ref = owner_base_content_ref(&cwd, &base_head)?;
                materialize_attempt_workspace(&cwd, &started.attempt_id, &content_ref)?;
                Ok((
                    Some(started.operation_id.clone()),
                    serde_json::to_value(started)?,
                    Vec::new(),
                ))
            })
        }
        AttemptCommand::List => command_result("attempt list", request_id, |cwd, _| {
            Ok((
                None,
                json!({ "attempts": forge_store::list_attempts(&cwd)? }),
                Vec::new(),
            ))
        }),
        AttemptCommand::Show { attempt_id } => {
            command_result("attempt show", request_id, |cwd, _| {
                Ok((
                    None,
                    serde_json::to_value(forge_store::show_attempt(&cwd, &attempt_id)?)?,
                    Vec::new(),
                ))
            })
        }
        AttemptCommand::Attach { attempt_id } => {
            command_result("attempt attach", request_id, |cwd, request_id| {
                // NER-134: worktree/base materialization goes through `ContentBackend`,
                // not `forge_content_git::` directly, so git-worktree semantics stay out
                // of core lifecycle code (PRD §23.4). Bind the configured backend once.
                let target_base_head = forge_store::attempt_base_head(&cwd, &attempt_id)?;
                let current_content = snapshot_effective_worktree(&cwd)?;
                let resolved_current = forge_store::resolve_attempt(&cwd, None).ok();
                let latest_content_ref = match resolved_current {
                    Some(resolved) => forge_store::latest_snapshot_content_ref(
                        &cwd,
                        Some(&resolved.attempt.attempt_id),
                    )?
                    .or_else(|| owner_base_content_ref(&cwd, &resolved.attempt.base_head).ok()),
                    None => {
                        let head = current_base(&cwd)?;
                        Some(owner_base_content_ref(&cwd, &head)?)
                    }
                };
                if latest_content_ref.as_deref() != Some(current_content.content_ref.as_str()) {
                    return Err(ForgeError::DirtyWorktree {
                        paths: current_content.changed_paths.clone(),
                    }
                    .into());
                }
                let content_ref = match forge_store::attempt_materialization_ref(&cwd, &attempt_id)?
                {
                    Some(content_ref) => content_ref,
                    None => owner_base_content_ref(&cwd, &target_base_head)?,
                };
                // Restore routes by the ref's own prefix: a `git-tree:` base ref is
                // materialized by the git backend even in a native repo (intentional
                // until the Phase 7 native walker; see ContentBackend::base_content_ref).
                restore_effective_worktree(&cwd, &content_ref)?;
                materialize_attempt_workspace(&cwd, &attempt_id, &content_ref)?;
                let attached =
                    forge_store::attach_attempt(&cwd, request_id, &attempt_id, &content_ref)?;
                Ok((
                    Some(attached.operation_id.clone()),
                    json!({
                        "attempt_id": attempt_id,
                        "content_ref": content_ref,
                        "current_view_id": attached.view_id
                    }),
                    Vec::new(),
                ))
            })
        }
        AttemptCommand::Compare(args) => compare_response(request_id, "attempt compare", args),
    }
}

/// `forge compare` / `forge attempt compare` — the read-only compare/rank surface
/// (NER-137). Both forms share this handler. Returns the per-intent grouped, ranked
/// comparison; with `--diff <a> <b>` it additionally attaches the file/hunk content
/// diff between the two attempts' proposals. Native refs use the native diff engine;
/// git refs keep the git interop adapter. Read-only: no operation_id, no lock.
/// Secret-risk changed paths are already dropped by the store; any dropped paths in
/// the pairwise diff surface as warnings.
fn compare_response(
    request_id: Option<String>,
    command: &'static str,
    args: CompareArgs,
) -> ResponseEnvelope {
    command_result(command, request_id, |cwd, _| {
        let selector = forge_store::CompareSelector {
            intent_id: args.intent.clone(),
            attempt_id: args.attempt.clone(),
        };
        let comparison = forge_store::compare_attempts(&cwd, selector)?;
        let mut data = serde_json::to_value(&comparison)?;
        let mut warnings = Vec::new();
        if let Some(pair) = &args.diff {
            // clap enforces exactly two values for --diff.
            let ref_a = forge_store::attempt_proposal_content_ref(&cwd, &pair[0])?;
            let ref_b = forge_store::attempt_proposal_content_ref(&cwd, &pair[1])?;
            let tree_diff = diff_content_refs(&cwd, &ref_a, &ref_b, native_diff_options(true))?;
            collect_diff_warnings(&tree_diff, &mut warnings);
            data["diff"] = serde_json::to_value(&tree_diff)?;
        }
        Ok((None, data, warnings))
    })
}

fn diff_response(request_id: Option<String>, args: DiffArgs) -> ResponseEnvelope {
    command_result("diff", request_id, |cwd, _| {
        let options = forge_content_native::DiffOptions {
            include_hunks: true,
            detect_renames: !args.no_renames,
            rename_threshold: args.find_renames.unwrap_or(50),
            rename_limit: 1000,
        };
        let tree_diff = if args.working {
            if args.from.is_some() {
                anyhow::bail!("--working cannot be combined with --from");
            }
            let context = forge_store::open_repository(&cwd)?;
            let store = forge_content_native::NativeObjectStore::new(&context.root_path);
            forge_content_native::diff_working_vs_tree(
                &store,
                &context.worktree_path,
                &args.to,
                &options,
            )?
        } else {
            let from = args
                .from
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("diff requires --from unless --working is set"))?;
            diff_content_refs(&cwd, from, &args.to, options)?
        };
        let mut warnings = Vec::new();
        collect_diff_warnings(&tree_diff, &mut warnings);
        Ok((None, serde_json::to_value(&tree_diff)?, warnings))
    })
}

fn merge_response(request_id: Option<String>, args: MergeArgs) -> ResponseEnvelope {
    command_result("merge", request_id, |cwd, request_id| {
        let proposal = forge_store::proposal_for_merge(&cwd, &args.proposal)?;
        let base_content_ref = owner_base_content_ref(&cwd, &proposal.base_head)?;
        let ours_head = current_base(&cwd)?;
        let ours_content_ref = owner_base_content_ref(&cwd, &ours_head)?;
        let theirs_content_ref = proposal.content_ref.clone();
        let ref_kinds = [
            classify_content_ref(&base_content_ref),
            classify_content_ref(&ours_content_ref),
            classify_content_ref(&theirs_content_ref),
        ];
        if !ref_kinds
            .iter()
            .all(|kind| matches!(kind, ContentRefKind::ForgeTree(_)))
        {
            return Err(forge_store::ForgeError::UnsupportedContentBackend {
                command: "merge".to_string(),
                required: "native".to_string(),
                actual: content_backend_label(&ref_kinds).to_string(),
            }
            .into());
        }
        let store = forge_content_native::NativeObjectStore::new(&cwd);
        let result = forge_content_native::merge_native_content_refs(
            &store,
            &base_content_ref,
            &ours_content_ref,
            &theirs_content_ref,
        )?;
        if let Some(merged_content_ref) = result.merged_content_ref {
            ensure_clean_worktree(&cwd, &merged_content_ref)?;
            restore_effective_worktree(&cwd, &merged_content_ref)?;
            let record = forge_store::record_merge_success(
                &cwd,
                request_id,
                "merge",
                &proposal,
                &forge_store::MergeSuccessInput {
                    base_head: proposal.base_head.clone(),
                    ours_head: ours_head.clone(),
                    base_content_ref,
                    ours_content_ref,
                    theirs_content_ref,
                    merged_content_ref,
                },
            )?;
            Ok((
                Some(record.operation_id.clone()),
                json!({
                    "merged": true,
                    "proposal_id": record.proposal_id,
                    "proposal_revision_id": record.proposal_revision_id,
                    "snapshot_id": record.snapshot_id,
                    "base_content_ref": record.base_content_ref,
                    "ours_content_ref": record.ours_content_ref,
                    "theirs_content_ref": record.theirs_content_ref,
                    "merged_content_ref": record.merged_content_ref,
                    "operation_id": record.operation_id,
                }),
                secret_export_warnings(&result.dropped_secret_paths),
            ))
        } else {
            let input = forge_store::MergeConflictInput {
                context: "native_merge".to_string(),
                proposal_id: Some(proposal.proposal_id.clone()),
                base_head: Some(proposal.base_head.clone()),
                ours_head: Some(ours_head),
                base_content_ref,
                ours_content_ref,
                theirs_content_ref,
                conflicts: result.conflicts,
            };
            let record = forge_store::record_merge_conflict(&cwd, request_id, "merge", &input)?;
            Ok((
                Some(record.operation_id.clone()),
                json!({
                    "merged": false,
                    "proposal_id": proposal.proposal_id,
                    "proposal_revision_id": proposal.proposal_revision_id,
                    "conflict_set_id": record.conflict_set_id,
                    "operation_id": record.operation_id,
                }),
                secret_export_warnings(&result.dropped_secret_paths),
            ))
        }
    })
}

fn conflict_response(request_id: Option<String>, args: ConflictArgs) -> ResponseEnvelope {
    match args.command {
        ConflictCommand::List => command_result("conflict list", request_id, |cwd, _| {
            Ok((
                None,
                serde_json::to_value(forge_store::conflict_list(&cwd)?)?,
                Vec::new(),
            ))
        }),
        ConflictCommand::Show {
            conflict_set_id,
            suggest,
        } => command_result("conflict show", request_id, |cwd, _| {
            Ok((
                None,
                serde_json::to_value(forge_store::conflict_show(&cwd, &conflict_set_id, suggest)?)?,
                Vec::new(),
            ))
        }),
        ConflictCommand::Resolve {
            conflict_set_id,
            tree,
        } => command_result("conflict resolve", request_id, |cwd, request_id| {
            forge_store::preflight_conflict_resolution(&cwd, &conflict_set_id, &tree)?;
            ensure_clean_worktree(&cwd, &tree)?;
            restore_effective_worktree(&cwd, &tree)?;
            let record =
                forge_store::resolve_conflict_with_tree(&cwd, request_id, &conflict_set_id, &tree)?;
            Ok((
                Some(record.operation_id.clone()),
                serde_json::to_value(record)?,
                Vec::new(),
            ))
        }),
    }
}

fn native_diff_options(include_hunks: bool) -> forge_content_native::DiffOptions {
    forge_content_native::DiffOptions {
        include_hunks,
        ..forge_content_native::DiffOptions::default()
    }
}

fn diff_content_refs(
    repo_root: &Path,
    ref_a: &str,
    ref_b: &str,
    options: forge_content_native::DiffOptions,
) -> Result<forge_content::TreeDiff> {
    match (classify_content_ref(ref_a), classify_content_ref(ref_b)) {
        (ContentRefKind::ForgeTree(_), ContentRefKind::ForgeTree(_)) => {
            let store = forge_content_native::NativeObjectStore::new(repo_root);
            forge_content_native::diff_native_content_refs(&store, ref_a, ref_b, &options)
        }
        _ => forge_export_git::diff_trees(repo_root, ref_a, ref_b, options.include_hunks),
    }
}

fn collect_diff_warnings(tree_diff: &forge_content::TreeDiff, warnings: &mut Vec<String>) {
    warnings.extend(secret_export_warnings(&tree_diff.dropped_secret_paths));
    warnings.extend(
        tree_diff
            .warnings
            .iter()
            .map(|warning| warning.message.clone()),
    );
    // Surface hunk truncation at the envelope level so an agent that only reads
    // warnings[] (not data.diff.files[].truncated) knows the diff is incomplete.
    for file in &tree_diff.files {
        if file.truncated {
            warnings.push(format!(
                "diff hunk truncated for {} (body exceeded the per-file cap)",
                file.path
            ));
        }
    }
}

fn save_response(request_id: Option<String>, args: AttemptScopedArgs) -> ResponseEnvelope {
    command_result("save", request_id, |cwd, request_id| {
        // NER-134: verify the worktree binding BEFORE snapshotting, so a mismatch fails
        // fast without writing orphan content objects. `save_snapshot` re-checks
        // authoritatively on the write path; this returns the resolved attempt id, which
        // we pass back as an explicit selector.
        let resolved_attempt = forge_store::verify_save_target(&cwd, args.attempt.as_deref())?;
        let content = snapshot_effective_worktree(&cwd)?;
        // Crash boundary (NER-132 U6, debug-only): objects are now durably fsynced
        // but no content_ref row is committed. A crash here must never leave a
        // committed ref pointing at a missing object — the objects are present, the
        // ref is absent.
        forge_content::maybe_crash("after_object_fsync_before_db_commit");
        let saved = forge_store::save_snapshot(
            &cwd,
            request_id,
            Some(resolved_attempt.as_str()),
            content.content_ref,
            content.changed_paths,
        )?;
        // Crash boundary (NER-132 U6, debug-only): the content_ref is committed and
        // durable (synchronous=NORMAL fsyncs the WAL on commit) even if the WAL is
        // not yet checkpointed. On reopen, WAL recovery must show the committed ref
        // AND its durably-retained object.
        forge_content::maybe_crash("after_db_commit_before_checkpoint");
        Ok((
            Some(saved.operation_id.clone()),
            serde_json::to_value(saved)?,
            Vec::new(),
        ))
    })
}

/// Refuse a dirty worktree BEFORE a materializing nav command (restore/checkout/undo) clobbers
/// it (NER-143 R1/R2): the single definition shared by those three chained-nav commands. (Note
/// `attempt attach` also materializes but uses its OWN switching-baseline dirty-check — it
/// compares against the attempt being switched *to*, not the expected/target model here — so it
/// deliberately does not route through this helper.)
///
/// Passes iff the worktree holds the content it is EXPECTED to hold
/// (`current_state.expected_content_ref`, set by the last materializing op; fallback to the
/// latest saved snapshot for a pre-007 / pre-first-materialize repo) **OR** it already holds the
/// `target` we are about to materialize. The OR-target clause is the crash-safety hinge (DR-F1):
/// after a materialize-then-crash before the record txn commits, the worktree holds `target`
/// while `expected_content_ref` is still the prior ref — re-running the same nav command then
/// passes via `worktree == target`, re-materialize is a no-op, and the record txn sets expected.
/// A genuine unsaved edit matches NEITHER and is refused (the safety property is preserved).
fn ensure_clean_worktree(cwd: &Path, target_content_ref: &str) -> Result<()> {
    let current = snapshot_effective_worktree(cwd)?;
    let expected = match forge_store::expected_content_ref(cwd)? {
        Some(expected) => Some(expected),
        None => forge_store::latest_snapshot_content_ref(cwd, None)?,
    };
    let matches_expected = expected.as_deref() == Some(current.content_ref.as_str());
    let matches_target = current.content_ref == target_content_ref;
    if matches_expected || matches_target {
        Ok(())
    } else {
        Err(ForgeError::DirtyWorktree {
            paths: current.changed_paths,
        }
        .into())
    }
}

fn ensure_worktree_matches_expected(cwd: &Path) -> Result<()> {
    let expected = match forge_store::expected_content_ref(cwd)? {
        Some(expected) => expected,
        None => match forge_store::latest_snapshot_content_ref(cwd, None)? {
            Some(content_ref) => content_ref,
            None => {
                let attempt = forge_store::resolve_attempt(cwd, None)?.attempt;
                owner_base_content_ref(cwd, &attempt.base_head)?
            }
        },
    };
    let current = snapshot_effective_worktree(cwd)?;
    if current.content_ref == expected {
        Ok(())
    } else {
        Err(ForgeError::DirtyWorktree {
            paths: current.changed_paths,
        }
        .into())
    }
}

fn ensure_clean_for_sync_import_materialize(cwd: &Path) -> Result<()> {
    let expected = match forge_store::expected_content_ref(cwd)? {
        Some(expected) => expected,
        None => {
            let head = current_base(cwd)?;
            native_commit_content_ref(cwd, &head)?
        }
    };
    let current = snapshot_effective_worktree(cwd)?;
    if current.content_ref == expected {
        Ok(())
    } else {
        Err(ForgeError::DirtyWorktree {
            paths: current.changed_paths,
        }
        .into())
    }
}

fn native_commit_content_ref(cwd: &Path, commit_id: &str) -> Result<String> {
    let context = forge_store::open_repository(cwd)?;
    let id = forge_content_native::ObjectId::parse(commit_id)?;
    let store = forge_content_native::NativeObjectStore::new(&context.root_path);
    let commit = store.read_commit(&id)?;
    Ok(format!("{FORGE_TREE_PREFIX}{}", commit.tree))
}

fn restore_response(request_id: Option<String>, args: RestoreArgs) -> ResponseEnvelope {
    command_result("restore", request_id, |cwd, request_id| {
        let content_ref = forge_store::snapshot_content_ref(&cwd, &args.snapshot_id)?;
        ensure_clean_worktree(&cwd, &content_ref)?;
        // NER-134 Piece 1b: refuse to materialize a snapshot that belongs to an attempt
        // other than the one the worktree is bound to — otherwise restore is a second
        // cross-attempt contamination vector (it would clobber the worktree with another
        // attempt's content while `attached_attempt_id` stays put, and a later
        // `save --attempt <bound>` would record the wrong content). Checked BEFORE
        // materialization so the worktree is never clobbered on the error path.
        let bound_attempt = forge_store::resolve_attempt(&cwd, None)?.attempt.attempt_id;
        let snapshot_attempt = forge_store::snapshot_owner_attempt_id(&cwd, &args.snapshot_id)?;
        if snapshot_attempt != bound_attempt {
            return Err(ForgeError::AttemptWorktreeMismatch {
                requested_attempt: snapshot_attempt,
                attached_attempt: bound_attempt,
            }
            .into());
        }
        restore_effective_worktree(&cwd, &content_ref)?;
        let restored =
            forge_store::record_restore(&cwd, request_id, &args.snapshot_id, &content_ref)?;
        Ok((
            Some(restored.operation_id.clone()),
            json!({
                "snapshot_id": args.snapshot_id,
                "content_ref": content_ref,
                "current_view_id": restored.view_id
            }),
            Vec::new(),
        ))
    })
}

fn run_response(request_id: Option<String>, args: RunArgs) -> ResponseEnvelope {
    command_result("run", request_id, |cwd, request_id| {
        if args.command.is_empty() {
            anyhow::bail!("missing command after --");
        }
        ensure_worktree_matches_expected(&cwd)?;
        let worktree = forge_store::effective_worktree_path(&cwd)?;
        let captured =
            forge_evidence::capture_with_timeout(&worktree, &args.command, args.timeout_ms)?;
        // Surface each secret redaction the capture applied as a warnings[] entry
        // (NER-136 §U4), grouped by detector kind with a count.
        let warnings = redaction_warnings(&captured.redactions);
        let recorded = forge_store::record_evidence(
            &cwd,
            request_id,
            args.attempt.as_deref(),
            forge_store::EvidenceInput {
                command: captured.command,
                args: captured.args,
                cwd: captured.cwd,
                exit_code: captured.exit_code,
                started_at_ms: captured.started_at_ms,
                ended_at_ms: captured.ended_at_ms,
                stdout_excerpt: captured.stdout_excerpt,
                stderr_excerpt: captured.stderr_excerpt,
                stdout_truncated: captured.stdout_truncated,
                stderr_truncated: captured.stderr_truncated,
                timed_out: captured.timed_out,
                sensitivity: captured.sensitivity,
                visibility: captured.visibility,
                trust: captured.trust,
                actor: resolve_actor(args.actor.as_deref()),
                structured_json: captured.structured_json,
            },
        )?;
        Ok((
            Some(recorded.operation_id.clone()),
            serde_json::to_value(recorded)?,
            warnings,
        ))
    })
}

fn propose_response(request_id: Option<String>, args: AttemptScopedArgs) -> ResponseEnvelope {
    command_result("propose", request_id, |cwd, request_id| {
        let proposal = forge_store::propose(&cwd, request_id, args.attempt.as_deref())?;
        Ok((
            Some(proposal.operation_id.clone()),
            serde_json::to_value(proposal)?,
            Vec::new(),
        ))
    })
}

fn check_response(request_id: Option<String>, args: ProposalScopedArgs) -> ResponseEnvelope {
    command_result("check", request_id, |cwd, request_id| {
        // The pass/fail verdict is derived inside record_check's IMMEDIATE txn from
        // the evidence row it binds (NER-132 U2), so there is no CLI-layer show()
        // read for a concurrent, lock-free `run` to race.
        let check = forge_store::record_check(
            &cwd,
            request_id,
            args.attempt.as_deref(),
            args.proposal.as_deref(),
        )?;
        Ok((
            Some(check.operation_id.clone()),
            serde_json::to_value(check)?,
            Vec::new(),
        ))
    })
}

fn accept_response(request_id: Option<String>, args: AcceptArgs) -> ResponseEnvelope {
    command_result("accept", request_id, |cwd, request_id| {
        let proposal = forge_store::exportable_proposal(
            &cwd,
            args.attempt.as_deref(),
            args.proposal.as_deref(),
        )?;
        let current_head = current_base(&cwd)?;
        if current_head != proposal.base_head {
            if forge_store::resolved_merge_ours_head(
                &cwd,
                &proposal.proposal_id,
                &proposal.content_ref,
            )?
            .as_deref()
                == Some(current_head.as_str())
            {
                // The proposal was explicitly resolved from a native merge against this
                // head. `decide` writes the two-parent commit from the stored merge
                // metadata, so this is not a stale-base bypass.
            } else {
                let base_content_ref = owner_base_content_ref(&cwd, &proposal.base_head)?;
                let ours_content_ref = owner_base_content_ref(&cwd, &current_head)?;
                return Err(forge_store::StaleBaseConflict {
                    input: forge_store::StaleBaseConflictInput {
                        context: "stale_base_accept".to_string(),
                        expected_head: proposal.base_head.clone(),
                        actual_head: current_head,
                        base_content_ref,
                        ours_content_ref,
                        theirs_content_ref: proposal.content_ref.clone(),
                        changed_paths: proposal.changed_paths.clone(),
                    },
                }
                .into());
            }
        }
        // Evidence gate (NER-135 R6): enforced in-txn inside `decide` unless
        // --allow-unverified. On bypass, surface the non-passing status as a warning.
        forge_store::enforce_trust_policy(
            &cwd,
            forge_store::TrustPolicyAction::Accept,
            &proposal.proposal_revision_id,
        )?;
        let record = forge_store::decide(
            &cwd,
            request_id,
            args.attempt.as_deref(),
            args.proposal.as_deref(),
            "accepted",
            !args.allow_unverified,
            &resolve_actor(args.actor.as_deref()),
        )?;
        let mut warnings = Vec::new();
        if args.allow_unverified {
            if let Some(status) = record.check_status.as_deref() {
                if status != "passed" {
                    warnings.push(format!(
                        "accepted without a passing check (--allow-unverified): status={status}"
                    ));
                }
            }
        }
        Ok((
            Some(record.operation_id.clone()),
            serde_json::to_value(record)?,
            warnings,
        ))
    })
}

fn reject_response(request_id: Option<String>, args: ProposalScopedArgs) -> ResponseEnvelope {
    command_result("reject", request_id, |cwd, request_id| {
        // Reject is never gated on evidence (enforce_check = false).
        let record = forge_store::decide(
            &cwd,
            request_id,
            args.attempt.as_deref(),
            args.proposal.as_deref(),
            "rejected",
            false,
            &resolve_actor(args.actor.as_deref()),
        )?;
        Ok((
            Some(record.operation_id.clone()),
            serde_json::to_value(record)?,
            Vec::new(),
        ))
    })
}

fn show_response(request_id: Option<String>, args: AttemptScopedArgs) -> ResponseEnvelope {
    command_result("show", request_id, |cwd, _request_id| {
        let show = forge_store::show(&cwd, args.attempt.as_deref())?;
        Ok((None, serde_json::to_value(show)?, Vec::new()))
    })
}

fn proposal_response(request_id: Option<String>, args: ProposalArgs) -> ResponseEnvelope {
    match args.command {
        ProposalCommand::List(args) => command_result("proposal list", request_id, |cwd, _| {
            Ok((
                None,
                json!({ "proposals": forge_store::list_proposals(&cwd, args.attempt.as_deref())? }),
                Vec::new(),
            ))
        }),
    }
}

fn checkout_response(request_id: Option<String>, args: CheckoutArgs) -> ResponseEnvelope {
    command_result("checkout", request_id, |cwd, request_id| {
        // Resolve + validate the target FIRST (NOT_FOUND for a typo vs NATIVE_HISTORY_CORRUPT
        // for a ledger-referenced-but-missing commit), before touching the worktree.
        let content_ref = forge_store::checkout_target_content_ref(&cwd, &args.commit_id)?;
        // Refuse a dirty worktree BEFORE materializing (shared helper): the irreversible
        // clobber must not run if there are unsaved changes to lose.
        ensure_clean_worktree(&cwd, &content_ref)?;
        // Materialize the historical tree (policy-excluded; symlink-aware + R15 via U9).
        restore_effective_worktree(&cwd, &content_ref)?;
        // Record the checkout in the op-log (so `undo` can reverse it and gc keeps the target
        // reachable). Checkout does NOT move the base anchor — surfaced as base_unchanged so an
        // agent is not misled into expecting git's HEAD-moving checkout semantics.
        let record = forge_store::record_checkout(&cwd, request_id, &args.commit_id, &content_ref)?;
        Ok((
            Some(record.operation_id.clone()),
            json!({
                "commit_id": args.commit_id,
                "content_ref": content_ref,
                "base_unchanged": true,
                "current_view_id": record.view_id
            }),
            Vec::new(),
        ))
    })
}

fn undo_response(request_id: Option<String>) -> ResponseEnvelope {
    command_result("undo", request_id, |cwd, request_id| {
        // Resolve the prior snapshot to restore (clear "nothing to undo" if none).
        let target = forge_store::undo_target(&cwd)?;
        // Refuse a dirty worktree BEFORE materializing (shared helper): undo must not clobber
        // unsaved edits.
        ensure_clean_worktree(&cwd, &target.content_ref)?;
        // Restore the prior snapshot (policy-excluded, crash-atomic, symlink-aware + R15).
        restore_effective_worktree(&cwd, &target.content_ref)?;
        // Record the undo as a forward op-log operation (never deletes a decisions/op row).
        let record = forge_store::record_undo(
            &cwd,
            request_id,
            &target.undone_operation_id,
            &target.restored_snapshot_id,
            &target.content_ref,
        )?;
        Ok((
            Some(record.operation_id.clone()),
            json!({
                "undone_operation_id": target.undone_operation_id,
                "restored_snapshot_id": target.restored_snapshot_id,
                "content_ref": target.content_ref,
                "current_view_id": record.view_id
            }),
            Vec::new(),
        ))
    })
}

fn log_response(request_id: Option<String>, args: LogArgs) -> ResponseEnvelope {
    // Read-only: "log" is not a mutating command, so command_result takes no lock and runs
    // no reconcile — `native_log` resolves the authoritative tip from the ledger directly,
    // tolerating a not-yet-reconciled HEAD.
    command_result("log", request_id, |cwd, _request_id| {
        let commits = forge_store::native_log(&cwd, args.intent.as_deref())?;
        Ok((None, json!({ "commits": commits }), Vec::new()))
    })
}

fn doctor_response(request_id: Option<String>) -> ResponseEnvelope {
    command_result("doctor", request_id, |cwd, _request_id| {
        let report = forge_store::doctor(&cwd)?;
        Ok((None, serde_json::to_value(report)?, Vec::new()))
    })
}

fn trust_response(request_id: Option<String>, args: TrustArgs) -> ResponseEnvelope {
    match args.command {
        TrustCommand::Policy(args) => command_result("trust policy", request_id, |cwd, _| {
            let policy = if args.accept.is_some() || args.export.is_some() {
                forge_store::set_trust_policy(&cwd, args.accept.as_deref(), args.export.as_deref())?
            } else {
                forge_store::trust_policy(&cwd)?
            };
            Ok((None, serde_json::to_value(policy)?, Vec::new()))
        }),
    }
}

fn key_response(request_id: Option<String>, args: KeyArgs) -> ResponseEnvelope {
    match args.command {
        KeyCommand::Status => command_result("key status", request_id, |cwd, _| {
            let status = forge_store::local_key_status(&cwd)?;
            Ok((None, serde_json::to_value(status)?, Vec::new()))
        }),
        KeyCommand::Rotate => command_result("key rotate", request_id, |cwd, _| {
            let rotation = forge_store::rotate_local_key(&cwd)?;
            Ok((None, serde_json::to_value(rotation)?, Vec::new()))
        }),
    }
}

fn gc_response(request_id: Option<String>, args: GcArgs) -> ResponseEnvelope {
    command_result("gc", request_id, |cwd, _request_id| {
        let report = if args.dry_run {
            forge_store::gc_dry_run(&cwd)?
        } else {
            forge_store::gc_delete(&cwd, args.plan_digest.as_deref().unwrap_or_default())?
        };
        Ok((None, serde_json::to_value(report)?, Vec::new()))
    })
}

fn sync_response(request_id: Option<String>, args: SyncArgs) -> ResponseEnvelope {
    match args.command {
        SyncCommand::Export(args) => command_result("sync export", request_id, |cwd, _| {
            let report =
                forge_sync::export_manifest_since(&cwd, &args.output, args.since.as_deref())?;
            Ok((None, serde_json::to_value(report)?, Vec::new()))
        }),
        SyncCommand::Inspect(args) => {
            let result = forge_sync::inspect_manifest(&args.path)
                .and_then(|report| Ok(serde_json::to_value(report)?));
            match result {
                Ok(data) => ResponseEnvelope::success("sync inspect", request_id, None, data),
                Err(error) => {
                    let (error_object, retry) = error_to_object("sync inspect", &error);
                    ResponseEnvelope::error_with(
                        "sync inspect",
                        request_id,
                        None,
                        error_object,
                        retry,
                    )
                }
            }
        }
        SyncCommand::Import(args) => {
            command_result("sync import", request_id, |cwd, request_id| {
                if args.materialize {
                    ensure_clean_for_sync_import_materialize(&cwd)
                        .context("preflight sync import materialize")?;
                }
                let report =
                    forge_sync::import_manifest(&cwd, &args.path).context("apply sync bundle")?;
                let mut operation_id = None;
                let mut data = serde_json::to_value(&report)?;
                if args.materialize {
                    let commit_id = report.native_head.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("sync bundle has no native head to materialize")
                    })?;
                    let content_ref = forge_store::checkout_target_content_ref(&cwd, commit_id)
                        .context("resolve imported native head")?;
                    restore_effective_worktree(&cwd, &content_ref)
                        .context("restore imported native head")?;
                    let record = forge_store::record_sync_import_materialized(
                        &cwd,
                        request_id,
                        commit_id,
                        &content_ref,
                    )
                    .context("record sync import materialization")?;
                    operation_id = Some(record.operation_id.clone());
                    if let Some(object) = data.as_object_mut() {
                        object.insert("materialized".to_string(), json!(true));
                        object.insert("materialized_content_ref".to_string(), json!(content_ref));
                        object.insert(
                            "materialized_operation_id".to_string(),
                            json!(record.operation_id),
                        );
                        object.insert("materialized_view_id".to_string(), json!(record.view_id));
                    }
                } else if let Some(object) = data.as_object_mut() {
                    object.insert("materialized".to_string(), json!(false));
                }
                Ok((operation_id, data, Vec::new()))
            })
        }
        SyncCommand::Clone(args) => {
            let result = env::current_dir()
                .map_err(anyhow::Error::from)
                .and_then(|cwd| {
                    let report = forge_sync::clone_manifest(&cwd, &args.path)
                        .context("clone sync bundle")?;
                    let commit_id = report.native_head.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("sync bundle has no native head to clone")
                    })?;
                    let content_ref = forge_store::checkout_target_content_ref(&cwd, commit_id)
                        .context("resolve cloned native head")?;
                    restore_effective_worktree(&cwd, &content_ref)
                        .context("restore cloned native head")?;
                    forge_store::set_sync_clone_expected_content_ref(&cwd, &content_ref)
                        .context("record cloned worktree baseline")?;
                    let mut data = serde_json::to_value(&report)?;
                    if let Some(object) = data.as_object_mut() {
                        object.insert("materialized".to_string(), json!(true));
                        object.insert("materialized_content_ref".to_string(), json!(content_ref));
                    }
                    Ok(data)
                });
            match result {
                Ok(data) => ResponseEnvelope::success("sync clone", request_id, None, data),
                Err(error) => {
                    let (error_object, retry) = error_to_object("sync clone", &error);
                    ResponseEnvelope::error_with(
                        "sync clone",
                        request_id,
                        None,
                        error_object,
                        retry,
                    )
                }
            }
        }
        SyncCommand::Fetch(args) => command_result("sync fetch", request_id, |cwd, request_id| {
            sync_fetch_peer(&cwd, request_id, &args.remote, false)
        }),
        SyncCommand::Pull(args) => command_result("sync pull", request_id, |cwd, request_id| {
            ensure_clean_for_sync_import_materialize(&cwd).context("preflight sync pull")?;
            sync_fetch_peer(&cwd, request_id, &args.remote, true)
        }),
        SyncCommand::Push(args) => command_result("sync push", request_id, |cwd, request_id| {
            sync_push_peer(&cwd, request_id, &args.remote)
        }),
    }
}

fn sync_fetch_peer(
    cwd: &Path,
    request_id: Option<String>,
    remote: &Path,
    materialize: bool,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let (local_manifest, remote_manifest, _peer_locks) = peer_manifests(cwd, remote)?;
    ensure_peer_compatible(
        &remote_manifest,
        &local_manifest,
        if materialize { "pull" } else { "fetch" },
    )?;
    if !forge_sync::manifest_head_descends_from(
        &remote_manifest,
        local_manifest.native_head.as_deref(),
    )? {
        if forge_sync::manifest_head_descends_from(
            &local_manifest,
            remote_manifest.native_head.as_deref(),
        )? {
            let direction = if materialize { "pull" } else { "fetch" };
            let command = if materialize {
                "sync pull"
            } else {
                "sync fetch"
            };
            let operation_id = if request_id.is_some() {
                Some(
                    forge_store::record_sync_request_marker(
                        cwd, request_id, command, direction, remote, None,
                    )
                    .with_context(|| format!("record local {command} request-id marker"))?
                    .operation_id,
                )
            } else {
                None
            };
            return Ok((
                operation_id,
                json!({
                    "protocol_version": remote_manifest.protocol_version,
                    "direction": direction,
                    "remote_path": remote.display().to_string(),
                    "base_native_head": local_manifest.native_head,
                    "remote_native_head": remote_manifest.native_head,
                    "imported_native_objects": 0,
                    "imported_ledger_rows": 0,
                    "materialized": false,
                    "up_to_date": true,
                }),
                Vec::new(),
            ));
        }
        return record_sync_peer_merge_conflict(SyncPeerMergeConflictInput {
            receiver_cwd: cwd,
            request_id,
            source: &remote_manifest,
            receiver: &local_manifest,
            remote,
            context: if materialize {
                "sync_pull_divergence"
            } else {
                "sync_fetch_divergence"
            },
            command: if materialize {
                "sync pull"
            } else {
                "sync fetch"
            },
            direction: if materialize { "pull" } else { "fetch" },
        });
    }

    let temp = SyncTransferDir::new(cwd)?;
    let local_base_path = temp.path().join("local-base.json");
    write_sync_manifest(&local_base_path, &local_manifest)?;
    let incoming_path = temp.path().join("incoming.json");
    let export_report =
        forge_sync::export_manifest_since(remote, &incoming_path, Some(&local_base_path))
            .context("export remote sync delta")?;
    let import_report =
        forge_sync::import_manifest(cwd, &incoming_path).context("apply remote sync delta")?;

    let mut operation_id = None;
    let mut materialized_content_ref = None;
    let mut materialized_operation_id = None;
    let mut materialized_view_id = None;
    if materialize {
        let commit_id = import_report
            .native_head
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("sync peer has no native head to materialize"))?;
        let content_ref = forge_store::checkout_target_content_ref(cwd, commit_id)
            .context("resolve fetched native head")?;
        restore_effective_worktree(cwd, &content_ref).context("restore fetched native head")?;
        let record =
            forge_store::record_sync_import_materialized(cwd, request_id, commit_id, &content_ref)
                .context("record sync pull materialization")?;
        operation_id = Some(record.operation_id.clone());
        materialized_operation_id = Some(record.operation_id);
        materialized_view_id = Some(record.view_id);
        materialized_content_ref = Some(content_ref);
    }

    let data = json!({
        "protocol_version": import_report.protocol_version,
        "direction": if materialize { "pull" } else { "fetch" },
        "remote_path": remote.display().to_string(),
        "base_native_head": local_manifest.native_head,
        "remote_native_head": remote_manifest.native_head,
        "exported_native_objects": export_report.native_object_count,
        "exported_native_payloads": export_report.native_payload_count,
        "exported_ledger_rows": export_report.ledger_row_count,
        "imported_native_objects": import_report.imported_native_objects,
        "imported_ledger_rows": import_report.imported_ledger_rows,
        "local_key_fingerprint": import_report.local_key_fingerprint,
        "materialized": materialize,
        "materialized_content_ref": materialized_content_ref,
        "materialized_operation_id": materialized_operation_id,
        "materialized_view_id": materialized_view_id,
    });
    Ok((operation_id, data, Vec::new()))
}

fn sync_push_peer(
    cwd: &Path,
    request_id: Option<String>,
    remote: &Path,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let (local_manifest, remote_manifest, _peer_locks) = peer_manifests(cwd, remote)?;
    ensure_peer_compatible(&local_manifest, &remote_manifest, "push")?;
    if !forge_sync::manifest_head_descends_from(
        &local_manifest,
        remote_manifest.native_head.as_deref(),
    )? {
        if forge_sync::manifest_head_descends_from(
            &remote_manifest,
            local_manifest.native_head.as_deref(),
        )? {
            let operation_id = if request_id.is_some() {
                Some(
                    forge_store::record_sync_request_marker(
                        cwd,
                        request_id,
                        "sync push",
                        "push",
                        remote,
                        None,
                    )
                    .context("record local sync push request-id marker")?
                    .operation_id,
                )
            } else {
                None
            };
            return Ok((
                operation_id,
                json!({
                    "protocol_version": local_manifest.protocol_version,
                    "direction": "push",
                    "remote_path": remote.display().to_string(),
                    "base_native_head": remote_manifest.native_head,
                    "local_native_head": local_manifest.native_head,
                    "imported_native_objects": 0,
                    "imported_ledger_rows": 0,
                    "materialized": false,
                    "up_to_date": true,
                }),
                Vec::new(),
            ));
        }
        let (remote_operation_id, mut data, warnings) =
            record_sync_peer_merge_conflict(SyncPeerMergeConflictInput {
                receiver_cwd: remote,
                request_id: None,
                source: &local_manifest,
                receiver: &remote_manifest,
                remote: cwd,
                context: "sync_push_divergence",
                command: "sync push",
                direction: "push",
            })?;
        let local_operation_id = if request_id.is_some() {
            let marker = forge_store::record_sync_request_marker(
                cwd,
                request_id,
                "sync push",
                "push",
                remote,
                remote_operation_id.as_deref(),
            )
            .context("record local sync push request-id marker")?;
            if let Some(object) = data.as_object_mut() {
                object.insert(
                    "remote_operation_id".to_string(),
                    json!(remote_operation_id),
                );
                object.insert("operation_id".to_string(), json!(marker.operation_id));
            }
            Some(marker.operation_id)
        } else {
            remote_operation_id
        };
        return Ok((local_operation_id, data, warnings));
    }

    let temp = SyncTransferDir::new(cwd)?;
    let remote_base_path = temp.path().join("remote-base.json");
    write_sync_manifest(&remote_base_path, &remote_manifest)?;
    let outgoing_path = temp.path().join("outgoing.json");
    let export_report =
        forge_sync::export_manifest_since(cwd, &outgoing_path, Some(&remote_base_path))
            .context("export local sync delta")?;
    let import_report =
        forge_sync::import_manifest(remote, &outgoing_path).context("apply local sync delta")?;

    let mut operation_id = None;
    let data = json!({
        "protocol_version": import_report.protocol_version,
        "direction": "push",
        "remote_path": remote.display().to_string(),
        "base_native_head": remote_manifest.native_head,
        "local_native_head": local_manifest.native_head,
        "exported_native_objects": export_report.native_object_count,
        "exported_native_payloads": export_report.native_payload_count,
        "exported_ledger_rows": export_report.ledger_row_count,
        "imported_native_objects": import_report.imported_native_objects,
        "imported_ledger_rows": import_report.imported_ledger_rows,
        "local_key_fingerprint": import_report.local_key_fingerprint,
        "materialized": false,
    });
    if request_id.is_some() {
        operation_id = Some(
            forge_store::record_sync_request_marker(
                cwd,
                request_id,
                "sync push",
                "push",
                remote,
                None,
            )
            .context("record local sync push request-id marker")?
            .operation_id,
        );
    }
    Ok((operation_id, data, Vec::new()))
}

struct PeerRepoLocks {
    _first: forge_store::RepoLock,
    _second: forge_store::RepoLock,
}

fn peer_manifests(
    cwd: &Path,
    remote: &Path,
) -> Result<(
    forge_sync::SyncManifest,
    forge_sync::SyncManifest,
    PeerRepoLocks,
)> {
    let local_context = forge_store::open_repository(cwd)?;
    let remote_context = forge_store::open_repository(remote)?;
    if local_context.root_path == remote_context.root_path {
        anyhow::bail!("sync peer remote must be a different repository");
    }
    let local_first = local_context.root_path <= remote_context.root_path;
    let first_path = if local_first {
        &local_context.root_path
    } else {
        &remote_context.root_path
    };
    let second_path = if local_first {
        &remote_context.root_path
    } else {
        &local_context.root_path
    };
    let first = forge_store::acquire_repository_lock(first_path)?;
    let second = forge_store::acquire_repository_lock(second_path)?;

    forge_store::reconcile_native_head(&local_context.root_path)?;
    forge_store::reconcile_native_head(&remote_context.root_path)?;
    let local_manifest = forge_sync::build_manifest(&local_context.root_path)?;
    let remote_manifest = forge_sync::build_manifest(&remote_context.root_path)?;
    Ok((
        local_manifest,
        remote_manifest,
        PeerRepoLocks {
            _first: first,
            _second: second,
        },
    ))
}

fn ensure_peer_compatible(
    source: &forge_sync::SyncManifest,
    receiver: &forge_sync::SyncManifest,
    action: &str,
) -> Result<()> {
    if source.content_backend != "native" || receiver.content_backend != "native" {
        anyhow::bail!("sync {action} requires native content repositories");
    }
    if source.repo_id != receiver.repo_id {
        anyhow::bail!(
            "sync {action} requires matching repo ids (source {}, receiver {})",
            source.repo_id,
            receiver.repo_id
        );
    }
    Ok(())
}

struct SyncPeerMergeConflictInput<'a> {
    receiver_cwd: &'a Path,
    request_id: Option<String>,
    source: &'a forge_sync::SyncManifest,
    receiver: &'a forge_sync::SyncManifest,
    remote: &'a Path,
    context: &'a str,
    command: &'a str,
    direction: &'a str,
}

fn record_sync_peer_merge_conflict(
    input: SyncPeerMergeConflictInput<'_>,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let base_head = forge_sync::manifest_common_ancestor_head(input.source, input.receiver)?
        .ok_or_else(|| ForgeError::SyncDivergenceUnsupported {
            direction: input.direction.to_string(),
            reason: "no_common_native_base".to_string(),
        })?;
    let base_content_ref = forge_sync::manifest_commit_content_ref(input.receiver, &base_head)
        .or_else(|_| forge_sync::manifest_commit_content_ref(input.source, &base_head))?;
    let ours_content_ref =
        forge_sync::manifest_head_content_ref(input.receiver)?.ok_or_else(|| {
            anyhow::anyhow!("sync {} receiver head has no content ref", input.direction)
        })?;
    let theirs_content_ref =
        forge_sync::manifest_head_content_ref(input.source)?.ok_or_else(|| {
            anyhow::anyhow!("sync {} source head has no content ref", input.direction)
        })?;
    let receiver_context = forge_store::open_repository(input.receiver_cwd)?;
    let store = forge_content_native::NativeObjectStore::new(&receiver_context.root_path);
    let pre_merge_loose_objects = store.loose_object_ids()?;
    let imported_native_objects =
        forge_sync::import_native_objects(input.receiver_cwd, input.source)
            .context("stage peer native objects for sync merge")?;
    let merge = match forge_content_native::merge_native_content_refs(
        &store,
        &base_content_ref,
        &ours_content_ref,
        &theirs_content_ref,
    ) {
        Ok(merge) => merge,
        Err(error) => {
            cleanup_new_native_objects(&store, &pre_merge_loose_objects)
                .context("remove staged native objects after failed sync merge")?;
            return Err(error).context("merge peer native content refs");
        }
    };
    if merge.merged_content_ref.is_some() {
        cleanup_new_native_objects(&store, &pre_merge_loose_objects)
            .context("remove staged native objects for unsupported clean sync merge")?;
        return Err(ForgeError::SyncDivergenceUnsupported {
            direction: input.direction.to_string(),
            reason: "clean_divergent_merge".to_string(),
        }
        .into());
    }
    let conflict_input = forge_store::MergeConflictInput {
        context: input.context.to_string(),
        proposal_id: None,
        base_head: Some(base_head),
        ours_head: input.receiver.native_head.clone(),
        base_content_ref,
        ours_content_ref,
        theirs_content_ref,
        conflicts: merge.conflicts,
    };
    let record = match forge_store::record_sync_merge_conflict(
        input.receiver_cwd,
        input.request_id,
        input.command,
        &conflict_input,
    ) {
        Ok(record) => record,
        Err(error) => {
            cleanup_new_native_objects(&store, &pre_merge_loose_objects)
                .context("remove staged native objects after failed sync conflict record")?;
            return Err(error).context("record sync merge conflict");
        }
    };
    Ok((
        Some(record.operation_id.clone()),
        json!({
            "protocol_version": input.source.protocol_version,
            "direction": input.direction,
            "remote_path": input.remote.display().to_string(),
            "merged": false,
            "conflict_set_id": record.conflict_set_id,
            "operation_id": record.operation_id,
            "base_native_head": input.receiver.native_head,
            "source_native_head": input.source.native_head,
            "imported_native_objects": imported_native_objects,
            "imported_ledger_rows": 0,
            "materialized": false,
        }),
        Vec::new(),
    ))
}

fn cleanup_new_native_objects(
    store: &forge_content_native::NativeObjectStore,
    before: &std::collections::BTreeSet<forge_content_native::ObjectId>,
) -> Result<()> {
    for id in store.loose_object_ids()?.difference(before) {
        store.delete_object(id)?;
    }
    Ok(())
}

fn write_sync_manifest(path: &Path, manifest: &forge_sync::SyncManifest) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(manifest)?)?;
    Ok(())
}

struct SyncTransferDir {
    path: PathBuf,
}

impl SyncTransferDir {
    fn new(cwd: &Path) -> Result<Self> {
        let context = forge_store::open_repository(cwd)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = context
            .root_path
            .join(".forge")
            .join("tmp")
            .join(format!("sync-peer-{}-{now}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SyncTransferDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn export_response(request_id: Option<String>, args: ExportArgs) -> ResponseEnvelope {
    match args.command {
        ExportCommand::VerifyBranch(args) => {
            command_result("export verify-branch", request_id, |cwd, _| {
                // Read-only: recompute the provenance digest from the local ledger and
                // confirm the published trailer matches (fail-closed PROVENANCE_MISMATCH /
                // EVIDENCE_TAMPERED). A PASS is trailer↔current-ledger consistency, not
                // cross-machine authenticity (NER-137 R7; see schema notes.provenance).
                let verification = forge_export_git::verify_publication_trailer(&cwd, &args.name)?;
                Ok((None, serde_json::to_value(verification)?, Vec::new()))
            })
        }
        ExportCommand::PrBody(args) => command_result("export pr-body", request_id, |cwd, _| {
            let (body, excluded) =
                forge_store::pr_body_for(&cwd, args.attempt.as_deref(), args.proposal.as_deref())?;
            Ok((
                None,
                json!({ "body": body }),
                secret_export_warnings(&excluded),
            ))
        }),
        ExportCommand::Branch(args) => {
            command_result("export branch", request_id, |cwd, request_id| {
                let proposal = forge_store::exportable_proposal(
                    &cwd,
                    args.attempt.as_deref(),
                    args.proposal.as_deref(),
                )?;
                match forge_store::decision_for_proposal_revision(
                    &cwd,
                    &proposal.proposal_revision_id,
                )?
                .as_deref()
                {
                    Some("accepted") => {}
                    Some("rejected") => return Err(ForgeError::Rejected.into()),
                    _ => return Err(ForgeError::NotAccepted.into()),
                }
                // Verify the accepted decision's integrity BEFORE creating the git
                // branch (NER-136 R4): a tampered decision row that forged `accepted`
                // is refused here, under the held repo lock, so no branch is created.
                forge_store::verify_decision_integrity(&cwd, &proposal.proposal_revision_id)?;
                forge_store::enforce_trust_policy(
                    &cwd,
                    forge_store::TrustPolicyAction::Export,
                    &proposal.proposal_revision_id,
                )?;
                let current_head = current_base(&cwd)?;
                // CLI-layer stale-base pre-check mirroring `accept`: persist the
                // divergence to `conflict_sets` under the held lock BEFORE bailing
                // (NER-133 U7). NER-138 slice 3: after commit-on-accept the ref-store HEAD
                // advances to the accepted proposal's OWN commit, so the expected current
                // head is that `commit_id` — not the proposal's `base_head`, which the accept
                // progressed past. Falls back to `base_head` for git repos / pre-006 accepts
                // (NULL commit_id), where the forge HEAD never moves. This CLI check is
                // authoritative; `export_branch`'s internal check is fed equal anchors below
                // so it never double-fires on the legitimate post-accept divergence.
                let expected_head = forge_store::accepted_commit_id_for_revision(
                    &cwd,
                    &proposal.proposal_revision_id,
                )?
                .unwrap_or_else(|| proposal.base_head.clone());
                if current_head != expected_head {
                    let base_content_ref = owner_base_content_ref(&cwd, &expected_head)?;
                    let ours_content_ref = owner_base_content_ref(&cwd, &current_head)?;
                    return Err(forge_store::StaleBaseConflict {
                        input: forge_store::StaleBaseConflictInput {
                            context: "stale_base_export".to_string(),
                            expected_head,
                            actual_head: current_head,
                            base_content_ref,
                            ours_content_ref,
                            theirs_content_ref: proposal.content_ref.clone(),
                            changed_paths: proposal.changed_paths.clone(),
                        },
                    }
                    .into());
                }
                // git-export interop adapter (NER-134): branch existence/creation is the
                // git-export *target* (publication), not worktree management — ROADMAP
                // keeps git export as interop, so this `forge_content_git::branch_exists`
                // intentionally remains. All worktree/base materialization now goes
                // through `ContentBackend`; this and the `GitContentBackend` constructors
                // in `selected_backend`/`backend_for_content_ref` are the only
                // `forge_content_git::` references left in core lifecycle code.
                if forge_store::publication_exists_for_branch(&cwd, &args.name)?
                    && forge_content_git::branch_exists(&cwd, &args.name)
                {
                    return Err(ForgeError::BranchExists {
                        name: args.name.clone(),
                    }
                    .into());
                }
                // Assemble the provenance trailer from the local ledger (NER-137):
                // this re-verifies the deciding evidence (R8 — EVIDENCE_TAMPERED fails
                // closed here, before the branch) and folds the deciding evidence
                // content_hashes + decision digest into a content-addressed digest the
                // published commit carries and `verify-branch` recomputes.
                let trailer =
                    forge_store::build_publication_trailer(&cwd, &proposal.proposal_revision_id)?;
                let message = forge_store::render_trailer_message(&trailer);
                // `base_head` is BOTH the synthesis source (the git parent is built from the
                // proposal's base tree) AND, passed again as `current_target`, makes
                // `export_branch`'s internal stale check a confirmed no-op — the CLI check
                // above is authoritative now that commit-on-accept advances HEAD off the base.
                let (commit_id, excluded) = forge_export_git::export_branch(
                    &cwd,
                    &args.name,
                    &proposal.base_head,
                    &proposal.base_head,
                    &proposal.content_ref,
                    &message,
                )?;
                let actor = resolve_actor(args.actor.as_deref());
                let publication = forge_store::record_publication(
                    &cwd,
                    request_id,
                    &proposal.proposal_id,
                    args.name,
                    commit_id,
                    &actor,
                )?;
                Ok((
                    Some(publication.operation_id.clone()),
                    serde_json::to_value(publication)?,
                    secret_export_warnings(&excluded),
                ))
            })
        }
    }
}

/// Build the replay response for an already-recorded `(repo, request_id)`
/// operation, preserving the command-aware and status-aware contract: a
/// different command is a `REQUEST_ID_CONFLICT`, a recorded failure replays the
/// failure, and a recorded success replays `idempotent_replay: true`. Shared by
/// the pre-flight check and the in-transaction `RequestIdReplay` path so both
/// give byte-identical replays.
fn replay_response(
    command: &'static str,
    request_id: Option<String>,
    existing: forge_store::RequestIdOperation,
) -> ResponseEnvelope {
    if existing.command != command {
        return ResponseEnvelope::error(
            command,
            request_id,
            None,
            ErrorObject::new(
                "REQUEST_ID_CONFLICT",
                format!("request id already used for command {}", existing.command),
            ),
        );
    }
    if existing.status == "failed" {
        // The code is read directly from the stored `error_json` (recorded by
        // `record_failed_operation`), never re-derived from the message — the
        // substring ladder is gone. Older rows without a stored code fall back to
        // COMMAND_FAILED.
        let error_json = existing.error_json.unwrap_or_default();
        let message = error_json
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "request id replayed failed operation".to_string());
        let code = error_json
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("COMMAND_FAILED")
            .to_string();
        // Re-attach the stored `details` so a replayed failure carries the SAME
        // structured payload as the first response (FIX C). Old rows recorded before
        // details were persisted lack the key — default to an empty object.
        let details = error_json
            .get("details")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        return ResponseEnvelope::error(
            command,
            request_id,
            Some(existing.operation_id),
            ErrorObject::new(code, message).with_details(details),
        );
    }
    let request_id_value = request_id.as_deref().unwrap_or_default().to_string();
    ResponseEnvelope::success(
        command,
        request_id,
        Some(existing.operation_id),
        json!({ "idempotent_replay": true, "request_id": request_id_value }),
    )
}

/// Format dropped secret-risk export paths as top-level `warnings[]` entries
/// (NER-133 U6), shared by every export egress surface so the message is uniform.
fn secret_export_warnings(excluded: &[String]) -> Vec<String> {
    excluded
        .iter()
        .map(|path| format!("excluded secret-risk path from export: {path}"))
        .collect()
}

fn command_result<F>(command: &'static str, request_id: Option<String>, f: F) -> ResponseEnvelope
where
    F: FnOnce(
        std::path::PathBuf,
        Option<String>,
    ) -> anyhow::Result<(Option<String>, Value, Vec<String>)>,
{
    let cwd = match env::current_dir().map_err(anyhow::Error::from) {
        Ok(cwd) => cwd,
        Err(error) => {
            return ResponseEnvelope::error(
                command,
                request_id,
                None,
                ErrorObject::new("COMMAND_FAILED", error.to_string()),
            )
        }
    };

    // Bring the schema to this binary's head BEFORE any per-command lock and
    // BEFORE the pre-flight replay check (NER-133 U4). `migrate` takes the repo
    // lock transiently only when a migration is pending and releases it before
    // returning, so it never nests inside the per-command lock or the `run` child
    // exec. A forward-versioned DB (HEAD+1) returns `UnknownSchemaVersion` here,
    // mapped to `SCHEMA_VERSION_UNSUPPORTED` and returned immediately — this MUST
    // short-circuit before `record_failed_operation` so the binary never writes
    // into a schema it is explicitly refusing.
    if let Err(error) = forge_store::migrate(&cwd) {
        let (error_object, retry) = error_to_object(command, &error);
        return ResponseEnvelope::error_with(command, request_id, None, error_object, retry);
    }

    // Hold the repo-level advisory write lock across the whole critical section
    // (determining reads + the write), so this command's CLI-layer reads — e.g.
    // `accept`'s `current_head`/`base_head` compare — are atomic against other
    // forge writers (NER-132). Acquired once, here; never nested. `run`, `init`,
    // and path-peer sync are excluded (see `requires_repo_lock`). Peer sync takes
    // both repository locks in canonical order inside its own critical section. A
    // contention timeout surfaces as the retryable `LOCK_TIMEOUT` code via the typed
    // `LockTimeout` downcast.
    let _repo_lock = if requires_repo_lock(command) {
        match forge_store::acquire_repo_lock(&cwd) {
            Ok(guard) => guard,
            Err(error) => {
                let (error_object, retry) = error_to_object(command, &error);
                return ResponseEnvelope::error_with(
                    command,
                    request_id,
                    None,
                    error_object,
                    retry,
                );
            }
        }
    } else {
        None
    };

    // NER-138 slice 3: heal a torn commit-on-accept (a committed decision whose ref-store
    // HEAD advance was lost to a crash) BEFORE the base anchor is read this command, and
    // BEFORE the preflight-replay short-circuit — so a same-`request_id` replay of a torn
    // accept still advances HEAD. Runs only for lock-holding commands (serialized, never
    // racing `run`); a no-op on git repos and before any justified commit. A dangling
    // ledger `commit_id` (the store-before-DB violation) surfaces here as
    // `NATIVE_HISTORY_CORRUPT`.
    if requires_repo_lock(command) {
        if let Err(error) = forge_store::reconcile_native_head(&cwd) {
            let (error_object, retry) = error_to_object(command, &error);
            return ResponseEnvelope::error_with(command, request_id, None, error_object, retry);
        }
    }

    // Pre-flight replay check: a sequential same-`request_id` retry replays the
    // original result without opening a write transaction. The concurrent race
    // (two retries that both pass this check before either commits) is closed by
    // the in-transaction `replay_guard` (U5), surfaced below as `RequestIdReplay`.
    if is_mutating_command(command) {
        if let Some(existing_request_id) = request_id.as_deref() {
            if let Some(existing) = forge_store::operation_for_request(&cwd, existing_request_id)
                .ok()
                .flatten()
            {
                return replay_response(command, request_id, existing);
            }
        }
    }

    let warning_cwd = cwd.clone();
    let result = f(cwd, request_id.clone());

    match result {
        Ok((operation_id, data, warnings)) => {
            let mut envelope = ResponseEnvelope::success(command, request_id, operation_id, data);
            envelope.warnings = warnings;
            if is_mutating_command(command) {
                if let Some(warning) = storage_budget_warning(&warning_cwd) {
                    envelope.warnings.push(warning);
                }
            }
            envelope
        }
        Err(error) => {
            // A concurrent same-`request_id` writer won the race: the in-txn
            // `replay_guard` rolled this attempt back. Replay the committed
            // operation instead of reporting a failure (U5, option a).
            if let Some(replay) = error.downcast_ref::<forge_store::RequestIdReplay>() {
                return replay_response(command, request_id, replay.operation.clone());
            }
            let (error_object, retry) = error_to_object(command, &error);
            // Transient errors (the singleton CAS `CONFLICT`, `LOCK_TIMEOUT`) must
            // NOT be persisted under the `--request-id` — a later retry of the same
            // id should re-execute, not replay a sticky failure (R7). Deterministic
            // domain failures keep the status-aware replay contract.
            let failed_operation_id = if let Some(stale_conflict) =
                error.downcast_ref::<forge_store::StaleBaseConflict>()
            {
                env::current_dir().ok().and_then(|cwd| {
                    forge_store::record_failed_operation_with_conflict(
                        &cwd,
                        request_id.clone(),
                        command,
                        &error_object.code,
                        &error_object.message,
                        error_object.details.clone(),
                        &stale_conflict.input,
                    )
                    .ok()
                    .map(|op| op.operation_id)
                })
            } else if is_mutating_command(command) && !is_transient_error(&error) {
                env::current_dir().ok().and_then(|cwd| {
                    forge_store::record_failed_operation(
                        &cwd,
                        request_id.clone(),
                        command,
                        &error_object.code,
                        &error_object.message,
                        // Carry the typed error's details so a replay reproduces them
                        // (FIX C). `error_object.details` is the empty object for
                        // untyped failures.
                        error_object.details.clone(),
                    )
                    .ok()
                    .map(|op| op.operation_id)
                })
            } else {
                None
            };
            ResponseEnvelope::error_with(
                command,
                request_id,
                failed_operation_id,
                error_object,
                retry,
            )
        }
    }
}

fn storage_budget_warning(cwd: &Path) -> Option<String> {
    let status = forge_store::storage_budget_status(cwd).ok()?;
    if !status.over_budget {
        return None;
    }
    Some(format!(
        "storage budget exceeded: used_bytes={} limit_bytes={} over_by_bytes={}",
        status.used_bytes, status.limit_bytes, status.over_by_bytes
    ))
}

fn init_response(request_id: Option<String>, args: InitArgs) -> ResponseEnvelope {
    let content_backend = args
        .content_backend
        .or_else(|| env::var("FORGE_CONTENT_BACKEND").ok())
        .unwrap_or_else(|| "git".to_string());
    match env::current_dir()
        .map_err(anyhow::Error::from)
        .and_then(|cwd| forge_store::init_repository(&cwd, request_id.clone(), content_backend))
    {
        Ok(repository) => ResponseEnvelope::success(
            "init",
            request_id,
            Some(repository.current_operation_id.clone()),
            serde_json::to_value(repository).unwrap(),
        ),
        Err(error) => {
            // init does not route through command_result, so map its errors here.
            // A contention timeout on the U5 init lock is the retryable LOCK_TIMEOUT;
            // a genuine "not a git repo" still maps to NOT_A_GIT_REPOSITORY (init's
            // un-masked classification, preserved through the typed mapping).
            let (error_object, retry) = error_to_object("init", &error);
            ResponseEnvelope::error_with("init", request_id, None, error_object, retry)
        }
    }
}

/// Emit the static `forge.cli.v0` machine contract. Deliberately does NOT route
/// through `command_result`: the contract is static and must work without a
/// repository (no `migrate`, no lock, no cwd dependency).
fn schema_response(request_id: Option<String>) -> ResponseEnvelope {
    ResponseEnvelope::success("schema", request_id, None, schema::contract())
}

/// Summarize the hardened redactor's per-occurrence kinds into one `warnings[]`
/// entry per detector class with a count (NER-136 §U4), so a leak that was redacted
/// before persistence is visible to the caller without re-emitting the secret.
fn redaction_warnings(redactions: &[forge_content::RedactionKind]) -> Vec<String> {
    use forge_content::RedactionKind;
    let mut counts: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    for kind in redactions {
        let label = match kind {
            RedactionKind::KeyValue => "key=value secret",
            RedactionKind::HighEntropyToken => "high-entropy token",
            RedactionKind::JsonSecret => "JSON-embedded secret",
            RedactionKind::PemPrivateKey => "PEM private key",
            RedactionKind::CredentialUrl => "credential URL password",
        };
        *counts.entry(label).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .map(|(label, count)| {
            format!("redacted {count} {label}(s) from captured output before persistence")
        })
        .collect()
}

/// Resolve the acting identity for the NER-136 actor model: the `--actor` flag, else
/// the `FORGE_ACTOR` environment variable, else `"unknown"`. This is *attribution*,
/// not authentication — the string is whatever the caller declares; Phase 5 protects
/// its integrity (it is folded into the tamper-evident digest), not its authenticity.
fn resolve_actor(flag: Option<&str>) -> String {
    flag.map(str::to_string)
        .or_else(|| env::var("FORGE_ACTOR").ok())
        .filter(|actor| !actor.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn selected_backend(cwd: &std::path::Path) -> anyhow::Result<Box<dyn ContentBackend>> {
    match forge_store::repository_content_backend(cwd)?.as_str() {
        "git" => Ok(Box::new(forge_content_git::GitContentBackend)),
        "native" => Ok(Box::new(forge_content_native::NativeContentBackend)),
        other => anyhow::bail!("unsupported content backend {other}"),
    }
}

fn snapshot_effective_worktree(cwd: &Path) -> anyhow::Result<SnapshotContent> {
    let context = forge_store::open_repository(cwd)?;
    match context.content_backend.as_str() {
        "git" => forge_content_git::GitContentBackend.snapshot_worktree(&context.root_path),
        "native" => forge_content_native::snapshot_worktree_into_store(
            &context.root_path,
            &context.worktree_path,
        ),
        other => anyhow::bail!("unsupported content backend {other}"),
    }
}

fn restore_effective_worktree(cwd: &Path, content_ref: &str) -> anyhow::Result<()> {
    let context = forge_store::open_repository(cwd)?;
    match classify_content_ref(content_ref) {
        ContentRefKind::GitTree(_) => {
            forge_content_git::GitContentBackend.restore_snapshot(&context.root_path, content_ref)
        }
        ContentRefKind::ForgeTree(_) => forge_content_native::restore_content_ref_to_worktree(
            &context.root_path,
            &context.worktree_path,
            content_ref,
        ),
        ContentRefKind::Unsupported => anyhow::bail!("unsupported content ref"),
    }
}

fn current_base(cwd: &Path) -> anyhow::Result<String> {
    let context = forge_store::open_repository(cwd)?;
    selected_backend(cwd)?.current_base(&context.root_path)
}

fn owner_base_content_ref(cwd: &Path, base: &str) -> anyhow::Result<String> {
    let context = forge_store::open_repository(cwd)?;
    selected_backend(cwd)?.base_content_ref(&context.root_path, base)
}

fn materialize_attempt_workspace(
    cwd: &Path,
    attempt_id: &str,
    content_ref: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let _worktree_lock = forge_store::acquire_worktree_lock(cwd, attempt_id)?;
    let workspace = forge_store::ensure_attempt_workspace_marker(cwd, attempt_id)?;
    match classify_content_ref(content_ref) {
        ContentRefKind::ForgeTree(_) => {
            let repo_root = forge_store::repository_root_path(cwd)?;
            forge_content_native::restore_content_ref_to_worktree(
                &repo_root,
                &workspace,
                content_ref,
            )?;
            forge_store::record_attempt_workspace_materialized(cwd, attempt_id, content_ref)?;
        }
        ContentRefKind::GitTree(_) => {}
        ContentRefKind::Unsupported => anyhow::bail!("unsupported content ref"),
    }
    Ok(workspace)
}

fn content_backend_label(kinds: &[ContentRefKind<'_>]) -> &'static str {
    let has_forge = kinds
        .iter()
        .any(|kind| matches!(kind, ContentRefKind::ForgeTree(_)));
    let has_git = kinds
        .iter()
        .any(|kind| matches!(kind, ContentRefKind::GitTree(_)));
    let has_unsupported = kinds
        .iter()
        .any(|kind| matches!(kind, ContentRefKind::Unsupported));
    match (has_forge, has_git, has_unsupported) {
        (true, false, false) => "native",
        (false, true, false) => "git",
        (false, false, true) => "unsupported",
        _ => "mixed",
    }
}

fn structured_error(
    command: impl Into<String>,
    request_id: Option<String>,
    code: impl Into<String>,
    message: impl Into<String>,
    details: Value,
) -> ResponseEnvelope {
    ResponseEnvelope::error(
        command,
        request_id,
        None,
        ErrorObject::new(code, message).with_details(details),
    )
}

fn print_human(response: &ResponseEnvelope) {
    if response.status == ResponseStatus::Success {
        match response.command.as_str() {
            "init" => {
                let root = response
                    .data
                    .get("root_path")
                    .and_then(Value::as_str)
                    .unwrap_or("<unknown>");
                let operation = response.operation_id.as_deref().unwrap_or("<unknown>");
                println!("Initialized Forge repository at {root}");
                println!("Current operation: {operation}");
            }
            "export pr-body" => {
                if let Some(body) = response.data.get("body").and_then(Value::as_str) {
                    print!("{body}");
                }
            }
            "schema" => {
                println!("{}", serde_json::to_string_pretty(&response.data).unwrap());
            }
            command => println!("{command} succeeded"),
        }
    } else if let Some(error) = response.errors.first() {
        eprintln!("forge {} failed: {}", response.command, error.message);
    }
}

/// Map a recovered error to its agent-visible `(ErrorObject, RetryMetadata)`.
///
/// No code is string-derived: a typed [`ForgeError`] supplies its own `code`,
/// `details`, and retry classification; the standalone [`forge_store::LockTimeout`]
/// sentinel maps to the retryable `LOCK_TIMEOUT`; everything else is an
/// untyped failure — `COMMAND_FAILED`, or `NOT_A_GIT_REPOSITORY` for a genuine
/// not-a-git-repo at `init` (the only place that classification is meaningful).
fn error_to_object(command: &str, error: &anyhow::Error) -> (ErrorObject, RetryMetadata) {
    let message = error.to_string();
    if let Some(stale_conflict) = error.downcast_ref::<forge_store::StaleBaseConflict>() {
        let forge_error = stale_conflict.forge_error();
        let retry = if forge_error.retryable() {
            RetryMetadata::retryable(forge_error.after_ms())
        } else {
            RetryMetadata::no()
        };
        return (
            ErrorObject::new(forge_error.code(), message).with_details(forge_error.details()),
            retry,
        );
    }
    if let Some(forge_error) = error.downcast_ref::<ForgeError>() {
        let retry = if forge_error.retryable() {
            RetryMetadata::retryable(forge_error.after_ms())
        } else {
            RetryMetadata::no()
        };
        return (
            ErrorObject::new(forge_error.code(), message).with_details(forge_error.details()),
            retry,
        );
    }
    if let Some(lock_timeout) = error.downcast_ref::<forge_store::LockTimeout>() {
        return (
            ErrorObject::new("LOCK_TIMEOUT", message)
                .with_details(json!({ "waited_ms": lock_timeout.waited_ms })),
            RetryMetadata::retryable(Some(RETRY_BACKOFF_MS)),
        );
    }
    let code = if command == "init" {
        "NOT_A_GIT_REPOSITORY"
    } else {
        "COMMAND_FAILED"
    };
    (ErrorObject::new(code, message), RetryMetadata::no())
}

/// Whether an error must NOT be persisted under its `--request-id`, so a retry
/// re-executes instead of replaying a sticky failure (R7). True for the transient
/// CAS (`CurrentStateChanged`) and a `LockTimeout`; false for deterministic
/// domain failures, which keep the status-aware replay contract.
fn is_transient_error(error: &anyhow::Error) -> bool {
    if let Some(forge_error) = error.downcast_ref::<ForgeError>() {
        return forge_error.retryable();
    }
    error.downcast_ref::<forge_store::LockTimeout>().is_some()
}

fn is_mutating_command(command: &str) -> bool {
    matches!(
        command,
        "init"
            | "start"
            | "attempt start"
            | "attempt attach"
            | "save"
            | "restore"
            | "run"
            | "propose"
            | "check"
            | "merge"
            | "conflict resolve"
            | "accept"
            | "reject"
            | "export branch"
            | "checkout"
            | "undo"
            | "trust policy"
            | "key status"
            | "key rotate"
            | "gc"
            | "sync import"
            | "sync clone"
            | "sync fetch"
            | "sync pull"
            | "sync push"
    )
}

/// Whether `command_result` should hold the repo-level advisory write lock across
/// this command's critical section (NER-132 U2). Excludes `run` — it executes its
/// child inside the closure and must not hold the lock (PRD §10.6) — and `init`,
/// which acquires the lock itself inside `init_repository` (it does not route
/// through `command_result`). Path-peer sync commands acquire both participating
/// repo locks in canonical root-path order inside `peer_manifests`, avoiding
/// opposite-direction lock inversion while keeping the same envelope/replay
/// behavior. The lock is acquired exactly once per command, never nested, per the
/// std file-locking re-entrancy caveat.
fn requires_repo_lock(command: &str) -> bool {
    is_mutating_command(command)
        && !matches!(
            command,
            "run" | "init" | "sync fetch" | "sync pull" | "sync push"
        )
}
