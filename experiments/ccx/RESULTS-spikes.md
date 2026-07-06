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

## T2 — blast-radius predicate in forge-dogfood clone — PENDING

## T3 — CooperBench recon — running (background agent), note to land here
