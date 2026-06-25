---
name: forge-cli
description: Use Forge's agent-native source-control CLI safely. Use when a user asks an agent to initialize or operate a Forge repository, checkpoint work, run checked evidence, create/review/accept proposals, compare attempts, inspect conflicts, sync native history, export accepted work to Git, verify provenance, or reason about Forge command contracts.
---

# Forge CLI

## Overview

Forge is a local CLI for checked change attempts. Treat it as the source of
truth for intent, attempts, snapshots, evidence, checks, decisions, native
history, trust policy, and Git export provenance.

Prefer Forge commands over direct `.forge` file or SQLite edits. Prefer JSON
output where available, and run `forge schema --json` when a command shape,
error code, or field contract is unclear.

## First Checks

1. Confirm the CLI exists:

```bash
command -v forge
```

2. If `forge` is missing, suggest the current release-candidate install path:

```bash
cargo install --git https://github.com/forge-vcs/forge --tag v0.1.0-rc7 forge-cli
```

3. For a quick machine-readable capability check, run:

```bash
forge schema --json
```

If you only need to check install health, run `scripts/check-forge.sh` from this
skill directory.

## Default Lifecycle

Use this flow for a normal checked change:

```bash
forge init --content-backend native
forge start "short intent description" --require "cargo test"

# edit files
forge save
forge run -- cargo test
forge propose
forge check
forge accept
```

Use `forge run -- <command>` for evidence-producing commands. Do not claim a
proposal is checked unless Forge has recorded the evidence and `forge check`
passes for the proposal revision.

For JavaScript/TypeScript projects, add `.forge/**` to broad test-runner and
lint/tooling excludes. Forge keeps per-attempt worktrees under `.forge/`, and
tools like Vitest can otherwise discover duplicate tests in those managed
worktrees.

## Agent Operating Rules

- Keep work inside a real project repository or a temporary test repository.
- Do not run ad-hoc `forge init` inside the Forge source repository itself.
- Do not edit `.forge/forge.db`, `.forge/objects`, `.forge/refs`, packs, or
  signatures directly.
- Do not bypass dirty-worktree refusals from `restore`, `checkout`, `undo`,
  `attempt attach`, or materializing sync commands.
- Use explicit IDs in multi-attempt or multi-agent flows: `--intent`,
  `--attempt`, and `--proposal` where the command supports them.
- Treat ranking as advisory and evidence as authoritative.
- Run `forge doctor` before trust-sensitive maintenance, export, or release
  operations.
- Use `forge key status` and `forge trust policy` before changing accept/export
  trust requirements.
- Use `forge export verify-branch <branch>` after exporting to Git.
- When `accept` or `export` returns `STALE_BASE`, do not edit `.forge` or retry
  blindly. Start a fresh intent/attempt from the current base, re-save the
  desired changes, rerun evidence, then propose/check/accept again.

## Common Tasks

Inspect current state:

```bash
forge show
forge intent list
forge proposal list
forge log
forge doctor
```

Work with competing attempts:

```bash
forge attempt start --intent <intent-id>
forge attempt attach <attempt-id>
forge compare --intent <intent-id>
forge diff --working --to <snapshot-content-ref>
```

Inspect and resolve native conflicts:

```bash
forge conflict list
forge conflict show <conflict-id>
forge conflict show <conflict-id> --suggest
forge conflict resolve --tree <resolved-tree-ref> <conflict-id>
```

Sync native history and provenance:

```bash
forge sync export --output ./bundle.forge-sync.json
forge sync inspect ./bundle.forge-sync.json
forge sync import ./bundle.forge-sync.json
forge sync fetch /path/to/peer
forge sync pull file:///absolute/path/to/peer
forge sync push ssh://host/absolute/path/to/peer
```

`sync pull` materializes into the worktree and requires a clean worktree.
`sync push` requires a peer from the same Forge repository lineage, such as one
created by `forge sync clone`.

Export accepted work to Git:

```bash
forge export branch forge/<topic>
forge export verify-branch forge/<topic>
forge export pr-body --proposal <proposal-id>
```

## References

- Read `references/workflows.md` for complete workflow patterns.
- Read `references/safety.md` before destructive, trust-sensitive, sync,
  export, restore, checkout, undo, conflict resolution, or maintenance work.
