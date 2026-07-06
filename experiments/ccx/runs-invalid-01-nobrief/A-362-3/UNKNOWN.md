# UNKNOWN

## What I need to know
The TASK CONTRACT itself. The instruction says to "implement exactly the task
specified in the TASK CONTRACT above," including its `allowed_changes` paths
and acceptance commands, but no contract text was included in the prompt I
received.

## Why the brief does not answer it
The only instruction delivered was the generic "TASK INSTRUCTION" harness text
(rules about allowed_changes, UNKNOWN.md, acceptance commands, no commits).
There is no contract content above it, and no contract file exists in the
repository:

- Searched the repo for `TASK CONTRACT` / `allowed_changes` across *.md,
  *.yaml, *.yml, *.json, *.toml — no matches outside target/.
- Repo root listing shows no TASK*, CONTRACT*, or brief file
  (only CLAUDE.md, PRD.md, README.md, RELEASE_NOTES.md, etc.).
- `.agents/` contains only `plugins/skills -> ../.claude/skills` and
  `marketplace.json` — no contract.
- `AGENTS.md:1` contains only `@CLAUDE.md`.
- Latest commit b9b3917 ("pilot: strip CLAUDE.md to mechanics (arm A)")
  touched only CLAUDE.md; the worktree is clean, so no uncommitted contract
  was left behind either.

## Kind
blocking — without the contract I cannot know what to implement, which paths
are allowed to change, or which acceptance commands must pass. Any work would
be a guess with a high risk of touching disallowed paths.

## Best guess
The harness intended to prepend a task-specific contract (this looks like a
pilot run, arm A) but the substitution failed or the contract was never
attached. Re-run with the contract text included, or drop it into the repo
(e.g. TASK_CONTRACT.md) and re-invoke.
