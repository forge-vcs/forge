# Phase 9 Release Audit

Date: 2026-06-10
Audited commit: `d53986a docs: prepare P9 release dogfood gate`

This document maps the Phase 9 roadmap exit criteria to current executable
evidence. It is intentionally stricter than a status note: an item is marked
proven only when a named test, script, or CI gate exercises the requirement.

## Verification Gate

The release gate is:

```bash
rtk bash scripts/dogfood-release-gate.sh
```

Latest local run on `d53986a` passed:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 478 passed, 52 suites
- `scripts/e2e-eval.sh`: PASS=95 FAIL=0
- `scripts/dogfood-hosted-runner-attestation.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-release-litmus.sh`: PASS=32 FAIL=0
- `scripts/dogfood-native-sync-peer.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-peer-nogit.sh`: PASS=26 FAIL=0
- `scripts/dogfood-typescript-native.sh`: PASS=44 FAIL=0
- `scripts/dogfood-native-storage-scale.sh --smoke`: PASS=30 FAIL=0

PR #75 also passed GitHub `verify` before merge.

## Exit Criteria

| Roadmap criterion | Evidence | Status |
| --- | --- | --- |
| Two machines with no git installed clone/fetch/push/pull and reach byte-identical object stores and ledgers with consistent refs. | `scripts/dogfood-native-sync-release-litmus.sh` runs all Forge commands through a `PATH` containing `sh` but no `git`, simulates two isolated directories, then compares exported manifests for native objects, payloads, refs, and syncable domain-ledger rows. `scripts/dogfood-native-sync-peer-nogit.sh` separately covers file-URL clone/fetch/pull/push without git. | Proven |
| Incoming proposal conflicting with a moved target is merged or surfaced as conflict-as-data. | `scripts/dogfood-native-sync-release-litmus.sh` asserts a conflicting fetch succeeds with `merged=false`, records one conflict, and leaves `doctor` healthy. `crates/forge-cli/tests/forge_sync.rs` covers fetch/pull/push divergence, conflict set transport, dedup, and remote push conflicts. | Proven |
| Clean divergent sync records native merge commits at the receiver boundary. | `scripts/dogfood-native-sync-release-litmus.sh`, `scripts/dogfood-native-sync-peer.sh`, and `scripts/dogfood-native-sync-peer-nogit.sh` assert clean fetch/pull/push merge commit ids, HEAD convergence, and domain-ledger convergence. `crates/forge-cli/tests/forge_sync.rs` covers local, file, fake-SSH, and HTTP transports. | Proven |
| Native commits, decisions, and evidence are signed and verified by doctor/verify. | `crates/forge-cli/tests/forge_signatures.rs` verifies evidence, decision, native commit, key rotation, missing signature, invalid signature, and disappeared-subject cases. `scripts/e2e-eval.sh` runs `doctor` through signed lifecycle paths. | Proven |
| Trust ladder is enforced, including failure when a policy requires a stronger tier than available. | `crates/forge-cli/tests/forge_trust_policy.rs` covers `locally_signed`, hosted-runner, third-party, spoofed-upgrade rejection, missing evidence, and export policy. `scripts/dogfood-hosted-runner-attestation.sh` proves hosted accept and third-party export fail closed before attestation and pass after explicit issuer-key attestations. | Proven |
| Git-export consumer verifies a signed trailer back to the ledger. | `scripts/e2e-eval.sh` exports a ranked winner and verifies `Forge-Provenance-Digest`. `crates/forge-cli/tests/forge_accept_export.rs` verifies local signature fingerprint trailers and mismatch failure modes. | Proven |
| Full litmus test covers init, commit history, branch/divergence, merge with conflict resolution, clone/push to another machine, all without git. | The aggregate gate covers this end-to-end surface across scripts: no-git init/history/restore/undo in `scripts/e2e-eval.sh`; no-git clone/fetch/pull/push in `scripts/dogfood-native-sync-release-litmus.sh` and `scripts/dogfood-native-sync-peer-nogit.sh`; real TypeScript conflict suggestion, explicit conflict resolution, re-check, accept, and multi-workspace isolation in `scripts/dogfood-typescript-native.sh`. | Proven as aggregate gate, not one monolithic script |

## Current Release Boundary

The local/native release claim is supportable:

- Forge can run the core native lifecycle with git removed from `PATH`.
- Forge can sync native history and provenance between local/file/SSH/HTTPS peers.
- Forge can represent remote-boundary conflicts as typed conflict-as-data.
- Forge can sign local evidence, decisions, native commits, and sync merge commits.
- Forge can enforce local, hosted-runner, and third-party trust policies.
- Forge can still export accepted work to Git branches for existing PR workflows.

The supported public wording should avoid claiming a hosted multi-tenant service,
global identity, revocation infrastructure, resumable network transfer, or
cross-organization certificate authority. Hosted collaboration, permissions, and
identity governance remain product follow-ons.
