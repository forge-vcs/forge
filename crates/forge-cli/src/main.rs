mod schema;

use clap::{error::ErrorKind, Args, Parser, Subcommand};
use forge_content::{classify_content_ref, ContentBackend, ContentRefKind};
use forge_protocol::{
    ErrorObject, ResponseEnvelope, ResponseStatus, RetryMetadata, RETRY_BACKOFF_MS,
};
use forge_store::ForgeError;
use serde_json::{json, Value};
use std::env;
use std::process::ExitCode;

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
    Doctor,
    Gc(GcArgs),
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
    /// proposals (via the git adapter): `--diff <attempt_a> <attempt_b>`.
    #[arg(long, num_args = 2, value_names = ["ATTEMPT_A", "ATTEMPT_B"])]
    diff: Option<Vec<String>>,
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
}

#[derive(Debug, Args)]
struct ExportArgs {
    #[command(subcommand)]
    command: ExportCommand,
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
        Command::Doctor => doctor_response(request_id),
        Command::Gc(args) => gc_response(request_id, args),
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
            if matches!(command.as_str(), "export" | "attempt" | "proposal") =>
        {
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
        let base_head = selected_backend(&cwd)?.current_base(&cwd)?;
        // Persist declared check gates on the intent (NER-135); competing attempts
        // under this intent inherit the same bar. None => default mode.
        let check_spec_json =
            forge_store::check_spec_json_from_requires(&args.require, &args.require_tests_pass);
        let started = forge_store::start_attempt(
            &cwd,
            request_id,
            args.intent
                .unwrap_or_else(|| "local agent attempt".to_string()),
            base_head,
            check_spec_json,
        )?;
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
                let base_head = selected_backend(&cwd)?.current_base(&cwd)?;
                let started = forge_store::start_attempt_for_intent(
                    &cwd,
                    request_id,
                    args.intent,
                    base_head,
                )?;
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
                let backend = selected_backend(&cwd)?;
                let target_base_head = forge_store::attempt_base_head(&cwd, &attempt_id)?;
                let current_content = backend.snapshot_worktree(&cwd)?;
                let resolved_current = forge_store::resolve_attempt(&cwd, None).ok();
                let latest_content_ref = match resolved_current {
                    Some(resolved) => forge_store::latest_snapshot_content_ref(
                        &cwd,
                        Some(&resolved.attempt.attempt_id),
                    )?
                    .or_else(|| {
                        backend
                            .base_content_ref(&cwd, &resolved.attempt.base_head)
                            .ok()
                    }),
                    None => {
                        let head = backend.current_base(&cwd)?;
                        Some(backend.base_content_ref(&cwd, &head)?)
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
                    None => backend.base_content_ref(&cwd, &target_base_head)?,
                };
                // Restore routes by the ref's own prefix: a `git-tree:` base ref is
                // materialized by the git backend even in a native repo (intentional
                // until the Phase 7 native walker; see ContentBackend::base_content_ref).
                backend_for_content_ref(&content_ref)?.restore_snapshot(&cwd, &content_ref)?;
                let attached = forge_store::attach_attempt(&cwd, request_id, &attempt_id)?;
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
/// diff between the two attempts' proposals (via the git adapter). Read-only: no
/// operation_id, no lock. Secret-risk changed paths are already dropped by the store;
/// any dropped paths in the pairwise diff surface as warnings.
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
            let tree_diff = forge_export_git::diff_trees(&cwd, &ref_a, &ref_b, true)?;
            warnings.extend(secret_export_warnings(&tree_diff.dropped_secret_paths));
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
            data["diff"] = serde_json::to_value(&tree_diff)?;
        }
        Ok((None, data, warnings))
    })
}

fn save_response(request_id: Option<String>, args: AttemptScopedArgs) -> ResponseEnvelope {
    command_result("save", request_id, |cwd, request_id| {
        // NER-134: verify the worktree binding BEFORE snapshotting, so a mismatch fails
        // fast without writing orphan content objects. `save_snapshot` re-checks
        // authoritatively on the write path; this returns the resolved attempt id, which
        // we pass back as an explicit selector.
        let resolved_attempt = forge_store::verify_save_target(&cwd, args.attempt.as_deref())?;
        let content = selected_backend(&cwd)?.snapshot_worktree(&cwd)?;
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

fn restore_response(request_id: Option<String>, args: RestoreArgs) -> ResponseEnvelope {
    command_result("restore", request_id, |cwd, request_id| {
        let content_ref = forge_store::snapshot_content_ref(&cwd, &args.snapshot_id)?;
        let current_content = selected_backend(&cwd)?.snapshot_worktree(&cwd)?;
        let latest_content_ref = forge_store::latest_snapshot_content_ref(&cwd, None)?;
        if latest_content_ref.as_deref() != Some(current_content.content_ref.as_str()) {
            return Err(ForgeError::DirtyWorktree {
                paths: current_content.changed_paths.clone(),
            }
            .into());
        }
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
        backend_for_content_ref(&content_ref)?.restore_snapshot(&cwd, &content_ref)?;
        let restored = forge_store::record_restore(&cwd, request_id, &args.snapshot_id)?;
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
        let captured = forge_evidence::capture_with_timeout(&cwd, &args.command, args.timeout_ms)?;
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
        let current_head = selected_backend(&cwd)?.current_base(&cwd)?;
        if current_head != proposal.base_head {
            // Persist the stale-base divergence under the held lock BEFORE bailing,
            // so the otherwise-unused `conflict_sets` table records it (NER-133 U7).
            // Metadata only — no merge engine. Best-effort: the conflict-set insert
            // must NEVER mask the domain error, so its Result is discarded and
            // STALE_BASE is always the surfaced error (FIX B). This CLI-layer read
            // runs under the held repo lock; the evidence gate runs in-txn inside
            // `decide` (NER-135), not here.
            let _ = forge_store::record_conflict_set(
                &cwd,
                "stale_base_accept",
                &proposal.base_head,
                &current_head,
                &proposal.changed_paths,
            );
            return Err(ForgeError::StaleBase {
                expected_head: proposal.base_head.clone(),
                actual_head: current_head,
            }
            .into());
        }
        // Evidence gate (NER-135 R6): enforced in-txn inside `decide` unless
        // --allow-unverified. On bypass, surface the non-passing status as a warning.
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

fn doctor_response(request_id: Option<String>) -> ResponseEnvelope {
    command_result("doctor", request_id, |cwd, _request_id| {
        let report = forge_store::doctor(&cwd)?;
        Ok((None, serde_json::to_value(report)?, Vec::new()))
    })
}

fn gc_response(request_id: Option<String>, args: GcArgs) -> ResponseEnvelope {
    command_result("gc", request_id, |cwd, _request_id| {
        if !args.dry_run {
            anyhow::bail!("gc only supports --dry-run in v0");
        }
        let report = forge_store::gc_dry_run(&cwd)?;
        Ok((None, serde_json::to_value(report)?, Vec::new()))
    })
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
                let current_head = selected_backend(&cwd)?.current_base(&cwd)?;
                // CLI-layer stale-base pre-check mirroring `accept`: persist the
                // divergence to `conflict_sets` under the held lock BEFORE bailing
                // (NER-133 U7). `export_branch` keeps its own internal stale-base
                // check as defense-in-depth; it just won't be reached on this path.
                if current_head != proposal.base_head {
                    // Best-effort metadata (FIX B): conflict-set persistence must
                    // never mask the domain error, so discard the Result and always
                    // surface STALE_BASE.
                    let _ = forge_store::record_conflict_set(
                        &cwd,
                        "stale_base_export",
                        &proposal.base_head,
                        &current_head,
                        &proposal.changed_paths,
                    );
                    return Err(ForgeError::StaleBase {
                        expected_head: proposal.base_head.clone(),
                        actual_head: current_head,
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
                let (commit_id, excluded) = forge_export_git::export_branch(
                    &cwd,
                    &args.name,
                    &proposal.base_head,
                    &current_head,
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
    // forge writers (NER-132). Acquired once, here; never nested. `run` and `init`
    // are excluded (see `requires_repo_lock`). A contention timeout surfaces as the
    // retryable `LOCK_TIMEOUT` code via the typed `LockTimeout` downcast.
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

    let result = f(cwd, request_id.clone());

    match result {
        Ok((operation_id, data, warnings)) => {
            let mut envelope = ResponseEnvelope::success(command, request_id, operation_id, data);
            envelope.warnings = warnings;
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
            let failed_operation_id = if is_mutating_command(command) && !is_transient_error(&error)
            {
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

fn backend_for_content_ref(content_ref: &str) -> anyhow::Result<Box<dyn ContentBackend>> {
    match classify_content_ref(content_ref) {
        ContentRefKind::GitTree(_) => Ok(Box::new(forge_content_git::GitContentBackend)),
        ContentRefKind::ForgeTree(_) => Ok(Box::new(forge_content_native::NativeContentBackend)),
        ContentRefKind::Unsupported => anyhow::bail!("unsupported content ref"),
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
            | "accept"
            | "reject"
            | "export branch"
    )
}

/// Whether `command_result` should hold the repo-level advisory write lock across
/// this command's critical section (NER-132 U2). Excludes `run` — it executes its
/// child inside the closure and must not hold the lock (PRD §10.6) — and `init`,
/// which acquires the lock itself inside `init_repository` (it does not route
/// through `command_result`). The lock is acquired exactly once per command, never
/// nested, per the std file-locking re-entrancy caveat.
fn requires_repo_lock(command: &str) -> bool {
    is_mutating_command(command) && !matches!(command, "run" | "init")
}
