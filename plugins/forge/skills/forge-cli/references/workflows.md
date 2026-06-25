# Forge CLI Workflow Reference

Use this file when a task needs a fuller Forge workflow than the quick lifecycle
in `SKILL.md`.

## Install and Inspect

Install the current release candidate:

```bash
cargo install --git https://github.com/forge-vcs/forge --tag v0.1.0-rc7 forge-cli
```

Inspect command contracts:

```bash
command -v forge
forge schema --json
```

Use `forge schema --json` instead of guessing JSON shapes, error codes, or
retryability. The schema is the machine-readable command contract for agents.

## Single Attempt

```bash
forge init --content-backend native
forge start "implement the requested change" --require "cargo test"
forge save
forge run -- cargo test
forge propose
forge check
forge accept
```

Notes:

- `forge save` snapshots the current worktree.
- `forge run -- <command>` records evidence for the current attempt.
- `forge propose` creates or updates a proposal over the saved content.
- `forge check` evaluates the proposal against declared requirements.
- `forge accept` records the decision and native accepted commit.

For JavaScript/TypeScript projects, exclude `.forge/**` from test runners and
linters before recording evidence. Example Vitest config:

```ts
import { configDefaults, defineConfig } from 'vitest/config'

export default defineConfig({
  test: {
    exclude: [...configDefaults.exclude, '.forge/**'],
  },
})
```

## Multiple Attempts for One Intent

Use explicit IDs. Do not rely on ambient state when more than one attempt is
active.

```bash
forge intent list
forge attempt start --intent <intent-id>
forge attempt attach <attempt-id>
forge save --attempt <attempt-id>
forge run --attempt <attempt-id> -- cargo test
forge propose --attempt <attempt-id>
forge compare --intent <intent-id>
```

If Forge reports an ambiguous attempt, inspect the candidate IDs and rerun with
an explicit selector.

## Review and Selection

```bash
forge proposal list
forge show --attempt <attempt-id>
forge compare --intent <intent-id>
forge diff --working --to <snapshot-content-ref>
forge diff --from <old-tree-content-ref> --to <new-tree-content-ref>
forge check --proposal <proposal-id>
forge accept --proposal <proposal-id>
forge reject --proposal <proposal-id>
```

Treat `compare` ranking as advisory. Use the underlying evidence, checks, and
diffs to justify selection.

## Stale Base Recovery

If `forge accept` or `forge export branch` returns `STALE_BASE`, the proposal's
base no longer matches the repository head. The checked proposal is not wrong,
but it is no longer directly acceptable.

Use a fresh attempt from the current base:

```bash
forge start "reapply <change> on current base"
# reapply the desired edits
forge save
forge run -- <required-check>
forge propose
forge check
forge accept
```

Do not edit `.forge` internals or force the old decision. If the original
attempt is still useful, inspect it with `forge show --attempt <attempt-id>` and
copy the source-level edits into the fresh attempt.

## Native Conflict Flow

When Forge surfaces conflict-as-data:

```bash
forge merge --proposal <proposal-id>
forge conflict list
forge conflict show <conflict-id>
forge conflict show <conflict-id> --suggest
forge conflict resolve --tree <resolved-tree-ref> <conflict-id>
forge check --proposal <proposal-id>
forge accept --proposal <proposal-id>
```

Do not silently choose one side of a conflict. A resolved conflict should become
new evidence and be checked again before accept.

## Native Sync

Use sync to move Forge content and ledger provenance between repositories:

```bash
forge sync export --output ./bundle.forge-sync.json
forge sync inspect ./bundle.forge-sync.json
forge sync import ./bundle.forge-sync.json
forge sync clone ./bundle.forge-sync.json
forge sync fetch /path/to/peer
forge sync pull file:///absolute/path/to/peer
forge sync push ssh://host/absolute/path/to/peer
```

Run `forge doctor` after imports or unusual sync results. If conflicts are
persisted, inspect them with `forge conflict list` and `forge conflict show`.
`sync pull` requires a clean worktree because it may materialize remote content.
`sync push` requires a peer from the same Forge repository lineage, such as a
repository created by `forge sync clone`; pushing into an unrelated
`forge init` repository is rejected.

## Git Export

Use Git export only after a proposal is accepted:

```bash
forge export branch forge/<topic>
forge export verify-branch forge/<topic>
forge export pr-body --proposal <proposal-id>
```

The verification step checks Forge provenance trailers back to the ledger. Do
not ask a reviewer to trust an exported branch until verification succeeds.
