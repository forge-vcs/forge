---
base-sha: 09c553240a8d1762fee73daa1c7aee4af59f50c9
head-sha: working-tree-before-commit
scope: codex/phase-9-signed-export-bridge
---

# Phase 9 Signed Export Bridge Code Review

## real-actionable

- Fixed: the first verification implementation required `Forge-Local-Signature-Fingerprint` on every verified Forge commit, which would have broken older exported branches that only carry `Forge-Provenance-Digest`. Verification now preserves older digest-only exports and validates the local signature fingerprint only when the published commit carries that trailer.

## defer-able

- None.

## defense-in-depth

- None.

## reviewed-and-rejected

- None.

## verification

- `rtk cargo fmt --all --check`
- `rtk cargo test --workspace`
- `rtk cargo clippy --workspace --all-targets -- -D warnings`
- `rtk bash scripts/e2e-eval.sh`
- Native dogfood with `trust policy --accept locally_signed --export locally_signed`, export, and `export verify-branch` fingerprint round-trip
