---
date: 2026-05-28
topic: competing-local-attempts
---

# Competing Local Attempts Requirements

## Summary

Forge should let a solo developer or local agent workflow create more than one attempt for the same intent, review the resulting proposals side by side, and safely choose which proposal to accept and export. The first slice should prove proposal-level competition in one checkout, not parallel physical worktrees.

## Problem Frame

Forge now proves the single-attempt local loop and native snapshot storage, but the product thesis depends on agents making multiple tries that can compete safely. If every new attempt overwrites the previous active attempt mentally or operationally, users are still managing branches, scratch commits, or ad hoc folders outside Forge.

The next wedge is not full workspace orchestration. It is the ability to keep multiple attempt histories alive, bind evidence and proposals to the correct attempt, compare their review state, and make accept/export decisions without accidentally acting on the wrong proposal.

---

## Key Decisions

- **Attempt is a domain object, not a copy of the codebase.** An attempt records one try against an intent: base revision, snapshots, evidence, proposals, checks, decisions, and publication. A workspace is where files are materialized.
- **Start with proposal-level competition.** Multiple attempts can exist and produce independent proposals, but the first slice uses the current checkout rather than creating separate physical worktrees.
- **Use hybrid context selection.** Humans can attach an attempt for ergonomic commands. Agents should pass explicit IDs with `--attempt <id>` and `--proposal <id>` to avoid hidden state.
- **Attach materializes files.** `forge attempt attach <id>` changes the current attempt and materializes that attempt's latest snapshot into the current checkout. If the attempt has no snapshot, attach materializes the attempt base.
- **Require explicit proposal selection when ambiguous.** Proposal-sensitive commands must not silently act on the global latest proposal once multiple proposals exist.
- **Compare metadata first.** The first comparison surface should show attempt/proposal status, changed paths, evidence, check, decision, and publication state. File-diff comparison, conflict prediction, and semantic merge are later slices.

---

## Actors

- A1. **Human developer.** Starts attempts, attaches an attempt in the current checkout, reviews candidate proposals, and chooses which one to accept.
- A2. **Local coding agent.** Runs non-interactive commands and should prefer explicit `--attempt` and `--proposal` arguments.
- A3. **Forge CLI.** Resolves context, materializes snapshots safely, records lifecycle state, and returns stable JSON envelopes.

---

## Key Flows

- F1. Competing attempts under one intent
  - **Trigger:** A user wants a second try for the same intent.
  - **Actors:** A1, A2, A3
  - **Steps:** The user starts or creates another attempt under an existing intent. Each attempt can save snapshots, record evidence, create proposals, and receive check/decision state independently.
  - **Outcome:** Attempts are grouped by intent but remain independently reviewable.

- F2. Human attaches an attempt
  - **Trigger:** A human wants to continue or inspect a different attempt in the current checkout.
  - **Actors:** A1, A3
  - **Steps:** The human runs `forge attempt attach <attempt-id>`. Forge refuses if the checkout has unsaved changes. Otherwise Forge materializes the attempt's latest snapshot, or its base revision if no snapshot exists, and records the attached attempt.
  - **Outcome:** The checkout content and attached attempt context agree.

- F3. Agent acts with explicit context
  - **Trigger:** An agent saves, runs evidence, proposes, checks, accepts, rejects, or exports in a multi-attempt repository.
  - **Actors:** A2, A3
  - **Steps:** The agent supplies `--attempt <id>` and, for proposal-sensitive commands, `--proposal <id>` when needed. Forge echoes the resolved IDs in JSON.
  - **Outcome:** Agent commands are deterministic and do not depend on hidden attached state.

- F4. Human compares candidates
  - **Trigger:** Multiple attempts or proposals exist for the same intent.
  - **Actors:** A1, A3
  - **Steps:** The human lists attempts/proposals and inspects metadata: intent, changed paths, latest snapshot, evidence summary, check status, decision, and publication.
  - **Outcome:** The human can choose which proposal to accept without reading raw database state or managing branches manually.

---

## Requirements

**Attempt lifecycle**

- R1. Forge must allow multiple attempts to exist for a repository and, when requested, for the same intent.
- R2. Forge must preserve existing single-attempt workflows when only one active or relevant attempt exists.
- R3. `forge start` or an adjacent attempt-starting command must be able to create a new attempt under an existing intent without forcing a new intent.
- R4. Attempts must remain independent lifecycle records: snapshots, evidence, proposals, checks, decisions, and publications must bind to the correct attempt.

**Context selection**

- R5. Forge must support an attached attempt for human-oriented workflows.
- R6. Commands that operate on attempt context must accept `--attempt <id>` where practical, and explicit `--attempt` must be preferred for agent workflows.
- R7. JSON success responses for attempt-scoped commands must echo the resolved `attempt_id`.
- R8. If the attached attempt is missing, ambiguous, abandoned, or unsafe to use, Forge must return a typed JSON error rather than guessing.

**Attach and workspace materialization**

- R9. `forge attempt attach <attempt-id>` must refuse to run when the current checkout has unsaved changes according to the existing dirty-worktree safety rules.
- R10. Attaching an attempt with snapshots must materialize that attempt's latest snapshot into the current checkout.
- R11. Attaching an attempt with no snapshots must materialize the attempt's base revision into the current checkout.
- R12. Attach must preserve protected paths such as `.forge`, `.env`, `.env.*`, private keys, credential files, and existing secret-risk paths.

**Proposal selection**

- R13. Proposal-sensitive commands must resolve proposal context deliberately once more than one candidate proposal exists.
- R14. `forge check` may default to the latest proposal only when exactly one candidate exists for the resolved attempt.
- R15. `forge accept`, `forge reject`, and `forge export branch` must require `--proposal <id>` or an equivalent explicit proposal selector when multiple proposals exist.
- R16. Ambiguous proposal selection must return a typed JSON error such as `AMBIGUOUS_PROPOSAL` and include candidate proposal IDs.
- R17. JSON success responses for proposal-scoped commands must echo `attempt_id`, `proposal_id`, and `proposal_revision_id`.

**Comparison and review surface**

- R18. Forge must provide a metadata-first listing or show surface for attempts and proposals.
- R19. The comparison surface must include enough information to choose between candidates: intent, attempt ID, proposal ID, changed paths, evidence status, check status, decision status, and publication status.
- R20. The first slice must not require file-level diff comparison, conflict prediction, semantic merge, or parallel workspace management.

---

## Acceptance Examples

- AE1. **Covers R1, R3, R4.** Given an initialized repo and an existing intent, when a user starts two attempts for that intent and each saves/proposes independently, then Forge records two attempts and two proposals without overwriting either attempt's lifecycle state.
- AE2. **Covers R5, R9, R10.** Given attempt A is attached and the checkout has unsaved changes, when the user attaches attempt B, then Forge refuses with a dirty-worktree error and leaves files unchanged.
- AE3. **Covers R10, R12.** Given attempt B has a latest snapshot and the checkout is clean, when the user attaches attempt B, then Forge materializes B's snapshot while preserving protected paths.
- AE4. **Covers R11.** Given attempt C has no snapshots, when the user attaches attempt C from a clean checkout, then Forge materializes C's base revision rather than leaving unrelated attempt content in place.
- AE5. **Covers R13, R15, R16.** Given two proposals exist for the resolved attempt, when the user runs `forge accept` without choosing one, then Forge returns `AMBIGUOUS_PROPOSAL` with candidate proposal IDs.
- AE6. **Covers R6, R7, R17.** Given an agent passes explicit attempt and proposal IDs, when the command succeeds, then JSON output echoes the resolved IDs so the agent can chain commands safely.
- AE7. **Covers R18, R19.** Given multiple attempts and proposals exist, when the user lists or shows competing work, then Forge displays metadata sufficient to compare review readiness without requiring Git branches or raw SQLite inspection.

---

## Success Criteria

- A temp-repo integration test can create two attempts for one intent, save different snapshots, create proposals for both, and accept/export the chosen proposal.
- Existing single-attempt dogfood loop still works without requiring new flags.
- Ambiguous multi-attempt or multi-proposal commands fail with typed JSON errors rather than silently using global latest state.
- Agents can drive the competing-attempt flow using explicit IDs only.
- Human attach behavior never leaves the checkout content inconsistent with the attached attempt.

---

## Scope Boundaries

### In Scope

- Multiple attempt records under one repository and one intent.
- Hybrid context selection with attached attempt plus explicit `--attempt`.
- Attach materialization in the current checkout.
- Explicit proposal selection for ambiguous proposal-sensitive commands.
- Metadata-first attempt/proposal comparison.
- JSON envelope stability and typed ambiguity errors.

### Deferred for Later

- Parallel physical worktrees or per-attempt workspace directories.
- Semantic merge, conflict materialization, and conflict prediction.
- File-diff comparison between attempts or proposals.
- Hosted review, team permissions, and multi-user coordination.
- Automatic agent spawning or orchestration across attempts.
- Full intent-management UI beyond what is needed to group competing attempts.

---

## Dependencies / Assumptions

- Existing snapshot restore behavior is safe enough to reuse for attach materialization.
- Native and Git-backed content refs remain interchangeable through the content backend boundary.
- The current SQLite lifecycle model can represent multiple attempts without replacing the operation/view spine.
- The first user remains a solo developer running local agents in one checkout.

---

## Sources / Research

- Product source: `PRD.md`, especially Attempt Isolation, Proposal Semantics, Conflict and Merge Model, and v0 success criteria.
- Current v0 requirements: `docs/brainstorms/forge-v0-wedge-requirements.md`, which deliberately limited the first workflow to one active local attempt.
- Prior implementation plans: `docs/plans/completed/2026-05-28-001-feat-forge-v0-local-loop-plan.md`, `docs/plans/completed/2026-05-28-002-hardening-forge-v0-local-loop-plan.md`, and `docs/plans/completed/2026-05-28-003-feat-native-forge-content-store-plan.md`.
