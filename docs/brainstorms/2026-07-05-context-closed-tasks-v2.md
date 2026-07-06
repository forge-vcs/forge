---
date: 2026-07-05
version: 2
topic: context-closed-tasks
status: brainstorm + pilot protocol
origin: Jan Skolte × Claude (Fable 5) design sessions, 2026-07-04/05
review_incorporated: external model review (GPT-5.5 Pro), 2026-07-05 — accepted ~85%; dissents recorded in §A9
supersedes: 2026-07-05-context-closed-tasks-and-experiment.md (v1)
depends_on: docs/brainstorms/2026-07-04-verified-handoff-requirements.md (scope split — see §A7)
artifact_readiness: pilot-ready (Part B), design-input (Part A)
---

# Context-Closed Tasks: Contracts as Enforced Substrate

**Thesis (v2, narrowed):** Agent context should be managed as a version-control problem. The durable unit is not the session transcript but the **contract revision**: a typed, revision-bound object carrying interface, invariants, negative constraints, authority/confidence, allowed blast radius, neighbor contracts, unknowns, and replayable evidence. **Handover becomes unnecessary for contract-closed implementation tasks; for everything else it transforms into unknown capture that improves the contract graph.**

## Summary

Continuity mechanisms designed so far (prose handoffs, verified handoff records, resume packs) optimize the fidelity of transferring *history* between sessions. This document argues history transfer is the wrong default variable: implementation decisions should depend on explicit constraints wherever possible, and session history is useful only insofar as it can be **compiled into typed artifacts** — constraints, evidence, examples, confidence, unresolved unknowns, review obligations. Untyped narrative is discarded *after* compilation, not instead of it.

Four primitives: (1) **contract records** as first-class ledger objects; (2) **context-closed tasks** — graded, not binary (§A4.2); (3) **`forge brief` / `forge compact`** — deterministic contract emission at session start, compilation (not summarization) at session end; (4) **`forge unknown`** — a typed, gated third move between guessing and failing, in three classes (blocking / assumption / observation).

Scaling claim, scoped honestly: **per-task injected implementation context becomes O(contract + neighbor contracts + global policy)** — independent of codebase size *when the relevant architectural context has already been compiled into a bounded contract neighborhood*. Decomposition, authoring, review, and maintenance costs do not vanish; they are amortized into durable, versioned objects. The bet is that this amortization pays.

**Run the pilot (Part B) before building any primitive.** The load-bearing unproven assumption is decomposition quality (A-CORE), not any Forge mechanism. Pilot results justify a thin harness, not a substrate; language throughout uses *pilot decision threshold*, not hypothesis validation.

---

## Part A — Design

### A1. First-principles core (corrected)

1. An agent needs context to make the next decision correctly.
2. For routine implementation, decisions depend chiefly on constraints: what must hold, what must not change, what was ruled out and why. **For architecture, debugging, product tradeoffs, and weakly specified domains, history carries non-constraint information that still changes decisions** — epistemic confidence (why do we believe this invariant?), preference ordering (what wins when constraints conflict: latency vs. simplicity, compat vs. elegance), causal lineage (a negative constraint without its failure's causal shape becomes superstition), and compressed domain intuition (compilable into examples, threat cases, benchmarks, checklists — not always into tests).
3. Therefore the move is not "discard history" but: **compile history into typed artifacts — constraints, evidence, decisions, confidence, examples, threat cases, open unknowns — and discard only untyped narrative after compilation.**
4. A **context-closed task** is one a fresh agent can complete from its contract neighborhood alone, with completion machine-verified. Closure is a *degree*, not a bool (A4.2).

### A2. Prior art and the actual wedge

Discipline lineage: Parnas 1972 (decomposition criteria determine comprehensibility), Meyer (design by contract), TDD (executable acceptance). This design's novelty claim is narrow and specific: these were **human discipline, which fails under pressure; Forge makes them enforced substrate** — for workers that never resent being refused.

Related work (verified 2026-07-05) — constraint metadata near commits is now commodity:
- Git natively supports **notes** (attach data to objects) and **trailers** (parseable key-value metadata in messages).
- **Lore** (arXiv 2603.15566, 2026): protocol restructuring commit messages via native trailers into decision records carrying constraints, rejected alternatives, agent directives, verification metadata. At least two further tools share the name and adjacent goals, including daemon-based Claude Code session-reasoning capture with just-in-time context injection.
- **Jujutsu / Pijul / Sapling** attack VCS ergonomics (operation logs, patch theory / first-class conflicts, scale + stacks). None makes agent-produced change validity a merge-time property.

**Forge's defensible wedge is therefore not "we store contracts near code" (a convention Git can host today) but: the VCS object model and merge gates treat contracts, unknowns, and evidence as first-class revision-bound objects that control whether a change is valid.** Conventions can be ignored; Forge's objects are load-bearing. Merge rule (target end state): a revision merges only if affected contracts have current passing evidence bound to the exact contract revision, no blocking unknown is unresolved, no assumption unknown is unreviewed, touched paths sit inside the task's blast radius, and contract changes carried their review tier.

### A3. Hallucination and task construction (corrected)

Hallucination is unconstrained generation: an unknown is an unconstrained region, and the model fills it with plausibility. Solid code is a **joint** property of model capability, task construction, architecture, tooling, verifiers, review, and domain clarity — but **task construction is the highest-leverage control surface** available to us, because strong contracts and executable checks shrink the gap between plausible and correct outputs. (v1's "solid code is a task-construction property, not a model property" retired as rhetorically strong, technically false.)

### A4. The primitives

**A4.1 Contract record** (ledger kind, hash-chained, revision-bound). Pre-pilot minimal schema — only fields that change what the pilot can detect:

```yaml
interface: …            # signatures / API surface / schema
invariants: […]         # machine-checkable where possible
acceptance: …           # command(s) whose pass defines done, run via forge run
negative_constraints:
  - rule: …
    scope: {paths: […], operations: […]}
    reason: …
    source_evidence: …   # incident/evidence/decision ref — causal lineage, not superstition
neighbors: […]           # contract IDs; briefs expose neighbor CONTRACTS, with audited exceptions (A4.5)
authority:                # NEW (review §2): clauses are not equally authoritative
  source: human|agent|test|prod-incident|external-doc|inference
  confidence: high|medium|low
  reviewer: …
allowed_changes:          # NEW (review §12): blast radius — Forge computes violations from the diff
  paths: […]
  forbidden_paths: […]
  public_api_change_policy: none|contract-update-required
```

Field typing is a **prompt-supply-chain control** (review §14): normative fields (interface, invariants, acceptance, negative_constraints, allowed_changes) vs. non-normative (rationale, examples, history), rendered by `forge brief` with explicit authority boundaries. External issue text never flows into a brief as normative instruction; it is normalized through reviewed contract fields.

**A4.2 Closure as a grade, not a bool** (review §4): C0 exploratory (no closure claim) · C1 bounded spike (output is knowledge, not a production patch) · C2 implementation-closed (code may be written; strict review) · C3 verification-closed (may merge after evidence) · C4 replay-closed (hermetic evidence, stable contract). The workflow keys review strictness and allowed modes off the grade. Real work lives between "closed" and "not."

**A4.3 `forge brief` / `forge compact`.** Brief: deterministic emission of contract + neighbor contracts + global policy; no LLM in the builder; regenerated fresh, never cached. Compact: session-end *compilation* — contract deltas (always proposed as reviewable changes, never auto-applied), negative constraints with lineage, decisions, confidence; grade what couldn't compile; discard the narrative remainder. **New risk owned here (A6.10):** compact's danger is not silent application (already blocked) but *review fatigue* — ten plausible deltas per session degrade the human gate to rubber-stamping, and poisoning happens through review, not around it. Mitigations: rate-limit deltas per session, require each delta to cite trace evidence, batch by contract so a reviewer sees coherent diffs.

**A4.4 `forge unknown` in three classes** (review §5 — adopted; v1's single stop-class retained only for the pilot):
- **blocking** — cannot continue without contract-author input; task → `blocked-on-unknown`.
- **assumption** — implementation proceeds, **merge is gated** until the assumption is accepted, rejected, or converted into a contract delta. (`--kind assumption --assumption "Store::get returns NotFound, not null" --evidence src/store/errors.rs:41`)
- **observation** — non-blocking discovered gap; logged; counts against contract quality.
Incentive inversion stands: when the brief is silent, surfacing beats guessing — and it is a *gate*, not an instruction. Resolved unknowns compile into contract revisions or negative constraints; **unknowns-per-task trending down remains the measurable proxy for A-CORE**.

**A4.5 Audited declassification** (review §8 — replaces v1's "neighbor contracts, *never* implementations"): some implementation facts are unpromoted API facts (error taxonomy, idempotency, ordering, latency class, locking, retry/transaction semantics). `forge inspect neighbor <x> --field <f> --why <reason>` returns an existing contract field, or creates an unknown, or triggers promotion of the fact into the neighbor's contract, or is denied because the task boundary is wrong. Information hiding without pretending boundaries are ever complete.

**A4.6 Evidence binding (hermeticity)** (review §6): "acceptance passed" is too soft. Evidence binds tree hash, contract hash, task hash, runner hash, command, toolchain lock, environment digest, output digests, artifact hashes, verdict. Non-hermetic dependencies must be declared. This is the C4 criterion and Forge's existing evidence-bound-to-revision semantics extended one level down.

### A5. Workflow integration

Unchanged from v1 in essence: `.forge/workflow.yaml` answers *what now*, contracts answer *what exactly, within what bounds*; transition requirements become contract-aware (evidence bound to the task's contract revision; blast-radius check on diff; unknown-class gates per A4.4). Contract changes carry a stricter lifecycle than code (review §9): draft → reviewed → active → deprecated → superseded, with escalations for weakening (rationale + affected-task scan), deletion (proof of no dependents), and negative-constraint removal (evidence the original reason no longer applies). Self-change protection extends from workflow file to contract records — same circularity breaker, same human anchor.

### A6. Assumptions under attack (v1 set retained; deltas below)

- **A-CORE unchanged and still the bet:** decomposition quality. Hallucination risk concentrates upward into the decomposer; Part B measures it.
- **A6.1–A6.9 carry over from v1** (spec-completeness spectrum; Goodharted acceptance — contract-author ≠ implementer remains load-bearing; contract corruption as deterministic hallucination injection, now with the §A5 lifecycle as mitigation; non-closable work coverage ratio; neighbor sufficiency → now A4.5; opaque-encoding rejection; unknowns can't be front-loaded → now A4.4; contract rot → now also §A8 reverse index; decomposer-role viability).
- **A6.10 (new) Compact review fatigue** — see A4.3.
- **A6.11 (new) Adoption cost is a first-class failure mode** (review §20): if contract authoring feels like writing a second codebase, the system dies socially before technically. This is why the pre-pilot schema is minimal and §A9 defers the rest: fields are *earned by defect classes*, not speculated by reviewers — human or model.

### A7. Scope split with verified handoffs

`forge brief` replaces the resume pack for contract-closed work (C2+). Verified handoffs (2026-07-04 doc) remain the mechanism for C0/C1 work and mid-task interruption. Update that doc's scope statement when this lands.

### A8. Reverse index (review §13)

Contract rot is unmanageable by sweeps alone. Forge maintains natively what Git needs scripts for: file ↔ owning contract(s), contract ↔ implementation files ↔ acceptance commands ↔ neighbors ↔ evidence. On every diff: changed files → affected contracts → required checks → stale evidence invalidated. This is where being a VCS is the unfair advantage.

### A9. Review dissents — recorded so v3 doesn't relitigate

Accepted ~85% of the external review. Three dissents:
1. **Schema maximalism deferred.** The review prescribes ~30 fields (closure scores, expiry, invalidation triggers, supersession chains, contradiction detection, security tiers, examples) before any pilot. That contradicts its own adoption-cost warning and the experiment-first discipline: **the leakage/defect logs design the schema.** Deferred-until-earned list: closure grade automation (earned by mis-graded-task defects), expiry/review-after on negative constraints (earned by staleness incidents), contradiction lint (earned by first observed contradiction), full contract lint battery (earned incrementally), object-model formalization §15 (post-pilot). Adopted pre-pilot only: authority/confidence, unknown kinds, allowed_changes — each changes what the pilot can *detect*.
2. **"forge compact is the most dangerous primitive" — half-right.** Silent application was already blocked in v1 (deltas are proposals). The live risk is review fatigue (A6.10), a different mechanism with different mitigations.
3. **Arm D rejected at this N.** Four arms across ~6 tasks measures nothing (the review itself calls N=6 signal, not evidence). Neighbor-contract ablation becomes **sequential pilot 2**, run only if pilot 1 clears its threshold.

---

## Part B — Pilot Protocol (run before building anything)

### B0. Hypotheses and pilot decision thresholds

**H1:** brief-only fresh sessions produce equal-or-fewer defects than status-quo continuity on decomposable implementation work. **H2:** injected context per task stays flat across tasks under briefs while status-quo context grows. **H-CORE:** the human+agent loop decomposes ≥70% of a real feature into C2+ tasks with ≤1 contract revision/task median.

**Decision rule:** thresholds met → build a **thin, Git-compatible harness** (files + scripts; no new Forge objects yet), then a second pilot (neighbor ablation) before any substrate work. H1 fails / H-CORE holds → contracts as hygiene, verified handoffs stay primary. H-CORE fails → thesis falsified at this granularity; publish the negative result. **Pilot results justify a harness, never the claim that a problem class is deleted.**

### B1. Setup

- Subject: Forge itself or a Holland service; existing tests + CI; tasks of 0.5–2h agent-effort.
- Decompose one real feature (candidate: NER-366 slice or small sync feature); log every ambiguity and the felt cost (A6.9/A6.11 data). **Freeze all contracts before any implementation.** Contract author ≠ implementer.
- Contracts: plain YAML in `experiments/ccx/contracts/`, minimal schema per A4.1. `brief.sh` = concatenation. No Forge changes.
- **Arms** (≥6 tasks, same tasks, order-randomized, clean branch per run):
  - **A:** fresh session/task; brief only; CLAUDE.md stripped to mechanics (build commands, layout). Unknown rule: pilot uses stop-on-unknown (single class) — the three-class machinery is a build-phase feature; record which class each stop *would have been*.
  - **B (pinned exactly, review §10.5):** one continuous session; allowed inputs: full CLAUDE.md, docs/handoffs, repo search, prior notes; same commands, same branch-reset, same reviewer prompt. Fixed before the pilot starts; no day-to-day drift.
  - **C:** brief without negative_constraints (isolates dead-end knowledge value).
  - *(Arm D — neighbor ablation — deliberately excluded; sequential pilot 2.)*
- **Review hygiene (review §10.1):** anonymize diffs (strip session metadata) before scoring; reviewer sees no session logs until defect scoring is done; score all of a task's arms in one sitting to limit drift.

### B2. Metrics

1. **Defects at first review, classified by the taxonomy (review §10.3):** *implementation defect* (agent violated an available contract clause) · *contract defect* (contract lacked the decision information — an H-CORE datum, not an agent failure) · *verifier defect* (acceptance passed despite invariant violation — Goodhart audit) · *workflow defect* (gating allowed an invalid state) · *model-behavior defect* (guessed despite the unknown rule).
2. **Headline metric: unlicensed decisions per accepted patch** — reviewer asks per decision "which brief line authorized this?"; counted identically in Arm B for comparability. Context size alone is vanity (review §10.6); pair every size number with defect and effort rates.
3. Unknowns: surfaced vs. guessed vs. silent-guess-defects; **score throughput and context-control separately** (review §10.4): first-run patch success · first-run unknown surfacing (a *success* signal, not a failure) · post-resolution success after one contract-author answer + one fresh re-run · total human+agent effort to green.
4. Acceptance first-pass rate and attempts-to-green; tokens injected at start and total; contract revisions per task; leakage events verbatim; blast-radius violations (diff paths outside `allowed_changes` — countable from day one); contract-staleness incidents; wall-clock and cost.

### B3–B4. Procedure and guardrails

As v1, plus: contracts frozen pre-implementation (B1); defect classification applied at review time, not post-hoc; no mid-arm contract tuning (revisions are data); no handoff peeking in Arm A — a halt is a successful protocol outcome; report first-run and post-resolution results separately; if <6 decomposable tasks emerge, add a second feature rather than shrink N. Surface to Jan before starting: feature choice, Arm C inclusion, confirmation that stripping CLAUDE.md process prose on a branch is acceptable, and the pinned Arm B definition.

### B5. Pre-registered readings

- **Strong:** Arm A ≤ B on defects with fewer unlicensed decisions per patch; ≥80% first-attempt success counting clean surface+resolve as success; flat brief size vs. growing Arm B context; ≤1 revision/task; surfaced ≥ guessed. → Build the thin harness; schedule pilot 2 (neighbor ablation).
- **Mixed (most likely):** A wins on hallucinated-API defects and unlicensed decisions, loses where leakage events cluster; H-CORE 60–70%; guessing persists despite the rule → confirms unknown-surfacing must be a merge gate, not an instruction. → Contracts default for closable work; leakage + defect taxonomy drive which deferred fields (§A9.1) get built.
- **Failure:** A materially worse, or decomposition cost exceeds savings, or >40% of tasks resist closure. → Falsified at this granularity; keep workflow-engine + verified-handoff tracks; contracts demoted to documentation hygiene. Publish honestly either way.

---

## Brief for the executing agent

Read Part B. Surface the B3–B4 questions to Jan. Execute the pilot exactly as pinned: build `brief.sh` + results logger, decompose with Jan, freeze contracts, run arms A/B/C with review hygiene, produce `experiments/ccx/RESULTS.md` using the defect taxonomy and the pre-registered readings. Build no Part A primitive until B0's decision rule says to. The most valuable outputs are the leakage log, the unknown log, and unlicensed-decisions-per-patch — they design whatever gets built next.
