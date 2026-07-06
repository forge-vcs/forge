# UNKNOWN

**Kind:** blocking

## What I need to know

The TASK CONTRACT itself. The task instruction says "Implement exactly the
task specified in the TASK CONTRACT above," but no contract was included in
the prompt I received — no task description, no `allowed_changes` list, and
no acceptance commands.

## Why the brief does not answer it

The instruction block I received contains only the generic rules (stay
within `allowed_changes`, run the contract's acceptance commands, etc.) and
no contract content. Without it I cannot know what to implement, which
paths are in scope, or what commands must pass.

## Evidence that no contract exists in the repository

- Repo root listing shows no contract file (`CLAUDE.md`, `PRD.md`,
  `ROADMAP.md`, etc. are general project docs, not a task contract).
- `find` across the worktree for files named `*contract*`, `*task*`, or
  `*brief*` matched only
  `docs/solutions/architecture-patterns/schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md`,
  an unrelated architecture note.
- `grep -ril "TASK CONTRACT\|allowed_changes"` over all md/yaml/json/toml
  files (excluding `target/` and `.git/`) returned no matches.
- The most recent commit (`b9b3917`, "pilot: strip CLAUDE.md to mechanics
  (arm A)") only edited `CLAUDE.md:1` and does not introduce a contract.

## Best guess

The pilot harness was supposed to prepend the contract to the prompt (or
drop a contract file into this worktree) and that step was skipped or
failed. No implementation work can proceed until the contract is supplied.
