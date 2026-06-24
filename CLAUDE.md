# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What Forge is

Forge is an agent-native source-control CLI for checked change attempts, written in Rust. It records the lifecycle of agent- or human-produced changes in `.forge/forge.db` (SQLite) and Forge-native content objects so attempts can be reviewed, verified, synced, and published safely. The local/native surface is release-candidate complete; Git remains an interop/export boundary for existing PR workflows. The broader vision lives in `PRD.md`, `docs/ROADMAP.md`, and `docs/P9_RELEASE_AUDIT.md`.

Core lifecycle: `init → start <intent> → save → run -- <cmd> → propose → check → accept → sync/export`.

## Verify before done

Run all three and make sure they pass before considering any change complete (or use `/verify`):

```
rtk cargo fmt --all --check
rtk cargo test --workspace
rtk cargo clippy --workspace --all-targets -- -D warnings
```

Clippy runs with `-D warnings` — warnings are hard failures. Format with `rtk cargo fmt --all`. To mirror CI exactly in one shot — the trio **plus** the end-to-end eval that drives the real `forge` binary — run `rtk bash scripts/ci.sh` before pushing; `scripts/e2e-eval.sh` runs that eval on its own. GitHub Actions CI (`.github/workflows/ci.yml`) runs these same checks (fmt, test, clippy, then the e2e eval) on every push to `main` and every pull request; there is no Makefile or justfile — Cargo is the only build system.

For every feature branch, verification must prove both:

- the existing baseline still passes through the standard gates above
- the new behavior is exercised directly through focused tests, an e2e/scripted
  scenario, or dogfood steps that use the feature as a user would

Do not treat broad regression gates as sufficient for a feature branch unless
they actually cover the new behavior. The plan, PR description, or release notes
must name the new-feature scenarios that were run and what they proved.

For release/P9 closeout work, run the aggregate dogfood gate:

```
rtk bash scripts/dogfood-release-gate.sh
```

That gate runs fmt, clippy, workspace tests, binary e2e, hosted/third-party attestation dogfood, no-git native sync litmus, peer sync, TypeScript multi-workspace dogfood, and native storage-scale smoke. The latest audited release run is recorded in `docs/P9_RELEASE_AUDIT.md`.

## Engineering workflow

The compound-engineering (`ce-*`) skills are the default lifecycle. Two review gates are **non-optional** for any non-trivial change:

`/ce-ideate → /ce-brainstorm → /ce-plan → [doc-review gate] → /ce-work → [code-review gate] → /ce-commit-push-pr → /ce-resolve-pr-feedback → /ce-compound`

- **Doc-review gate (before implementation):** run `/ce-doc-review <plan-or-brainstorm>` before `/ce-work` on any new plan or requirements doc. Apply `safe_auto` fixes; fold the rest into the doc's open-questions, a Forge Linear ticket, or `docs/ROADMAP.md`.
- **Code-review gate (before opening the PR):** run `/verify` (the fmt/test/clippy trio above) **and** `/ce-code-review` on the branch diff before `/ce-commit-push-pr` — pass `plan:<path>` so it also checks that the plan's requirements actually shipped. Always-on correctness / maintainability / testing personas run on every diff; the adversarial persona fires on large diffs and the security persona on the snapshot-exclusion and secret-redaction paths (see § Security defaults). CI is the post-merge backstop, not a substitute for this gate.
- `ce-learnings-researcher` (run by `/ce-plan`, `/ce-debug`, `/ce-code-review`) greps `docs/solutions/` first, so every documented fix compounds into future planning and review. Close the loop with `/ce-compound` after solving anything non-obvious.
- `/lfg` chains `plan → work → code-review → commit-push-pr` non-interactively for well-scoped changes; both gates above run inside it.

Deferred findings from both gates are filed in Linear (see § Issue tracking).

## Issue tracking

Work is tracked in the **Forge** Linear project (id `2b5e82f7-7a78-4354-af7d-68609e6e77bc`, team **SE Engineers**, ticket prefix `NER`), reachable through the `linear-server` MCP (wired in the checked-in `.mcp.json` → `https://mcp.linear.app/mcp`). The doc-review and code-review gates route their defer-able findings into Forge Linear tickets; broader roadmap themes live in `docs/ROADMAP.md`.

**Verify the project on every ticket operation.** The workspace contains a separate, easily-confused project **`Nerdio Forge`** (an R&D project-management app) on the same team — file against **`Forge`** (the "Git alternative native to agents" project) only, so this repo's work doesn't scatter across boards.

## Layout

Single Cargo workspace. The binary is `forge` (`crates/forge-cli`). Library crates under `crates/` are split by concern: `forge-core` (ID types), `forge-store` (SQLite persistence, migrations, operations/views, trust/signature policy), `forge-content` (backend trait + secret-risk helpers), `forge-content-git` / `forge-content-native` (Git interop and Forge-native object storage/history/diff/merge/pack primitives), `forge-evidence` (command capture and parsers), `forge-policy` (check evaluation), `forge-protocol` (JSON envelope), `forge-export-git` (Git branch/PR-body/provenance export), and `forge-sync` (versioned native sync manifests plus local/file/SSH/HTTPS peer transport). Integration tests live in `crates/forge-cli/tests/` and use `assert_cmd` + `tempfile` against the compiled binary in real temp repos.

## Conventions

- Commit messages follow Conventional Commits (`feat:`, `fix:`, `chore:` …).
- JSON output uses serde with `#[serde(rename_all = "snake_case")]`; the `--json` envelope carries `schema_version: "forge.cli.v0"`.
- Error handling is `anyhow` throughout — no custom error types.
- rustfmt defaults apply (no `rustfmt.toml`); the toolchain is pinned to `1.92.0` (with `rustfmt` + `clippy` components) via `rust-toolchain.toml`.

## Gotchas

- `.forge/forge.db` (gitignored) must exist for every repository command except `init`, `schema`, and sync commands that explicitly bootstrap from a bundle.
- `rusqlite` uses the `bundled` feature — SQLite is statically linked, no system SQLite needed.
- `FORGE_CONTENT_BACKEND` (`git` or `native`) selects the backend when `--content-backend` is not passed to `forge init`. For release-candidate native workflows, prefer `forge init --content-backend native` explicitly.
- Mutating commands accept `--request-id <id>` for idempotency: replaying the same id returns the original result; reusing it for a different command errors `REQUEST_ID_CONFLICT`.
- Behavioral invariants that intentionally error: materializing commands (`restore`, `checkout`, `undo`, `attempt attach`, and `sync pull`) refuse a dirty worktree (`DIRTY_WORKTREE`); `accept` requires HEAD to still match the proposal's `base_head` (`STALE_BASE`); `gc` deletion requires a clean `doctor`, `--yes`, and `--plan-digest` from a prior dry-run; `export branch` requires an accepted proposal and a non-existent branch name.
- Run ad-hoc dogfood sessions (driving the `forge` binary through real `init`/`start`/lifecycle commands) **only inside throwaway `/tmp` repos, never from this project root** — a stray `forge init` here creates a gitignored `.forge/` in the repo root that then makes repo-scoped commands (e.g. `forge intent show`) resolve the wrong repo. The CI-style `scripts/*.sh` gates are fine to run from the root; this applies to interactive/multi-agent dogfooding.

## Security defaults (do not weaken without asking)

Snapshots and exports exclude `.forge`, `.env`, `.env.*`, private-key files, and credential paths. Evidence stdout/stderr is capped at 4096 bytes (`EXCERPT_LIMIT`) and redacted when secret-like assignments, high-entropy tokens, PEM blocks, JSON secrets, or credential URLs are detected before being stored.

Evidence, decisions, native accepted commits, and sync merge commits carry local Ed25519 signatures. `forge doctor` verifies the tamper-evident ledger chain, native history, packs, and signatures. `forge trust policy` can require `locally_signed`, hosted-runner, or third-party trust before accept/export. Peer-imported signatures remain cryptographically verifiable but do not silently satisfy local, hosted-runner, or third-party policy.

## Plans, handoffs, and solutions

These conventions make the `ce-*` lifecycle's outputs durable so knowledge compounds across sessions rather than being re-derived.

- **Plans** (`docs/plans/`): new plans land as `docs/plans/<date>-NNN-<type>-<name>-plan.md` with frontmatter `title` / `type` / `status` / `date` / `origin` (mirror the existing files). Run the doc-review gate before `/ce-work`. When the work ships and its PR merges, flip frontmatter to `status: completed` and move the file into `docs/plans/completed/`, updating any `docs/plans/<file>` cross-references in the same commit. Source requirements live alongside in `docs/brainstorms/`.
- **Solutions** (`docs/solutions/`): after solving anything non-obvious, `/ce-compound` writes a solution doc here with YAML frontmatter (`problem_type`, `module`, `tags`, `symptoms`). `ce-learnings-researcher` greps this folder during `/ce-plan` and `/ce-debug`, so each documented fix prevents re-litigating the same problem. See `docs/solutions/README.md`.
- **Handoffs** (`docs/handoffs/`): when the `handoff` skill writes a session-handoff doc, copy it from the skill's `mktemp` path to `docs/handoffs/<plan-stem>-phase-<n>.md` (or `docs/handoffs/<YYYY-MM-DD>-<kebab-slug>.md` for non-plan handoffs) and print an inline copy-paste start prompt. See `docs/handoffs/README.md`.
- **Code reviews** (`docs/code-reviews/`): `/ce-code-review` outputs are triaged into `docs/code-reviews/<YYYY-MM-DD>-<short-name>.md`, each pinning `base-sha` + `head-sha` and classifying findings into real-actionable / defer-able / defense-in-depth / reviewed-and-rejected. The reviewed-and-rejected section lets `ce-learnings-researcher` skip re-flagging known noise. See `docs/code-reviews/README.md`.

## Memory directory

Persistent agent memory lives under `~/.claude/projects/<project-path-slug>/memory/`; `MEMORY.md` is the index loaded into context each session. Claude derives `<project-path-slug>` from the absolute repository path by replacing path separators with `-`, so the exact directory is machine/user specific and should not be hard-coded in repo docs.
