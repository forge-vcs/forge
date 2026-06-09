# Phase 9 Key Lifecycle and Sync Manifest Code Review

Scope: `codex/phase-9-key-lifecycle-wire-mvp` against `origin/main`.

Review mode: `compound-engineering:ce-code-review` skill discipline applied manually. The Codex environment exposed the skill instructions but no CE shell command or subagent dispatch tool, so the full persona pipeline could not be invoked. The review covered the diff, new untracked files, CLI JSON contract changes, key rotation behavior, sync manifest versioning, and test coverage.

## Actionable Findings

1. **Fixed - `sync inspect` accepted unsupported protocol versions.**
   - Risk: a future or malformed manifest with a different `protocol_version` could be summarized as if it were a supported `forge-sync.v1` artifact.
   - Fix: `forge-sync::inspect_manifest` now rejects manifests whose `protocol_version` is not `forge-sync.v1`, with a focused unit test.

## Verification Reviewed

- `rtk cargo fmt --all --check`
- `rtk cargo test --workspace`
- `rtk cargo clippy --workspace --all-targets -- -D warnings`
- `rtk bash scripts/e2e-eval.sh`
- Focused TypeScript dogfood: native backend, `locally_signed` accept policy, key rotation, second attempt, sync export/inspect, and `doctor`.

## Residual Scope

The sync work in this PR is a protocol manifest boundary only. It does not yet apply clone/fetch/push/pull, transfer object payload bytes, or import remote ledger rows. Those remain Phase 9 follow-on slices.
