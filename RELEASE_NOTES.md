# Forge Public Release Notes

## v0.1.0-rc2

Forge v0.1.0-rc2 is a public release candidate focused on the open-source
launch surface around rc1. The core local/native Forge CLI remains the rc1
feature set, with release readiness improvements for installation, security,
contribution, and agent onboarding.

### What Changed Since rc1

- Added the Forge agent plugin for Codex and Claude Code, including plugin
  manifests, marketplace metadata, and the `forge-cli` skill.
- Added README installation instructions that use the tagged GitHub release
  candidate:

```bash
cargo install --git https://github.com/freezscholte/forge --tag v0.1.0-rc2 forge-cli
```

- Added public `SECURITY.md` and `CONTRIBUTING.md` guides.
- Documented current package boundaries: Cargo GitHub tag install is supported;
  Homebrew and crates.io packages are planned but not published yet.
- Dogfooded the agent skill in a temporary repository against the local Forge
  binary, including lifecycle, multi-attempt, compare, export, and sync flows.

### Release Validation

Quick validation on `main` before preparing rc2:

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

All three commands passed locally on `d35f3a1`.

## v0.1.0-rc1

Forge v0.1.0-rc1 is the first public release candidate for the local/native
Forge surface: agent-native source control for checked change attempts, with
Git interop where existing PR workflows still need it.

### What Is Included

- Native Forge content storage with content-addressed blob, tree, and commit
  objects.
- Intent, attempt, snapshot, proposal, evidence, check, decision, publication,
  operation, and view records in a local SQLite ledger.
- Stable JSON envelopes for agent use with typed errors, warnings, details, and
  retry metadata.
- Declarative check gates bound to the exact proposal revision under review.
- Evidence redaction, structured evidence summaries, tamper-evident row hashes,
  and operation-chain verification.
- Compare/rank for competing attempts under one intent, including diff,
  per-gate results, structured metrics, and deterministic ranking.
- Native history navigation with `log`, `checkout`, `restore`, and `undo`.
- Native diff, merge, typed conflict-as-data, explicit conflict resolution, and
  evidence-backed conflict suggestions.
- Physical per-attempt workspaces, mark-sweep garbage collection, pack/index
  storage, retention accounting, and storage budget warnings.
- Local Ed25519 signatures for evidence, accepted decisions, native accepted
  commits, and sync merge commits.
- Trust policy enforcement for `locally_signed`, hosted-runner attestations, and
  third-party attestations.
- Versioned Forge sync manifests and peer clone/fetch/pull/push over local
  paths, `file://`, fake/real SSH command transport, and HTTPS `sync serve`
  endpoints.
- Git export interop with structured `Forge-*` provenance trailers and
  `export verify-branch`.

### Release Validation

The release gate is:

```bash
rtk bash scripts/dogfood-release-gate.sh
```

The gate runs formatting, clippy, the workspace test suite, binary e2e checks,
hosted/third-party attestation dogfood, no-git native sync litmus, peer sync,
TypeScript multi-workspace dogfood, and native storage-scale smoke.

The latest audited run is recorded in [docs/P9_RELEASE_AUDIT.md](docs/P9_RELEASE_AUDIT.md).

### Supported Claim

Forge can now honestly claim:

- the core native lifecycle runs without the git binary in `PATH`
- native sync transfers both content and the review/provenance ledger
- clean divergent peers converge through native merge commits
- true remote-boundary conflicts surface as typed conflict-as-data
- local evidence, decisions, and native commits are signed and verified
- accept/export can enforce local, hosted-runner, and third-party trust rungs
- accepted work can still be exported to Git branches for existing PR workflows

### Current Boundaries

This release candidate does not claim:

- hosted multi-tenant collaboration
- global identity governance
- certificate authority or revocation infrastructure
- resumable/partial network transfer
- hosted permissions or organization policy management
- stable hosted API compatibility

Those are product follow-ons, not blockers for the local/native public release.

### Before Tagging

1. Confirm `main` is clean and synced with `origin/main`.
2. Run `rtk bash scripts/dogfood-release-gate.sh`.
3. Confirm GitHub `verify` is green on the release-prep PR.
4. Confirm [docs/P9_RELEASE_AUDIT.md](docs/P9_RELEASE_AUDIT.md) still names the
   commit validated by the latest release gate, or update it.
5. Tag the release candidate, for example:

```bash
git tag -a v0.1.0-rc1 -m "Forge v0.1.0-rc1"
git push origin v0.1.0-rc1
```

6. Make the repository public only after the pushed tag, README, license, and
   release notes are visible on `main`.
