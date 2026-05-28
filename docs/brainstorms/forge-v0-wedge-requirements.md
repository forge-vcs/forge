# Forge v0 Wedge Requirements

## Summary

Forge v0 targets a solo developer running local AI agents against an existing Git repository. The first winning workflow is a single-agent local loop that turns messy agent work into a reviewable, checked proposal that can be accepted and exported to a Git branch.

The goal is not to replace Git in v0. The goal is to prove that Forge makes agent-produced changes safer and easier to review than manual branches, worktrees, ad hoc checkpoint commits, shell logs, and hand-written PR descriptions.

Implementation should be split into vertical milestone plans. Each plan must end with a locally testable CLI slice backed by an automated integration harness that creates temporary Git repositories, runs Forge commands, mutates files, and asserts behavior.

## Decisions

### D1. First User

The first v0 user is a solo developer using local AI agents.

This user needs:

- A CLI that works in an existing Git repository.
- A simple local workflow.
- Recoverable snapshots of agent work.
- Evidence from commands the agent ran.
- A proposal that can be checked, accepted, rejected, and exported.
- Automated tests that prove the workflow locally.

Team review, hosted collaboration, permissions, and multi-user flows are deferred.

### D2. First Killer Workflow

The first killer workflow is the single-agent local loop:

1. `forge init`
2. `forge start "intent text"`
3. Agent or human edits files.
4. `forge save`
5. `forge run -- <command>`
6. `forge propose`
7. `forge check`
8. `forge accept`
9. `forge export branch`

This workflow must work end to end before broader multi-agent comparison or hosted review work begins.

### D3. Local Testability

Forge v0 is only real when it can be tested locally through an automated integration harness.

The harness must:

- Create temporary Git repositories.
- Initialize Forge.
- Run Forge commands through the CLI.
- Modify files between commands.
- Run checks through `forge run`.
- Create proposals.
- Accept proposals.
- Export Git branches.
- Assert human output, JSON output, filesystem state, `.forge` state, and Git refs where relevant.

Manual dogfooding is useful, but it is not enough.

### D4. Plan Split

Implementation should be split into vertical milestone plans, not subsystem-only plans.

Each plan should ship a testable slice:

- Plan 1: repository initialization, SQLite metadata, operation/view skeleton, CLI JSON contract, integration harness.
- Plan 2: start/save local attempt workflow with Git-backed snapshots.
- Plan 3: `forge run` evidence capture with bounded output and sensitivity metadata.
- Plan 4: proposal creation, diff/show, and basic check policy.
- Plan 5: accept decision, stale-base protection, Git branch export, PR-body export, and doctor/GC baseline.

The exact plan boundaries may change during planning, but every plan must preserve the rule: build vertical, locally testable slices.

### D5. Evidence Behavior

Default evidence capture records command metadata plus bounded output excerpts.

Capture by default:

- Command and args.
- Working directory.
- Start/end time.
- Exit code.
- Attempt/proposal context.
- Small stdout/stderr excerpts.
- Output truncation metadata.
- Sensitivity label.
- Trust level.

Do not capture by default:

- Full environment.
- Full stdout/stderr.
- Raw long logs.
- `.env` contents.
- Credentials or secret-bearing files.
- Network payloads.
- Agent private reasoning.

Raw output capture must be opt-in or policy-required.

### D6. Accept and Export Behavior

`forge accept` records a decision. It does not directly mutate the user's current branch.

`forge export branch` publishes the accepted proposal to a Git branch for existing GitHub/PR workflows.

This split is required because decision and publication are different operations. It also keeps v0 safer for local dogfooding and leaves room for future hosted review, merge queues, branch protection, and server-side checks.

## Requirements

### R1. CLI-First Workflow

Forge v0 must expose a small default CLI surface:

- `forge init`
- `forge start`
- `forge save`
- `forge run`
- `forge propose`
- `forge check`
- `forge accept`
- `forge reject`
- `forge show`
- `forge doctor`
- `forge export branch`
- `forge export pr-body`

Advanced internal noun commands may exist later, but the default user-facing loop must stay simple.

### R2. JSON Contract

Every v0 command must support `--json`.

JSON output must include:

- Schema version.
- Command.
- Request ID when supplied.
- Operation ID when created.
- Status.
- Data.
- Warnings.
- Errors.
- Retry metadata.

No command should be implemented before its JSON schema, error codes, idempotency behavior, and prompt behavior are specified and covered by golden tests.

### R3. Storage Foundation

Forge v0 must use SQLite for metadata and operation/view state.

The metadata store must support:

- Repository records.
- Operation records.
- View records.
- Intents.
- Attempts.
- Snapshots.
- Proposals.
- Evidence metadata.
- Check results.
- Decisions.
- Publications.
- Conflict sets.
- Schema migrations.

File content snapshots should be Git-backed through a `ContentBackend` abstraction in v0.

### R4. Operation/View Model

Every mutating command must produce an operation and resulting view, or a recoverable failed operation.

The current repository state is:

`current_operation -> current_view`

This must be true from the first implementation plan so later recovery, concurrency, and hosted sync are not retrofitted.

### R5. Attempt and Snapshot Workflow

Forge must support one active local attempt for the first workflow.

`forge start` creates an intent and attempt.

`forge save` creates a snapshot of the current worktree through the Git-backed content backend.

Snapshot restore should be implemented early enough that users can trust the system, even if restore starts with conservative confirmation behavior.

### R6. Evidence Capture

`forge run -- <command>` must execute a command and record evidence metadata with bounded output excerpts.

Evidence must be linked to the active attempt and, when applicable, to a proposal revision.

Evidence must include sensitivity and trust metadata from v0.

### R7. Proposal and Check Workflow

`forge propose` creates a proposal from the active attempt or latest snapshot.

`forge check` evaluates a constrained local policy against the proposal revision and available evidence.

Check results must bind to:

- Proposal revision.
- Policy version.
- Evidence IDs.
- Command path where available.
- Environment allowlist hash where available.
- Trust level.

### R8. Accept and Export Workflow

`forge accept` records a decision for a proposal revision.

`forge export branch` creates a Git branch from the accepted proposal.

Export must not silently overwrite an existing branch.

Accept/export must detect stale base conditions and fail safely unless an explicit future rebase/merge flow handles them.

### R9. Local Integration Harness

Every milestone must add or extend integration tests using temporary Git repositories.

The integration harness must be treated as product infrastructure, not test garnish.

### R10. Doctor and Recovery

`forge doctor` must be present before v0 completion.

It must detect at least:

- Missing or invalid `.forge` metadata.
- Interrupted operations.
- Dangling temporary files.
- Missing content refs where detectable.
- Schema mismatch.

## Scope Boundaries

### In Scope for v0

- Existing Git repository support.
- CLI-first local workflow.
- SQLite metadata.
- Git-backed content snapshots.
- Operation/view state model.
- One active local attempt.
- Manual snapshots.
- Bounded evidence capture.
- Simple check policy.
- Decision records.
- Git branch export.
- PR-body export.
- Automated local integration tests.
- Basic doctor and GC dry-run.

### Deferred for Later

- Multiple competing attempts UI.
- Hosted service.
- Web UI.
- Native Forge content backend.
- Native Git protocol.
- Semantic merge engine.
- Server-side evidence attestation.
- Fine-grained permissions.
- IDE integration.
- Automatic snapshots.
- Full remote sync.
- Team review workflow.

### Outside v0 Product Identity

- General-purpose Git replacement.
- Better Jujutsu.
- GitHub clone.
- Agent orchestration platform.
- CI platform.

Forge v0 is an agent-native local change-control loop. It earns the right to become more only after that loop works.

## Success Criteria

Forge v0 wedge succeeds when:

- A solo developer can run the full workflow locally in an existing Git repo.
- The workflow is covered by automated integration tests.
- A proposal can be created from agent edits.
- Evidence shows what command ran and whether it passed.
- The proposal can be accepted without mutating the current branch.
- The accepted proposal can be exported to a Git branch.
- The generated PR body is useful enough to replace hand-written agent summaries.
- `.forge` state can be inspected and checked by `forge doctor`.
- The product feels simpler than manual worktrees, checkpoint commits, shell logs, and PR body assembly.

## Open Questions for Planning

- Should Plan 1 use a single crate first or a workspace split from the start?
- Which SQLite crate and migration strategy should v0 use?
- Should `forge start` create both intent and attempt implicitly, or should an explicit intent command exist behind the scenes?
- What is the minimum proposal diff representation for v0?
- What exact bounded output size should `forge run` capture by default?
- What is the first check policy format?
- How should PR-body export summarize evidence without leaking sensitive output?
- How much restore behavior is required before v0 can be trusted?

