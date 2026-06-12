# Contributing to Forge

Thanks for helping improve Forge. The project is in public release-candidate
status, so small, well-tested changes are much easier to review than broad
rewrites.

Forge is MIT licensed. Unless stated otherwise, contributions are accepted under
the same license.

## Where to Start

- Use GitHub Issues for bugs, design questions, and feature requests.
- Use GitHub private vulnerability reporting for security issues:
  <https://github.com/freezscholte/forge/security/advisories/new>
- Check the current release boundary in `docs/P9_RELEASE_AUDIT.md` before
  expanding public claims.
- Keep roadmap-sized ideas in or linked from `docs/ROADMAP.md`.

## Development Setup

Forge is a Rust Cargo workspace. The Rust toolchain is pinned in
`rust-toolchain.toml`.

The repository uses `rtk` in maintainer automation, but public contributors can
run the same commands directly:

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

To mirror the normal CI gate:

```bash
bash scripts/ci.sh
```

For release-sensitive changes, run the broader dogfood gate:

```bash
bash scripts/dogfood-release-gate.sh
```

That gate covers formatting, clippy, workspace tests, binary end-to-end
evaluation, hosted and third-party attestation, native sync, no-git peer sync,
TypeScript dogfood, and native storage smoke testing.

## Working on Forge

Use a normal Git branch for changes:

```bash
git checkout -b your-branch-name
```

When dogfooding the `forge` binary interactively, use throwaway repositories
under `/tmp` or another scratch directory. Do not run ad-hoc `forge init` from
the Forge project root; that creates a local `.forge/` repository and can make
repo-scoped commands resolve the wrong repository. The checked-in `scripts/*.sh`
test gates are safe to run from the project root.

## Pull Request Expectations

A good PR includes:

- one logical change
- a clear summary of behavior changed
- tests or a reason tests are not applicable
- the exact commands used for validation
- documentation updates when commands, JSON output, trust behavior, sync
  behavior, or release claims change

Before opening a PR, run at least:

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

PRs to `main` must pass the GitHub `verify` check before merge.

## Code and Test Guidelines

- Keep command output machine-readable where `--json` is supported.
- Preserve the `forge.cli.v0` JSON envelope contract unless the change is
  intentionally versioned.
- Prefer focused integration tests in `crates/forge-cli/tests/` for CLI
  behavior.
- Add lower-level crate tests when changing storage, native content, sync,
  evidence, policy, export, or protocol behavior.
- Do not weaken snapshot exclusions, redaction, signature checks, trust policy,
  dirty-worktree guards, or provenance verification without an explicit design
  discussion.
- Use fake credentials and synthetic fixtures only. Never commit real tokens,
  private keys, customer data, proprietary source, or personal data.
- Keep fixtures small and explain unusual binary or generated test data.

## Commit Style

Use concise Conventional Commit messages when possible:

```text
feat: add native sync conflict summary
fix: preserve trust policy on export
docs: clarify release gate
test: cover redaction edge case
```

## AI-Assisted Contributions

AI-assisted and agent-produced contributions are welcome, but the contributor is
responsible for the final patch. Review generated code carefully, verify that it
does not copy incompatible licensed material, and include the tests and evidence
needed for maintainers to trust the change.
