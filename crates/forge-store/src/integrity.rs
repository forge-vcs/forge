//! Canonical, domain-separated SHA-256 digests for the tamper-evident evidence
//! chain (NER-136, Phase 5).
//!
//! Every digest is computed over a **length-prefixed** field encoding so the byte
//! stream is *injective*: moving bytes across a field boundary, or an empty field
//! disappearing, cannot collide with a different field tuple (the NIST SP 800-185
//! TupleHash principle, applied under SHA-256 — no SHA-3 / TupleHash crate needed).
//! Each record kind carries a distinct domain-separation tag so an evidence digest
//! can never be confused with an operation-link or decision digest.
//!
//! These are **pure functions with zero IO** — the store is the only caller, and
//! every encoding property is a fast unit test. The hash is computed over the
//! values that are *persisted* (redacted + truncated excerpts), so a later
//! verification that recomputes from the stored columns matches by construction.
//!
//! **Scope (Phase 5): tamper-EVIDENT, not tamper-PROOF.** A hash chain detects an
//! editor who cannot recompute the whole downstream chain; an actor with full DB
//! write access can rewrite every link. Cryptographic signing (a key the rewriter
//! lacks) is Phase 9. `sha2` is already in-tree (migration checksums); custom
//! crypto is banned.

use sha2::{Digest, Sha256};

/// Domain-separation tag for an evidence-row digest.
const EVIDENCE_TAG: &[u8] = b"forge.evidence.v0\0";
/// Domain-separation tag for an operation chain-link digest.
const OPERATION_TAG: &[u8] = b"forge.op.v0\0";
/// Domain-separation tag for a decision-row digest.
const DECISION_TAG: &[u8] = b"forge.decision.v0\0";
/// Domain-separation tag for a publication provenance digest (NER-137). The trailer is
/// a new *aggregate* record kind — it bundles the proposal identity, the deciding
/// evidence rows' Phase 5 `content_hash`es, the decision digest, and the gate outcomes
/// — so it gets its own tag and can never collide with an evidence/decision/op digest.
const PUBLICATION_TAG: &[u8] = b"forge.publication.v0\0";
/// Domain-separation tag for a conflict-set digest (NER-139 Phase 8 S2a). The
/// digest covers the conflict_sets row plus ordered path_conflicts child rows.
const CONFLICT_SET_TAG: &[u8] = b"forge.conflict_set.v0\0";
/// Domain-separation tag for an opaque, non-reversible path identifier emitted in
/// conflict JSON instead of raw paths.
const PATH_FINGERPRINT_TAG: &[u8] = b"forge.path_fingerprint.v0\0";

/// The documented genesis parent hash: 64 hex zeros (a SHA-256 hex digest is 64
/// chars). Used as the `parent_hash` input for the `init` genesis operation and as
/// the fallback when an operation's parent predates Phase 5 (a legacy NULL hash), so
/// the first hashed link in any chain anchors against one canonical value.
pub const GENESIS_PARENT_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Length-prefixed, domain-separated digest builder. Each field is written as a
/// little-endian `u64` byte length followed by the field bytes, so the encoding is
/// injective for a fixed schema. `Option` fields write a 1-byte presence tag
/// (`0` = absent, `1` = present) so `None` ≠ `Some("")`.
struct DigestWriter {
    hasher: Sha256,
}

impl DigestWriter {
    fn new(domain_tag: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(domain_tag);
        Self { hasher }
    }

    fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.hasher.update((value.len() as u64).to_le_bytes());
        self.hasher.update(value);
        self
    }

    fn str(&mut self, value: &str) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    fn i64(&mut self, value: i64) -> &mut Self {
        self.bytes(&value.to_le_bytes())
    }

    fn bool(&mut self, value: bool) -> &mut Self {
        self.bytes(&[u8::from(value)])
    }

    /// A list of strings: the element count (length-prefixed) then each element
    /// (length-prefixed), so `["a","b"]` ≠ `["ab"]` ≠ `["a","","b"]`.
    fn str_slice(&mut self, values: &[String]) -> &mut Self {
        self.bytes(&(values.len() as u64).to_le_bytes());
        for value in values {
            self.str(value);
        }
        self
    }

    fn opt_str(&mut self, value: Option<&str>) -> &mut Self {
        match value {
            Some(inner) => {
                self.hasher.update([1u8]);
                self.str(inner);
            }
            None => {
                self.hasher.update([0u8]);
            }
        }
        self
    }

    fn finish(self) -> String {
        hex_encode(self.hasher.finalize())
    }
}

fn hex_encode(digest: impl AsRef<[u8]>) -> String {
    let digest = digest.as_ref();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Every identity-, outcome-, and behavior-bearing field of an evidence row that a
/// reviewer must be able to trust. The digest covers all mutable columns — including
/// `timed_out`, the truncation flags, `sensitivity`, `actor`, and `created_at_ms` —
/// so editing any of them without recomputing the hash is detectable (NER-136 R1).
pub struct EvidenceDigestInput<'a> {
    pub attempt_id: &'a str,
    pub snapshot_id: Option<&'a str>,
    pub command: &'a str,
    pub args: &'a [String],
    pub cwd: &'a str,
    pub exit_code: i64,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub timed_out: bool,
    pub stdout_excerpt: &'a str,
    pub stderr_excerpt: &'a str,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub sensitivity: &'a str,
    pub actor: &'a str,
    pub structured_json: Option<&'a str>,
    pub created_at_ms: i64,
}

/// The hex SHA-256 digest of an evidence row (its `content_hash`).
pub fn evidence_digest(input: &EvidenceDigestInput) -> String {
    let mut writer = DigestWriter::new(EVIDENCE_TAG);
    writer
        .str(input.attempt_id)
        .opt_str(input.snapshot_id)
        .str(input.command)
        .str_slice(input.args)
        .str(input.cwd)
        .i64(input.exit_code)
        .i64(input.started_at_ms)
        .i64(input.ended_at_ms)
        .bool(input.timed_out)
        .str(input.stdout_excerpt)
        .str(input.stderr_excerpt)
        .bool(input.stdout_truncated)
        .bool(input.stderr_truncated)
        .str(input.sensitivity)
        .str(input.actor)
        .opt_str(input.structured_json)
        .i64(input.created_at_ms);
    writer.finish()
}

/// The mutable fields of a decision row that attribution and integrity depend on.
pub struct DecisionDigestInput<'a> {
    pub proposal_id: &'a str,
    pub proposal_revision_id: &'a str,
    pub decision: &'a str,
    pub actor: &'a str,
    pub created_at_ms: i64,
}

/// The hex SHA-256 digest of a decision row (its `content_hash`).
pub fn decision_digest(input: &DecisionDigestInput) -> String {
    let mut writer = DigestWriter::new(DECISION_TAG);
    writer
        .str(input.proposal_id)
        .str(input.proposal_revision_id)
        .str(input.decision)
        .str(input.actor)
        .i64(input.created_at_ms);
    writer.finish()
}

/// The immutable identity of an operation row, folded into its chain link.
pub struct OperationDigestInput<'a> {
    pub operation_id: &'a str,
    pub command: &'a str,
    pub kind: &'a str,
    pub created_at_ms: i64,
}

/// The hex SHA-256 chain-link digest stored in `operations.content_hash`. Folds the
/// parent operation's hash (`GENESIS_PARENT_HASH` for the `init` genesis op or a
/// legacy NULL-hash parent) plus the operation's identity plus the `domain_digest`
/// of the evidence/decision row this operation created (`None` for operations with
/// no domain row — `init`, `propose`, `attach`, …). Folding the domain digest is
/// what lets `doctor` catch a "recompute the evidence row's own hash" tamper: the
/// op's link still folds the *old* digest, so the re-walk mismatches.
pub fn operation_link_hash(
    parent_hash: &str,
    op: &OperationDigestInput,
    domain_digest: Option<&str>,
) -> String {
    let mut writer = DigestWriter::new(OPERATION_TAG);
    writer
        .str(parent_hash)
        .str(op.operation_id)
        .str(op.command)
        .str(op.kind)
        .i64(op.created_at_ms)
        .opt_str(domain_digest);
    writer.finish()
}

/// The fields of a published proposal that a provenance trailer commits to (NER-137).
/// The "content-addressed evidence digest" is `evidence_hashes` — the deciding gates'
/// Phase 5 `content_hash`es, in gate order — so the digest is recomputable from the
/// local ledger and changes if any deciding evidence row is edited. `gate_outcomes`
/// are canonical (sorted) `"identity=verdict"` strings.
pub struct PublicationDigestInput<'a> {
    pub proposal_id: &'a str,
    pub proposal_revision_id: &'a str,
    pub evidence_hashes: &'a [String],
    pub decision_digest: &'a str,
    pub gate_outcomes: &'a [String],
}

/// The hex SHA-256 provenance digest carried in a published commit's
/// `Forge-Provenance-Digest` trailer (NER-137). Built with the same length-prefixed,
/// domain-separated `DigestWriter` discipline as every other digest — never an ad-hoc
/// `format!` + hash — so `verify-branch` recomputes it from the ledger by construction.
pub fn publication_digest(input: &PublicationDigestInput) -> String {
    let mut writer = DigestWriter::new(PUBLICATION_TAG);
    writer
        .str(input.proposal_id)
        .str(input.proposal_revision_id)
        .str_slice(input.evidence_hashes)
        .str(input.decision_digest)
        .str_slice(input.gate_outcomes);
    writer.finish()
}

pub struct PathConflictDigestInput<'a> {
    pub id: &'a str,
    pub path: &'a str,
    pub path_fingerprint: &'a str,
    pub base_path: Option<&'a str>,
    pub ours_path: Option<&'a str>,
    pub theirs_path: Option<&'a str>,
    pub kind: &'a str,
    pub base_ref: Option<&'a str>,
    pub ours_ref: Option<&'a str>,
    pub theirs_ref: Option<&'a str>,
    pub base_status: Option<&'a str>,
    pub ours_status: Option<&'a str>,
    pub theirs_status: Option<&'a str>,
    pub base_mode: Option<&'a str>,
    pub ours_mode: Option<&'a str>,
    pub theirs_mode: Option<&'a str>,
    pub resolution_ref: Option<&'a str>,
    pub status: &'a str,
    pub created_at_ms: i64,
}

pub struct ConflictSetDigestInput<'a> {
    pub id: &'a str,
    pub repo_id: &'a str,
    pub context: &'a str,
    pub paths_json: &'a str,
    pub base_content_ref: Option<&'a str>,
    pub ours_content_ref: Option<&'a str>,
    pub theirs_content_ref: Option<&'a str>,
    pub generated_by_operation_id: Option<&'a str>,
    pub resolver_backend: Option<&'a str>,
    pub status: &'a str,
    pub created_at_ms: i64,
    pub path_conflicts: &'a [PathConflictDigestInput<'a>],
}

pub fn path_fingerprint(path: &str) -> String {
    let mut writer = DigestWriter::new(PATH_FINGERPRINT_TAG);
    writer.str(path);
    writer.finish()
}

pub fn conflict_set_digest(input: &ConflictSetDigestInput) -> String {
    let mut writer = DigestWriter::new(CONFLICT_SET_TAG);
    writer
        .str(input.id)
        .str(input.repo_id)
        .str(input.context)
        .str(input.paths_json)
        .opt_str(input.base_content_ref)
        .opt_str(input.ours_content_ref)
        .opt_str(input.theirs_content_ref)
        .opt_str(input.generated_by_operation_id)
        .opt_str(input.resolver_backend)
        .str(input.status)
        .i64(input.created_at_ms)
        .bytes(&(input.path_conflicts.len() as u64).to_le_bytes());
    for path in input.path_conflicts {
        writer
            .str(path.id)
            .str(path.path)
            .str(path.path_fingerprint)
            .opt_str(path.base_path)
            .opt_str(path.ours_path)
            .opt_str(path.theirs_path)
            .str(path.kind)
            .opt_str(path.base_ref)
            .opt_str(path.ours_ref)
            .opt_str(path.theirs_ref)
            .opt_str(path.base_status)
            .opt_str(path.ours_status)
            .opt_str(path.theirs_status)
            .opt_str(path.base_mode)
            .opt_str(path.ours_mode)
            .opt_str(path.theirs_mode)
            .opt_str(path.resolution_ref)
            .str(path.status)
            .i64(path.created_at_ms);
    }
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_evidence() -> EvidenceDigestInput<'static> {
        EvidenceDigestInput {
            attempt_id: "attempt_1",
            snapshot_id: Some("snap_1"),
            command: "cargo",
            args: &[],
            cwd: "/repo",
            exit_code: 0,
            started_at_ms: 100,
            ended_at_ms: 200,
            timed_out: false,
            stdout_excerpt: "ok",
            stderr_excerpt: "",
            stdout_truncated: false,
            stderr_truncated: false,
            sensitivity: "normal",
            actor: "unknown",
            structured_json: None,
            created_at_ms: 150,
        }
    }

    #[test]
    fn evidence_digest_is_deterministic_and_64_hex() {
        let input = sample_evidence();
        let a = evidence_digest(&input);
        let b = evidence_digest(&sample_evidence());
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn evidence_digest_is_a_known_golden_vector() {
        // Pin the exact encoding so an accidental field-order / length-prefix change
        // is a test failure, not a silent chain break.
        assert_eq!(
            evidence_digest(&sample_evidence()),
            "0c2585df471e15fdc245390ba0561c716177fab1995cd7cab283d725b80adb51"
        );
    }

    #[test]
    fn args_length_prefix_is_injective() {
        // ["a","b"] must not collide with ["ab"] or ["a","","b"].
        let digest_with = |args: Vec<String>| {
            let mut input = sample_evidence();
            input.args = Box::leak(args.into_boxed_slice());
            evidence_digest(&input)
        };
        let ab_split = digest_with(vec!["a".to_string(), "b".to_string()]);
        let ab_joined = digest_with(vec!["ab".to_string()]);
        let ab_empty = digest_with(vec!["a".to_string(), String::new(), "b".to_string()]);
        assert_ne!(ab_split, ab_joined);
        assert_ne!(ab_split, ab_empty);
    }

    #[test]
    fn none_differs_from_empty_string_for_optionals() {
        let mut none_snap = sample_evidence();
        none_snap.snapshot_id = None;
        let mut empty_snap = sample_evidence();
        empty_snap.snapshot_id = Some("");
        assert_ne!(evidence_digest(&none_snap), evidence_digest(&empty_snap));
    }

    #[test]
    fn flipping_timed_out_changes_the_digest() {
        // A mutable behavioral flag must be covered, or an attacker could turn a
        // timeout-kill into an apparent clean exit without breaking the hash.
        let mut not_timed = sample_evidence();
        not_timed.timed_out = false;
        let mut timed = sample_evidence();
        timed.timed_out = true;
        assert_ne!(evidence_digest(&not_timed), evidence_digest(&timed));
    }

    #[test]
    fn flipping_sensitivity_or_truncation_changes_the_digest() {
        let base = evidence_digest(&sample_evidence());
        let mut secret = sample_evidence();
        secret.sensitivity = "secret_risk";
        assert_ne!(base, evidence_digest(&secret));
        let mut truncated = sample_evidence();
        truncated.stdout_truncated = true;
        assert_ne!(base, evidence_digest(&truncated));
    }

    #[test]
    fn changing_actor_changes_the_digest() {
        let base = evidence_digest(&sample_evidence());
        let mut other = sample_evidence();
        other.actor = "alice";
        assert_ne!(base, evidence_digest(&other));
    }

    #[test]
    fn empty_evidence_hashes_deterministically() {
        let mut empty = sample_evidence();
        empty.command = "true";
        empty.stdout_excerpt = "";
        empty.stderr_excerpt = "";
        let a = evidence_digest(&empty);
        assert_eq!(a, evidence_digest(&empty));
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn domain_tags_separate_record_kinds() {
        // An evidence digest, a decision digest, and an op link over comparable
        // inputs must not collide — the domain tag separates them.
        let evidence = evidence_digest(&sample_evidence());
        let decision = decision_digest(&DecisionDigestInput {
            proposal_id: "p",
            proposal_revision_id: "r",
            decision: "accepted",
            actor: "unknown",
            created_at_ms: 150,
        });
        let op = operation_link_hash(
            GENESIS_PARENT_HASH,
            &OperationDigestInput {
                operation_id: "o",
                command: "run",
                kind: "evidence_captured",
                created_at_ms: 150,
            },
            None,
        );
        assert_ne!(evidence, decision);
        assert_ne!(evidence, op);
        assert_ne!(decision, op);
    }

    #[test]
    fn operation_link_depends_on_parent_and_domain_digest() {
        let op = OperationDigestInput {
            operation_id: "o1",
            command: "run",
            kind: "evidence_captured",
            created_at_ms: 150,
        };
        let genesis = operation_link_hash(GENESIS_PARENT_HASH, &op, None);
        let other_parent = operation_link_hash("aa", &op, None);
        let with_domain = operation_link_hash(GENESIS_PARENT_HASH, &op, Some("dd"));
        assert_ne!(genesis, other_parent);
        assert_ne!(genesis, with_domain);
    }

    fn sample_publication() -> (String, String, Vec<String>, String, Vec<String>) {
        (
            "proposal_1".to_string(),
            "revision_1".to_string(),
            vec!["evhash_a".to_string(), "evhash_b".to_string()],
            "decdigest".to_string(),
            vec!["cargo test=passed".to_string()],
        )
    }

    fn pub_digest_of(p: &str, r: &str, ev: &[String], dec: &str, gates: &[String]) -> String {
        publication_digest(&PublicationDigestInput {
            proposal_id: p,
            proposal_revision_id: r,
            evidence_hashes: ev,
            decision_digest: dec,
            gate_outcomes: gates,
        })
    }

    #[test]
    fn publication_digest_is_deterministic_and_a_known_golden_vector() {
        let (p, r, ev, dec, gates) = sample_publication();
        let a = pub_digest_of(&p, &r, &ev, &dec, &gates);
        let b = pub_digest_of(&p, &r, &ev, &dec, &gates);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Pin the encoding so a field-order / length-prefix change is a test failure,
        // not a silent change to every published commit's digest.
        assert_eq!(
            a,
            "824f0e1d7e7c5d712cd26ac57ece58fd670cd88d0a57edb3e84ce98700fa6d9d"
        );
    }

    #[test]
    fn publication_digest_changes_with_any_folded_field() {
        let (p, r, ev, dec, gates) = sample_publication();
        let base = pub_digest_of(&p, &r, &ev, &dec, &gates);
        // A different deciding evidence hash (the content-addressed part) changes it.
        let mut ev2 = ev.clone();
        ev2[0] = "TAMPERED".to_string();
        assert_ne!(base, pub_digest_of(&p, &r, &ev2, &dec, &gates));
        // A different decision digest changes it.
        assert_ne!(base, pub_digest_of(&p, &r, &ev, "other", &gates));
        // A different gate outcome changes it.
        assert_ne!(
            base,
            pub_digest_of(&p, &r, &ev, &dec, &["cargo test=failed".to_string()])
        );
        // Length-prefix injectivity: dropping an evidence hash is not the same as
        // joining two.
        let joined = vec!["evhash_aevhash_b".to_string()];
        assert_ne!(base, pub_digest_of(&p, &r, &joined, &dec, &gates));
    }

    #[test]
    fn publication_digest_does_not_collide_with_other_record_kinds() {
        let (p, r, ev, dec, gates) = sample_publication();
        let publication = pub_digest_of(&p, &r, &ev, &dec, &gates);
        let evidence = evidence_digest(&sample_evidence());
        let decision = decision_digest(&DecisionDigestInput {
            proposal_id: "p",
            proposal_revision_id: "r",
            decision: "accepted",
            actor: "unknown",
            created_at_ms: 150,
        });
        assert_ne!(publication, evidence);
        assert_ne!(publication, decision);
    }
}
