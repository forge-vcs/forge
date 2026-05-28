# Forge PRD — Agent-Native Source Control System

## 1. Product Summary

Forge is a Rust-native source control system designed for human developers and AI agents working together. It replaces branch-first and commit-first workflows with intent-driven workspaces, continuous checkpoints, native storage, evidence capture, and controlled promotion.

Forge is CLI-first. It is installed locally as a single developer tool and stores project metadata and native source-control state inside a .forge folder.

Forge should support Git interoperability, but Git must be treated as an adapter, not the internal model. The core system should be Rust-native and designed around agent-era workflows from first principles.

## 2. Core Thesis

Traditional Git workflows were designed around human-paced collaboration, deliberate commits, manual branches, and review through diffs. AI agents change this model. Agents create many exploratory changes, run repeated tests, inspect and modify files rapidly, and often require rollback, comparison, and evidence tracking.

Forge exists to make source control agent-native while remaining usable by humans.

The central primitive is not the commit.

The central primitive is the verified change lifecycle:

Intent → Session → Checkpoints → Patchset → Evidence → Verification → Promotion

## 3. Goals

Forge must provide:

- A Rust-native source-control backend.
- CLI-first local workflow.
- .forge as the hidden project control folder.
- Intent-based change tracking.
- Isolated agent and human work sessions.
- Continuous checkpoints instead of manual micro-commits.
- Patchsets as curated change proposals.
- Evidence bundles for commands, tests, changed files, and agent provenance.
- Promotion flow for accepting verified patchsets into durable history.
- Git import/export compatibility.
- Clean architecture where Git does not define the internal model.
- Agent-friendly command surface.
- Human-readable project state and audit trails.
- Future path toward remote collaboration, review UI, and hosted Forge repositories.

## 4. Non-Goals for Initial Version

Forge v0 should not attempt to become a full GitHub replacement.

Initial version should not require:

- Web frontend.
- Hosted service.
- Multi-user remote Forge server.
- Cloud account.
- Enterprise permission model.
- Semantic merge engine.
- Full monorepo permission system.
- Native IDE plugin.
- Replacement for GitHub PR review.
- Full custom distributed synchronization protocol.
- Advanced filesystem virtualization.

These can be future capabilities.

## 5. Target Users

### 5.1 AI Coding Agents

Agents should use Forge to:

- Start work from a clear intent.
- Operate in isolated sessions.
- Create automatic or explicit checkpoints.
- Run commands through Forge when evidence capture is needed.
- Produce patchsets.
- Export evidence for human review.
- Avoid manual Git branch and worktree handling.

### 5.2 Human Developers

Developers should use Forge to:

- Manage agent-generated changes.
- Compare competing agent sessions.
- Review evidence before accepting code.
- Promote clean patchsets.
- Export accepted work to Git-compatible systems.
- Maintain understandable project history.

### 5.3 Technical Leads and Reviewers

Reviewers should use Forge to:

- Understand why a change exists.
- See what commands and tests ran.
- Understand which files were inspected or changed.
- Compare agent attempts.
- Approve or reject patchsets based on evidence, not only diffs.

## 6. Product Principles

### 6.1 Agent-Native First

Forge must assume agents are first-class contributors, not human developers with a different username.

### 6.2 Git-Compatible, Not Git-Shaped

Forge should import from and export to Git, but internal concepts must not be limited to Git concepts.

### 6.3 CLI Before UI

The first product surface is a CLI. Agents already work well through command-line tools. A web UI can come later.

### 6.4 Evidence Over Trust

Agent-generated code should not be trusted because it compiles. Forge must capture evidence around how the change was produced and verified.

### 6.5 Clean Promotion Over Raw History

Forge should allow noisy exploration internally, but promote only clean, curated patchsets into durable history.

### 6.6 Local-First

Forge should work fully offline in a local repository. Remote collaboration should be additive, not required.

## 7. High-Level Architecture

Forge consists of seven major components.

### 7.1 Forge CLI

The CLI is the primary interface for humans and agents.

Responsibilities:

- Initialize Forge in a project.
- Create and manage intents.
- Start and inspect sessions.
- Create checkpoints.
- Run commands with evidence capture.
- Show status.
- Create patchsets.
- Verify patchsets.
- Promote patchsets.
- Export to Git-compatible systems.
- Inspect evidence.
- Clean up stale sessions and checkpoints.

The CLI should be fast, predictable, scriptable, and suitable for agent invocation.

### 7.2 Forge Native Storage Backend

Forge should have its own Rust-native storage backend.

Responsibilities:

- Store content snapshots.
- Store file tree state.
- Store checkpoint graph.
- Store session state.
- Store patchsets.
- Store evidence metadata.
- Store promotion records.
- Store references to imported Git states.
- Support integrity checks.
- Support garbage collection.
- Support local-first operation.

The native backend should eventually be capable of operating without Git. Git should be one import/export format, not the permanent source of truth.

### 7.3 .forge Project Folder

Every Forge-enabled project contains a .forge folder.

The .forge folder stores:

- Forge configuration.
- Native storage data.
- Intent records.
- Session records.
- Checkpoint metadata.
- Evidence bundles.
- Patchset metadata.
- Promotion records.
- Git adapter metadata.
- Logs and diagnostic state.

Requirements:

- .forge must be hidden by default.
- .forge must be portable enough for backup.
- .forge should be inspectable for debugging.
- .forge should avoid storing secrets by default.
- .forge must support future migration/versioning.
- Forge must detect and validate .forge schema versions.

### 7.4 Intent Engine

An intent describes the purpose and boundaries of work.

An intent should capture:

- Human-readable title.
- Problem statement.
- Desired outcome.
- Optional scope.
- Optional constraints.
- Optional success criteria.
- Associated sessions.
- Associated patchsets.
- Current lifecycle state.

Intent lifecycle states may include:

- Draft.
- Active.
- Paused.
- Completed.
- Abandoned.
- Promoted.

The intent is the anchor for all agent work.

### 7.5 Session Engine

A session is an isolated work attempt against an intent.

A session may represent:

- One AI agent attempt.
- One human attempt.
- One CI-generated repair attempt.
- One experimental branch of work.

A session should capture:

- Intent reference.
- Base revision.
- Agent or actor identity.
- Start time.
- Current state.
- Working tree relationship.
- Checkpoints.
- Commands executed through Forge.
- Changed files.
- Evidence links.
- Completion state.

Session lifecycle states may include:

- Created.
- Active.
- Suspended.
- Finished.
- Failed.
- Abandoned.
- Converted to patchset.

Initial session isolation may use existing filesystem and Git mechanisms internally, but the Forge abstraction must hide that implementation detail.

### 7.6 Checkpoint Engine

A checkpoint captures a meaningful state during a session.

Checkpoints replace manual micro-commits for exploratory work.

A checkpoint should capture:

- Session reference.
- Parent checkpoint.
- Snapshot reference.
- Timestamp.
- Actor.
- Summary.
- Changed files.
- Optional command context.
- Optional test state.
- Optional risk annotation.

Forge should support both explicit checkpoints and future automatic checkpoints.

### 7.7 Patchset Engine

A patchset is a curated candidate change derived from one or more sessions and checkpoints.

A patchset should capture:

- Intent reference.
- Source session references.
- Source checkpoint references.
- Final file changes.
- Summary.
- Changed-file list.
- Evidence bundle reference.
- Verification status.
- Promotion status.

Patchsets are the main reviewable unit.

A patchset is not necessarily equivalent to a branch. It is a verified change proposal.

### 7.8 Evidence Engine

The evidence engine records what happened during work.

Evidence may include:

- Commands run through Forge.
- Command output.
- Exit codes.
- Test results.
- Changed files.
- Inspected files if detectable.
- Generated summaries.
- Actor identity.
- Agent identity.
- Model identity where available.
- Tool/runtime metadata.
- Policy checks.
- Verification results.
- Risk notes.

Evidence should be exportable as Markdown for PR descriptions, review notes, and external analysis.

### 7.9 Verification Engine

The verification engine determines whether a patchset is ready for promotion.

Verification may include:

- Required commands.
- Test execution.
- Linting.
- Formatting checks.
- Static analysis.
- Security scan hooks.
- Changed-file policy checks.
- Required evidence presence.
- Optional human approval marker.

Initial verification can be simple and local. Future verification can become policy-driven.

### 7.10 Promotion Engine

Promotion accepts a verified patchset into durable history.

Promotion should capture:

- Patchset reference.
- Target line of development.
- Verification result.
- Actor approving promotion.
- Timestamp.
- Final durable revision.
- Optional Git export metadata.

Promotion is the equivalent of accepting a change, but it should be richer than a merge.

### 7.11 Git Adapter

The Git adapter provides compatibility.

Responsibilities:

- Initialize Forge from an existing Git repository.
- Import Git history or selected revisions.
- Map Git HEAD or branch state to Forge base revisions.
- Export promoted patchsets to Git commits.
- Export patchsets to Git branches.
- Support GitHub PR workflows through external tooling where possible.
- Preserve escape hatch to Git.

The Git adapter must not define Forge’s core domain model.

## 8. Data Model Requirements

Forge should model these core entities:

- Repository.
- Intent.
- Session.
- Actor.
- Agent.
- Checkpoint.
- Snapshot.
- File blob.
- Tree state.
- Patchset.
- Evidence bundle.
- Verification result.
- Promotion record.
- Git import/export record.

Every entity should have:

- Stable identifier.
- Creation timestamp.
- Modification timestamp where relevant.
- Schema version.
- Integrity metadata where relevant.

Native storage should support:

- Content addressing.
- Snapshot deduplication.
- Integrity checks.
- Safe writes.
- Crash recovery.
- Local garbage collection.
- Future remote synchronization.

## 9. CLI Requirements

Forge CLI should support these high-level capability groups.

### 9.1 Repository Commands

- Initialize Forge in a project.
- Show Forge repository status.
- Validate .forge health.
- Upgrade .forge schema.
- Run maintenance and cleanup.

### 9.2 Intent Commands

- Create an intent.
- List intents.
- Show intent details.
- Update intent metadata.
- Close or abandon intent.

### 9.3 Session Commands

- Start a session.
- List sessions.
- Show session details.
- Switch or attach to a session.
- Finish a session.
- Abandon a session.
- Compare sessions.

### 9.4 Checkpoint Commands

- Create checkpoint.
- List checkpoints.
- Show checkpoint details.
- Restore to checkpoint.
- Compare checkpoints.

### 9.5 Evidence Commands

- Run command with evidence capture.
- Show evidence.
- Export evidence.
- Attach external evidence.
- Summarize evidence.

### 9.6 Patchset Commands

- Create patchset.
- Show patchset.
- Compare patchsets.
- Verify patchset.
- Promote patchset.
- Export patchset.

### 9.7 Git Compatibility Commands

- Import from Git.
- Export to Git branch.
- Export to Git commit.
- Export PR-ready evidence.
- Show Git mapping status.

## 10. Storage Backend Requirements

Forge native storage should be designed as a long-term replacement-capable backend.

Requirements:

- Written in Rust.
- Local-first.
- Safe against partial writes.
- Content-addressed where appropriate.
- Able to store snapshots independently of Git.
- Able to store metadata separately from content.
- Able to validate object integrity.
- Able to compact unused objects.
- Able to support future remote synchronization.
- Able to support large repositories incrementally.
- Able to support future lazy materialization.
- Able to support future permissioned views.

Initial implementation can be simple, but the abstraction must not assume Git is the permanent backend.

## 11. Agent Workflow Requirements

Forge should support the following agent workflow:

1. A human or orchestrator creates an intent.
2. One or more agents start sessions from that intent.
3. Agents modify files using the normal filesystem.
4. Agents use Forge to create checkpoints and run commands.
5. Forge captures evidence.
6. Agents create patchsets from session state.
7. Forge verifies patchsets.
8. A human or policy promotes a patchset.
9. Forge exports to Git when needed.

Agent requirements:

- Commands must be deterministic and scriptable.
- Output should be structured enough for agents to parse.
- Human-readable output should remain clean.
- Machine-readable output should be supported.
- Agent identity should be captured where available.
- Forge should avoid requiring interactive prompts in agent mode.

## 12. Human Review Requirements

Forge should help humans answer:

- What was the intent?
- Who or what made the change?
- What files changed?
- What files were involved?
- What commands ran?
- What tests passed or failed?
- What evidence supports the change?
- What risks remain?
- Which session produced this patchset?
- Was this patchset promoted?
- Can it be exported to Git?
- Can it be rolled back?

Review should be evidence-first, not diff-only.

## 13. Security Requirements

Forge must be designed with security boundaries in mind.

Initial security requirements:

- Do not store secrets in .forge by default.
- Redact sensitive command output where feasible in future versions.
- Clearly mark evidence that may contain sensitive data.
- Support .forge ignore/exclude rules.
- Avoid executing arbitrary commands without explicit user or agent action.
- Store actor and agent provenance.
- Preserve audit records for promotion.
- Make destructive operations explicit.

Future security requirements:

- Path-level session permissions.
- Restricted agent views.
- Signed checkpoints.
- Signed promotion records.
- Tamper-evident evidence logs.
- Remote identity integration.
- Policy-based command restrictions.

## 14. Performance Requirements

Forge should be fast enough to feel like a normal developer tool.

Performance requirements:

- CLI startup should be fast.
- Status operations should be fast on normal repositories.
- Checkpoint creation should avoid unnecessary full copies.
- Storage should deduplicate unchanged content.
- Evidence capture should not make command execution painfully slow.
- Large file handling should be deliberate and safe.
- Repository maintenance should be explicit and observable.

## 15. Reliability Requirements

Forge must not corrupt user work.

Requirements:

- Safe writes to .forge.
- Recoverable metadata operations.
- Clear failure messages.
- No destructive filesystem changes without explicit command.
- Ability to validate .forge health.
- Ability to recover from interrupted commands.
- Ability to export user work even if Forge metadata is partially damaged.
- Compatibility fallback to Git where available.

## 16. Interoperability Requirements

Forge should interoperate with:

- Git.
- GitHub workflows.
- Existing local development tools.
- Existing filesystems.
- Existing build tools.
- Existing test runners.
- AI coding agents.
- CI systems.

Initial GitHub support can be export-oriented. Forge does not need to replace GitHub review in v0.

## 17. Suggested MVP Scope

### MVP Objective

Build a single Rust CLI that works inside an existing Git repository, creates a .forge folder, tracks intents, sessions, checkpoints, evidence, patchsets, and exports patchsets back to Git-compatible workflows.

### MVP Must-Haves

- .forge initialization.
- Local metadata storage.
- Intent creation and listing.
- Session creation and listing.
- Manual checkpoints.
- Command execution with evidence capture.
- Patchset creation from current changes.
- Evidence export.
- Git branch export.
- Basic verification command.
- Human-readable CLI output.
- Machine-readable output mode.

### MVP Should-Haves

- Basic native snapshot storage.
- Checkpoint restore.
- Session comparison.
- Patchset summary generation.
- GitHub PR body export.
- Repository health validation.

### MVP Could-Haves

- Automatic checkpoints.
- Agent identity detection.
- Test result parsing.
- Basic risk summary.
- Simple policy configuration.
- Cleanup and garbage collection.

## 18. Future Roadmap

### Phase 1 — CLI Workflow Layer

Forge becomes useful as a local agent workflow tool on top of Git.

### Phase 2 — Native Storage Maturity

Forge native storage becomes capable of storing project state independently of Git.

### Phase 3 — Better Session Isolation

Forge manages workspaces without exposing branch or worktree complexity.

### Phase 4 — Evidence and Policy System

Forge gains configurable verification gates, policy checks, and richer evidence models.

### Phase 5 — Remote Forge

Forge adds remote synchronization, shared repositories, and multi-user collaboration.

### Phase 6 — Review UI

Forge introduces a native review experience centered around intents, patchsets, and evidence.

### Phase 7 — Permissioned Code Views

Forge supports path-level and context-level visibility for agents, contractors, and external contributors.

## 19. Key Design Questions for Review

GPT-5.5 Pro should analyze:

- Is the core domain model correct?
- Are intents, sessions, checkpoints, patchsets, evidence, and promotion the right primitives?
- Should Forge build native storage immediately or stage it behind Git compatibility first?
- What is the minimal viable native storage design?
- What is the safest .forge folder layout?
- How should Forge prevent corruption?
- What should be content-addressed versus metadata-indexed?
- Should SQLite be used for metadata, or should Forge use an embedded Rust-native storage approach from the start?
- How should Forge handle large repositories?
- How should Forge handle binary files?
- How should checkpoint restore work safely?
- How should Git import/export be modeled without leaking Git concepts into the core?
- What should agent-friendly CLI output look like?
- What should be excluded from v0?
- What are the most dangerous architectural traps?
- What would make this product meaningfully better than Git plus scripts?
- What would make this product meaningfully better than Jujutsu for agent workflows?
- What is the strongest wedge for adoption?

## 20. Success Criteria

Forge v0 is successful if:

- A coding agent can use it through CLI without special integration.
- A human can understand what the agent did.
- Work can be checkpointed without manual Git commits.
- Evidence can be exported for review.
- A patchset can be promoted or rejected.
- The final work can be exported to Git.
- The .forge folder contains enough structured state to reconstruct the work lifecycle.
- The system feels simpler than manual branches, worktrees, commits, and PR descriptions.
- The architecture does not trap Forge inside Git’s mental model.

## 21. Final Product Definition

Forge is an agent-native, Rust-based source-control system.

It uses .forge as its project control folder.

It has its own native storage backend.

It starts as a CLI-first local tool.

It treats Git as an adapter, not the core model.

Its primary workflow is:

Intent → Session → Checkpoint → Patchset → Evidence → Verification → Promotion

Forge is not a Git clone.

Forge is a control system for code changes created by humans and agents.