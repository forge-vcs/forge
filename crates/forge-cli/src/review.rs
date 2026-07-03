use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use forge_protocol::ResponseEnvelope;
use forge_store::{ProposalReview, ReviewFactor, ReviewTerminalHandoff};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

#[derive(Debug, Args)]
pub(crate) struct ReviewArgs {
    #[command(subcommand)]
    command: ReviewCommand,
}

#[derive(Debug, Subcommand)]
enum ReviewCommand {
    /// Emit the machine-readable review aggregate for one proposal.
    Show(ReviewSelectorArgs),
    /// Export a self-contained static HTML review page.
    Export(ReviewExportArgs),
    /// Export and best-effort open a static HTML review page.
    Open(ReviewOpenArgs),
}

#[derive(Debug, Args)]
struct ReviewSelectorArgs {
    /// Proposal id to review.
    #[arg(long)]
    proposal: String,
    /// Optional projection recipient used for policy-backed projection checks.
    #[arg(long)]
    recipient: Option<String>,
}

#[derive(Debug, Args)]
struct ReviewExportArgs {
    /// Proposal id to review.
    #[arg(long)]
    proposal: String,
    /// Output HTML file path.
    #[arg(long)]
    output: PathBuf,
    /// Optional projection recipient used for policy-backed projection checks.
    #[arg(long)]
    recipient: Option<String>,
}

#[derive(Debug, Args)]
struct ReviewOpenArgs {
    /// Proposal id to review.
    #[arg(long)]
    proposal: String,
    /// Output HTML file path. Defaults to the OS temp directory.
    #[arg(long)]
    output: Option<PathBuf>,
    /// Optional projection recipient used for policy-backed projection checks.
    #[arg(long)]
    recipient: Option<String>,
    /// Export the page but skip browser launch.
    #[arg(long)]
    no_browser: bool,
}

pub(crate) fn review_response(request_id: Option<String>, args: ReviewArgs) -> ResponseEnvelope {
    match args.command {
        ReviewCommand::Show(args) => crate::command_result("review show", request_id, |cwd, _| {
            let review =
                forge_store::proposal_review(&cwd, &args.proposal, args.recipient.as_deref())?;
            Ok((None, serde_json::to_value(review)?, Vec::new()))
        }),
        ReviewCommand::Export(args) => {
            crate::command_result("review export", request_id, |cwd, _| {
                let review =
                    forge_store::proposal_review(&cwd, &args.proposal, args.recipient.as_deref())?;
                write_review_html(&args.output, &review)?;
                Ok((
                    None,
                    json!({
                        "output_path": args.output.to_string_lossy(),
                        "proposal_id": review.proposal.proposal_id,
                        "readiness": review.readiness.status,
                    }),
                    Vec::new(),
                ))
            })
        }
        ReviewCommand::Open(args) => crate::command_result("review open", request_id, |cwd, _| {
            let review =
                forge_store::proposal_review(&cwd, &args.proposal, args.recipient.as_deref())?;
            let output = args
                .output
                .unwrap_or_else(|| default_review_output_path(&review.proposal.proposal_id));
            write_review_html(&output, &review)?;
            let mut warnings = Vec::new();
            let opened = if args.no_browser {
                warnings.push("browser launch skipped by --no-browser".to_string());
                false
            } else {
                match open_in_browser(&output) {
                    Ok(()) => true,
                    Err(error) => {
                        warnings.push(format!(
                            "browser launch failed; exported review is at {}: {error}",
                            output.display()
                        ));
                        false
                    }
                }
            };
            Ok((
                None,
                json!({
                    "output_path": output.to_string_lossy(),
                    "opened": opened,
                    "proposal_id": review.proposal.proposal_id,
                    "readiness": review.readiness.status,
                }),
                warnings,
            ))
        }),
    }
}

fn write_review_html(path: &Path, review: &ProposalReview) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(path, render_review_html(review)).with_context(|| format!("write {}", path.display()))
}

fn render_review_html(review: &ProposalReview) -> String {
    let readiness_class = match review.readiness.status.as_str() {
        "ready" => "ready",
        "risky" => "risky",
        _ => "blocked",
    };
    let factors = review
        .readiness
        .deciding_factors
        .iter()
        .map(render_factor)
        .collect::<String>();
    let handoffs = review
        .terminal_handoffs
        .iter()
        .map(render_handoff)
        .collect::<String>();
    let attempts = review
        .lifecycle
        .sibling_attempts
        .iter()
        .map(|attempt| {
            format!(
                "<li><strong>{}</strong> {} - {} proposal(s)</li>",
                escape_html(&attempt.attempt_id),
                if attempt.is_owner { "(owner)" } else { "" },
                attempt.proposal_count
            )
        })
        .collect::<String>();
    let paths = review
        .diff
        .changed_paths
        .iter()
        .map(|path| {
            format!(
                "<li><code>{}</code> <span>{}</span></li>",
                escape_html(&path.path),
                escape_html(&path.status)
            )
        })
        .collect::<String>();
    let projection_checks = review
        .visibility
        .projection_checks
        .iter()
        .map(|decision| {
            format!(
                "<li><code>{}</code>: {} / {}</li>",
                escape_html(&decision.capability),
                if decision.allowed {
                    "allowed"
                } else {
                    "blocked"
                },
                escape_html(&decision.disclosure)
            )
        })
        .collect::<String>();
    let embargo = review
        .visibility
        .embargo
        .as_ref()
        .map(|embargo| {
            format!(
                "<p>Embargo: <strong>{}</strong>. release={} reveal={} publish={} export={}</p>",
                escape_html(&embargo.state),
                embargo.release_allowed,
                embargo.reveal_allowed,
                embargo.publish_allowed,
                embargo.export_allowed
            )
        })
        .unwrap_or_else(|| "<p>Embargo: none</p>".to_string());
    let check = optional_value(review.evidence_audit.latest_check.as_ref().map(|check| {
        format!(
            "{} ({})",
            escape_html(&check.status),
            escape_html(&check.reason)
        )
    }));
    let evidence = optional_value(
        review
            .evidence_audit
            .latest_evidence
            .as_ref()
            .map(|evidence| {
                format!(
                    "{} {} exit={} trust={}",
                    escape_html(&evidence.command),
                    escape_html(&evidence.args.join(" ")),
                    evidence.exit_code,
                    escape_html(&evidence.trust)
                )
            }),
    );
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Forge Review {proposal_id}</title>
<style>
:root {{ color-scheme: light; --border: #c9c4b5; --bg: #f7f7f3; --ink: #202124; --muted: #5f6368; --ready: #176b4d; --risky: #8a5a00; --blocked: #a32929; }}
* {{ box-sizing: border-box; }}
body {{ margin: 0; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; color: var(--ink); background: var(--bg); line-height: 1.45; }}
main {{ max-width: 1120px; margin: 0 auto; padding: 24px; }}
header, section {{ border-bottom: 1px solid var(--border); padding: 18px 0; }}
h1, h2 {{ margin: 0 0 10px; letter-spacing: 0; }}
h1 {{ font-size: 30px; }}
h2 {{ font-size: 20px; }}
.decision {{ display: grid; grid-template-columns: minmax(0, 1fr) minmax(260px, 360px); gap: 20px; align-items: start; }}
.status {{ display: inline-block; padding: 4px 8px; border-radius: 6px; color: white; font-weight: 700; }}
.ready {{ background: var(--ready); }}
.risky {{ background: var(--risky); }}
.blocked {{ background: var(--blocked); }}
dl {{ display: grid; grid-template-columns: minmax(130px, 220px) minmax(0, 1fr); gap: 8px 14px; }}
dt {{ color: var(--muted); }}
dd {{ margin: 0; overflow-wrap: anywhere; }}
ul {{ padding-left: 18px; }}
li {{ margin: 6px 0; }}
code {{ background: #ecebe4; padding: 2px 4px; border-radius: 4px; overflow-wrap: anywhere; }}
a:focus, button:focus, code:focus {{ outline: 2px solid #1a73e8; outline-offset: 2px; }}
@media (max-width: 760px) {{ main {{ padding: 16px; }} .decision, dl {{ display: block; }} dd {{ margin-bottom: 8px; }} }}
</style>
</head>
<body>
<main>
<header>
<h1>Forge Review</h1>
<p><span class="status {readiness_class}">{status}</span> {summary}</p>
</header>
<section class="decision" aria-labelledby="decision-console">
<div>
<h2 id="decision-console">Decision Console</h2>
<ul>{factors}</ul>
</div>
<aside aria-labelledby="terminal-handoff">
<h2 id="terminal-handoff">Terminal Handoff</h2>
<ul>{handoffs}</ul>
</aside>
</section>
<section aria-labelledby="story">
<h2 id="story">Work Package Story</h2>
<dl>
<dt>Proposal</dt><dd><code>{proposal_id}</code></dd>
<dt>Revision</dt><dd><code>{proposal_revision_id}</code></dd>
<dt>Intent</dt><dd>{intent}</dd>
<dt>Attempt</dt><dd><code>{attempt_id}</code></dd>
<dt>Check</dt><dd>{check_status}</dd>
<dt>Decision</dt><dd>{decision_status}</dd>
<dt>Publication</dt><dd>{publication_status}</dd>
</dl>
<h3>Attempt Context</h3>
<ul>{attempts}</ul>
</section>
<section aria-labelledby="visibility">
<h2 id="visibility">Visibility and Embargo</h2>
<p>Projection: <strong>{projection}</strong>; visibility: <strong>{visibility}</strong>; disclosure: <strong>{disclosure}</strong>.</p>
<p>Private path detail: {private_detail}; count: {private_count}</p>
{embargo}
<ul>{projection_checks}</ul>
</section>
<section aria-labelledby="audit">
<h2 id="audit">Evidence Audit</h2>
<dl>
<dt>Latest check</dt><dd>{check}</dd>
<dt>Latest evidence</dt><dd>{evidence}</dd>
<dt>Accept trust policy</dt><dd>{trust}</dd>
</dl>
</section>
<section aria-labelledby="diff">
<h2 id="diff">Diff and Content Review</h2>
<p>Content ref: <code>{content_ref}</code></p>
<ul>{paths}</ul>
</section>
</main>
</body>
</html>
"#,
        proposal_id = escape_html(&review.proposal.proposal_id),
        proposal_revision_id = escape_html(&review.proposal.proposal_revision_id),
        attempt_id = escape_html(&review.attempt.attempt_id),
        intent = escape_html(&review.intent.title),
        readiness_class = readiness_class,
        status = escape_html(&review.readiness.status),
        summary = escape_html(&review.readiness.summary),
        factors = factors,
        handoffs = if handoffs.is_empty() {
            "<li>No terminal action suggested by this review state.</li>".to_string()
        } else {
            handoffs
        },
        attempts = attempts,
        check_status = optional_value(review.lifecycle.check_status.as_deref().map(escape_html)),
        decision_status =
            optional_value(review.lifecycle.decision_status.as_deref().map(escape_html)),
        publication_status = optional_value(
            review
                .lifecycle
                .publication_status
                .as_deref()
                .map(escape_html)
        ),
        projection = escape_html(&review.visibility.projection),
        visibility = escape_html(&review.visibility.visibility),
        disclosure = escape_html(&review.visibility.disclosure),
        private_detail = escape_html(&review.visibility.private_path_detail),
        private_count = review.visibility.private_path_label_count,
        embargo = embargo,
        projection_checks = if projection_checks.is_empty() {
            "<li>Sanitized projection; no recipient-specific grants evaluated.</li>".to_string()
        } else {
            projection_checks
        },
        check = check,
        evidence = evidence,
        trust = escape_html(&review.evidence_audit.trust_policy.min_accept_trust),
        content_ref = escape_html(&review.diff.content_ref),
        paths = if paths.is_empty() {
            "<li>No changed paths recorded.</li>".to_string()
        } else {
            paths
        },
    )
}

fn render_factor(factor: &ReviewFactor) -> String {
    let source = factor
        .source
        .as_ref()
        .map(|source| format!(" <span>({})</span>", escape_html(source)))
        .unwrap_or_default();
    format!(
        "<li><strong>{}</strong>: {}{}</li>",
        escape_html(&factor.code),
        escape_html(&factor.message),
        source
    )
}

fn render_handoff(handoff: &ReviewTerminalHandoff) -> String {
    format!(
        "<li><strong>{}</strong><br><code>{}</code><br><span>{}</span></li>",
        escape_html(&handoff.label),
        escape_html(&handoff.command),
        escape_html(&handoff.reason)
    )
}

fn optional_value(value: Option<String>) -> String {
    value.unwrap_or_else(|| "none".to_string())
}

fn default_review_output_path(proposal_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "forge-review-{}.html",
        sanitize_filename(proposal_id)
    ))
}

fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn open_in_browser(path: &Path) -> Result<()> {
    let status = if cfg!(target_os = "macos") {
        ProcessCommand::new("open").arg(path).status()
    } else if cfg!(target_os = "windows") {
        ProcessCommand::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .status()
    } else {
        ProcessCommand::new("xdg-open").arg(path).status()
    }
    .with_context(|| format!("launch browser for {}", path.display()))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("browser command exited with {status}")
    }
}

fn escape_html(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            '\'' => "&#39;".to_string(),
            _ => ch.to_string(),
        })
        .collect()
}
