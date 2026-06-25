use age::secrecy::ExposeSecret;
use forge_private::{
    decrypt_private_payload, encrypt_private_payload, EncryptionIdentity, EncryptionRecipient,
    PrivateCryptoError, ENVELOPE_FORMAT_AGE_X25519_V1,
};

#[test]
fn encrypts_and_decrypts_for_one_recipient() {
    let identity = EncryptionIdentity::generate();
    let recipient = identity.recipient();
    let plaintext = b"private extension source";

    let encrypted = encrypt_private_payload(&recipient, plaintext).expect("encrypts");
    let decrypted = decrypt_private_payload(&identity, &encrypted).expect("decrypts");

    assert_eq!(decrypted, plaintext);
    assert_eq!(encrypted.envelope_format, ENVELOPE_FORMAT_AGE_X25519_V1);
    assert_eq!(encrypted.recipient_fingerprint, recipient.fingerprint());
    assert!(!encrypted
        .ciphertext
        .windows(plaintext.len())
        .any(|window| window == plaintext));
}

#[test]
fn wrong_recipient_cannot_decrypt() {
    let owner = EncryptionIdentity::generate();
    let wrong = EncryptionIdentity::generate();
    let encrypted = encrypt_private_payload(&owner.recipient(), b"private").expect("encrypts");

    let error = decrypt_private_payload(&wrong, &encrypted).expect_err("wrong identity fails");

    assert!(matches!(error, PrivateCryptoError::Decrypt));
}

#[test]
fn tampered_ciphertext_fails_before_plaintext_returns() {
    let identity = EncryptionIdentity::generate();
    let mut encrypted =
        encrypt_private_payload(&identity.recipient(), b"private").expect("encrypts");
    let last = encrypted
        .ciphertext
        .last_mut()
        .expect("ciphertext is non-empty");
    *last ^= 0x01;

    let error = decrypt_private_payload(&identity, &encrypted).expect_err("tamper fails");

    assert!(matches!(error, PrivateCryptoError::Decrypt));
}

#[test]
fn unsupported_envelope_format_fails_before_plaintext_returns() {
    let identity = EncryptionIdentity::generate();
    let mut encrypted =
        encrypt_private_payload(&identity.recipient(), b"private").expect("encrypts");
    encrypted.envelope_format = "unsupported-format".to_string();

    let error = decrypt_private_payload(&identity, &encrypted).expect_err("format fails");

    assert!(matches!(error, PrivateCryptoError::Decrypt));
}

#[test]
fn identity_round_trips_through_secret_string() {
    let identity = EncryptionIdentity::generate();
    let secret = identity.to_secret();
    let parsed = EncryptionIdentity::from_secret(&secret).expect("parses identity");

    assert_eq!(parsed.recipient().as_str(), identity.recipient().as_str());
    assert!(secret.expose_secret().starts_with("AGE-SECRET-KEY-"));
}

#[test]
fn metadata_and_debug_do_not_expose_secret_or_plaintext() {
    let identity = EncryptionIdentity::generate();
    let recipient = identity.recipient();
    let plaintext = b"super-sensitive-private-source";
    let encrypted = encrypt_private_payload(&recipient, plaintext).expect("encrypts");

    let identity_debug = format!("{identity:?}");
    let metadata_json = serde_json::to_string(&encrypted.metadata()).expect("serializes metadata");
    let recipient_json = serde_json::to_string(&recipient).expect("serializes recipient");

    assert!(!identity_debug.contains("AGE-SECRET-KEY-"));
    assert!(identity_debug.contains("[REDACTED]"));
    assert!(!metadata_json.contains(std::str::from_utf8(plaintext).unwrap()));
    assert!(!metadata_json.contains("AGE-SECRET-KEY-"));
    assert!(metadata_json.contains(ENVELOPE_FORMAT_AGE_X25519_V1));
    assert!(recipient_json.contains(recipient.fingerprint()));
}

#[test]
fn invalid_recipient_is_typed_error() {
    let error = EncryptionRecipient::parse("not-an-age-recipient").expect_err("invalid");

    assert!(matches!(error, PrivateCryptoError::InvalidRecipient));
}
