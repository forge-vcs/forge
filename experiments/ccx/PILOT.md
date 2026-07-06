# CCX Part B Pilot — Pinned Protocol

Status: **draft pending Jan's approval of §2 task boundaries and §3 Arm B pin**.
Once approved, contracts are authored and FROZEN before any implementation;
after that, revisions are data, not fixes (v2 B3–B4).

Parent protocol: `docs/brainstorms/2026-07-05-context-closed-tasks-v2.md` Part B.
Amendments: `docs/brainstorms/2026-07-06-context-closed-tasks-landscape-and-substrate.md`
§7.1–§7.3 (sequencing, measurement addendum, byte-stable briefs).

## §1 Scope decision (Jan, 2026-07-06)

Work set: **NER-362 (intent-aware blame/annotate) + NER-382 (attach discards
pre-attach workspace edits)**. Rationale: 362 is a coherent feature we
decompose ourselves (the H-CORE test — a pre-decomposed ticket cluster would
grade decomposition without exercising it); 382 is feature-shaped bug-fix
work found in our own dogfooding. Both are wanted regardless of pilot
outcome. Refactor-slice candidates were eliminated after verifying NER-366
and NER-381 are fully done (store lib.rs 191 lines, cli main.rs 107 lines;
content-native consciously kept whole under an allowlisted cap).

Known bias to record: NER-382's ticket (authored by the same agent that will
author contracts) already contains a proposed fix, and the author deep-explored
the relevant modules. Authoring-cost numbers for those tasks are lower
bounds; the A-vs-B defect comparison is unaffected (both arms see the same
task specs).

## §2 Task decomposition — DRAFT (needs Jan's approval)

Target: 8 tasks, each 0.5–2h agent-effort. Every ambiguity found while
finalizing boundaries gets logged (H-CORE / A6.9 / A6.11 data).

**NER-362 — intent-aware blame (5 tasks):**
- **362.1 Path provenance walk.** Given a path, walk native history
  (tip→genesis) and emit, per commit touching the path, the provenance
  tuple already stored on `CommitObject` (intent_id, proposal_revision_id,
  decision_id, evidence_digest, actor, authored_time). New domain module —
  NOT in `forge-content-native/src/lib.rs` (its allowlisted line cap may
  shrink but must not grow).
- **362.2 Line attribution engine.** For a file at HEAD, attribute each
  line to the commit that last changed it, using the existing native diff
  primitives. Pure function over the 362.1 walk.
- **362.3 CLI surface.** `forge blame <path>` (name TBD in contract) +
  `--json` envelope, snake_case, additive under `forge.cli.v0`.
- **362.4 Ledger enrichment.** Join attribution output to store records:
  intent title, decision status, check verdict — the "agent-native
  provenance, not just author/date" half of the ticket.
- **362.5 Integration tests + docs.** assert_cmd tests in
  `crates/forge-cli/tests/`, README/help text.

**NER-382 — attach drift guard (3 tasks):**
- **382.1 Payload/docs honesty.** Qualify `workspace_path` in
  `start`/`attempt start` JSON + help text as a materialization target.
- **382.2 Drift guard.** At attach, diff workspace dir against recorded
  `materialized_content_ref`; refuse with new typed `WORKSPACE_DRIFT` error
  unless `--discard-workspace-changes`. Error registry/schema updated.
- **382.3 Tests.** Integration tests for drift-refusal, override, and the
  original silent-loss repro now failing loudly.

## §3 Arms (pinned)

Common to all arms: same tasks, order randomized per arm; clean branch per
task run (`experiment/ccx-pilot` base); same verify commands; same reviewer
procedure; implementation sessions are NOT this contract-authoring session.

- **Arm A (brief-only):** fresh agent session per task. Input = `brief.sh`
  output for the task's contract ONLY (global policy + contract + neighbor
  contracts). CLAUDE.md on the pilot branch stripped to mechanics (approved
  by Jan 2026-07-06, branch-only). Unknown rule: stop-on-unknown, single
  class; the stop is recorded with which class (blocking/assumption/
  observation) it would have been. No repo search beyond the task's
  allowed_changes paths; out-of-radius reads go through the audited-
  declassification stand-in: ask, don't read.
- **Arm B (status quo), pinned per Jan's request:** ONE continuous session
  working through all tasks of a ticket sequentially. Inputs allowed: full
  (unstripped) CLAUDE.md, the Linear ticket text, docs/ (handoffs,
  solutions), unrestricted repo search, its own accumulated session
  context. Same branch-reset between tasks, same verify commands. No
  contract briefs. Fixed now; no day-to-day drift.
- **Arm C (ablation):** as Arm A but briefs emitted WITHOUT
  negative_constraints (isolates dead-end-knowledge value). Run after A and
  B if budget allows; drop first if time-constrained (v2 B1 lists it, §A9.3
  spirit: N is precious).

## §4 Measurement

Per v2 §B2 + landscape doc §7.2: defect taxonomy at first review; headline
= unlicensed decisions per accepted patch (scored identically in A and B);
unknowns surfaced vs. guessed; acceptance first-pass; tokens injected
(cached vs. fresh separated) and $ cost; wall-clock; blast-radius
violations via `blast-check.py`; contract revisions per task.

Review hygiene: diffs anonymized before scoring; reviewer sees no session
logs until defect scoring is done; all arms of a task scored in one
sitting. **Scoring rubric is written and frozen together with the
contracts, before any arm runs.** Inter-rater check: ≥2 tasks scored
independently by Jan + one fresh agent session; agreement reported.

## §5 Order of work

1. Jan approves §2 boundaries + §3 Arm B pin (or amends).
2. Contracts authored for all 8 tasks + scoring rubric; both FROZEN in one
   commit on `experiment/ccx-spikes` (or successor branch).
3. Pilot branch `experiment/ccx-pilot` cut; CLAUDE.md stripped there.
4. Arms run (A and B first; C if budget allows); every run logged under
   `experiments/ccx/runs/`.
5. Review + scoring; `experiments/ccx/RESULTS.md` written against the
   pre-registered readings (v2 B5). Decision rule applied as written.
