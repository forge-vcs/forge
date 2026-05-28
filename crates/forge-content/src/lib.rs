use anyhow::Result;
use serde::Serialize;
use std::path::Path;

pub const SECRET_RISK_SENSITIVITY: &str = "secret_risk";

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotContent {
    pub content_ref: String,
    pub changed_paths: Vec<String>,
}

pub trait ContentBackend {
    fn snapshot_worktree(&self, repo_root: &Path) -> Result<SnapshotContent>;
    fn restore_snapshot(&self, repo_root: &Path, content_ref: &str) -> Result<()>;
}

pub fn is_secret_risk_path(path: &str) -> bool {
    let normalized = path.trim_start_matches("./");
    let filename = normalized.rsplit('/').next().unwrap_or(normalized);
    let lower = filename.to_ascii_lowercase();

    lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
        || lower == "id_rsa"
        || lower == "id_dsa"
        || lower == "id_ecdsa"
        || lower == "id_ed25519"
        || lower.contains("credential")
        || lower.contains("credentials")
        || lower.contains("secret")
}

pub fn redact_secret_like_text(text: &str) -> (String, bool) {
    let mut redacted_any = false;
    let mut redacted = Vec::new();

    for line in text.lines() {
        let Some(separator) = line.find(['=', ':']) else {
            redacted.push(line.to_string());
            continue;
        };
        let key = line[..separator].trim().to_ascii_lowercase();
        if is_secret_like_key(&key) {
            redacted_any = true;
            redacted.push(format!(
                "{}{}[REDACTED]",
                &line[..separator],
                &line[separator..=separator]
            ));
        } else {
            redacted.push(line.to_string());
        }
    }

    let mut output = redacted.join("\n");
    if text.ends_with('\n') {
        output.push('\n');
    }
    (output, redacted_any)
}

fn is_secret_like_key(key: &str) -> bool {
    key.contains("token")
        || key.contains("password")
        || key.contains("passwd")
        || key.contains("secret")
        || key.contains("api_key")
        || key.contains("apikey")
        || key.contains("access_key")
        || key.contains("private_key")
}
