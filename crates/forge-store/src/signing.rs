use crate::{SignatureFinding, SignatureFindingKind};
use anyhow::{anyhow, Context, Result};
use forge_core::new_id;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const SIGNING_KEY_PATH: &str = ".forge/keys/local-ed25519.pk8";
const SIGNATURE_ALG: &str = "ed25519";
const TRUST_LEVEL: &str = "locally_signed";

pub(crate) struct LocalSigner {
    key_pair: Ed25519KeyPair,
    public_key: String,
    key_fingerprint: String,
}

impl LocalSigner {
    pub(crate) fn load_or_create(repo_root: &Path) -> Result<Self> {
        let path = repo_root.join(SIGNING_KEY_PATH);
        let pkcs8 = if path.exists() {
            fs::read(&path).with_context(|| "read local signing key")?
        } else {
            let rng = SystemRandom::new();
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
                .map_err(|_| anyhow!("generate local Ed25519 signing key"))?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).with_context(|| "create local signing key directory")?;
                set_private_dir_permissions(parent)?;
            }
            fs::write(&path, pkcs8.as_ref()).with_context(|| "write local signing key")?;
            set_private_file_permissions(&path)?;
            pkcs8.as_ref().to_vec()
        };
        let key_pair = Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map_err(|_| anyhow!("parse local Ed25519 signing key"))?;
        let public_key_bytes = key_pair.public_key().as_ref();
        let public_key = hex_lower(public_key_bytes);
        let key_fingerprint = key_fingerprint(public_key_bytes);
        Ok(Self {
            key_pair,
            public_key,
            key_fingerprint,
        })
    }

    pub(crate) fn sign_subject(
        &self,
        tx: &Transaction<'_>,
        repo_id: &str,
        subject_kind: &str,
        subject_id: &str,
        signed_digest: &str,
        created_at_ms: i64,
    ) -> Result<()> {
        let message = signing_message(subject_kind, subject_id, signed_digest);
        let signature = self.key_pair.sign(&message);
        tx.execute(
            "INSERT OR IGNORE INTO ledger_signatures (
                id, repo_id, subject_kind, subject_id, signed_digest, signature_alg,
                public_key, key_fingerprint, signature, trust_level, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                new_id("sig"),
                repo_id,
                subject_kind,
                subject_id,
                signed_digest,
                SIGNATURE_ALG,
                self.public_key,
                self.key_fingerprint,
                hex_lower(signature.as_ref()),
                TRUST_LEVEL,
                created_at_ms
            ],
        )?;
        Ok(())
    }
}

pub(crate) fn verify_signatures(conn: &Connection) -> Result<Vec<SignatureFinding>> {
    let mut findings = Vec::new();
    let mut valid = BTreeSet::new();

    let mut stmt = conn.prepare(
        "SELECT subject_kind, subject_id, signed_digest, public_key, key_fingerprint, signature
         FROM ledger_signatures
         ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;

    for row in rows {
        let (subject_kind, subject_id, signed_digest, public_key, key_fingerprint, signature) =
            row?;
        match current_subject_digest(conn, &subject_kind, &subject_id)? {
            None => findings.push(finding(
                SignatureFindingKind::SubjectMissing,
                &subject_kind,
                &subject_id,
                Some(&key_fingerprint),
            )),
            Some(current) if current != signed_digest => findings.push(finding(
                SignatureFindingKind::DigestMismatch,
                &subject_kind,
                &subject_id,
                Some(&key_fingerprint),
            )),
            Some(_) => {
                let public_key_bytes = match hex_decode(&public_key) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        findings.push(finding(
                            SignatureFindingKind::MalformedSignature,
                            &subject_kind,
                            &subject_id,
                            Some(&key_fingerprint),
                        ));
                        continue;
                    }
                };
                let signature_bytes = match hex_decode(&signature) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        findings.push(finding(
                            SignatureFindingKind::MalformedSignature,
                            &subject_kind,
                            &subject_id,
                            Some(&key_fingerprint),
                        ));
                        continue;
                    }
                };
                let message = signing_message(&subject_kind, &subject_id, &signed_digest);
                if UnparsedPublicKey::new(&ED25519, public_key_bytes)
                    .verify(&message, &signature_bytes)
                    .is_ok()
                {
                    valid.insert((subject_kind, subject_id, signed_digest));
                } else {
                    findings.push(finding(
                        SignatureFindingKind::InvalidSignature,
                        &subject_kind,
                        &subject_id,
                        Some(&key_fingerprint),
                    ));
                }
            }
        }
    }

    for (subject_kind, subject_id, signed_digest) in expected_signed_subjects(conn)? {
        if !valid.contains(&(
            subject_kind.clone(),
            subject_id.clone(),
            signed_digest.clone(),
        )) {
            findings.push(finding(
                SignatureFindingKind::MissingSignature,
                &subject_kind,
                &subject_id,
                None,
            ));
        }
    }

    Ok(findings)
}

fn expected_signed_subjects(conn: &Connection) -> Result<Vec<(String, String, String)>> {
    let marker = signature_marker(conn)?;
    let mut subjects = Vec::new();

    let mut evidence = conn.prepare(
        "SELECT id, content_hash FROM evidence
         WHERE rowid > ?1 AND content_hash IS NOT NULL
         ORDER BY rowid",
    )?;
    for row in evidence.query_map(params![marker.evidence_high_water], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })? {
        let (id, digest) = row?;
        subjects.push(("evidence".to_string(), id, digest));
    }

    let mut decisions = conn.prepare(
        "SELECT id, content_hash, commit_id FROM decisions
         WHERE rowid > ?1 AND content_hash IS NOT NULL
         ORDER BY rowid",
    )?;
    for row in decisions.query_map(params![marker.decision_high_water], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })? {
        let (id, digest, commit_id) = row?;
        subjects.push(("decision".to_string(), id, digest));
        if let Some(commit_id) = commit_id {
            subjects.push(("commit".to_string(), commit_id.clone(), commit_id));
        }
    }

    Ok(subjects)
}

fn current_subject_digest(
    conn: &Connection,
    subject_kind: &str,
    subject_id: &str,
) -> Result<Option<String>> {
    match subject_kind {
        "evidence" => conn
            .query_row(
                "SELECT content_hash FROM evidence WHERE id = ?1",
                params![subject_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(Into::into),
        "decision" => conn
            .query_row(
                "SELECT content_hash FROM decisions WHERE id = ?1",
                params![subject_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|value| value.flatten())
            .map_err(Into::into),
        "commit" => {
            let exists = conn
                .query_row(
                    "SELECT 1 FROM decisions WHERE commit_id = ?1 LIMIT 1",
                    params![subject_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            Ok(exists.then(|| subject_id.to_string()))
        }
        _ => Ok(None),
    }
}

struct SignatureMarker {
    evidence_high_water: i64,
    decision_high_water: i64,
}

fn signature_marker(conn: &Connection) -> Result<SignatureMarker> {
    conn.query_row(
        "SELECT evidence_high_water, decision_high_water
         FROM signature_marker WHERE singleton = 1",
        [],
        |row| {
            Ok(SignatureMarker {
                evidence_high_water: row.get(0)?,
                decision_high_water: row.get(1)?,
            })
        },
    )
    .map_err(Into::into)
}

fn signing_message(subject_kind: &str, subject_id: &str, signed_digest: &str) -> Vec<u8> {
    format!(
        "forge-ledger-signature-v1\nsubject_kind={subject_kind}\nsubject_id={subject_id}\nsigned_digest={signed_digest}\n"
    )
    .into_bytes()
}

fn finding(
    kind: SignatureFindingKind,
    subject_kind: &str,
    subject_id: &str,
    key_fingerprint: Option<&str>,
) -> SignatureFinding {
    SignatureFinding {
        kind,
        subject_kind: subject_kind.to_string(),
        subject_id: subject_id.to_string(),
        key_fingerprint: key_fingerprint.map(ToString::to_string),
    }
}

fn key_fingerprint(public_key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"forge-ed25519-public-key-v1\n");
    hasher.update(public_key);
    hex_lower(&hasher.finalize()[..16])
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hex_decode(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return Err(anyhow!("odd-length hex"));
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
        _ => Err(anyhow!("invalid hex")),
    }
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
