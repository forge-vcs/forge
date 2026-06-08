---
base-sha: 4a97fca4338ac77cd4a861bab2ef76070b0d780c
head-sha: working-tree-before-commit
scope: codex/phase-9-trust-policy
---

# Phase 9 Trust Policy Code Review

## real-actionable

- Fixed: `trust policy --accept locally_signed` initially accepted a proposal with no evidence because there were no signature subjects to verify. The enforcement path now treats an empty subject set as `TRUST_POLICY_UNMET` with a `proposal_revision` missing-signature issue, and `forge_trust_policy.rs` covers the regression.

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
- Native dogfood with `trust policy --accept locally_signed --export locally_signed`
