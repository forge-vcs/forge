use clap::{error::ErrorKind, Args, Parser, Subcommand};
use forge_content::{classify_content_ref, ContentBackend, ContentRefKind};
use forge_protocol::{ErrorObject, ResponseEnvelope, ResponseStatus};
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
    Save,
    Restore(RestoreArgs),
    Run(RunArgs),
    Propose,
    Check,
    Accept,
    Reject,
    Show,
    Doctor,
    Gc(GcArgs),
    Export(ExportArgs),
}

#[derive(Debug, Args)]
struct InitArgs {
    #[arg(long, value_parser = ["git", "native"])]
    content_backend: Option<String>,
}

#[derive(Debug, Args)]
struct IntentArgs {
    intent: Option<String>,
}

#[derive(Debug, Args)]
struct RestoreArgs {
    snapshot_id: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Args)]
struct RunArgs {
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
    Branch { name: String },
    PrBody,
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
        Command::Save => save_response(request_id),
        Command::Restore(args) if !args.yes => structured_error(
            "restore",
            request_id,
            "CONFIRMATION_REQUIRED",
            "restore requires --yes",
            json!({ "snapshot_id": args.snapshot_id }),
        ),
        Command::Restore(args) => restore_response(request_id, args),
        Command::Run(args) => run_response(request_id, args),
        Command::Propose => propose_response(request_id),
        Command::Check => check_response(request_id),
        Command::Accept => decision_response(request_id, "accepted"),
        Command::Reject => decision_response(request_id, "rejected"),
        Command::Show => show_response(request_id),
        Command::Doctor => doctor_response(request_id),
        Command::Gc(args) => gc_response(request_id, args),
        Command::Export(args) => export_response(request_id, args),
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
        [command, subcommand, ..] if command == "export" => format!("{command} {subcommand}"),
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
        let base_head = forge_content_git::current_head(&cwd)?;
        let started = forge_store::start_attempt(
            &cwd,
            request_id,
            args.intent
                .unwrap_or_else(|| "local agent attempt".to_string()),
            base_head,
        )?;
        Ok((
            Some(started.operation_id.clone()),
            serde_json::to_value(started)?,
        ))
    })
}

fn save_response(request_id: Option<String>) -> ResponseEnvelope {
    command_result("save", request_id, |cwd, request_id| {
        let content = selected_backend(&cwd)?.snapshot_worktree(&cwd)?;
        let saved = forge_store::save_snapshot(
            &cwd,
            request_id,
            content.content_ref,
            content.changed_paths,
        )?;
        Ok((
            Some(saved.operation_id.clone()),
            serde_json::to_value(saved)?,
        ))
    })
}

fn restore_response(request_id: Option<String>, args: RestoreArgs) -> ResponseEnvelope {
    command_result("restore", request_id, |cwd, request_id| {
        let content_ref = forge_store::snapshot_content_ref(&cwd, &args.snapshot_id)?;
        let current_content = selected_backend(&cwd)?.snapshot_worktree(&cwd)?;
        let latest_content_ref = forge_store::latest_snapshot_content_ref(&cwd)?;
        if latest_content_ref.as_deref() != Some(current_content.content_ref.as_str()) {
            anyhow::bail!("dirty worktree has unsaved changes");
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
        ))
    })
}

fn run_response(request_id: Option<String>, args: RunArgs) -> ResponseEnvelope {
    command_result("run", request_id, |cwd, request_id| {
        if args.command.is_empty() {
            anyhow::bail!("missing command after --");
        }
        let captured = forge_evidence::capture_with_timeout(&cwd, &args.command, args.timeout_ms)?;
        let recorded = forge_store::record_evidence(
            &cwd,
            request_id,
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
            },
        )?;
        Ok((
            Some(recorded.operation_id.clone()),
            serde_json::to_value(recorded)?,
        ))
    })
}

fn propose_response(request_id: Option<String>) -> ResponseEnvelope {
    command_result("propose", request_id, |cwd, request_id| {
        let proposal = forge_store::propose(&cwd, request_id)?;
        Ok((
            Some(proposal.operation_id.clone()),
            serde_json::to_value(proposal)?,
        ))
    })
}

fn check_response(request_id: Option<String>) -> ResponseEnvelope {
    command_result("check", request_id, |cwd, request_id| {
        let show = forge_store::show(&cwd)?;
        let latest_exit_code = show.latest_evidence.map(|e| e.exit_code);
        let evaluation = forge_policy::evaluate(latest_exit_code);
        let check =
            forge_store::record_check(&cwd, request_id, evaluation.status, evaluation.reason)?;
        Ok((
            Some(check.operation_id.clone()),
            serde_json::to_value(check)?,
        ))
    })
}

fn decision_response(request_id: Option<String>, decision: &'static str) -> ResponseEnvelope {
    command_result(decision_command(decision), request_id, |cwd, request_id| {
        if decision == "accepted" {
            let proposal = forge_store::latest_exportable_proposal(&cwd)?;
            let current_head = forge_content_git::current_head(&cwd)?;
            if current_head != proposal.base_head {
                anyhow::bail!("stale base");
            }
        }
        let record = forge_store::decide(&cwd, request_id, decision)?;
        Ok((
            Some(record.operation_id.clone()),
            serde_json::to_value(record)?,
        ))
    })
}

fn show_response(request_id: Option<String>) -> ResponseEnvelope {
    command_result("show", request_id, |cwd, _request_id| {
        let show = forge_store::show(&cwd)?;
        Ok((None, serde_json::to_value(show)?))
    })
}

fn doctor_response(request_id: Option<String>) -> ResponseEnvelope {
    command_result("doctor", request_id, |cwd, _request_id| {
        let report = forge_store::doctor(&cwd)?;
        Ok((None, serde_json::to_value(report)?))
    })
}

fn gc_response(request_id: Option<String>, args: GcArgs) -> ResponseEnvelope {
    command_result("gc", request_id, |cwd, _request_id| {
        if !args.dry_run {
            anyhow::bail!("gc only supports --dry-run in v0");
        }
        let report = forge_store::gc_dry_run(&cwd)?;
        Ok((None, serde_json::to_value(report)?))
    })
}

fn export_response(request_id: Option<String>, args: ExportArgs) -> ResponseEnvelope {
    match args.command {
        ExportCommand::PrBody => command_result("export pr-body", request_id, |cwd, _| {
            let body = forge_store::pr_body(&cwd)?;
            Ok((None, json!({ "body": body })))
        }),
        ExportCommand::Branch { name } => {
            command_result("export branch", request_id, |cwd, request_id| {
                let proposal = forge_store::latest_exportable_proposal(&cwd)?;
                match forge_store::decision_for_proposal_revision(
                    &cwd,
                    &proposal.proposal_revision_id,
                )?
                .as_deref()
                {
                    Some("accepted") => {}
                    Some("rejected") => anyhow::bail!("proposal was rejected"),
                    _ => anyhow::bail!("proposal is not accepted"),
                }
                let current_head = forge_content_git::current_head(&cwd)?;
                if forge_store::publication_exists_for_branch(&cwd, &name)?
                    && forge_content_git::branch_exists(&cwd, &name)
                {
                    anyhow::bail!("branch already exists");
                }
                let commit_id = forge_export_git::export_branch(
                    &cwd,
                    &name,
                    &proposal.base_head,
                    &current_head,
                    &proposal.content_ref,
                    "Forge accepted proposal",
                )?;
                let publication =
                    forge_store::record_publication(&cwd, request_id, name, commit_id)?;
                Ok((
                    Some(publication.operation_id.clone()),
                    serde_json::to_value(publication)?,
                ))
            })
        }
    }
}

fn command_result<F>(command: &'static str, request_id: Option<String>, f: F) -> ResponseEnvelope
where
    F: FnOnce(std::path::PathBuf, Option<String>) -> anyhow::Result<(Option<String>, Value)>,
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

    if is_mutating_command(command) {
        if let Some(existing_request_id) = request_id.clone() {
            if let Some(existing) = forge_store::operation_for_request(&cwd, &existing_request_id)
                .ok()
                .flatten()
            {
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
                    let message = existing
                        .error_json
                        .and_then(|value| {
                            value
                                .get("message")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .unwrap_or_else(|| "request id replayed failed operation".to_string());
                    return ResponseEnvelope::error(
                        command,
                        request_id,
                        Some(existing.operation_id),
                        ErrorObject::new(error_code(command, &message), message),
                    );
                }
                return ResponseEnvelope::success(
                    command,
                    request_id,
                    Some(existing.operation_id),
                    json!({ "idempotent_replay": true, "request_id": existing_request_id }),
                );
            }
        }
    }

    let result = f(cwd, request_id.clone());

    match result {
        Ok((operation_id, data)) => {
            ResponseEnvelope::success(command, request_id, operation_id, data)
        }
        Err(error) => {
            let message = error.to_string();
            let failed_operation_id = if is_mutating_command(command) {
                env::current_dir().ok().and_then(|cwd| {
                    forge_store::record_failed_operation(
                        &cwd,
                        request_id.clone(),
                        command,
                        &message,
                    )
                    .ok()
                    .map(|op| op.operation_id)
                })
            } else {
                None
            };
            ResponseEnvelope::error(
                command,
                request_id,
                failed_operation_id,
                ErrorObject::new(error_code(command, &message), message),
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
        Err(error) => structured_error(
            "init",
            request_id,
            "NOT_A_GIT_REPOSITORY",
            error.to_string(),
            Value::Object(Default::default()),
        ),
    }
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
            command => println!("{command} succeeded"),
        }
    } else if let Some(error) = response.errors.first() {
        eprintln!("forge {} failed: {}", response.command, error.message);
    }
}

fn error_code(command: &str, message: &str) -> &'static str {
    if message.contains("request id already used") {
        "REQUEST_ID_CONFLICT"
    } else if message.contains("not initialized") {
        "NOT_INITIALIZED"
    } else if message.contains("no active attempt") {
        "NO_ACTIVE_ATTEMPT"
    } else if message.contains("no snapshot") {
        "NO_SNAPSHOT"
    } else if message.contains("no proposal") {
        "NO_PROPOSAL"
    } else if message.contains("not accepted") {
        "NOT_ACCEPTED"
    } else if message.contains("rejected") {
        "REJECTED"
    } else if message.contains("branch already exists") {
        "BRANCH_EXISTS"
    } else if message.contains("stale base") {
        "STALE_BASE"
    } else if message.contains("dirty worktree") {
        "DIRTY_WORKTREE"
    } else if command == "init" {
        "NOT_A_GIT_REPOSITORY"
    } else {
        "COMMAND_FAILED"
    }
}

fn decision_command(decision: &str) -> &'static str {
    match decision {
        "accepted" => "accept",
        "rejected" => "reject",
        _ => "decision",
    }
}

fn is_mutating_command(command: &str) -> bool {
    matches!(
        command,
        "init"
            | "start"
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
