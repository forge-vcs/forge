use serde::{Deserialize, Serialize};

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct OrgEncryptionKeyBindingRecord {
    pub binding_id: String,
    pub principal_id: String,
    pub key_fingerprint: String,
    pub public_key: String,
    pub binding_authority: String,
    pub state: String,
    pub valid_from_revision: i64,
    pub valid_until_revision: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivatePathLabelRecord {
    pub path_label_id: String,
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path_hash: String,
    pub encrypted_display_path: String,
    pub visibility: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPrivatePathLabel {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub visibility: String,
}

#[derive(Debug, Clone)]
pub struct EncryptedPrivatePayloadInput {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub snapshot_id: Option<String>,
    pub path_label_id: String,
    pub path_hash: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub private_object_path: String,
    pub encrypted_metadata_json: String,
}

#[derive(Debug, Clone)]
pub struct SaveSnapshotPrivateOverlayInput {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub private_object_path: String,
    pub encrypted_metadata_json: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EncryptedPrivatePayloadRecord {
    pub payload_id: String,
    pub work_package_kind: String,
    pub work_package_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    pub path_label_id: String,
    pub path_hash: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub private_object_path: String,
    pub encrypted_metadata_json: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrivateDecryptAuthority {
    pub principal_id: String,
    pub key_fingerprint: String,
    pub public_key: String,
    pub recipient_fingerprint: String,
    pub policy_revision: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalEncryptedPrivateObject {
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub private_object_path: String,
}

#[derive(Debug, Clone)]
pub struct PrivateOverlayTransportRecord {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub snapshot_id: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub path: String,
    pub visibility: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct PrivateOverlayMaterializeInput {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub path: String,
    pub visibility: String,
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
    pub ciphertext: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct MaterializedPrivateOverlay {
    pub work_package_kind: String,
    pub work_package_id: String,
    pub path_label_id: String,
    pub path_hash: String,
    pub path: String,
    pub visibility: String,
    pub plaintext: Vec<u8>,
}

pub fn scoped_private_path_hash(
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    path: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"forge-private-path-v1-unkeyed-deprecated\n");
    hasher.update(repo_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(work_package_kind.as_bytes());
    hasher.update(b"\n");
    hasher.update(work_package_id.as_bytes());
    hasher.update(b"\n");
    hasher.update(path.as_bytes());
    format!("sha256:{}", hex_bytes(&hasher.finalize()))
}

pub fn keyed_private_path_hash(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    path: &str,
) -> Result<String> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let key = local_private_path_hash_key(&context)?;
    let hmac_key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &key);
    let mut payload = Vec::new();
    payload.extend_from_slice(b"forge-private-path-v2\n");
    payload.extend_from_slice(context.repo_id.as_bytes());
    payload.extend_from_slice(b"\n");
    payload.extend_from_slice(work_package_kind.as_bytes());
    payload.extend_from_slice(b"\n");
    payload.extend_from_slice(work_package_id.as_bytes());
    payload.extend_from_slice(b"\n");
    payload.extend_from_slice(path.as_bytes());
    let tag = ring::hmac::sign(&hmac_key, &payload);
    Ok(format!("hmac-sha256:{}", hex_bytes(tag.as_ref())))
}

pub fn bind_org_encryption_key(
    cwd: &Path,
    principal_id: &str,
    public_key: &str,
    binding_authority: &str,
    reason: Option<&str>,
) -> Result<OrgEncryptionKeyBindingRecord> {
    let recipient =
        EncryptionRecipient::parse(public_key).map_err(|_| ForgeError::PrivateContentInvalid {
            reason: "invalid_encryption_recipient".to_string(),
        })?;
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        let org = org_status_on(tx, &context.repo_id)?;
        if !org.enabled {
            return Err(ForgeError::OrgNotEnabled.into());
        }
        ensure_active_org_principal(tx, &context.repo_id, principal_id)?;
        ensure_active_org_principal(tx, &context.repo_id, binding_authority)?;
        ensure_active_org_role(
            tx,
            &context.repo_id,
            binding_authority,
            &["owner", "maintainer"],
        )?;

        let now = now_ms();
        let binding_id = new_id("org_enc_key");
        let key_fingerprint = recipient.fingerprint().to_string();
        tx.execute(
            "INSERT INTO org_encryption_key_bindings (
                id, repo_id, principal_id, key_fingerprint, public_key, binding_authority,
                state, valid_from_revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?8)
             ON CONFLICT(repo_id, key_fingerprint)
             DO UPDATE SET
                principal_id = excluded.principal_id,
                public_key = excluded.public_key,
                binding_authority = excluded.binding_authority,
                state = 'active',
                valid_from_revision = excluded.valid_from_revision,
                valid_until_revision = NULL,
                revocation_reason = NULL,
                updated_at_ms = excluded.updated_at_ms",
            params![
                binding_id,
                context.repo_id,
                principal_id,
                key_fingerprint,
                public_key,
                binding_authority,
                org.policy_revision,
                now,
            ],
        )?;
        insert_private_content_audit(
            tx,
            &context.repo_id,
            None,
            None,
            None,
            None,
            Some(principal_id),
            Some(&key_fingerprint),
            "bind_encryption_key",
            reason,
            now,
        )?;
        org_encryption_key_binding_on(tx, &context.repo_id, &key_fingerprint)?
            .ok_or_else(|| anyhow!("org encryption key binding missing after insert"))
    })
}

pub fn record_private_path_label(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    path_hash: &str,
    encrypted_display_path: &str,
    visibility: &str,
) -> Result<PrivatePathLabelRecord> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_label(visibility)?;
    validate_private_hash(path_hash)?;
    if encrypted_display_path.trim().is_empty() {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "empty_encrypted_display_path".to_string(),
        }
        .into());
    }
    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        ensure_work_package_exists(tx, &context.repo_id, work_package_kind, work_package_id)?;
        let now = now_ms();
        let label_id = new_id("private_path");
        tx.execute(
            "INSERT INTO private_path_labels (
                id, repo_id, work_package_kind, work_package_id, path_hash,
                encrypted_display_path, visibility, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
             ON CONFLICT(repo_id, work_package_kind, work_package_id, path_hash)
             DO UPDATE SET
                encrypted_display_path = excluded.encrypted_display_path,
                visibility = excluded.visibility,
                updated_at_ms = excluded.updated_at_ms",
            params![
                label_id,
                context.repo_id,
                work_package_kind,
                work_package_id,
                path_hash,
                encrypted_display_path,
                visibility,
                now,
            ],
        )?;
        private_path_label_on(
            tx,
            &context.repo_id,
            work_package_kind,
            work_package_id,
            path_hash,
        )?
        .ok_or_else(|| anyhow!("private path label missing after insert"))
    })
}

pub fn set_local_private_path_label(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    path: &str,
    visibility: &str,
) -> Result<PrivatePathLabelRecord> {
    validate_work_package_kind(work_package_kind)?;
    validate_visibility_label(visibility)?;
    if visibility == VISIBILITY_PUBLIC {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "public_private_path_label".to_string(),
        }
        .into());
    }
    let context = open_repository(cwd)?;
    let path = normalize_private_label_path(path)?;
    let path_hash = keyed_private_path_hash(cwd, work_package_kind, work_package_id, &path)?;
    let label = record_private_path_label(
        cwd,
        work_package_kind,
        work_package_id,
        &path_hash,
        &format!("local-private-path:{path_hash}"),
        visibility,
    )?;
    let mut labels = read_local_private_path_labels(&context)?;
    labels.retain(|existing| {
        !(existing.work_package_kind == work_package_kind
            && existing.work_package_id == work_package_id
            && (existing.path_hash == path_hash || existing.path == path))
    });
    labels.push(LocalPrivatePathLabel {
        work_package_kind: work_package_kind.to_string(),
        work_package_id: work_package_id.to_string(),
        path,
        path_label_id: label.path_label_id.clone(),
        path_hash,
        visibility: visibility.to_string(),
    });
    write_local_private_path_labels(&context, &labels)?;
    Ok(label)
}

pub fn local_private_path_labels(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<Vec<LocalPrivatePathLabel>> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let labels = read_local_private_path_labels(&context)?;
    if labels.is_empty()
        && private_path_label_count(&context, work_package_kind, work_package_id)? > 0
    {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "missing_local_private_path_labels".to_string(),
        }
        .into());
    }
    Ok(labels
        .into_iter()
        .filter(|label| {
            label.work_package_kind == work_package_kind && label.work_package_id == work_package_id
        })
        .collect())
}

pub fn local_private_path_exclusions(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<Vec<String>> {
    let labels = local_private_path_labels(cwd, work_package_kind, work_package_id)?;
    for label in &labels {
        let full_path = cwd.join(&label.path);
        if !full_path.is_file() {
            return Err(ForgeError::PrivateContentInvalid {
                reason: "private_path_not_regular_file".to_string(),
            }
            .into());
        }
    }
    Ok(labels.into_iter().map(|label| label.path).collect())
}

pub fn capture_local_private_overlays(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<Vec<SaveSnapshotPrivateOverlayInput>> {
    let labels = local_private_path_labels(cwd, work_package_kind, work_package_id)?;
    let mut overlays = Vec::with_capacity(labels.len());
    for label in labels {
        let full_path = cwd.join(&label.path);
        if !full_path.is_file() {
            return Err(ForgeError::PrivateContentInvalid {
                reason: "private_path_not_regular_file".to_string(),
            }
            .into());
        }
        let plaintext = fs::read(&full_path).with_context(|| "read private path payload")?;
        let encrypted = encrypt_private_payload_to_local_store(cwd, &plaintext)?;
        overlays.push(SaveSnapshotPrivateOverlayInput {
            work_package_kind: label.work_package_kind,
            work_package_id: label.work_package_id,
            path_label_id: label.path_label_id,
            path_hash: label.path_hash,
            envelope_format: encrypted.envelope_format,
            recipient_fingerprint: encrypted.recipient_fingerprint,
            ciphertext_digest: encrypted.ciphertext_digest,
            private_object_path: encrypted.private_object_path,
            encrypted_metadata_json: "{}".to_string(),
        });
    }
    Ok(overlays)
}

pub fn record_encrypted_private_payload(
    cwd: &Path,
    input: EncryptedPrivatePayloadInput,
) -> Result<EncryptedPrivatePayloadRecord> {
    validate_work_package_kind(&input.work_package_kind)?;
    validate_private_hash(&input.path_hash)?;
    validate_private_hash(&input.ciphertext_digest)?;
    if input.envelope_format.trim().is_empty()
        || input.recipient_fingerprint.trim().is_empty()
        || input.private_object_path.trim().is_empty()
        || input.encrypted_metadata_json.trim().is_empty()
    {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "empty_private_payload_metadata".to_string(),
        }
        .into());
    }

    let context = open_repository(cwd)?;
    let mut connection = open_connection(&context.database_path)?;
    with_immediate_retry(&mut connection, |tx| {
        insert_encrypted_private_payload_on(tx, &context.repo_id, input.clone(), now_ms())
    })
}

pub fn private_decrypt_authority(
    cwd: &Path,
    work_package_kind: &str,
    work_package_id: &str,
    principal_id: &str,
) -> Result<PrivateDecryptAuthority> {
    validate_work_package_kind(work_package_kind)?;
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    ensure_work_package_exists(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
    )?;
    private_decrypt_authority_on(
        &connection,
        &context.repo_id,
        work_package_kind,
        work_package_id,
        principal_id,
    )
}

pub fn encrypt_private_payload_to_local_store(
    cwd: &Path,
    plaintext: &[u8],
) -> Result<LocalEncryptedPrivateObject> {
    let context = open_repository(cwd)?;
    let identity = local_encryption_identity(&context)?;
    let recipient = identity.recipient();
    let encrypted =
        forge_private::encrypt_private_payload(&recipient, plaintext).map_err(|_| {
            ForgeError::PrivateContentInvalid {
                reason: "private_payload_encrypt_failed".to_string(),
            }
        })?;
    let relative_path = PathBuf::from(".forge")
        .join("private")
        .join("objects")
        .join("sha256")
        .join(&encrypted.ciphertext_digest);
    let object_path = context.root_path.join(&relative_path);
    write_private_object_durable(&object_path, &encrypted.ciphertext)?;
    Ok(LocalEncryptedPrivateObject {
        envelope_format: encrypted.envelope_format,
        recipient_fingerprint: encrypted.recipient_fingerprint,
        ciphertext_digest: encrypted.ciphertext_digest,
        private_object_path: relative_path.to_string_lossy().replace('\\', "/"),
    })
}

fn write_private_object_durable(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("private object path has no parent"))?;
    fs::create_dir_all(parent).with_context(|| "create private object dir")?;
    signing::set_private_dir_permissions(parent)?;
    let temp_path = parent.join(format!(
        ".tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("private-object")
    ));
    {
        let mut file =
            fs::File::create(&temp_path).with_context(|| "create private object temp")?;
        use std::io::Write as _;
        file.write_all(bytes)
            .with_context(|| "write private ciphertext temp")?;
        file.sync_all()
            .with_context(|| "fsync private ciphertext")?;
    }
    signing::set_private_file_permissions(&temp_path)?;
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        if !path.exists() {
            return Err(error).with_context(|| "install private ciphertext object");
        }
    }
    fsync_dir(parent).with_context(|| "fsync private object dir")?;
    Ok(())
}

fn read_private_object(context: &RepositoryContext, relative_path: &str) -> Result<Vec<u8>> {
    let relative = normalize_private_object_path(relative_path)?;
    fs::read(context.root_path.join(relative)).with_context(|| "read private ciphertext object")
}

fn normalize_private_object_path(relative_path: &str) -> Result<PathBuf> {
    if relative_path.starts_with('/')
        || relative_path.contains('\\')
        || relative_path.contains("..")
        || !relative_path.starts_with(".forge/private/objects/sha256/")
    {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "invalid_private_object_path".to_string(),
        }
        .into());
    }
    let path = PathBuf::from(relative_path);
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "invalid_private_object_path".to_string(),
                }
                .into());
            }
        }
    }
    Ok(path)
}

fn write_materialized_private_file(cwd: &Path, path: &str, bytes: &[u8]) -> Result<()> {
    let path = normalize_private_label_path(path)?;
    ensure_no_symlink_components(cwd, &path)?;
    let full_path = cwd.join(&path);
    if let Some(parent) = full_path.parent() {
        fs::create_dir_all(parent).with_context(|| "create materialized private parent")?;
        ensure_no_symlink_components(cwd, &path)?;
    }
    fs::write(&full_path, bytes).with_context(|| "write materialized private path")?;
    Ok(())
}

fn ensure_no_symlink_components(cwd: &Path, path: &str) -> Result<()> {
    let mut current = PathBuf::from(cwd);
    for component in Path::new(path).components() {
        let Component::Normal(component) = component else {
            return Err(ForgeError::PrivateContentInvalid {
                reason: "invalid_private_path_label".to_string(),
            }
            .into());
        };
        current.push(component);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            if metadata.file_type().is_symlink() {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "private_path_symlink_escape".to_string(),
                }
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn fsync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| "open directory for fsync")?
        .sync_all()
        .with_context(|| "fsync directory")
}

#[cfg(not(unix))]
fn fsync_dir(_path: &Path) -> Result<()> {
    Ok(())
}

pub fn local_encryption_recipient(cwd: &Path) -> Result<String> {
    let context = open_repository(cwd)?;
    Ok(local_encryption_identity(&context)?
        .recipient()
        .as_str()
        .to_string())
}

pub fn private_overlay_transports_for_snapshots(
    cwd: &Path,
    snapshot_ids: &[String],
    recipient_principal_id: &str,
) -> Result<Vec<PrivateOverlayTransportRecord>> {
    let context = open_repository(cwd)?;
    let connection = open_connection(&context.database_path)?;
    let labels = read_local_private_path_labels(&context)?;
    let identity = local_encryption_identity(&context)?;
    let mut transports = Vec::new();

    for snapshot_id in snapshot_ids {
        let mut statement = connection.prepare(
            "SELECT id, work_package_kind, work_package_id, snapshot_id, path_label_id,
                    path_hash, envelope_format, recipient_fingerprint, ciphertext_digest,
                    private_object_path, encrypted_metadata_json, created_at_ms
             FROM encrypted_private_payloads
             WHERE repo_id = ?1 AND snapshot_id = ?2
             ORDER BY rowid",
        )?;
        let rows = statement.query_map(params![context.repo_id, snapshot_id], |row| {
            Ok(EncryptedPrivatePayloadRecord {
                payload_id: row.get(0)?,
                work_package_kind: row.get(1)?,
                work_package_id: row.get(2)?,
                snapshot_id: row.get(3)?,
                path_label_id: row.get(4)?,
                path_hash: row.get(5)?,
                envelope_format: row.get(6)?,
                recipient_fingerprint: row.get(7)?,
                ciphertext_digest: row.get(8)?,
                private_object_path: row.get(9)?,
                encrypted_metadata_json: row.get(10)?,
                created_at_ms: row.get(11)?,
            })
        })?;
        for row in rows {
            let record = row?;
            let authority = match private_decrypt_authority_on(
                &connection,
                &context.repo_id,
                &record.work_package_kind,
                &record.work_package_id,
                recipient_principal_id,
            ) {
                Ok(authority) => authority,
                Err(error)
                    if matches!(
                        error.downcast_ref::<ForgeError>(),
                        Some(ForgeError::PrivateDecryptAuthorityMissing { .. })
                    ) =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            };
            let Some(label) = labels.iter().find(|label| {
                label.work_package_kind == record.work_package_kind
                    && label.work_package_id == record.work_package_id
                    && label.path_label_id == record.path_label_id
                    && label.path_hash == record.path_hash
            }) else {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "missing_local_private_path_labels".to_string(),
                }
                .into());
            };
            let local_ciphertext = read_private_object(&context, &record.private_object_path)?;
            let plaintext = forge_private::decrypt_private_payload(
                &identity,
                &EncryptedPayload {
                    envelope_format: record.envelope_format.clone(),
                    recipient_fingerprint: record.recipient_fingerprint.clone(),
                    ciphertext: local_ciphertext,
                    ciphertext_digest: record.ciphertext_digest.clone(),
                },
            )
            .map_err(|_| ForgeError::PrivateContentInvalid {
                reason: "private_payload_decrypt_failed".to_string(),
            })?;
            let recipient = EncryptionRecipient::parse(&authority.public_key).map_err(|_| {
                ForgeError::PrivateContentInvalid {
                    reason: "invalid_encryption_recipient".to_string(),
                }
            })?;
            let encrypted = forge_private::encrypt_private_payload(&recipient, &plaintext)
                .map_err(|_| ForgeError::PrivateContentInvalid {
                    reason: "private_payload_encrypt_failed".to_string(),
                })?;
            transports.push(PrivateOverlayTransportRecord {
                work_package_kind: record.work_package_kind,
                work_package_id: record.work_package_id,
                snapshot_id: snapshot_id.clone(),
                path_label_id: record.path_label_id,
                path_hash: record.path_hash,
                path: label.path.clone(),
                visibility: label.visibility.clone(),
                envelope_format: encrypted.envelope_format,
                recipient_fingerprint: encrypted.recipient_fingerprint,
                ciphertext_digest: encrypted.ciphertext_digest,
                ciphertext: encrypted.ciphertext,
            });
        }
    }
    Ok(transports)
}

pub fn prepare_materialized_private_overlay(
    cwd: &Path,
    input: PrivateOverlayMaterializeInput,
) -> Result<MaterializedPrivateOverlay> {
    validate_work_package_kind(&input.work_package_kind)?;
    validate_visibility_label(&input.visibility)?;
    if input.visibility == VISIBILITY_PUBLIC {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "public_private_path_label".to_string(),
        }
        .into());
    }
    validate_private_hash(&input.path_hash)?;
    validate_private_hash(&input.ciphertext_digest)?;
    let context = open_repository(cwd)?;
    let identity = local_encryption_identity(&context)?;
    if identity.recipient().fingerprint() != input.recipient_fingerprint {
        return Err(ForgeError::PrivateDecryptAuthorityMissing {
            principal_id: input.recipient_fingerprint,
            reason: "recipient_private_key_missing".to_string(),
        }
        .into());
    }
    let path = normalize_private_label_path(&input.path)?;
    let plaintext = forge_private::decrypt_private_payload(
        &identity,
        &EncryptedPayload {
            envelope_format: input.envelope_format,
            recipient_fingerprint: input.recipient_fingerprint,
            ciphertext: input.ciphertext,
            ciphertext_digest: input.ciphertext_digest,
        },
    )
    .map_err(|_| ForgeError::PrivateContentInvalid {
        reason: "private_payload_decrypt_failed".to_string(),
    })?;
    Ok(MaterializedPrivateOverlay {
        work_package_kind: input.work_package_kind,
        work_package_id: input.work_package_id,
        path_label_id: input.path_label_id,
        path_hash: input.path_hash,
        path,
        visibility: input.visibility,
        plaintext,
    })
}

pub fn install_materialized_private_overlays(
    cwd: &Path,
    overlays: &[MaterializedPrivateOverlay],
) -> Result<usize> {
    for overlay in overlays {
        set_local_private_path_label(
            cwd,
            &overlay.work_package_kind,
            &overlay.work_package_id,
            &overlay.path,
            &overlay.visibility,
        )?;
    }
    for overlay in overlays {
        write_materialized_private_file(cwd, &overlay.path, &overlay.plaintext)?;
    }
    Ok(overlays.len())
}

pub(crate) fn validate_private_hash(value: &str) -> Result<()> {
    let hex = value
        .strip_prefix("hmac-sha256:")
        .or_else(|| value.strip_prefix("sha256:"))
        .unwrap_or(value);
    let valid = hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(ForgeError::PrivateContentInvalid {
            reason: "invalid_private_digest".to_string(),
        }
        .into())
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

fn normalize_private_label_path(path: &str) -> Result<String> {
    let path = path.trim();
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(':')
        || path.contains("..")
        || path.contains('*')
        || path.contains('?')
        || path.contains('[')
        || path.contains(']')
        || path.contains('\\')
    {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "invalid_private_path_label".to_string(),
        }
        .into());
    }
    for component in Path::new(path).components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(ForgeError::PrivateContentInvalid {
                    reason: "invalid_private_path_label".to_string(),
                }
                .into());
            }
        }
    }
    Ok(path.trim_start_matches("./").to_string())
}

fn local_private_label_path(context: &RepositoryContext) -> PathBuf {
    context
        .root_path
        .join(".forge")
        .join("private")
        .join("path-labels.json")
}

fn local_private_path_hash_key_path(context: &RepositoryContext) -> PathBuf {
    context
        .root_path
        .join(".forge")
        .join("keys")
        .join("private-path-hash.key")
}

fn local_private_path_hash_key(context: &RepositoryContext) -> Result<Vec<u8>> {
    let path = local_private_path_hash_key_path(context);
    if path.exists() {
        let encoded = fs::read_to_string(&path).with_context(|| "read private path hash key")?;
        let key = decode_hex(encoded.trim()).map_err(|_| ForgeError::PrivateContentInvalid {
            reason: "invalid_private_path_hash_key".to_string(),
        })?;
        if key.len() == 32 {
            return Ok(key);
        }
        return Err(ForgeError::PrivateContentInvalid {
            reason: "invalid_private_path_hash_key".to_string(),
        }
        .into());
    }

    if private_path_label_count_all(context)? > 0 {
        return Err(ForgeError::PrivateContentInvalid {
            reason: "missing_private_path_hash_key".to_string(),
        }
        .into());
    }

    let rng = ring::rand::SystemRandom::new();
    let mut key = [0_u8; 32];
    ring::rand::SecureRandom::fill(&rng, &mut key).map_err(|_| {
        ForgeError::PrivateContentInvalid {
            reason: "generate_private_path_hash_key_failed".to_string(),
        }
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| "create local key dir")?;
        signing::set_private_dir_permissions(parent)?;
    }
    fs::write(&path, format!("{}\n", hex_bytes(&key)))
        .with_context(|| "write private path hash key")?;
    signing::set_private_file_permissions(&path)?;
    Ok(key.to_vec())
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        bail!("odd-length hex");
    }
    let mut out = Vec::with_capacity(value.len() / 2);
    for chunk in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex"),
    }
}

fn read_local_private_path_labels(
    context: &RepositoryContext,
) -> Result<Vec<LocalPrivatePathLabel>> {
    let path = local_private_label_path(context);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(&path).with_context(|| "read local private path labels")?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&bytes).with_context(|| "parse local private path labels")
}

fn private_path_label_count(
    context: &RepositoryContext,
    work_package_kind: &str,
    work_package_id: &str,
) -> Result<usize> {
    let connection = open_connection(&context.database_path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM private_path_labels
         WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3",
        params![context.repo_id, work_package_kind, work_package_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

fn private_path_label_count_all(context: &RepositoryContext) -> Result<usize> {
    let connection = open_connection(&context.database_path)?;
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM private_path_labels WHERE repo_id = ?1",
        params![context.repo_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as usize)
}

fn write_local_private_path_labels(
    context: &RepositoryContext,
    labels: &[LocalPrivatePathLabel],
) -> Result<()> {
    let path = local_private_label_path(context);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| "create local private label dir")?;
        signing::set_private_dir_permissions(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(labels)?;
    fs::write(&path, bytes).with_context(|| "write local private path labels")?;
    signing::set_private_file_permissions(&path)?;
    Ok(())
}

fn local_encryption_identity_path(context: &RepositoryContext) -> PathBuf {
    context
        .root_path
        .join(".forge")
        .join("keys")
        .join("local-age-x25519.txt")
}

fn local_encryption_identity(
    context: &RepositoryContext,
) -> Result<forge_private::EncryptionIdentity> {
    let path = local_encryption_identity_path(context);
    if path.exists() {
        let secret = fs::read_to_string(&path).with_context(|| "read local encryption key")?;
        return forge_private::EncryptionIdentity::from_secret_str(secret.trim()).map_err(|_| {
            ForgeError::PrivateContentInvalid {
                reason: "invalid_local_encryption_identity".to_string(),
            }
            .into()
        });
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| "create local key dir")?;
        signing::set_private_dir_permissions(parent)?;
    }
    let identity = forge_private::EncryptionIdentity::generate();
    fs::write(&path, format!("{}\n", identity.to_secret_string()))
        .with_context(|| "write local encryption key")?;
    signing::set_private_file_permissions(&path)?;
    Ok(identity)
}

fn ensure_private_path_label_matches(
    conn: &Connection,
    repo_id: &str,
    path_label_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    path_hash: &str,
) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM private_path_labels
            WHERE repo_id = ?1
              AND id = ?2
              AND work_package_kind = ?3
              AND work_package_id = ?4
              AND path_hash = ?5
        )",
        params![
            repo_id,
            path_label_id,
            work_package_kind,
            work_package_id,
            path_hash
        ],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(ForgeError::PrivateContentInvalid {
            reason: "private_path_label_mismatch".to_string(),
        }
        .into())
    }
}

fn ensure_snapshot_exists(conn: &Connection, repo_id: &str, snapshot_id: &str) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE repo_id = ?1 AND id = ?2)",
        params![repo_id, snapshot_id],
        |row| row.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(ForgeError::PrivateContentInvalid {
            reason: "missing_snapshot".to_string(),
        }
        .into())
    }
}

fn org_encryption_key_binding_on(
    conn: &Connection,
    repo_id: &str,
    key_fingerprint: &str,
) -> Result<Option<OrgEncryptionKeyBindingRecord>> {
    conn.query_row(
        "SELECT id, principal_id, key_fingerprint, public_key, binding_authority, state,
                valid_from_revision, valid_until_revision, created_at_ms, updated_at_ms
         FROM org_encryption_key_bindings
         WHERE repo_id = ?1 AND key_fingerprint = ?2",
        params![repo_id, key_fingerprint],
        |row| {
            Ok(OrgEncryptionKeyBindingRecord {
                binding_id: row.get(0)?,
                principal_id: row.get(1)?,
                key_fingerprint: row.get(2)?,
                public_key: row.get(3)?,
                binding_authority: row.get(4)?,
                state: row.get(5)?,
                valid_from_revision: row.get(6)?,
                valid_until_revision: row.get(7)?,
                created_at_ms: row.get(8)?,
                updated_at_ms: row.get(9)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn private_path_label_on(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    path_hash: &str,
) -> Result<Option<PrivatePathLabelRecord>> {
    conn.query_row(
        "SELECT id, work_package_kind, work_package_id, path_hash, encrypted_display_path,
                visibility, created_at_ms, updated_at_ms
         FROM private_path_labels
         WHERE repo_id = ?1 AND work_package_kind = ?2 AND work_package_id = ?3 AND path_hash = ?4",
        params![repo_id, work_package_kind, work_package_id, path_hash],
        |row| {
            Ok(PrivatePathLabelRecord {
                path_label_id: row.get(0)?,
                work_package_kind: row.get(1)?,
                work_package_id: row.get(2)?,
                path_hash: row.get(3)?,
                encrypted_display_path: row.get(4)?,
                visibility: row.get(5)?,
                created_at_ms: row.get(6)?,
                updated_at_ms: row.get(7)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

fn encrypted_private_payload_on(
    conn: &Connection,
    repo_id: &str,
    payload_id: &str,
) -> Result<Option<EncryptedPrivatePayloadRecord>> {
    conn.query_row(
        "SELECT id, work_package_kind, work_package_id, snapshot_id, path_label_id,
                path_hash, envelope_format, recipient_fingerprint, ciphertext_digest,
                private_object_path, encrypted_metadata_json, created_at_ms
         FROM encrypted_private_payloads
         WHERE repo_id = ?1 AND id = ?2",
        params![repo_id, payload_id],
        |row| {
            Ok(EncryptedPrivatePayloadRecord {
                payload_id: row.get(0)?,
                work_package_kind: row.get(1)?,
                work_package_id: row.get(2)?,
                snapshot_id: row.get(3)?,
                path_label_id: row.get(4)?,
                path_hash: row.get(5)?,
                envelope_format: row.get(6)?,
                recipient_fingerprint: row.get(7)?,
                ciphertext_digest: row.get(8)?,
                private_object_path: row.get(9)?,
                encrypted_metadata_json: row.get(10)?,
                created_at_ms: row.get(11)?,
            })
        },
    )
    .optional()
    .map_err(Into::into)
}

pub(crate) fn insert_encrypted_private_payload_on(
    tx: &Transaction<'_>,
    repo_id: &str,
    input: EncryptedPrivatePayloadInput,
    now: i64,
) -> Result<EncryptedPrivatePayloadRecord> {
    ensure_work_package_exists(
        tx,
        repo_id,
        &input.work_package_kind,
        &input.work_package_id,
    )?;
    ensure_private_path_label_matches(
        tx,
        repo_id,
        &input.path_label_id,
        &input.work_package_kind,
        &input.work_package_id,
        &input.path_hash,
    )?;
    if let Some(snapshot_id) = input.snapshot_id.as_deref() {
        ensure_snapshot_exists(tx, repo_id, snapshot_id)?;
    }
    let payload_id = new_id("private_payload");
    tx.execute(
        "INSERT INTO encrypted_private_payloads (
            id, repo_id, work_package_kind, work_package_id, snapshot_id, path_label_id,
            path_hash, envelope_format, recipient_fingerprint, ciphertext_digest,
            private_object_path, encrypted_metadata_json, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            payload_id,
            repo_id,
            input.work_package_kind,
            input.work_package_id,
            input.snapshot_id,
            input.path_label_id,
            input.path_hash,
            input.envelope_format,
            input.recipient_fingerprint,
            input.ciphertext_digest,
            input.private_object_path,
            input.encrypted_metadata_json,
            now,
        ],
    )?;
    encrypted_private_payload_on(tx, repo_id, &payload_id)?
        .ok_or_else(|| anyhow!("encrypted private payload missing after insert"))
}

#[allow(clippy::too_many_arguments)]
fn insert_private_content_audit(
    tx: &Transaction<'_>,
    repo_id: &str,
    work_package_kind: Option<&str>,
    work_package_id: Option<&str>,
    snapshot_id: Option<&str>,
    path_label_id: Option<&str>,
    principal_id: Option<&str>,
    key_fingerprint: Option<&str>,
    action: &str,
    reason: Option<&str>,
    now: i64,
) -> Result<()> {
    let audit_id = new_id("private_audit");
    tx.execute(
        "INSERT INTO private_content_audit (
            id, repo_id, work_package_kind, work_package_id, snapshot_id, path_label_id,
            principal_id, key_fingerprint, action, reason, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            audit_id,
            repo_id,
            work_package_kind,
            work_package_id,
            snapshot_id,
            path_label_id,
            principal_id,
            key_fingerprint,
            action,
            reason,
            now,
        ],
    )?;
    Ok(())
}

fn private_decrypt_authority_on(
    conn: &Connection,
    repo_id: &str,
    work_package_kind: &str,
    work_package_id: &str,
    principal_id: &str,
) -> Result<PrivateDecryptAuthority> {
    let org = org_status_on(conn, repo_id)?;
    if !org.enabled {
        return Err(ForgeError::OrgNotEnabled.into());
    }
    ensure_active_org_principal(conn, repo_id, principal_id)?;
    ensure_active_org_role(
        conn,
        repo_id,
        principal_id,
        &[
            "owner",
            "maintainer",
            "member",
            "external_reviewer",
            "service",
        ],
    )?;
    if !has_active_visibility_grant(
        conn,
        repo_id,
        work_package_kind,
        work_package_id,
        principal_id,
        CAPABILITY_SYNC_MATERIALIZE,
    )? {
        let decision = projection_decision_on(
            conn,
            repo_id,
            work_package_kind,
            work_package_id,
            principal_id,
            CAPABILITY_SYNC_MATERIALIZE,
        )?;
        return Err(ForgeError::PrivateDecryptAuthorityMissing {
            principal_id: principal_id.to_string(),
            reason: format!("missing_visibility_grant:{}", decision.disclosure),
        }
        .into());
    }

    let binding: Option<(String, String, String)> = conn
        .query_row(
            "SELECT key_fingerprint, public_key, state
             FROM org_encryption_key_bindings
             WHERE repo_id = ?1
               AND principal_id = ?2
               AND state = 'active'
               AND valid_from_revision <= ?3
               AND (valid_until_revision IS NULL OR valid_until_revision > ?3)
             ORDER BY updated_at_ms DESC, created_at_ms DESC
             LIMIT 1",
            params![repo_id, principal_id, org.policy_revision],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((key_fingerprint, public_key, _state)) = binding else {
        return Err(ForgeError::PrivateDecryptAuthorityMissing {
            principal_id: principal_id.to_string(),
            reason: "missing_active_encryption_key".to_string(),
        }
        .into());
    };
    Ok(PrivateDecryptAuthority {
        principal_id: principal_id.to_string(),
        key_fingerprint: key_fingerprint.clone(),
        public_key,
        recipient_fingerprint: key_fingerprint,
        policy_revision: org.policy_revision,
    })
}
