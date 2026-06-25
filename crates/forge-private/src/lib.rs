use age::secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

pub const ENVELOPE_FORMAT_AGE_X25519_V1: &str = "age-x25519-v1";

#[derive(Debug)]
pub enum PrivateCryptoError {
    InvalidIdentity,
    InvalidRecipient,
    Encrypt,
    Decrypt,
}

impl fmt::Display for PrivateCryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidIdentity => "invalid private encryption identity",
            Self::InvalidRecipient => "invalid private encryption recipient",
            Self::Encrypt => "encrypt private payload",
            Self::Decrypt => "decrypt private payload",
        };
        f.write_str(message)
    }
}

impl std::error::Error for PrivateCryptoError {}

pub type Result<T> = std::result::Result<T, PrivateCryptoError>;

pub struct EncryptionIdentity {
    identity: age::x25519::Identity,
}

impl EncryptionIdentity {
    pub fn generate() -> Self {
        Self {
            identity: age::x25519::Identity::generate(),
        }
    }

    pub fn from_secret(secret: &SecretString) -> Result<Self> {
        let identity = age::x25519::Identity::from_str(secret.expose_secret())
            .map_err(|_| PrivateCryptoError::InvalidIdentity)?;
        Ok(Self { identity })
    }

    pub fn from_secret_str(secret: &str) -> Result<Self> {
        let identity = age::x25519::Identity::from_str(secret)
            .map_err(|_| PrivateCryptoError::InvalidIdentity)?;
        Ok(Self { identity })
    }

    pub fn to_secret(&self) -> SecretString {
        SecretString::from(self.identity.to_string())
    }

    pub fn to_secret_string(&self) -> String {
        self.identity.to_string().expose_secret().to_string()
    }

    pub fn recipient(&self) -> EncryptionRecipient {
        EncryptionRecipient::from_age_recipient(self.identity.to_public())
    }
}

impl fmt::Debug for EncryptionIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptionIdentity")
            .field("secret", &"[REDACTED]")
            .field("recipient_fingerprint", &self.recipient().fingerprint)
            .finish()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptionRecipient {
    recipient: String,
    fingerprint: String,
}

impl EncryptionRecipient {
    pub fn parse(recipient: &str) -> Result<Self> {
        let parsed = age::x25519::Recipient::from_str(recipient)
            .map_err(|_| PrivateCryptoError::InvalidRecipient)?;
        Ok(Self::from_age_recipient(parsed))
    }

    pub fn as_str(&self) -> &str {
        &self.recipient
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    fn from_age_recipient(recipient: age::x25519::Recipient) -> Self {
        let recipient = recipient.to_string();
        let fingerprint = recipient_fingerprint(&recipient);
        Self {
            recipient,
            fingerprint,
        }
    }

    fn age_recipient(&self) -> Result<age::x25519::Recipient> {
        age::x25519::Recipient::from_str(&self.recipient)
            .map_err(|_| PrivateCryptoError::InvalidRecipient)
    }
}

impl fmt::Debug for EncryptionRecipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncryptionRecipient")
            .field("recipient", &self.recipient)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedPayload {
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext: Vec<u8>,
    pub ciphertext_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvelopeMetadata {
    pub envelope_format: String,
    pub recipient_fingerprint: String,
    pub ciphertext_digest: String,
}

impl EncryptedPayload {
    pub fn metadata(&self) -> EnvelopeMetadata {
        EnvelopeMetadata {
            envelope_format: self.envelope_format.clone(),
            recipient_fingerprint: self.recipient_fingerprint.clone(),
            ciphertext_digest: self.ciphertext_digest.clone(),
        }
    }
}

pub fn encrypt_private_payload(
    recipient: &EncryptionRecipient,
    plaintext: &[u8],
) -> Result<EncryptedPayload> {
    let age_recipient = recipient.age_recipient()?;
    let ciphertext =
        age::encrypt(&age_recipient, plaintext).map_err(|_| PrivateCryptoError::Encrypt)?;
    let ciphertext_digest = sha256_hex(&ciphertext);
    Ok(EncryptedPayload {
        envelope_format: ENVELOPE_FORMAT_AGE_X25519_V1.to_string(),
        recipient_fingerprint: recipient.fingerprint().to_string(),
        ciphertext,
        ciphertext_digest,
    })
}

pub fn decrypt_private_payload(
    identity: &EncryptionIdentity,
    encrypted: &EncryptedPayload,
) -> Result<Vec<u8>> {
    if encrypted.envelope_format != ENVELOPE_FORMAT_AGE_X25519_V1 {
        return Err(PrivateCryptoError::Decrypt);
    }
    if sha256_hex(&encrypted.ciphertext) != encrypted.ciphertext_digest {
        return Err(PrivateCryptoError::Decrypt);
    }
    age::decrypt(&identity.identity, &encrypted.ciphertext).map_err(|_| PrivateCryptoError::Decrypt)
}

pub fn recipient_fingerprint(recipient: &str) -> String {
    format!("age-x25519:{}", sha256_hex(recipient.as_bytes()))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
