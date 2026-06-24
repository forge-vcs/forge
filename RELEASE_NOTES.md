# Forge Public Release Notes

## v0.1.0-rc5

Forge v0.1.0-rc5 is a public release candidate focused on permissioned Forge
projections. It makes local visibility policy explicit and adds
recipient-scoped sync export/import hardening so restricted work can be kept out
of projected bundles.

### What Changed Since rc4

- Added visibility policy storage for work packages, including visibility
  labels, grants, revocation, audit rows, and typed visibility errors.
- Added `forge visibility` command coverage and schema metadata so agents can
  inspect policy and projection decisions without scraping human output.
- Added protocol-visible sync projection metadata for full and recipient-scoped
  manifests.
- Added recipient-scoped `sync export --recipient` support for
  `sync_materialize` projections.
- Hardened projected sync boundaries by filtering unsafe ledger tables,
  recomputing projected counts, validating reachable native object closures,
  rejecting unsupported projection capabilities, and rejecting mismatched
  incremental projection bases.
- Updated the shell e2e migration-head gate so it derives the expected migration
  version from migration files instead of a hardcoded version literal.

### Installation

```bash
cargo install --git https://github.com/freezscholte/forge --tag v0.1.0-rc5 forge-cli
```

### Release Validation

The rc5 preparation ran the aggregate release gate on `main` at `ba24105`:

```bash
rtk bash scripts/dogfood-release-gate.sh
```

Gate results:

- `cargo fmt --all --check`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- `cargo test --workspace`: 561 passed
- `scripts/e2e-eval.sh`: PASS=95 FAIL=0
- `scripts/dogfood-hosted-runner-attestation.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-release-litmus.sh`: PASS=32 FAIL=0
- `scripts/dogfood-native-sync-peer.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-peer-nogit.sh`: PASS=26 FAIL=0
- `scripts/dogfood-typescript-native.sh`: PASS=44 FAIL=0 with TypeScript 5.9.3
- `scripts/dogfood-native-storage-scale.sh --smoke`: PASS=30 FAIL=0

External dogfood validation also ran against the published `v0.1.0-rc5` tag at
`ba241050` in the `forge-dogfood` checkout:

- isolated `cargo install --git ... --tag v0.1.0-rc5` succeeded
- `forge --version`, `forge schema --json`, and `forge doctor --json` passed
- baseline `npm run typecheck`, `npm test`, and `npm run build` passed
- baseline `npm run lint` exposed dogfood app tooling friction: ESLint scanned
  Forge-managed attempt worktrees and needed a `.forge/**` ignore plus explicit
  TypeScript parser root
- the fix was completed through the rc5 Forge lifecycle:
  `forge start`, `forge save`, four `forge run` evidence commands,
  `forge propose --summary`, `forge check`, and `forge accept`
- `forge check` passed all four required gates: `npm run typecheck`,
  `npm test`, `npm run build`, and `npm run lint`
- accepted native commit:
  `f1:commit:sha256:377a30002adb0dda9b342dcef1291a8eec6f7ee56d01e7c548d2e79179f9edfe`

Follow-up feature-specific dogfood for permissioned projections found a
receiver-side blocker:

- `forge visibility set` marked the dogfood proposal private
- `forge visibility check` denied `rc5-outsider` with `disclosure: hidden`
- `forge visibility grant` allowed `rc5-release-auditor` to
  `sync_materialize`
- `forge sync export --recipient rc5-outsider` produced a projected bundle with
  `native_head: null`
- `forge sync export --recipient rc5-release-auditor` produced a projected
  bundle with native head
  `f1:commit:sha256:377a30002adb0dda9b342dcef1291a8eec6f7ee56d01e7c548d2e79179f9edfe`
- full, non-projected `forge sync export` plus `forge sync import --materialize`
  succeeded in a fresh receiver
- projected `forge sync import` failed with `COMMAND_FAILED: apply sync bundle`
  for the allowed recipient bundle, with and without `--materialize`
- projected `forge sync clone` failed with `COMMAND_FAILED: clone sync bundle`

That means rc5 validates visibility decisions and projected export metadata, but
does not validate projected receiver import/materialization. Treat this as a
known rc5 limitation and a required fix before the next release candidate. This
is tracked in Linear as NER-363.

### Current Boundary

This RC is still a local/native release candidate. Permissioned projections are
Forge-managed projection enforcement, not encryption or a hosted
identity/governance system. Recipient-scoped projection import/clone is not yet
validated in rc5 because feature-specific dogfood exposed receiver-side
`COMMAND_FAILED` failures for projected bundles.

## v0.1.0-rc4

Forge v0.1.0-rc4 is a public release candidate focused on the second
open-source dogfood feedback pass after rc3. It keeps the rc3 release boundary
intact while fixing command-line friction that showed up in fresh agent
workflows.

### What Changed Since rc3

- Fixed `forge --version` and help-display paths so they print successfully and
  exit with status `0`.
- Made `STALE_BASE` responses actionable by adding a reason, recovery hint, and
  concrete recovery steps to the JSON details and human message.
- Added a non-fatal `forge doctor` warning when JavaScript/TypeScript test
  configuration may scan Forge-managed `.forge/worktrees` directories without a
  `.forge/**` exclude.
- Confirmed the rc4 build locally through focused dogfood smokes for version
  exit status, stale-base recovery JSON, and the doctor JS/TS worktree warning.

### Installation

```bash
cargo install --git https://github.com/freezscholte/forge --tag v0.1.0-rc4 forge-cli
```

### Release Validation

The rc4 preparation ran the aggregate release gate:

```bash
bash scripts/dogfood-release-gate.sh
```

Gate results:

- `cargo fmt --all --check`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- `cargo test --workspace`: passed
- `scripts/e2e-eval.sh`: PASS=95 FAIL=0
- `scripts/dogfood-hosted-runner-attestation.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-release-litmus.sh`: PASS=32 FAIL=0
- `scripts/dogfood-native-sync-peer.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-peer-nogit.sh`: PASS=26 FAIL=0
- `scripts/dogfood-typescript-native.sh`: PASS=44 FAIL=0 with TypeScript 6.0.3
- `scripts/dogfood-native-storage-scale.sh --smoke`: PASS=30 FAIL=0

## v0.1.0-rc3

Forge v0.1.0-rc3 is a public release candidate focused on the first
open-source dogfood feedback after rc2. It keeps the rc1/rc2 release boundary
intact while tightening agent-facing UX, evidence hygiene, and public issue
intake.

### What Changed Since rc2

- Added GitHub issue templates for bug reports, feature requests, and security
  hardening requests.
- Added `forge propose --summary <text>` so agents can attach a concise proposal
  summary without depending on an unsupported positional argument.
- Improved unsupported structured-gate errors. For commands such as
  `--require-tests-pass "npm test"`, Forge now returns the original gate and a
  concrete `--require "npm test"` fallback suggestion for plain exit-code
  gating.
- Redacted local repository/worktree paths from persisted `forge run` evidence
  excerpts, including macOS `/private/tmp`/`/tmp` and
  `/private/var`/`/var` aliases.
- Documented `.forge/**` excludes for broad JavaScript/TypeScript test runners
  so tools such as Vitest do not discover duplicate tests in managed attempt
  worktrees.
- Documented stale-base recovery guidance for `accept` and `export`: start a
  fresh attempt from the current base, re-save, rerun evidence, then
  propose/check/accept again.
- Updated the Forge agent plugin install guidance to use the rc3 tag.

### Installation

```bash
cargo install --git https://github.com/freezscholte/forge --tag v0.1.0-rc3 forge-cli
```

### Release Validation

The rc3 preparation ran the aggregate release gate:

```bash
bash scripts/dogfood-release-gate.sh
```

The TypeScript dogfood portion used the local dogfood repository's TypeScript
binary on `PATH` because `tsc` is an explicit environment prerequisite for that
script. Gate results:

- `cargo fmt --all --check`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- `cargo test --workspace`: passed
- `scripts/e2e-eval.sh`: PASS=95 FAIL=0
- `scripts/dogfood-hosted-runner-attestation.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-release-litmus.sh`: PASS=32 FAIL=0
- `scripts/dogfood-native-sync-peer.sh`: PASS=26 FAIL=0
- `scripts/dogfood-native-sync-peer-nogit.sh`: PASS=26 FAIL=0
- `scripts/dogfood-typescript-native.sh`: PASS=44 FAIL=0 with TypeScript 6.0.3
- `scripts/dogfood-native-storage-scale.sh --smoke`: PASS=30 FAIL=0

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
