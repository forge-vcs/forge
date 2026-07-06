# CCX spike results log

Protocol: docs/brainstorms/2026-07-06-context-closed-tasks-landscape-and-substrate.md §7.

## T1 — brief.sh + two real contracts (2026-07-06) — DONE

Artifacts: `contracts/_global-policy.yaml`, `contracts/forge-policy-check-engine.yaml`,
`contracts/forge-evidence-capture.yaml`, `brief.sh` (deterministic concat,
task contract + neighbors one level + global policy).

**Measurements**
- Brief for `forge-policy-check-engine` (global policy + contract + 1
  neighbor contract): **8,092 bytes / 906 words ≈ ~2.0k tokens** (4 B/token
  heuristic).
- Byte-stability: verified — two consecutive emissions are `cmp`-identical
  (prompt-cache-friendly per §7.3; no timestamps/env in output).
- Baseline standing instructions (project CLAUDE.md + user CLAUDE.md +
  RTK.md): 16,060 bytes / 2,144 words ≈ ~4.0k tokens — and that baseline
  contains **zero** module-specific decision information; an Arm B session
  additionally spends tokens on repo search, file reads, and handoff docs to
  recover what the brief states directly. The H2-relevant claim: a full
  contract-neighborhood brief for a real module costs ~2k tokens and is flat
  per task.

**Authoring cost (A6.11 datum — LOWER BOUND, author-familiarity caveat per
§7.1):** ~45 min wall-clock for two contracts + global policy + brief.sh,
authored immediately after a deep exploration of both modules. Cold-start
authoring would be materially higher; treat as floor, not estimate.

**Leakage/unknown observations while authoring (pilot-design data):**
1. Both contracts needed to reference a consumer (`forge-store`
   proposals/accept, evidence persistence) whose contract does not exist.
   Recorded in-contract as "not yet authored; inspect requests are
   unknowns". Confirms A4.5 (audited declassification) will be exercised
   immediately — the contract graph has a boundary from day one.
2. One cross-module negative constraint (excerpt hash computed over
   persisted bytes) does not fit cleanly inside a single module's
   `allowed_changes` — its scope spans forge-evidence and forge-store.
   Schema pressure on A4.1: constraint scope vs. contract module boundary.
   Deferred per §A9.1 (earned, now with one datum).
3. The invariant lists are load-bearing exactly where the test suite is:
   every forge-policy invariant traces to a named test. Where tests are
   thinner (evidence timeout/kill path), confidence in the invariant wording
   drops. Supports v2's "acceptance defines done" and the authority field.

## T2 — blast-radius predicate in forge-dogfood clone (2026-07-06) — DONE

Setup: temp clone of `forge-dogfood` in the session scratchpad (never the
original, never the forge root), `forge init --content-backend native`,
debug binary from this branch. Artifact: `blast-check.py` (~60-line
predicate over any forge.cli.v0 payload carrying `changed_paths`).

**Results — the "just a predicate" claim from §4 is proven:**
- `save --json` and `propose --json` both carry `changed_paths` directly;
  no extra plumbing was needed.
- PASS case: allow `src/**` + `index.html` → `within_blast_radius`, exit 0.
- VIOLATION case: `index.html` flagged `outside allowlist` and
  `src/main.ts` flagged `forbidden` in one run, exit 2 — both violation
  kinds distinguished from day one, matching v2 §B2's "countable from day
  one" claim.

**U3 (competing attempts × blast radii) — RESOLVED, positive:**
- Two attempts under one intent are fully isolated: attempt 2 materialized
  from the shared `base_head` without attempt 1's added file.
- `forge attempt compare --intent … --json` already surfaces per-attempt
  `changed_paths` side by side — per-attempt blast-radius scoring needs
  zero new data.
- Worktree discipline is enforced with typed errors:
  `ATTEMPT_WORKTREE_MISMATCH` (with the remedial command in the message)
  when saving against a non-attached attempt.

**Friction observation (dogfood datum F1):** edits made directly inside
`.forge/worktrees/<attempt>/` *before* attaching are silently discarded by
`attempt attach` (the workspace dir is a materialization target, not an
editing surface; the repo root is the single live worktree). An agent that
"helpfully" edits the workspace path from `attempt start`'s output loses
work with no warning. Worth a Linear ticket: either warn on attach when the
workspace dir has drifted, or document the workspace path as read-only.

## T3 — CooperBench recon (2026-07-06) — DONE

Full note: `T3-cooperbench-feasibility.md`. Headlines:
- **Feasible without heavy forking** — `--no-messaging` is a first-class
  flag; the agent's task prompt is a flat `feature.md` extendable via a
  shadow `--dataset-dir` (zero code) or `--agent-config` template override;
  worst case ~10 lines in `runner/coop.py`.
- Gold `feature.patch`/`tests.patch` file lists give **ground-truth blast
  radii for free**; `eval.json` auto-splits merge-conflict vs. test-failure.
- U1 **resolved (yes)**; U2 **partially resolved**: paper numbers used
  OpenHands v0.54 (methodology since changed in-repo); Shepherd's
  28.8%→54.7% is a different harness (Haiku 4.5 workers, 479 pairs) — not
  matchable; compare our own arms instead, cite Shepherd as reference point.
- Caveats: Rust subset = one homogeneous typst PR (45 pairs, 100%
  gold-conflicting); failure-cause classifier and allowlist enforcement are
  ours to build. MIT licensed.
- **Smallest viable pilot-3:** ~10 typst pairs × 3 arms (chat / no-chat +
  briefs / chat + briefs), fixed Sonnet-class model, ~$30–75, one afternoon.
