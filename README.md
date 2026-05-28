# Forge

Agent-first local change control for existing Git repositories.

Forge v0 is a Rust CLI that records a local agent-work lifecycle:

```text
init -> start -> save -> run -> propose -> check -> accept -> export branch
```

The current implementation stores lifecycle metadata in `.forge/forge.db` and
emits stable JSON envelopes for agent use with `--json`. Repositories default to
Git tree objects as the content backend, but `forge init --content-backend native`
stores snapshots as Forge-native loose objects under `.forge/objects` while
still exporting accepted proposals to Git branches for existing PR workflows.

## Safety Defaults

- Snapshots and exported branches exclude `.forge`, `.env`, `.env.*`, private
  keys, credential files, and obvious secret-risk paths.
- Evidence excerpts redact common token, password, secret, and key assignments
  before JSON output or SQLite persistence. Redacted evidence is marked
  `secret_risk`.
- `forge run` caps captured stdout/stderr excerpts and defaults to a 30 second
  timeout. Use `forge run --timeout-ms <n> -- <command>` for a shorter local
  bound.
- `forge restore <snapshot-id> --yes` refuses unsaved dirty work. Restoring
  between saved snapshots materializes the target snapshot and removes files
  absent from that snapshot, except protected Forge/secret-risk paths.
- Mutating `--request-id` values are scoped to the command and replay the
  original success or failure. Reusing one for a different mutating command
  returns `REQUEST_ID_CONFLICT`.
- Checks, decisions, branch exports, and PR body context are bound to the exact
  proposal revision being reviewed or published.

## Current Commands

- `forge init`
- `forge init --content-backend native`
- `forge start <intent>`
- `forge save`
- `forge restore <snapshot-id> --yes`
- `forge run [--timeout-ms <n>] -- <command>`
- `forge propose`
- `forge show`
- `forge check`
- `forge accept`
- `forge reject`
- `forge doctor`
- `forge gc --dry-run`
- `forge export branch <name>`
- `forge export pr-body`

## Native Content Backend

Native mode writes immutable `f1:<type>:sha256:<digest>` blob and tree objects
under `.forge/objects/sha256`. It supports regular files, directory structure,
executable bits where available, exact restore, doctor integrity checks, and
`forge gc --dry-run` reachability reporting. It intentionally does not implement
packfiles, compression, remote sync, semantic merge, symlink/submodule handling,
or automatic snapshots yet.

## Development

```bash
rtk cargo fmt --all --check
rtk cargo test --workspace
rtk cargo clippy --workspace --all-targets -- -D warnings
```
