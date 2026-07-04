use anyhow::{Context, Result};
use forge_protocol::{ResponseEnvelope, ResponseStatus};
use serde_json::{json, Value};
use std::env;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use crate::{
    command_result, ensure_clean_for_sync_import_materialize, ensure_clean_worktree,
    error_to_object, restore_effective_worktree, secret_export_warnings,
    sync_manifest_head_content_ref, ForgeError, SyncArgs, SyncCommand, SyncServeArgs,
    SyncServeCommand,
};

pub(crate) fn sync_response(request_id: Option<String>, args: SyncArgs) -> ResponseEnvelope {
    match args.command {
        SyncCommand::Export(args) => command_result("sync export", request_id, |cwd, _| {
            let report = if let Some(recipient) = args.recipient.as_deref() {
                forge_sync::export_manifest_projected_since(
                    &cwd,
                    &args.output,
                    args.since.as_deref(),
                    recipient,
                    &args.capability,
                )?
            } else {
                forge_sync::export_manifest_since(&cwd, &args.output, args.since.as_deref())?
            };
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
                    let manifest = forge_sync::read_supported_manifest(&args.path)
                        .context("preflight sync import materialize manifest")?;
                    forge_sync::ensure_manifest_materializable(&manifest)
                        .context("preflight sync import materialize projection")?;
                    let _prepared_private_overlays =
                        forge_sync::prepare_private_overlay_materialization(&cwd, &manifest)
                            .context("preflight sync import private overlays")?;
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
                    let manifest = forge_sync::read_supported_manifest(&args.path)
                        .context("read sync import private overlays")?;
                    let prepared_private_overlays =
                        forge_sync::prepare_private_overlay_materialization(&cwd, &manifest)
                            .context("prepare sync import private overlays")?;
                    restore_effective_worktree(&cwd, &content_ref)
                        .context("restore imported native head")?;
                    let materialized_private_overlay_count =
                        forge_sync::install_prepared_private_overlays(
                            &cwd,
                            &prepared_private_overlays,
                        )
                        .context("materialize sync import private overlays")?;
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
                        object.insert(
                            "materialized_private_overlay_count".to_string(),
                            json!(materialized_private_overlay_count),
                        );
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
            let remote = SyncPeerRemote::parse(&args.remote)?;
            match remote {
                SyncPeerRemote::Local(path) => sync_fetch_peer(&cwd, request_id, &path, false),
                SyncPeerRemote::Ssh(remote) => {
                    sync_fetch_ssh_peer(&cwd, request_id, &remote, false)
                }
                SyncPeerRemote::Http(remote) => {
                    sync_fetch_http_peer(&cwd, request_id, &remote, false)
                }
            }
        }),
        SyncCommand::Pull(args) => command_result("sync pull", request_id, |cwd, request_id| {
            let remote = SyncPeerRemote::parse(&args.remote)?;
            match remote {
                SyncPeerRemote::Local(path) => sync_fetch_peer(&cwd, request_id, &path, true),
                SyncPeerRemote::Ssh(remote) => sync_fetch_ssh_peer(&cwd, request_id, &remote, true),
                SyncPeerRemote::Http(remote) => {
                    sync_fetch_http_peer(&cwd, request_id, &remote, true)
                }
            }
        }),
        SyncCommand::Push(args) => command_result("sync push", request_id, |cwd, request_id| {
            let remote = SyncPeerRemote::parse(&args.remote)?;
            match remote {
                SyncPeerRemote::Local(path) => sync_push_peer(&cwd, request_id, &path),
                SyncPeerRemote::Ssh(remote) => sync_push_ssh_peer(&cwd, request_id, &remote),
                SyncPeerRemote::Http(remote) => sync_push_http_peer(&cwd, request_id, &remote),
            }
        }),
        SyncCommand::Serve(args) => sync_serve_response(request_id, args),
    }
}

fn sync_serve_response(request_id: Option<String>, args: SyncServeArgs) -> ResponseEnvelope {
    match args.command {
        SyncServeCommand::Export(args) => {
            command_result("sync serve export", request_id, |cwd, _request_id| {
                let since_manifest = if args.stdin_since {
                    let value: forge_sync::SyncManifest = serde_json::from_reader(std::io::stdin())
                        .context("read sync serve export base manifest from stdin")?;
                    Some(value)
                } else {
                    None
                };
                let (manifest, report) =
                    forge_sync::export_manifest_for_transport_since(&cwd, since_manifest.as_ref())
                        .context("export sync transport manifest")?;
                Ok((
                    None,
                    json!({
                        "manifest": manifest,
                        "report": report,
                    }),
                    Vec::new(),
                ))
            })
        }
        SyncServeCommand::Receive(args) => {
            command_result("sync serve receive", request_id, |cwd, request_id| {
                if !args.stdin_manifest {
                    anyhow::bail!("sync serve receive requires --stdin-manifest");
                }
                let manifest: forge_sync::SyncManifest = serde_json::from_reader(std::io::stdin())
                    .context("read sync serve receive manifest from stdin")?;
                let remote_label = args
                    .remote_label
                    .unwrap_or_else(|| "<transport>".to_string());
                sync_receive_push_manifest(&cwd, request_id, &manifest, &remote_label)
            })
        }
    }
}

enum SyncPeerRemote {
    Local(PathBuf),
    Ssh(SshPeerRemote),
    Http(HttpPeerRemote),
}

struct SshPeerRemote {
    host: String,
    path: PathBuf,
}

struct HttpPeerRemote {
    url: String,
}

impl SyncPeerRemote {
    fn parse(raw: &OsStr) -> Result<Self> {
        let Some(raw) = raw.to_str() else {
            return Ok(Self::Local(PathBuf::from(raw)));
        };
        if let Some((scheme, rest)) = raw.split_once("://") {
            return match scheme {
                "file" => Ok(Self::Local(file_url_path(rest)?)),
                "ssh" => Ok(Self::Ssh(ssh_url_remote(rest)?)),
                "https" => Ok(Self::Http(HttpPeerRemote {
                    url: raw.trim_end_matches('/').to_string(),
                })),
                "http" => {
                    if env::var_os("FORGE_SYNC_ALLOW_INSECURE_HTTP").is_none() {
                        anyhow::bail!(
                            "http sync remote requires FORGE_SYNC_ALLOW_INSECURE_HTTP=1; use https for network transport"
                        );
                    }
                    Ok(Self::Http(HttpPeerRemote {
                        url: raw.trim_end_matches('/').to_string(),
                    }))
                }
                _ => anyhow::bail!(
                    "unsupported sync remote scheme {scheme}; supported schemes: local path, file, ssh, https"
                ),
            };
        }
        Ok(Self::Local(PathBuf::from(raw)))
    }
}

impl SshPeerRemote {
    fn label(&self) -> String {
        format!("ssh://{}{}", self.host, self.path.display())
    }
}

impl HttpPeerRemote {
    fn label(&self) -> String {
        self.url.clone()
    }
}

fn file_url_path(rest: &str) -> Result<PathBuf> {
    let path = if let Some(path) = rest.strip_prefix("localhost/") {
        format!("/{path}")
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        anyhow::bail!("file sync remote must use an absolute path");
    };
    Ok(PathBuf::from(percent_decode_file_url_path(&path)?))
}

fn ssh_url_remote(rest: &str) -> Result<SshPeerRemote> {
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("ssh sync remote must include an absolute path"))?;
    if host.is_empty() {
        anyhow::bail!("ssh sync remote must include a host");
    }
    let path = format!("/{path}");
    Ok(SshPeerRemote {
        host: host.to_string(),
        path: PathBuf::from(percent_decode_url_path(&path)?),
    })
}

fn percent_decode_file_url_path(path: &str) -> Result<String> {
    percent_decode_url_path(path)
}

fn percent_decode_url_path(path: &str) -> Result<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = bytes
                .get(index + 1)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("file sync remote has incomplete percent escape"))?;
            let lo = bytes
                .get(index + 2)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("file sync remote has incomplete percent escape"))?;
            let value = (hex_value(hi)? << 4) | hex_value(lo)?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).context("file sync remote path is not valid UTF-8")
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => anyhow::bail!("file sync remote has invalid percent escape"),
    }
}

fn ssh_export_manifest(
    remote: &SshPeerRemote,
    since_manifest: Option<&forge_sync::SyncManifest>,
) -> Result<(forge_sync::SyncManifest, forge_sync::SyncExportReport)> {
    let remote_forge = env::var("FORGE_SYNC_REMOTE_FORGE").unwrap_or_else(|_| "forge".to_string());
    let script = format!(
        "cd {} && {} --json sync serve export{}",
        shell_quote(&remote.path.display().to_string()),
        shell_quote(&remote_forge),
        if since_manifest.is_some() {
            " --stdin-since"
        } else {
            ""
        }
    );
    let stdin = since_manifest
        .map(serde_json::to_vec)
        .transpose()
        .context("serialize ssh sync base manifest")?
        .unwrap_or_default();
    let envelope = run_ssh_envelope(remote, &script, &stdin)?;
    if envelope.status != ResponseStatus::Success {
        let message = envelope
            .errors
            .first()
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "remote sync serve export failed".to_string());
        anyhow::bail!("ssh sync export failed on {}: {message}", remote.label());
    }
    let manifest = serde_json::from_value(
        envelope
            .data
            .get("manifest")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("ssh sync export response missing manifest"))?,
    )
    .context("decode ssh sync export manifest")?;
    let report = serde_json::from_value(
        envelope
            .data
            .get("report")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("ssh sync export response missing report"))?,
    )
    .context("decode ssh sync export report")?;
    Ok((manifest, report))
}

fn ssh_receive_manifest(
    remote: &SshPeerRemote,
    manifest: &forge_sync::SyncManifest,
    source_label: &str,
) -> Result<(Option<String>, Value)> {
    let remote_forge = env::var("FORGE_SYNC_REMOTE_FORGE").unwrap_or_else(|_| "forge".to_string());
    let script = format!(
        "cd {} && {} --json sync serve receive --stdin-manifest --remote-label {}",
        shell_quote(&remote.path.display().to_string()),
        shell_quote(&remote_forge),
        shell_quote(source_label),
    );
    let stdin = serde_json::to_vec(manifest).context("serialize ssh sync receive manifest")?;
    let envelope = run_ssh_envelope(remote, &script, &stdin)?;
    if envelope.status != ResponseStatus::Success {
        let message = envelope
            .errors
            .first()
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "remote sync serve receive failed".to_string());
        anyhow::bail!("ssh sync receive failed on {}: {message}", remote.label());
    }
    Ok((envelope.operation_id, envelope.data))
}

fn http_export_manifest(
    remote: &HttpPeerRemote,
    since_manifest: Option<&forge_sync::SyncManifest>,
) -> Result<(forge_sync::SyncManifest, forge_sync::SyncExportReport)> {
    let envelope = run_http_envelope(
        remote,
        "sync/serve/export",
        json!({ "since_manifest": since_manifest }),
    )?;
    if envelope.status != ResponseStatus::Success {
        let message = envelope
            .errors
            .first()
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "remote sync serve export failed".to_string());
        anyhow::bail!("https sync export failed on {}: {message}", remote.label());
    }
    let manifest = serde_json::from_value(
        envelope
            .data
            .get("manifest")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("https sync export response missing manifest"))?,
    )
    .context("decode https sync export manifest")?;
    let report = serde_json::from_value(
        envelope
            .data
            .get("report")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("https sync export response missing report"))?,
    )
    .context("decode https sync export report")?;
    Ok((manifest, report))
}

fn http_receive_manifest(
    remote: &HttpPeerRemote,
    manifest: &forge_sync::SyncManifest,
    source_label: &str,
) -> Result<(Option<String>, Value)> {
    let envelope = run_http_envelope(
        remote,
        "sync/serve/receive",
        json!({
            "manifest": manifest,
            "remote_label": source_label,
        }),
    )?;
    if envelope.status != ResponseStatus::Success {
        let message = envelope
            .errors
            .first()
            .map(|error| error.message.clone())
            .unwrap_or_else(|| "remote sync serve receive failed".to_string());
        anyhow::bail!("https sync receive failed on {}: {message}", remote.label());
    }
    Ok((envelope.operation_id, envelope.data))
}

fn run_http_envelope(
    remote: &HttpPeerRemote,
    endpoint: &str,
    body: Value,
) -> Result<ResponseEnvelope> {
    let url = format!("{}/{}", remote.url, endpoint);
    let response = ureq::post(&url)
        .set("content-type", "application/json")
        .send_json(body)
        .map_err(|error| anyhow::anyhow!("https sync request failed for {url}: {error}"))?;
    response
        .into_json()
        .with_context(|| format!("decode https sync JSON envelope from {url}"))
}

fn run_ssh_envelope(
    remote: &SshPeerRemote,
    script: &str,
    stdin: &[u8],
) -> Result<ResponseEnvelope> {
    let ssh_command = env::var("FORGE_SYNC_SSH_COMMAND").unwrap_or_else(|_| "ssh".to_string());
    let mut child = ProcessCommand::new(&ssh_command)
        .arg(&remote.host)
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn ssh sync command {ssh_command}"))?;
    if !stdin.is_empty() {
        child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("ssh sync stdin unavailable"))?
            .write_all(stdin)
            .context("write ssh sync stdin")?;
    }
    drop(child.stdin.take());
    let output = child
        .wait_with_output()
        .context("wait for ssh sync command")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "ssh sync command failed on {} with status {}: {}",
            remote.label(),
            output.status,
            stderr.trim()
        );
    }
    serde_json::from_slice(&output.stdout).context("decode ssh sync JSON envelope")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn sync_fetch_ssh_peer(
    cwd: &Path,
    request_id: Option<String>,
    remote: &SshPeerRemote,
    materialize: bool,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let local_context = forge_store::open_repository(cwd)?;
    let _local_lock = forge_store::acquire_repository_lock(&local_context.root_path)?;
    forge_store::reconcile_native_head(&local_context.root_path)?;
    let local_manifest = forge_sync::build_manifest(&local_context.root_path)?;
    let (remote_manifest, _) = ssh_export_manifest(remote, None)?;
    ensure_peer_compatible(
        &remote_manifest,
        &local_manifest,
        if materialize { "pull" } else { "fetch" },
    )?;
    let remote_path = PathBuf::from(remote.label());

    if !forge_sync::manifest_head_descends_from(
        &remote_manifest,
        local_manifest.native_head.as_deref(),
    )? {
        if forge_sync::manifest_head_descends_from(
            &local_manifest,
            remote_manifest.native_head.as_deref(),
        )? {
            return sync_fetch_up_to_date(
                cwd,
                request_id,
                &local_manifest,
                &remote_manifest,
                &remote_path,
                materialize,
            );
        }
        return record_sync_peer_merge_conflict(SyncPeerMergeConflictInput {
            receiver_cwd: cwd,
            request_id,
            source: &remote_manifest,
            receiver: &local_manifest,
            remote: &remote_path,
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
            materialize,
        });
    }

    let (incoming_manifest, export_report) = ssh_export_manifest(remote, Some(&local_manifest))?;
    let materialized_target_ref = if materialize {
        let content_ref = sync_manifest_head_content_ref(cwd, &incoming_manifest)?
            .ok_or_else(|| anyhow::anyhow!("sync peer has no native head to materialize"))?;
        ensure_clean_worktree(cwd, &content_ref).context("preflight sync pull")?;
        Some(content_ref)
    } else {
        None
    };
    let import_report = forge_sync::import_manifest_value(cwd, &incoming_manifest)
        .context("apply ssh remote sync delta")?;

    let mut operation_id = None;
    let mut materialized_content_ref = None;
    let mut materialized_operation_id = None;
    let mut materialized_view_id = None;
    if materialize {
        let content_ref = materialized_target_ref
            .clone()
            .expect("materialized target ref precomputed");
        restore_effective_worktree(cwd, &content_ref).context("restore fetched native head")?;
        let state = json!({
            "protocol_version": import_report.protocol_version.clone(),
            "direction": "pull",
            "remote_path": remote.label(),
            "base_native_head": local_manifest.native_head.clone(),
            "remote_native_head": remote_manifest.native_head.clone(),
            "exported_native_objects": export_report.native_object_count,
            "exported_native_payloads": export_report.native_payload_count,
            "exported_ledger_rows": export_report.ledger_row_count,
            "imported_native_objects": import_report.imported_native_objects,
            "imported_ledger_rows": import_report.imported_ledger_rows,
            "local_key_fingerprint": import_report.local_key_fingerprint.clone(),
            "materialized": true,
            "materialized_content_ref": content_ref.clone(),
        });
        let record = forge_store::record_sync_pull_materialized(
            cwd,
            request_id.clone(),
            forge_store::SyncPullMaterializedInput {
                state,
                content_ref: content_ref.clone(),
            },
        )
        .context("record sync pull materialization")?;
        operation_id = Some(record.operation_id.clone());
        materialized_operation_id = Some(record.operation_id);
        materialized_view_id = Some(record.view_id);
        materialized_content_ref = Some(content_ref);
    }

    let data = json!({
        "protocol_version": import_report.protocol_version,
        "direction": if materialize { "pull" } else { "fetch" },
        "remote_path": remote.label(),
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
    if !materialize && request_id.is_some() {
        let marker = forge_store::record_sync_request_marker(
            cwd,
            request_id,
            "sync fetch",
            "fetch",
            &remote_path,
            None,
            Some(data.clone()),
        )
        .context("record local sync fetch request-id marker")?;
        operation_id = Some(marker.operation_id);
    }
    Ok((operation_id, data, Vec::new()))
}

fn sync_fetch_http_peer(
    cwd: &Path,
    request_id: Option<String>,
    remote: &HttpPeerRemote,
    materialize: bool,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let local_context = forge_store::open_repository(cwd)?;
    let _local_lock = forge_store::acquire_repository_lock(&local_context.root_path)?;
    forge_store::reconcile_native_head(&local_context.root_path)?;
    let local_manifest = forge_sync::build_manifest(&local_context.root_path)?;
    let (remote_manifest, _) = http_export_manifest(remote, None)?;
    ensure_peer_compatible(
        &remote_manifest,
        &local_manifest,
        if materialize { "pull" } else { "fetch" },
    )?;
    let remote_path = PathBuf::from(remote.label());

    if !forge_sync::manifest_head_descends_from(
        &remote_manifest,
        local_manifest.native_head.as_deref(),
    )? {
        if forge_sync::manifest_head_descends_from(
            &local_manifest,
            remote_manifest.native_head.as_deref(),
        )? {
            return sync_fetch_up_to_date(
                cwd,
                request_id,
                &local_manifest,
                &remote_manifest,
                &remote_path,
                materialize,
            );
        }
        return record_sync_peer_merge_conflict(SyncPeerMergeConflictInput {
            receiver_cwd: cwd,
            request_id,
            source: &remote_manifest,
            receiver: &local_manifest,
            remote: &remote_path,
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
            materialize,
        });
    }

    let (incoming_manifest, export_report) = http_export_manifest(remote, Some(&local_manifest))?;
    let materialized_target_ref = if materialize {
        let content_ref = sync_manifest_head_content_ref(cwd, &incoming_manifest)?
            .ok_or_else(|| anyhow::anyhow!("sync peer has no native head to materialize"))?;
        ensure_clean_worktree(cwd, &content_ref).context("preflight sync pull")?;
        Some(content_ref)
    } else {
        None
    };
    let import_report = forge_sync::import_manifest_value(cwd, &incoming_manifest)
        .context("apply https remote sync delta")?;

    let mut operation_id = None;
    let mut materialized_content_ref = None;
    let mut materialized_operation_id = None;
    let mut materialized_view_id = None;
    if materialize {
        let content_ref = materialized_target_ref
            .clone()
            .expect("materialized target ref precomputed");
        restore_effective_worktree(cwd, &content_ref).context("restore fetched native head")?;
        let state = json!({
            "protocol_version": import_report.protocol_version.clone(),
            "direction": "pull",
            "remote_path": remote.label(),
            "base_native_head": local_manifest.native_head.clone(),
            "remote_native_head": remote_manifest.native_head.clone(),
            "exported_native_objects": export_report.native_object_count,
            "exported_native_payloads": export_report.native_payload_count,
            "exported_ledger_rows": export_report.ledger_row_count,
            "imported_native_objects": import_report.imported_native_objects,
            "imported_ledger_rows": import_report.imported_ledger_rows,
            "local_key_fingerprint": import_report.local_key_fingerprint.clone(),
            "materialized": true,
            "materialized_content_ref": content_ref.clone(),
        });
        let record = forge_store::record_sync_pull_materialized(
            cwd,
            request_id.clone(),
            forge_store::SyncPullMaterializedInput {
                state,
                content_ref: content_ref.clone(),
            },
        )
        .context("record sync pull materialization")?;
        operation_id = Some(record.operation_id.clone());
        materialized_operation_id = Some(record.operation_id);
        materialized_view_id = Some(record.view_id);
        materialized_content_ref = Some(content_ref);
    }

    let data = json!({
        "protocol_version": import_report.protocol_version,
        "direction": if materialize { "pull" } else { "fetch" },
        "remote_path": remote.label(),
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
    if !materialize && request_id.is_some() {
        let marker = forge_store::record_sync_request_marker(
            cwd,
            request_id,
            "sync fetch",
            "fetch",
            &remote_path,
            None,
            Some(data.clone()),
        )
        .context("record local sync fetch request-id marker")?;
        operation_id = Some(marker.operation_id);
    }
    Ok((operation_id, data, Vec::new()))
}

fn sync_fetch_up_to_date(
    cwd: &Path,
    request_id: Option<String>,
    local_manifest: &forge_sync::SyncManifest,
    remote_manifest: &forge_sync::SyncManifest,
    remote_path: &Path,
    materialize: bool,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let direction = if materialize { "pull" } else { "fetch" };
    let command = if materialize {
        "sync pull"
    } else {
        "sync fetch"
    };
    let mut operation_id = None;
    let mut materialized_content_ref = None;
    let mut materialized_operation_id = None;
    let mut materialized_view_id = None;
    if materialize {
        let commit_id = local_manifest
            .native_head
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("sync pull has no local native head to materialize"))?;
        let content_ref = forge_store::checkout_target_content_ref(cwd, commit_id)
            .context("resolve current native head")?;
        ensure_clean_worktree(cwd, &content_ref).context("preflight sync pull")?;
        restore_effective_worktree(cwd, &content_ref).context("restore current native head")?;
        let state = json!({
            "protocol_version": remote_manifest.protocol_version.clone(),
            "direction": direction,
            "remote_path": remote_path.display().to_string(),
            "base_native_head": local_manifest.native_head.clone(),
            "remote_native_head": remote_manifest.native_head.clone(),
            "imported_native_objects": 0,
            "imported_ledger_rows": 0,
            "materialized": true,
            "materialized_content_ref": content_ref.clone(),
            "up_to_date": true,
        });
        let record = forge_store::record_sync_pull_materialized(
            cwd,
            request_id.clone(),
            forge_store::SyncPullMaterializedInput {
                state: state.clone(),
                content_ref: content_ref.clone(),
            },
        )
        .context("record sync pull materialization")?;
        operation_id = Some(record.operation_id.clone());
        materialized_operation_id = Some(record.operation_id);
        materialized_view_id = Some(record.view_id);
        materialized_content_ref = Some(content_ref);
    }
    let data = json!({
        "protocol_version": remote_manifest.protocol_version,
        "direction": direction,
        "remote_path": remote_path.display().to_string(),
        "base_native_head": local_manifest.native_head,
        "remote_native_head": remote_manifest.native_head,
        "imported_native_objects": 0,
        "imported_ledger_rows": 0,
        "materialized": materialize,
        "materialized_content_ref": materialized_content_ref,
        "materialized_operation_id": materialized_operation_id,
        "materialized_view_id": materialized_view_id,
        "up_to_date": true,
    });
    if !materialize && request_id.is_some() {
        let marker = forge_store::record_sync_request_marker(
            cwd,
            request_id,
            command,
            direction,
            remote_path,
            None,
            Some(data.clone()),
        )
        .with_context(|| format!("record local {command} request-id marker"))?;
        operation_id = Some(marker.operation_id);
    }
    Ok((operation_id, data, Vec::new()))
}

fn manifest_ledger_row_count(manifest: &forge_sync::SyncManifest) -> usize {
    manifest
        .ledger_rows
        .iter()
        .map(|table| table.rows.len())
        .sum()
}

fn record_local_sync_push_marker(
    cwd: &Path,
    request_id: Option<String>,
    remote: &Path,
    remote_operation_id: Option<&str>,
    data: &Value,
) -> Result<Option<String>> {
    if request_id.is_none() {
        return Ok(remote_operation_id.map(str::to_string));
    }
    Ok(Some(
        forge_store::record_sync_request_marker(
            cwd,
            request_id,
            "sync push",
            "push",
            remote,
            remote_operation_id,
            Some(data.clone()),
        )
        .context("record local sync push request-id marker")?
        .operation_id,
    ))
}

fn sync_push_up_to_date(
    cwd: &Path,
    request_id: Option<String>,
    local_manifest: &forge_sync::SyncManifest,
    remote_manifest: &forge_sync::SyncManifest,
    remote: &Path,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let mut data = json!({
        "protocol_version": local_manifest.protocol_version,
        "direction": "push",
        "remote_path": remote.display().to_string(),
        "base_native_head": remote_manifest.native_head,
        "local_native_head": local_manifest.native_head,
        "imported_native_objects": 0,
        "imported_ledger_rows": 0,
        "materialized": false,
        "up_to_date": true,
    });
    let operation_id = record_local_sync_push_marker(cwd, request_id, remote, None, &data)?;
    if let (Some(operation_id), Some(object)) = (operation_id.as_ref(), data.as_object_mut()) {
        object.insert("operation_id".to_string(), json!(operation_id));
    }
    Ok((operation_id, data, Vec::new()))
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
            return sync_fetch_up_to_date(
                cwd,
                request_id,
                &local_manifest,
                &remote_manifest,
                remote,
                materialize,
            );
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
            materialize,
        });
    }

    let (incoming_manifest, export_report) =
        forge_sync::export_manifest_for_transport_since(remote, Some(&local_manifest))
            .context("export remote sync delta")?;
    let materialized_target_ref = if materialize {
        let content_ref = sync_manifest_head_content_ref(cwd, &incoming_manifest)?
            .ok_or_else(|| anyhow::anyhow!("sync peer has no native head to materialize"))?;
        ensure_clean_worktree(cwd, &content_ref).context("preflight sync pull")?;
        Some(content_ref)
    } else {
        None
    };
    let import_report = forge_sync::import_manifest_value(cwd, &incoming_manifest)
        .context("apply remote sync delta")?;

    let mut operation_id = None;
    let mut materialized_content_ref = None;
    let mut materialized_operation_id = None;
    let mut materialized_view_id = None;
    if materialize {
        let content_ref = materialized_target_ref
            .clone()
            .expect("materialized target ref precomputed");
        restore_effective_worktree(cwd, &content_ref).context("restore fetched native head")?;
        let state = json!({
            "protocol_version": import_report.protocol_version.clone(),
            "direction": "pull",
            "remote_path": remote.display().to_string(),
            "base_native_head": local_manifest.native_head.clone(),
            "remote_native_head": remote_manifest.native_head.clone(),
            "exported_native_objects": export_report.native_object_count,
            "exported_native_payloads": export_report.native_payload_count,
            "exported_ledger_rows": export_report.ledger_row_count,
            "imported_native_objects": import_report.imported_native_objects,
            "imported_ledger_rows": import_report.imported_ledger_rows,
            "local_key_fingerprint": import_report.local_key_fingerprint.clone(),
            "materialized": true,
            "materialized_content_ref": content_ref.clone(),
        });
        let record = forge_store::record_sync_pull_materialized(
            cwd,
            request_id.clone(),
            forge_store::SyncPullMaterializedInput {
                state,
                content_ref: content_ref.clone(),
            },
        )
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
    if !materialize && request_id.is_some() {
        let marker = forge_store::record_sync_request_marker(
            cwd,
            request_id,
            "sync fetch",
            "fetch",
            remote,
            None,
            Some(data.clone()),
        )
        .context("record local sync fetch request-id marker")?;
        operation_id = Some(marker.operation_id);
    }
    Ok((operation_id, data, Vec::new()))
}

fn sync_push_ssh_peer(
    cwd: &Path,
    request_id: Option<String>,
    remote: &SshPeerRemote,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let local_context = forge_store::open_repository(cwd)?;
    let _local_lock = forge_store::acquire_repository_lock(&local_context.root_path)?;
    forge_store::reconcile_native_head(&local_context.root_path)?;
    let local_manifest = forge_sync::build_manifest(&local_context.root_path)?;
    let (remote_manifest, _) = ssh_export_manifest(remote, None)?;
    ensure_peer_compatible(&local_manifest, &remote_manifest, "push")?;
    let remote_path = PathBuf::from(remote.label());

    if !forge_sync::manifest_head_descends_from(
        &local_manifest,
        remote_manifest.native_head.as_deref(),
    )? {
        if forge_sync::manifest_head_descends_from(
            &remote_manifest,
            local_manifest.native_head.as_deref(),
        )? {
            return sync_push_up_to_date(
                cwd,
                request_id,
                &local_manifest,
                &remote_manifest,
                &remote_path,
            );
        }
        let (remote_operation_id, mut data) = ssh_receive_manifest(
            remote,
            &local_manifest,
            &local_context.root_path.display().to_string(),
        )?;
        if let Some(object) = data.as_object_mut() {
            object.insert("remote_path".to_string(), json!(remote.label()));
            object.insert(
                "remote_operation_id".to_string(),
                json!(remote_operation_id),
            );
        }
        let local_operation_id = record_local_sync_push_marker(
            cwd,
            request_id,
            &remote_path,
            remote_operation_id.as_deref(),
            &data,
        )?;
        if let (Some(operation_id), Some(object)) =
            (local_operation_id.as_ref(), data.as_object_mut())
        {
            object.insert("operation_id".to_string(), json!(operation_id));
        }
        return Ok((local_operation_id, data, Vec::new()));
    }

    let (outgoing_manifest, export_report) =
        forge_sync::export_manifest_for_transport_since(cwd, Some(&remote_manifest))
            .context("export local sync delta")?;
    let (remote_operation_id, mut data) = ssh_receive_manifest(
        remote,
        &outgoing_manifest,
        &local_context.root_path.display().to_string(),
    )?;
    if let Some(object) = data.as_object_mut() {
        object.insert("remote_path".to_string(), json!(remote.label()));
        object.insert(
            "exported_native_objects".to_string(),
            json!(export_report.native_object_count),
        );
        object.insert(
            "exported_native_payloads".to_string(),
            json!(export_report.native_payload_count),
        );
        object.insert(
            "exported_ledger_rows".to_string(),
            json!(export_report.ledger_row_count),
        );
    }
    let operation_id = record_local_sync_push_marker(
        cwd,
        request_id,
        &remote_path,
        remote_operation_id.as_deref(),
        &data,
    )?;
    if let (Some(operation_id), Some(object)) = (operation_id.as_ref(), data.as_object_mut()) {
        object.insert("operation_id".to_string(), json!(operation_id));
    }
    Ok((operation_id, data, Vec::new()))
}

fn sync_push_http_peer(
    cwd: &Path,
    request_id: Option<String>,
    remote: &HttpPeerRemote,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let local_context = forge_store::open_repository(cwd)?;
    let _local_lock = forge_store::acquire_repository_lock(&local_context.root_path)?;
    forge_store::reconcile_native_head(&local_context.root_path)?;
    let local_manifest = forge_sync::build_manifest(&local_context.root_path)?;
    let (remote_manifest, _) = http_export_manifest(remote, None)?;
    ensure_peer_compatible(&local_manifest, &remote_manifest, "push")?;
    let remote_path = PathBuf::from(remote.label());

    if !forge_sync::manifest_head_descends_from(
        &local_manifest,
        remote_manifest.native_head.as_deref(),
    )? {
        if forge_sync::manifest_head_descends_from(
            &remote_manifest,
            local_manifest.native_head.as_deref(),
        )? {
            return sync_push_up_to_date(
                cwd,
                request_id,
                &local_manifest,
                &remote_manifest,
                &remote_path,
            );
        }
        let (remote_operation_id, mut data) = http_receive_manifest(
            remote,
            &local_manifest,
            &local_context.root_path.display().to_string(),
        )?;
        if let Some(object) = data.as_object_mut() {
            object.insert("remote_path".to_string(), json!(remote.label()));
            object.insert(
                "remote_operation_id".to_string(),
                json!(remote_operation_id),
            );
        }
        let local_operation_id = record_local_sync_push_marker(
            cwd,
            request_id,
            &remote_path,
            remote_operation_id.as_deref(),
            &data,
        )?;
        if let (Some(operation_id), Some(object)) =
            (local_operation_id.as_ref(), data.as_object_mut())
        {
            object.insert("operation_id".to_string(), json!(operation_id));
        }
        return Ok((local_operation_id, data, Vec::new()));
    }

    let (outgoing_manifest, export_report) =
        forge_sync::export_manifest_for_transport_since(cwd, Some(&remote_manifest))
            .context("export local sync delta")?;
    let (remote_operation_id, mut data) = http_receive_manifest(
        remote,
        &outgoing_manifest,
        &local_context.root_path.display().to_string(),
    )?;
    if let Some(object) = data.as_object_mut() {
        object.insert("remote_path".to_string(), json!(remote.label()));
        object.insert(
            "exported_native_objects".to_string(),
            json!(export_report.native_object_count),
        );
        object.insert(
            "exported_native_payloads".to_string(),
            json!(export_report.native_payload_count),
        );
        object.insert(
            "exported_ledger_rows".to_string(),
            json!(export_report.ledger_row_count),
        );
    }
    let operation_id = record_local_sync_push_marker(
        cwd,
        request_id,
        &remote_path,
        remote_operation_id.as_deref(),
        &data,
    )?;
    if let (Some(operation_id), Some(object)) = (operation_id.as_ref(), data.as_object_mut()) {
        object.insert("operation_id".to_string(), json!(operation_id));
    }
    Ok((operation_id, data, Vec::new()))
}

fn sync_receive_push_manifest(
    cwd: &Path,
    request_id: Option<String>,
    incoming: &forge_sync::SyncManifest,
    remote_label: &str,
) -> Result<(Option<String>, Value, Vec<String>)> {
    let receiver_context = forge_store::open_repository(cwd)?;
    let receiver_manifest = forge_sync::build_manifest(&receiver_context.root_path)?;
    ensure_peer_compatible(incoming, &receiver_manifest, "push")?;
    let remote_path = PathBuf::from(remote_label);

    if !forge_sync::manifest_head_descends_from(incoming, receiver_manifest.native_head.as_deref())?
    {
        if forge_sync::manifest_head_descends_from(
            &receiver_manifest,
            incoming.native_head.as_deref(),
        )? {
            let data = json!({
                "protocol_version": incoming.protocol_version,
                "direction": "push",
                "remote_path": remote_label,
                "base_native_head": receiver_manifest.native_head,
                "local_native_head": incoming.native_head,
                "imported_native_objects": 0,
                "imported_ledger_rows": 0,
                "materialized": false,
                "up_to_date": true,
            });
            return Ok((None, data, Vec::new()));
        }
        return record_sync_peer_merge_conflict(SyncPeerMergeConflictInput {
            receiver_cwd: cwd,
            request_id,
            source: incoming,
            receiver: &receiver_manifest,
            remote: &remote_path,
            context: "sync_push_divergence",
            command: "sync push",
            direction: "push",
            materialize: false,
        });
    }

    let import_report =
        forge_sync::import_manifest_value(cwd, incoming).context("apply ssh push sync delta")?;
    let data = json!({
        "protocol_version": import_report.protocol_version,
        "direction": "push",
        "remote_path": remote_label,
        "base_native_head": receiver_manifest.native_head,
        "local_native_head": incoming.native_head,
        "exported_native_objects": incoming.native_objects.len(),
        "exported_native_payloads": incoming.native_payloads.len(),
        "exported_ledger_rows": manifest_ledger_row_count(incoming),
        "imported_native_objects": import_report.imported_native_objects,
        "imported_ledger_rows": import_report.imported_ledger_rows,
        "local_key_fingerprint": import_report.local_key_fingerprint,
        "materialized": false,
    });
    Ok((None, data, Vec::new()))
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
            let mut data = json!({
                "protocol_version": local_manifest.protocol_version,
                "direction": "push",
                "remote_path": remote.display().to_string(),
                "base_native_head": remote_manifest.native_head,
                "local_native_head": local_manifest.native_head,
                "imported_native_objects": 0,
                "imported_ledger_rows": 0,
                "materialized": false,
                "up_to_date": true,
            });
            let operation_id = if request_id.is_some() {
                let marker = forge_store::record_sync_request_marker(
                    cwd,
                    request_id,
                    "sync push",
                    "push",
                    remote,
                    None,
                    Some(data.clone()),
                )
                .context("record local sync push request-id marker")?;
                if let Some(object) = data.as_object_mut() {
                    object.insert("operation_id".to_string(), json!(marker.operation_id));
                }
                Some(marker.operation_id)
            } else {
                None
            };
            return Ok((operation_id, data, Vec::new()));
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
                materialize: false,
            })?;
        let local_operation_id = if request_id.is_some() {
            if let Some(object) = data.as_object_mut() {
                object.insert(
                    "remote_operation_id".to_string(),
                    json!(remote_operation_id),
                );
            }
            let marker = forge_store::record_sync_request_marker(
                cwd,
                request_id,
                "sync push",
                "push",
                remote,
                remote_operation_id.as_deref(),
                Some(data.clone()),
            )
            .context("record local sync push request-id marker")?;
            if let Some(object) = data.as_object_mut() {
                object.insert("operation_id".to_string(), json!(marker.operation_id));
            }
            Some(marker.operation_id)
        } else {
            remote_operation_id
        };
        return Ok((local_operation_id, data, warnings));
    }

    let (outgoing_manifest, export_report) =
        forge_sync::export_manifest_for_transport_since(cwd, Some(&remote_manifest))
            .context("export local sync delta")?;
    let import_report = forge_sync::import_manifest_value(remote, &outgoing_manifest)
        .context("apply local sync delta")?;

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
        let replay_data = data.clone();
        operation_id = Some(
            forge_store::record_sync_request_marker(
                cwd,
                request_id,
                "sync push",
                "push",
                remote,
                None,
                Some(replay_data),
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
    materialize: bool,
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
    if let Some(merged_content_ref) = merge.merged_content_ref {
        if input.materialize {
            if let Err(error) = ensure_clean_worktree(input.receiver_cwd, &merged_content_ref) {
                cleanup_new_native_objects(&store, &pre_merge_loose_objects)
                    .context("remove staged native objects after dirty sync merge preflight")?;
                return Err(error).context("preflight clean sync merge restore");
            }
        }
        let imported_ledger_rows =
            match forge_sync::import_ledger_rows_from_manifest(input.receiver_cwd, input.source) {
                Ok(count) => count,
                Err(error) => {
                    cleanup_new_native_objects(&store, &pre_merge_loose_objects)
                        .context("remove staged native objects after failed sync ledger import")?;
                    return Err(error).context("import peer ledger rows for clean sync merge");
                }
            };
        let imported_native_objects_i64 = match i64::try_from(imported_native_objects) {
            Ok(count) => count,
            Err(error) => {
                cleanup_new_native_objects(&store, &pre_merge_loose_objects)
                    .context("remove staged native objects after invalid native object count")?;
                return Err(error).context("sync merge imported native object count exceeds i64");
            }
        };
        let imported_ledger_rows_i64 = match i64::try_from(imported_ledger_rows) {
            Ok(count) => count,
            Err(error) => {
                cleanup_new_native_objects(&store, &pre_merge_loose_objects)
                    .context("remove staged native objects after invalid ledger row count")?;
                return Err(error).context("sync merge imported ledger row count exceeds i64");
            }
        };
        let record = match forge_store::record_sync_merge_commit(
            input.receiver_cwd,
            input.request_id,
            input.command,
            forge_store::SyncMergeCommitInput {
                protocol_version: &input.source.protocol_version,
                direction: input.direction,
                remote_path: input.remote,
                base_native_head: &base_head,
                ours_native_head: input
                    .receiver
                    .native_head
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("sync receiver has no native head"))?,
                theirs_native_head: input
                    .source
                    .native_head
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("sync source has no native head"))?,
                merged_content_ref: &merged_content_ref,
                materialized: input.materialize,
                imported_native_objects: imported_native_objects_i64,
                imported_ledger_rows: imported_ledger_rows_i64,
            },
        ) {
            Ok(record) => record,
            Err(error) => {
                // Peer ledger rows are already durable here. Keep the imported native objects so
                // those rows cannot reference commits that cleanup just deleted; reconcile filters
                // divergent peer decisions from the local native tip until a merge op lands.
                return Err(error).context("record clean sync merge commit");
            }
        };
        if input.materialize {
            restore_effective_worktree(input.receiver_cwd, &merged_content_ref)
                .context("restore clean sync merge tree")?;
            forge_store::set_materialized_expected_content_ref(
                input.receiver_cwd,
                &merged_content_ref,
            )
            .context("record clean sync merge materialized content")?;
        }
        return Ok((
            Some(record.operation.operation_id.clone()),
            json!({
                "protocol_version": input.source.protocol_version,
                "direction": input.direction,
                "remote_path": input.remote.display().to_string(),
                "merged": true,
                "operation_id": record.operation.operation_id,
                "merge_commit_id": record.commit_id,
                "base_native_head": input.receiver.native_head,
                "receiver_native_head": input.receiver.native_head,
                "common_ancestor_native_head": base_head,
                "source_native_head": input.source.native_head,
                "merged_content_ref": merged_content_ref,
                "imported_native_objects": imported_native_objects,
                "imported_ledger_rows": imported_ledger_rows,
                "materialized": input.materialize,
            }),
            secret_export_warnings(&merge.dropped_secret_paths),
        ));
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
