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
                "<li class=\"list-row\"><code>{}</code><span>{}{} proposal(s)</span></li>",
                escape_html(&attempt.attempt_id),
                if attempt.is_owner { "owner - " } else { "" },
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
                "<li class=\"list-row\"><code>{}</code><span class=\"tag\">{}</span></li>",
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
                "<li class=\"list-row\"><code>{}</code><span>{} / {}</span></li>",
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
                "<div class=\"notice\"><span>Embargo</span><strong>{}</strong><small>release={} reveal={} publish={} export={}</small></div>",
                escape_html(&embargo.state),
                embargo.release_allowed,
                embargo.reveal_allowed,
                embargo.publish_allowed,
                embargo.export_allowed
            )
        })
        .unwrap_or_else(|| {
            "<div class=\"notice\"><span>Embargo</span><strong>none</strong><small>No embargo workflow is active.</small></div>".to_string()
        });
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
:root {{
  color-scheme: light;
  --bg: #f5f6f3;
  --surface: #ffffff;
  --surface-soft: #f0f4f1;
  --ink: #17191c;
  --muted: #5d6670;
  --subtle: #737b84;
  --line: #d8ddd7;
  --line-strong: #bcc7c0;
  --ready: #116149;
  --ready-soft: #e3f3ec;
  --risky: #8a5a00;
  --risky-soft: #fff2cc;
  --blocked: #aa2e2e;
  --blocked-soft: #fde7e7;
  --info: #255f99;
  --info-soft: #e6f0fa;
  --shadow: 0 18px 46px rgba(23, 25, 28, 0.08);
}}
* {{ box-sizing: border-box; }}
html, body {{ min-width: 0; overflow-x: hidden; }}
body {{
  margin: 0;
  font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  color: var(--ink);
  background:
    linear-gradient(180deg, #eef2ee 0, var(--bg) 360px),
    var(--bg);
  line-height: 1.45;
}}
main {{ width: min(1180px, 100%); margin: 0 auto; padding: 28px 24px 44px; }}
.hero {{
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(240px, 360px);
  gap: 24px;
  align-items: stretch;
  padding: 24px;
  border: 1px solid var(--line-strong);
  border-radius: 8px;
  background: var(--surface);
  box-shadow: var(--shadow);
}}
.eyebrow {{ margin: 0 0 6px; color: var(--muted); font-size: 12px; font-weight: 800; letter-spacing: 0.08em; text-transform: uppercase; }}
h1, h2, h3, p {{ letter-spacing: 0; }}
h1 {{ margin: 0; font-size: 34px; line-height: 1.1; }}
h2 {{ margin: 0; font-size: 21px; line-height: 1.2; }}
h3 {{ margin: 18px 0 10px; font-size: 15px; }}
p {{ margin: 0; }}
.summary {{ margin-top: 14px; max-width: 760px; color: var(--muted); font-size: 17px; }}
.status {{
  display: inline-flex;
  align-items: center;
  min-height: 28px;
  padding: 4px 10px;
  border-radius: 6px;
  color: white;
  font-size: 14px;
  font-weight: 800;
  text-transform: uppercase;
}}
.ready {{ background: var(--ready); }}
.risky {{ background: var(--risky); }}
.blocked {{ background: var(--blocked); }}
.hero-aside {{
  display: grid;
  gap: 12px;
  align-content: start;
  padding: 18px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface-soft);
  min-width: 0;
}}
.hero-aside span {{ color: var(--muted); font-size: 12px; font-weight: 800; text-transform: uppercase; }}
.hero-aside code {{ display: block; }}
.hero-aside strong {{ display: block; overflow-wrap: anywhere; }}
.layout {{ display: grid; grid-template-columns: minmax(0, 1fr) 360px; gap: 20px; margin-top: 20px; align-items: start; }}
.stack, .side-stack {{ display: grid; gap: 16px; min-width: 0; }}
section, aside.panel {{
  min-width: 0;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface);
  padding: 20px;
}}
.section-head {{ display: flex; justify-content: space-between; gap: 12px; align-items: flex-start; margin-bottom: 14px; }}
.section-head p {{ margin-top: 5px; color: var(--muted); }}
.factor-list, .handoff-list, .plain-list {{ display: grid; gap: 10px; margin: 0; padding: 0; list-style: none; }}
.factor {{
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 10px;
  align-items: start;
  padding: 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: #fbfcfb;
}}
.factor strong, .handoff strong {{ display: block; overflow-wrap: anywhere; }}
.factor span:last-child, .handoff span, .list-row span {{ color: var(--muted); }}
.factor small {{ display: block; margin: 2px 0 4px; color: var(--subtle); font-size: 12px; font-weight: 700; text-transform: uppercase; }}
.pill, .tag {{
  display: inline-flex;
  align-items: center;
  width: fit-content;
  max-width: 100%;
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 12px;
  font-weight: 800;
  text-transform: uppercase;
  overflow-wrap: anywhere;
}}
.pill.info {{ color: var(--info); background: var(--info-soft); }}
.pill.risk {{ color: var(--risky); background: var(--risky-soft); }}
.pill.blocker {{ color: var(--blocked); background: var(--blocked-soft); }}
.tag {{ color: var(--info); background: var(--info-soft); text-transform: none; }}
.handoff {{
  padding: 14px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: #fbfcfb;
}}
.command {{
  display: block;
  width: 100%;
  margin: 8px 0;
  padding: 10px;
  border: 1px solid #dfe4df;
  border-radius: 6px;
  background: #eef1ee;
  color: #1f2933;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
  font-size: 13px;
  line-height: 1.45;
  white-space: pre-wrap;
  word-break: break-word;
  overflow-wrap: anywhere;
}}
dl {{ display: grid; grid-template-columns: minmax(120px, 180px) minmax(0, 1fr); gap: 10px 14px; margin: 0; }}
dt {{ color: var(--muted); }}
dd {{ margin: 0; min-width: 0; overflow-wrap: anywhere; }}
.list-row {{ display: flex; flex-wrap: wrap; gap: 8px 10px; align-items: center; min-width: 0; overflow-wrap: anywhere; }}
.notice {{
  display: grid;
  gap: 4px;
  margin: 12px 0;
  padding: 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: var(--surface-soft);
}}
.notice span {{ color: var(--muted); font-size: 12px; font-weight: 800; text-transform: uppercase; }}
.notice small {{ color: var(--muted); overflow-wrap: anywhere; }}
code {{
  max-width: 100%;
  min-width: 0;
  padding: 2px 5px;
  border-radius: 5px;
  background: #eef1ee;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace;
  font-size: 0.92em;
  white-space: pre-wrap;
  word-break: break-word;
  overflow-wrap: anywhere;
}}
a:focus, button:focus, code:focus {{ outline: 2px solid #1a73e8; outline-offset: 2px; }}
@media (max-width: 900px) {{
  main {{ padding: 18px 14px 32px; }}
  .hero, .layout {{ grid-template-columns: minmax(0, 1fr); }}
  .layout {{ gap: 16px; }}
  aside.panel {{ order: -1; }}
}}
@media (max-width: 560px) {{
  h1 {{ font-size: 28px; }}
  .summary {{ font-size: 15px; }}
  .hero, section, aside.panel {{ padding: 16px; }}
  .section-head {{ display: block; }}
  .factor {{ grid-template-columns: minmax(0, 1fr); }}
  dl {{ display: block; }}
  dt {{ margin-top: 10px; }}
  dd {{ margin-top: 3px; }}
}}
</style>
</head>
<body>
<main>
<header class="hero">
<div>
<p class="eyebrow">Proposal Review</p>
<h1>Forge Review</h1>
<p class="summary"><span class="status {readiness_class}">{status}</span> {summary}</p>
</div>
<div class="hero-aside" aria-label="Review identity">
<div><span>Proposal</span><code>{proposal_id}</code></div>
<div><span>Attempt</span><code>{attempt_id}</code></div>
<div><span>Intent</span><strong>{intent}</strong></div>
</div>
</header>
<div class="layout">
<div class="stack">
<section class="decision" aria-labelledby="decision-console">
<div class="section-head">
<div>
<h2 id="decision-console">Decision Console</h2>
<p>Readiness factors derived from check, evidence, trust, visibility, and lifecycle state.</p>
</div>
</div>
<ul class="factor-list">{factors}</ul>
</section>
<section aria-labelledby="story">
<div class="section-head">
<div>
<h2 id="story">Work Package Story</h2>
<p>Where this proposal sits in the Forge lifecycle.</p>
</div>
</div>
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
<ul class="plain-list">{attempts}</ul>
</section>
<section aria-labelledby="audit">
<div class="section-head">
<div>
<h2 id="audit">Evidence Audit</h2>
<p>The proof material that backs the review conclusion.</p>
</div>
</div>
<dl>
<dt>Latest check</dt><dd>{check}</dd>
<dt>Latest evidence</dt><dd>{evidence}</dd>
<dt>Accept trust policy</dt><dd>{trust}</dd>
</dl>
</section>
<section aria-labelledby="diff">
<div class="section-head">
<div>
<h2 id="diff">Diff and Content Review</h2>
<p>Projection-safe content summary for this proposal.</p>
</div>
</div>
<dl>
<dt>Content ref</dt><dd><code>{content_ref}</code></dd>
</dl>
<ul class="plain-list">{paths}</ul>
</section>
</div>
<div class="side-stack">
<aside class="panel" aria-labelledby="terminal-handoff">
<div class="section-head">
<div>
<h2 id="terminal-handoff">Terminal Handoff</h2>
<p>Trust-bearing actions stay in the terminal.</p>
</div>
</div>
<ul class="handoff-list">{handoffs}</ul>
</aside>
<section aria-labelledby="visibility">
<div class="section-head">
<div>
<h2 id="visibility">Visibility and Embargo</h2>
<p>What this local projection is allowed to reveal.</p>
</div>
</div>
<dl>
<dt>Projection</dt><dd><strong>{projection}</strong></dd>
<dt>Visibility</dt><dd><strong>{visibility}</strong></dd>
<dt>Disclosure</dt><dd><strong>{disclosure}</strong></dd>
<dt>Private paths</dt><dd>{private_detail}; count: {private_count}</dd>
</dl>
{embargo}
<ul class="plain-list">{projection_checks}</ul>
</section>
</div>
</div>
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
            "<li class=\"handoff\">No terminal action suggested by this review state.</li>"
                .to_string()
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
            "<li class=\"list-row\">Sanitized projection; no recipient-specific grants evaluated.</li>"
                .to_string()
        } else {
            projection_checks
        },
        check = check,
        evidence = evidence,
        trust = escape_html(&review.evidence_audit.trust_policy.min_accept_trust),
        content_ref = escape_html(&review.diff.content_ref),
        paths = if paths.is_empty() {
            "<li class=\"list-row\">No changed paths recorded.</li>".to_string()
        } else {
            paths
        },
    )
}

fn render_factor(factor: &ReviewFactor) -> String {
    let source = factor
        .source
        .as_ref()
        .map(|source| format!("<small>{}</small>", escape_html(source)))
        .unwrap_or_default();
    format!(
        "<li class=\"factor\"><span class=\"pill {severity}\">{severity}</span><span><strong>{}</strong>{}<span>{}</span></span></li>",
        escape_html(&factor.code),
        source,
        escape_html(&factor.message),
        severity = escape_html(&factor.severity)
    )
}

fn render_handoff(handoff: &ReviewTerminalHandoff) -> String {
    format!(
        "<li class=\"handoff\"><strong>{}</strong><code class=\"command\">{}</code><span>{}</span></li>",
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
