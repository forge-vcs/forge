pub mod parsers;

use anyhow::{Context, Result};
use forge_content::{redact_evidence_excerpt, RedactionKind, SECRET_RISK_SENSITIVITY};
use serde::Serialize;
use std::env;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

pub const EXCERPT_LIMIT: usize = 4096;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// Bytes read for the redaction pass before truncating to `EXCERPT_LIMIT`. Reading a
/// margin past the cap lets a secret straddling byte 4096 be redacted before the
/// excerpt is cut, so its prefix is never persisted (NER-136 §U4). Bounded so a
/// command emitting megabytes of output never reads its whole stream into memory.
const REDACTION_WINDOW: usize = EXCERPT_LIMIT * 4;

#[derive(Debug, Clone, Serialize)]
pub struct CapturedCommand {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub exit_code: i32,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub stdout_excerpt: String,
    pub stderr_excerpt: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub sensitivity: String,
    pub visibility: String,
    pub trust: String,
    /// Machine-readable outcome parsed from the full captured output (NER-136 §U5),
    /// serialized JSON. `None` when no tool-specific parser matched.
    pub structured_json: Option<String>,
    /// Every redaction the hardened redactor applied to the captured output, so the
    /// CLI can surface one `warnings[]` entry per redaction (NER-136 §U4).
    pub redactions: Vec<RedactionKind>,
}

pub fn capture(repo_root: &Path, argv: &[String]) -> Result<CapturedCommand> {
    capture_with_timeout(repo_root, argv, DEFAULT_TIMEOUT_MS)
}

pub fn capture_with_timeout(
    repo_root: &Path,
    argv: &[String],
    timeout_ms: u64,
) -> Result<CapturedCommand> {
    let (program, args) = argv.split_first().context("missing command after --")?;
    let temp_dir = tempfile::tempdir().context("create evidence temp dir")?;
    let stdout_path = temp_dir.path().join("stdout");
    let stderr_path = temp_dir.path().join("stderr");
    let stdout_file = File::create(&stdout_path).context("create stdout capture file")?;
    let stderr_file = File::create(&stderr_path).context("create stderr capture file")?;
    let started_at_ms = now_ms();
    let mut child = Command::new(program)
        .args(args)
        .current_dir(repo_root)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .with_context(|| format!("failed to run {program}"))?;
    let deadline = started_at_ms + timeout_ms as i64;
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait()? {
            break (status, false);
        }
        if now_ms() >= deadline {
            let _ = child.kill();
            let status = child.wait()?;
            break (status, true);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let ended_at_ms = now_ms();
    let (stdout_excerpt, stdout_truncated, mut redactions) = excerpt_file(&stdout_path, repo_root)?;
    let (stderr_excerpt, stderr_truncated, stderr_redactions) =
        excerpt_file(&stderr_path, repo_root)?;
    redactions.extend(stderr_redactions);

    // Structured parse over the FULL captured output (the summary line often sits past
    // the 4096 excerpt cap), serialized to persist alongside the excerpt (NER-136 §U5).
    let structured = parsers::parse_structured(
        program,
        args,
        &read_for_parsing(&stdout_path)?,
        &read_for_parsing(&stderr_path)?,
    );
    let structured_json = structured
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serialize structured outcome")?;

    Ok(CapturedCommand {
        command: program.clone(),
        args: args.to_vec(),
        cwd: repo_root.to_string_lossy().into_owned(),
        exit_code: status.code().unwrap_or(-1),
        started_at_ms,
        ended_at_ms,
        stdout_excerpt,
        stderr_excerpt,
        stdout_truncated,
        stderr_truncated,
        timed_out,
        sensitivity: if redactions.is_empty() {
            "normal".to_string()
        } else {
            SECRET_RISK_SENSITIVITY.to_string()
        },
        visibility: "local".to_string(),
        // The trust-ladder rung. Now backed by a verifiable content hash the store
        // computes (NER-136) rather than asserted as a bare string; higher rungs
        // (signed/attested) are Phase 9.
        trust: "locally_observed".to_string(),
        structured_json,
        redactions,
    })
}

/// Bytes read for the structured parse. The summary lives at the END of cargo output,
/// so for a large stream read only the tail — bounded so a verbose command never
/// reads its whole output into memory.
const PARSE_LIMIT: u64 = 1 << 20;

/// Read up to the last `PARSE_LIMIT` bytes of a capture file for parsing.
fn read_for_parsing(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len > PARSE_LIMIT {
        file.seek(SeekFrom::Start(len - PARSE_LIMIT))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Read a bounded redaction window, redact it, then truncate the redacted text to
/// `EXCERPT_LIMIT` so a secret straddling byte 4096 is removed before its prefix is
/// persisted (NER-136 §U4). The hash is computed over this persisted (redacted +
/// truncated) excerpt, so verification recomputes from the same bytes.
fn excerpt_file(path: &Path, repo_root: &Path) -> Result<(String, bool, Vec<RedactionKind>)> {
    let mut file = File::open(path)?;
    let mut bytes = vec![0; REDACTION_WINDOW + 1];
    let read = file.read(&mut bytes)?;
    bytes.truncate(read.min(REDACTION_WINDOW));
    // The excerpt is truncated whenever the raw output exceeded the persisted cap,
    // independent of the larger redaction window.
    let truncated = read > EXCERPT_LIMIT;
    let (redacted, mut redactions) = redact_evidence_excerpt(&String::from_utf8_lossy(&bytes));
    let (redacted, local_path_redacted) = redact_local_worktree_paths(&redacted, repo_root);
    if local_path_redacted {
        redactions.push(RedactionKind::LocalPath);
    }
    let excerpt = truncate_to_bytes(&redacted, EXCERPT_LIMIT);
    Ok((excerpt, truncated, redactions))
}

fn redact_local_worktree_paths(text: &str, repo_root: &Path) -> (String, bool) {
    let mut candidates = Vec::new();
    push_path_candidate(&mut candidates, repo_root);
    if let Ok(canonical) = repo_root.canonicalize() {
        push_path_candidate(&mut candidates, &canonical);
    }
    if repo_root.is_relative() {
        if let Ok(current_dir) = env::current_dir() {
            push_path_candidate(&mut candidates, &current_dir.join(repo_root));
        }
        if let Ok(pwd) = env::var("PWD") {
            push_path_candidate(&mut candidates, &PathBuf::from(pwd).join(repo_root));
        }
    }

    let mut output = text.to_string();
    let mut redacted = false;
    for candidate in candidates {
        if output.contains(&candidate) {
            output = output.replace(&candidate, "[REPO_ROOT]");
            redacted = true;
        }
    }
    (output, redacted)
}

fn push_path_candidate(candidates: &mut Vec<String>, path: &Path) {
    let normalized: PathBuf = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect();
    let candidate = normalized.to_string_lossy().into_owned();
    push_candidate_string(candidates, candidate.clone());
    if let Some(rest) = candidate.strip_prefix("/private/tmp/") {
        push_candidate_string(candidates, format!("/tmp/{rest}"));
    } else if let Some(rest) = candidate.strip_prefix("/tmp/") {
        push_candidate_string(candidates, format!("/private/tmp/{rest}"));
    }
}

fn push_candidate_string(candidates: &mut Vec<String>, candidate: String) {
    if candidate.len() > 1 && !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

/// Truncate a string to at most `limit` bytes on a UTF-8 char boundary.
fn truncate_to_bytes(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
