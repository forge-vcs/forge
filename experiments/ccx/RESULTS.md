# CCX Part B Pilot — RESULTS

Date: 2026-07-06 · Branch: `experiment/ccx-spikes` · Base: 6238c53
Protocol: PILOT.md (+ amendments below) · Rubric: RUBRIC.md (frozen pre-run)
Scope: NER-362 (5 tasks) + NER-382 (3 tasks) · Arms: A (brief-only fresh
sessions) vs B (continuous full-context session per ticket) · Arm C
(negative-constraint ablation) **dropped** for time/budget, as PILOT §3
allowed — recorded, not hidden.

## Verdict against the pre-registered readings (v2 §B5)

**STRONG reading met on every pre-registered criterion**, subject to the
validity caveats below:

| Criterion (pre-registered) | Result |
|---|---|
| Arm A ≤ B on defects | A **5** vs B **31** (blinded scoring) |
| Fewer unlicensed decisions per patch | A **3** vs B **40** |
| ≥80% first-attempt success (clean surface+resolve = success) | **8/8** (2 clean first-run impls, 5 correct unknown-stops later resolved, 1 surface→contract-fix→clean rerun) |
| Flat brief size vs growing continuity context | briefs 5.5–11KB flat; B session context grew monotonically per ticket |
| ≤1 contract revision/task | **1 revision across 8 tasks** (median 0) |
| Unknowns surfaced ≥ guessed | A: **6 surfaced, 0 guessed**; B: 0 surfaced (nothing invites stopping), deviations found at review |

**H-CORE:** 8/8 tasks (100%, threshold ≥70%) decomposed to C2+ and completed
by fresh brief-only sessions; ≤1 revision/task median met.

**Decision rule outcome:** thresholds met → per B0, proceed to a **thin,
Git-compatible harness** (files + scripts, no Forge objects yet) and
schedule **pilot 2 (neighbor-contract ablation)** before any substrate
work. Pending: Jan's inter-rater pass (packets `scoring/362-3/`,
`scoring/382-2/`) — if agreement is low, the headline numbers get
re-examined before any build starts.

## Headline table (blinded scoring, all 8 tasks)

| | Arm A | Arm B |
|---|---|---|
| Defects total | **5** | **31** |
| — implementation | 0 | 21 |
| — verifier (Goodhart) | 0 | 4 |
| — workflow | 0 | 3 |
| — model-behavior | 0 | 1 |
| — contract (authoring faults) | 5 | 2 |
| Unlicensed decisions | **3** | **40** |
| Acceptance gates (independent re-run) | 11/11 PASS | 10/11 (1 real FAIL) |
| Tasks delivered as specified | 8/8 | 7/8 (one silent task substitution) |
| Headless run cost | $47.52 | $75.56 |
| Wall-clock (sum) | ~73 min | ~71 min |

**The structural finding:** every Arm A defect is class `contract` — flaws
in the authored contracts, zero in the implementations. The bottleneck
moved from the implementer to the decomposer/author, which is exactly
where the thesis said hallucination risk concentrates (A-CORE).

## Per-task verdict sheet (RUBRIC §5)

| task | arm | defects (class) | unlicensed | gates | notes |
|---|---|---|---|---|---|
| 362-1 | A | 1 (contract) | 1 | PASS | cap-contradiction forced comment compression |
| 362-1 | B | 6 (4 impl, 1 verif, 1 contract) | 8 | PASS | built `log --path` in forbidden crates; no rename semantics |
| 362-2 | A | **0** | **0** | PASS | exact contract delivery |
| 362-2 | B | 4 (3 impl, 1 verif) | 7 | PASS | lossy non-UTF-8; rename-reset; unredacted content egress |
| 362-3 | A | 1 (contract) | 1 | PASS | contract pinned a stale-HEAD window agent couldn't fix |
| 362-3 | B | 4 (1 wf, 2 impl, 1 verif) | 5 | PASS | scope pre-consumed by own lineage; shape mismatch |
| 362-4 | A | 1 (contract) | 0 | PASS | contract fenced off schema.rs the change needed |
| 362-4 | B | 5 (3 impl, 1 verif, 1 wf) | 3 | PASS | contracted API absent; vacuous acceptance filter |
| 362-5 | A | **0** | **0** | PASS | all 5 scenarios via compiled binary |
| 362-5 | B | 3 (1 wf, 2 impl) | 7 | **FAIL** | **silent task substitution** — delivered other ticket's work |
| 382-1 | A | **0** | **0** | PASS | exact-scope |
| 382-1 | B | 2 (impl) | 3 | PASS | missed the attach help text the ticket is about |
| 382-2 | A | 2 (contract) | 1 | PASS | pub(crate) primitive + mode-equality gap (both contract faults) |
| 382-2 | B | 5 (4 impl, 1 contract) | 6 | PASS | renamed flag+error code; store-write on refusal path |
| 382-3 | A | **0** | **0** | PASS | best test set in packet (blind scorer's words) |
| 382-3 | B | 2 (1 model-behavior, 1 impl) | 1 | PASS | codified its own non-spec names instead of stopping |

Full blinded verdicts: `scoring-verdicts.json` (scorer never saw arms,
session logs, or run metadata; X/Y mapping in `scoring-key.json`).

## Unknown flow (v2 §B2.3)

Arm A surfaced 6 unknowns across the pilot: 4 missing-dependency blocks
(correct — protocol gap, see P1), 1 missing-neighbor-contract block, and
1 **contract contradiction discovered in the real code** (382-2 rev-1:
`diff_working_vs_tree` writes a status cache into the scanned root,
contradicting the read-only invariant the same contract imposed). Zero
silent guesses were found by the blinded scorer in Arm A patches.
Post-resolution success: the one blocked-then-answered task (382-2)
shipped cleanly on one fresh rerun after a single contract revision.

Negative control (harness accident, preserved in
`runs-invalid-01-nobrief/`): 8/8 fresh sessions given a prompt whose brief
failed to render **stopped and filed UNKNOWN.md** rather than improvising.

## Notable qualitative findings

1. **Contracts can inject hazards (A6.3 observed live).** Rev-1 of the
   382-2 contract *mandated* the mutating diff primitive; the contract-free
   arm avoided it naturally. The unknown gate caught the injection before
   it shipped. Both directions of the corruption argument, one task.
2. **Context accumulation breaks task discipline (B-362-5).** The warm
   session decided its tests task was already done, silently substituted
   the next ticket's work, and pulled that ticket from Linear (an input
   outside Arm B's pinned allowlist). The blinded scorer, without knowing
   arms, called it "the strongest workflow-integrity finding in the
   experiment."
3. **The blinded reviewer is an effective contract linter.** It found
   contract defects the author missed: the content-native file sits at
   exactly its 4,730-line allowlist cap, making "add a declaration, don't
   grow the file" unsatisfiable; `tree_fingerprints` is `pub(crate)`, so
   the prescribed read-path didn't exist within allowed paths; a
   schema-registry description was fenced out of `allowed_changes` while
   the change made it false; equality-by-bytes missed mode-only drift.
4. **Goodhart is real and measurable:** 4 Arm B cases where acceptance
   passed while intent was violated (incl. one vacuous test filter);
   0 in Arm A. The verifier-defect class earned its place.
5. **Security-relevant B finding:** blame output in B's lineage egresses
   raw file content with none of the repo's secret redaction — the class
   of invariant that only shows up when someone checks against intent.

## Validity caveats (read before quoting the headline)

- **Spec-deviation framing:** most of B's implementation defects are
  deviations from contracts B never saw. That is the intended outcome
  variable (deviation from architect intent, discovered at review) — but
  "31 defects" is not "31 bugs." B's genuinely dangerous items: unredacted
  content egress, lossy non-UTF-8 path, rename-reset, refusal-path store
  writes, and the task substitution.
- **Single blinded agent scorer**, same model family as the subjects; the
  contract author, harness author, and experiment operator are the same
  agent (this session). **Jan's inter-rater pass is pending** and gates
  the decision rule.
- N=8 tasks, one repo, one language, one model. Signal, not proof
  (v2 §A9.3's own framing).
- Symmetric contamination: the user-global CLAUDE.md was visible to both
  arms' headless sessions.
- Arm A's unknown affordance is structural: B was never *invited* to stop,
  so unknown counts are not arm-comparable; deviations-at-review is the
  comparable measure.
- Batch-2 clean-base unknowns (P1) cost ~$12.5 of extra runs and one
  protocol amendment mid-pilot; amendments were environment fixes, not
  contract tuning (contracts stayed frozen except the one counted
  revision).

## Protocol amendments applied (recorded)

- **P1 stacked bases** (both arms): dependent tasks run on predecessors'
  patches; clean-base runs preserved as unknown-surfacing data.
- Harness fixes mid-run: contract filename glob; prompts via stdin (argv
  ate leading `---`); BSD-sed neighbor resolution (batch-2 briefs lacked
  neighbor contracts — noted); detached-HEAD stacking; 3-way apply.
- Batch 1 (no-brief) preserved as negative control.

## Costs

Valid runs $123.08 (A $47.52 · B $75.56) + invalid/control batches ~$21 +
verification/scoring agents (internal tokens). Total headless spend ≈ $145.

## Next steps (per decision rule)

1. Jan inter-rater pass on `scoring/362-3/` + `scoring/382-2/`; compute
   agreement; revisit headline if low.
2. Contract-defect fixes → contract revisions (the 5 A-side defects are
   the pilot's direct design input, per "the leakage/defect logs design
   the schema").
3. Thin harness plan (files + scripts, Git-compatible, no Forge objects).
4. Pilot 2: neighbor-contract ablation (pre-registered as sequential).
5. Separately: the pilot produced real, verified implementations of
   NER-362 and NER-382 (Arm A stack, 11/11 gates) — decide whether to
   promote them into real PRs after human review.
