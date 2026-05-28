use anyhow::{Context, Result};
use forge_content::{redact_secret_like_text, SECRET_RISK_SENSITIVITY};
use serde::Serialize;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

pub const EXCERPT_LIMIT: usize = 4096;
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

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
    let (stdout_excerpt, stdout_truncated, stdout_redacted) = excerpt_file(&stdout_path)?;
    let (stderr_excerpt, stderr_truncated, stderr_redacted) = excerpt_file(&stderr_path)?;

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
        sensitivity: if stdout_redacted || stderr_redacted {
            SECRET_RISK_SENSITIVITY.to_string()
        } else {
            "normal".to_string()
        },
        visibility: "local".to_string(),
        trust: "locally_observed".to_string(),
    })
}

fn excerpt_file(path: &Path) -> Result<(String, bool, bool)> {
    let mut file = File::open(path)?;
    let mut bytes = vec![0; EXCERPT_LIMIT + 1];
    let read = file.read(&mut bytes)?;
    bytes.truncate(read.min(EXCERPT_LIMIT));
    let truncated = read > EXCERPT_LIMIT;
    let (redacted, was_redacted) = redact_secret_like_text(&String::from_utf8_lossy(&bytes));
    Ok((redacted, truncated, was_redacted))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
