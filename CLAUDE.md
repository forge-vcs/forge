# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Forge is

Forge is an agent-first local change-control CLI for existing Git repositories, written in Rust. It records the lifecycle of agent-produced changes in `.forge/forge.db` (SQLite) so they can be reviewed and published safely, without replacing Git. v0 is scoped to the solo-developer local loop; the broader vision lives in `PRD.md`.

Lifecycle: `init → start <intent> → save → run -- <cmd> → propose → check → accept → export branch <name>`.

## Verify before done

Run all three and make sure they pass before considering any change complete (or use `/verify`):

```
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Clippy runs with `-D warnings` — warnings are hard failures. Format with `cargo fmt --all`. There is no CI, Makefile, or justfile; Cargo is the only build system.

## Layout

Single Cargo workspace. The binary is `forge` (`crates/forge-cli`). Library crates under `crates/` are split by concern: `forge-core` (ID types), `forge-store` (SQLite persistence + `migrations/`), `forge-content` (backend trait + secret-risk helpers), `forge-content-git` / `forge-content-native` (the two backends), `forge-evidence` (command capture), `forge-policy` (check evaluation), `forge-protocol` (JSON envelope), `forge-export-git`. Integration tests live in `crates/forge-cli/tests/` and use `assert_cmd` + `tempfile` against the compiled binary in real temp Git repos.

## Conventions

- Commit messages follow Conventional Commits (`feat:`, `fix:`, `chore:` …).
- JSON output uses serde with `#[serde(rename_all = "snake_case")]`; the `--json` envelope carries `schema_version: "forge.cli.v0"`.
- Error handling is `anyhow` throughout — no custom error types.
- rustfmt defaults apply (no `rustfmt.toml`); stable toolchain (no `rust-toolchain.toml`).

## Gotchas

- `.forge/forge.db` (gitignored) must exist for every command except `init`.
- `rusqlite` uses the `bundled` feature — SQLite is statically linked, no system SQLite needed.
- `FORGE_CONTENT_BACKEND` (`git` default, or `native`) selects the backend when `--content-backend` is not passed to `forge init`.
- Mutating commands accept `--request-id <id>` for idempotency: replaying the same id returns the original result; reusing it for a different command errors `REQUEST_ID_CONFLICT`.
- Behavioral invariants that intentionally error: `restore` refuses a dirty worktree (`DIRTY_WORKTREE`); `accept` requires HEAD to still match the proposal's `base_head` (`STALE_BASE`); `gc` only supports `--dry-run` in v0; `export branch` requires an accepted proposal and a non-existent branch name.

## Security defaults (do not weaken without asking)

Snapshots and exports exclude `.forge`, `.env`, `.env.*`, private-key files, and credential paths. Evidence stdout/stderr is capped at 4096 bytes (`EXCERPT_LIMIT`) and redacted when secret-like `key=value` patterns are detected before being stored.
