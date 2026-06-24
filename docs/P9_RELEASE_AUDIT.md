# Phase 9 Release Audit

Date: 2026-06-24
Audited commit: `3c45e83 Merge pull request #99 from freezscholte/codex/permissioned-forge-projections`

This document maps the Phase 9 roadmap exit criteria to current executable
evidence. It is intentionally stricter than a status note: an item is marked
proven only when a named test, script, or CI gate exercises the requirement.

## Verification Gate

The release gate is:

```bash
rtk bash scripts/dogfood-release-gate.sh
```

The gate script also runs without `rtk` when `rtk` is not installed, which keeps
the public release check usable from a plain shell:

```bash
bash scripts/dogfood-release-gate.sh
```

Latest local run while preparing `v0.1.0-rc5` passed:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: passed
- `scripts/e2e-eval.sh`: PASS=95 FAIL=0
- `scripts/dogfood-hosted-runner-attestation.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-release-litmus.sh`: PASS=32 FAIL=0
- `scripts/dogfood-native-sync-peer.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-peer-nogit.sh`: PASS=26 FAIL=0
- `scripts/dogfood-typescript-native.sh`: PASS=44 FAIL=0 with TypeScript 5.9.3
- `scripts/dogfood-native-storage-scale.sh --smoke`: PASS=30 FAIL=0

PR #82 merged the public release metadata cleanup before the original audit
refresh. PR #88 added public issue templates, PR #94 addressed the first
Forge CLI dogfood feedback, PR #95 fixed macOS `/private/var` path-alias
redaction before the rc3 audit refresh, PR #97 fixed the second dogfood
feedback pass before the rc4 audit refresh, and PR #99 added permissioned sync
projections before this rc5 audit refresh.

## External Dogfood Validation

After publishing `v0.1.0-rc5`, the release candidate was installed from the
published Git tag at `ba241050` into an isolated PATH and run against the
`forge-dogfood` checkout.

Baseline app checks:

- `npm run typecheck`: passed
- `npm test`: passed
- `npm run build`: passed
- `npm run lint`: failed before the dogfood fix because ESLint scanned
  Forge-managed attempt worktrees and the TypeScript parser saw multiple
  candidate tsconfig roots

The lint/tooling friction was then fixed through the rc5 Forge lifecycle:

- `forge --version`: `forge 0.1.0`
- `forge schema --json`: passed
- `forge doctor --json`: passed on schema version 18
- `forge start --json --require "npm run typecheck" --require "npm test" --require "npm run build" --require "npm run lint" ...`: passed
- `forge save --json`: changed `docs/DOGFOOD_PLAN.md` and `eslint.config.js`
- `forge run --json -- npm run typecheck`: passed
- `forge run --json -- npm test`: passed, with local checkout path redacted from
  persisted output
- `forge run --json -- npm run build`: passed
- `forge run --json -- npm run lint`: passed
- `forge propose --json --summary ...`: passed
- `forge check --json`: passed all four required gates
- `forge accept --json`: accepted proposal
  `proposal_019ef8cfa5e6728391725ce8a0086aef` as native commit
  `f1:commit:sha256:377a30002adb0dda9b342dcef1291a8eec6f7ee56d01e7c548d2e79179f9edfe`

This generic lifecycle dogfood pass did not expose an rc5 Forge lifecycle
blocker. It did expose a repeatable JavaScript tooling requirement for projects
using Forge worktrees: broad lint/test tools must exclude `.forge/**`, and typed
ESLint configs should pin their parser root.

## Feature-Specific Projection Dogfood

Follow-up dogfood explicitly exercised the rc5 permissioned projection feature
in the `forge-dogfood` checkout.

Policy and export-side checks:

- `forge visibility set --kind proposal --id proposal_019ef8cfa5e6728391725ce8a0086aef --visibility private`: passed
- `forge visibility check --recipient rc5-outsider --capability sync_materialize`: passed with `allowed: false` and `disclosure: hidden`
- `forge visibility grant --recipient rc5-release-auditor --capability sync_materialize`: passed
- `forge visibility check --recipient rc5-release-auditor --capability sync_materialize`: passed with `allowed: true` and `disclosure: full`
- `forge sync export --recipient rc5-outsider --capability sync_materialize`: passed, producing a projected bundle with `native_head: null`
- `forge sync export --recipient rc5-release-auditor --capability sync_materialize`: passed, producing a projected bundle with native head `f1:commit:sha256:377a30002adb0dda9b342dcef1291a8eec6f7ee56d01e7c548d2e79179f9edfe`
- full, non-projected `forge sync export` followed by `forge sync import --materialize` into a fresh receiver: passed

Receiver-side projected bundle checks:

- allowed projected `forge sync import --materialize`: failed with `COMMAND_FAILED: apply sync bundle`
- allowed projected `forge sync import` without materialization: failed with `COMMAND_FAILED: apply sync bundle`
- allowed projected `forge sync clone`: failed with `COMMAND_FAILED: clone sync bundle`

Conclusion: rc5 proves visibility policy decisions and recipient-scoped export
metadata/count behavior, but does not prove projected receiver import or
materialization. This is a release-candidate limitation and should be fixed
before the next RC that claims end-to-end permissioned projections. Tracking:
NER-363.

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
- Forge can emit recipient-scoped sync projection bundles for local
  permissioned collaboration boundaries, but rc5 feature-specific dogfood found
  projected import/clone failures on the receiving side.
- Forge can still export accepted work to Git branches for existing PR workflows.

The supported public wording should avoid claiming a hosted multi-tenant service,
global identity, revocation infrastructure, resumable network transfer, or
cross-organization certificate authority. Hosted collaboration, organization-wide
policy management, and identity governance remain product follow-ons.
