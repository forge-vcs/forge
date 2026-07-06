# CCX pilot scoring rubric — FROZEN with the contracts (2026-07-06)

Applied at first review of every arm's patch, before the reviewer sees any
session log. All of a task's arms are scored in one sitting. ≥2 tasks are
scored independently by Jan + one fresh agent session; report agreement.

## 1. Defect classification (v2 §B2.1)

Score each distinct defect found in the patch, one class per defect:

- **implementation defect** — the brief/contract (Arm A/C) or ticket+repo
  docs (Arm B) contained the deciding information and the patch violates
  it. Cite the violated line.
- **contract defect** — the contract lacked information the task needed;
  the agent could not have decided correctly. (H-CORE datum, not an agent
  failure.) Cite what was missing.
- **verifier defect** — acceptance commands pass but an invariant is
  violated (Goodhart). Cite invariant + passing command.
- **workflow defect** — the procedure allowed an invalid state (e.g. task
  ran against a stale base).
- **model-behavior defect** — the agent guessed where the unknown rule
  required a stop (Arm A/C), or ignored explicit ticket text (Arm B).

## 2. Unlicensed decisions (headline)

For every discrete decision visible in the patch (new name, new dependency,
changed behavior, added file, error text, algorithm choice), ask: **"which
brief line (Arm A/C) / which ticket-or-doc line (Arm B) authorized this?"**
- Authorized → not counted.
- Reasonable-but-unauthorized → 1 unlicensed decision.
- Unauthorized AND wrong → 1 unlicensed decision + its defect above.
Count identically in every arm. Report per accepted patch.

Calibration examples (from the T2 spike codebase):
- Adding `serde` rename to a new field the contract specifies → authorized.
- Renaming an existing field "for consistency" → unlicensed.
- Choosing Myers vs. line-hash matching where the contract says "algorithm
  free" → authorized (explicitly licensed).

## 3. Unknown scoring (Arm A/C)

- **surfaced** — agent stopped and stated the unknown (successful outcome).
- **guessed-visible** — agent noted uncertainty but proceeded.
- **silent guess** — discovered only by the reviewer via a defect.
Record the would-have-been class (blocking/assumption/observation).

## 4. Mechanical metrics (no judgment)

Per run: acceptance first-pass (all contract acceptance commands green on
first execution) · attempts-to-green · tokens in/out (cached vs fresh
separated, from the runner's usage capture) · $ cost · wall-clock ·
blast-radius violations (`blast-check.py` over the run's diff vs. the
task's allowed_changes) · files touched outside allowed_changes (count).

## 5. Per-task verdict sheet

task / arm / defects by class / unlicensed count / unknowns
(surfaced|guessed|silent) / first-pass? / blast violations / tokens / $ /
minutes / reviewer initials. One row per (task, arm) in
experiments/ccx/RESULTS.md.
