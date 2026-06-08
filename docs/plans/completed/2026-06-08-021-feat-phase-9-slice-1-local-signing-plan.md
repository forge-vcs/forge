---
title: "feat: Phase 9 Slice 1 - local signing and doctor verification"
status: completed
date: 2026-06-08
origin: docs/ROADMAP.md
---

# Phase 9 Slice 1: Local Signing and Doctor Verification

## Problem Frame

Phase 5 made the Forge ledger tamper-evident with SHA-256 row hashes and an operation hash chain, but it deliberately did not make the chain tamper-proof. A writer with whole-DB access could still rewrite rows and recompute the chain. Phase 9 needs cryptographic signing and a trust ladder before Forge can claim stronger provenance.

This first slice adds local signed attestations without taking on remote sync, key identity, key rotation, hosted-runner trust, or third-party attestation.

## Requirements

- New evidence rows, decision rows, and native accepted commit ids get local Ed25519 signatures.
- Signatures are additive and do not change existing JSON envelope behavior for `run`, `accept`, or Git export.
- The signed subject is an existing stable digest or id:
  - evidence: `evidence.content_hash`
  - decision: `decisions.content_hash`
  - native commit: the `f1:commit:...` object id
- Legacy rows that predate the signing migration are grandfathered by rowid marker.
- `forge doctor` verifies local signatures and reports machine-readable `signature_issues`.
- Local signing is scoped honestly: it is not distributed attestation, hosted-runner trust, or cross-machine provenance.

## Implementation

- Added migration 011:
  - `ledger_signatures`
  - `signature_marker`
- Added a local Ed25519 signer backed by an audited crypto crate (`ring`).
- Stored the local private key at `.forge/keys/local-ed25519.pk8` with private filesystem permissions where supported.
- Signed:
  - normal command evidence from `forge run`
  - internal evidence from `forge conflict resolve`
  - accept/reject decision rows
  - native accepted commit ids
- Extended `doctor` with `signature_issues`, including:
  - `missing_signature`
  - `invalid_signature`
  - `digest_mismatch`
  - `subject_missing`
  - `malformed_signature`
- Updated `forge schema` notes to describe local signing and the remaining Phase 9 trust boundary.

## Verification

- `cargo test -p forge-cli --test forge_signatures`
- `cargo test -p forge-cli --test forge_tamper`
- `cargo test -p forge-cli --test forge_native_merge native_merge_overlapping_changes_persists_conflict_set`
- `cargo test -p forge-store`
- `cargo test --workspace`

## Deferred

- Trust policy enforcement such as "accept requires at least locally_signed".
- Key identity, user binding, key rotation, and revocation.
- Hosted-runner and third-party attestation.
- Cross-machine ledger sync and remote verification.
- Signed Git export trailer verification.
