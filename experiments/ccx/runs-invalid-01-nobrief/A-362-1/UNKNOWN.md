# UNKNOWN

## What I need to know
The actual TASK CONTRACT. The task instruction says "Implement exactly the task
specified in the TASK CONTRACT above", but no contract was included in the
prompt, and no contract file exists in the repository.

## Why the brief does not answer it
The instruction block contains only the generic rules (allowed_changes scoping,
UNKNOWN.md protocol, acceptance commands, no commits). It references a contract
"above" that was never provided, so there is no task description, no
allowed_changes list, and no acceptance commands to run.

## Kind
blocking — without the contract there is no task to implement, no path
allowlist to respect, and no acceptance criteria to satisfy. Any edit I made
would risk violating the (unknown) allowed_changes.

## Evidence
- Task instruction (prompt): "Implement exactly the task specified in the TASK
  CONTRACT above" — no contract text precedes it.
- Repo root listing: no TASK*, CONTRACT*, or BRIEF* file at the root
  (only CLAUDE.md, PRD.md, README.md, etc.).
- `find . -iname '*contract*' -o -iname 'task*' -o -iname 'brief*'` across the
  worktree (excluding .git/ and target/) matches only
  docs/solutions/architecture-patterns/schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md:1,
  which is a prior architecture note, not a task contract.
- Latest commit b9b3917 ("pilot: strip CLAUDE.md to mechanics (arm A)") touches
  only CLAUDE.md; git status is clean, so no uncommitted contract was staged.

## Best guess
The harness that spawned this session was supposed to prepend the contract
(likely a YAML/markdown block with task_id, brief, allowed_changes, and
acceptance commands) but the injection step failed or was skipped for this
pilot arm. No implementation work was performed.
