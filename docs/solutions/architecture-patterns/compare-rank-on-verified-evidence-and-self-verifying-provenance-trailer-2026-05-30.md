---
title: "Rank competing attempts on verified evidence and carry a self-verifying provenance trailer: the read-only selection surface, the git-adapter boundary, and recompute-don't-persist"
date: 2026-05-30
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: compare-rank-and-provenance-trailer
severity: high
applies_when:
  - A read-only API selects a "winner" from records whose trustworthiness was established by a separate integrity layer
  - A store-level API conceptually needs an operation (a git diff) that requires a dependency the store crate cannot take
  - A published artifact must carry a digest that a later step recomputes from the local ledger to confirm consistency
  - A new egress filters or drops paths returned by `git diff` / `git ls-tree`
  - An earlier phase deferred a persistence decision to "the consumer", and you are the consumer
tags: [compare-rank, verify-before-rank, read-only-advisory, git-adapter-boundary, dependency-cycle, provenance-trailer, content-addressed-digest, domain-separation, recompute-dont-persist, secret-redaction, git-z-quoting, additive-error-drift-guard, ner-137]
---

# Rank competing attempts on verified evidence and carry a self-verifying provenance trailer

## Context

NER-137 (Forge M2 Phase 6) built the headline wedge surface: `forge compare`/`attempt compare` ranks competing attempts under one intent on Phase-5 tamper-evident evidence, and `export branch` carries a structured `Forge-Provenance-Digest` trailer that `export verify-branch` recomputes from the local ledger (fail-closed `PROVENANCE_MISMATCH`). It consumes Phase 5's integrity rather than re-deriving it. The visible features (a ranking, a digest, a verify command) were the easy part. The parts that bite — and that doc-review and an 11-persona code-review surfaced after a green 280-test suite — are *where the git-adapter work can live*, *how a read-only selection surface handles a tampered record*, *what a recompute-from-ledger PASS actually proves*, and *why `-z` is load-bearing on a diff egress*. This captures them so Phase 8 (native diff) and Phase 9 (signing/sync) don't re-derive them. Builds on the M1/M2 substrate ([Related](#related)).

## Guidance

### 1. A store-level API that needs `git diff` can't host it — push the adapter work out, expose only the git-free data

The plan said "`compare_attempts` computes the per-attempt diff summary via the git adapter." It can't: `forge-store` must **not** depend on `forge-export-git`/`forge-content-git` (the former *depends on* `forge-store`, so it is a cycle; and routing git through core re-opens the §23.4 "git leaks into the core" risk Phase 3 closed). The resolution that the dependency graph *forces* is also the cleaner design:
- The **store** exposes only git-free data: the per-attempt "diff summary" is the **already-stored `changed_paths`** (file-level, on the proposal row), and a git-free recompute primitive (`build_publication_trailer`).
- The **adapter** (`forge-export-git`, which *may* depend on `forge-store`) hosts everything that shells to git: the tree-vs-tree `diff_trees` (the pairwise file/hunk diff) and the `verify_publication_trailer` orchestration (read the commit, recompute via the store, compare).
- The **CLI** wires them: `compare --diff <a> <b>` calls the adapter's `diff_trees` with content_refs the store returned.

The reusable rule: when a store/core API "needs" an operation that requires a dependency it cannot take, that is a signal to **invert** — the core exposes the *data and the pure recompute*, the adapter does the *impure work and the orchestration*. Don't add the dependency; don't inline git into core. (A plan is a decision artifact; the dependency reality refines the *how*, and the refinement was strictly better than the literal plan.)

### 2. Verify-before-rank is a determining read that feeds a SELECTION, not a write — so it's read-only/advisory, and a tampered record becomes a label, not an error

The in-txn determining-read rule (push the read into the writer's transaction) is about a read that feeds a **write** racing a lock-free writer. Compare **never writes**, so it takes **no** advisory lock and its ranking is an **advisory snapshot** a concurrent lock-free `run` can stale — the authoritative integrity gate stays in-txn at `accept`/`decide`. Three non-obvious consequences:
- **Re-verify per record, but don't propagate.** Compare runs the Phase-5 cheap per-row check on each attempt's deciding evidence and **records** a `tampered`/`legacy_unverified`/`verified` label instead of raising — a tampered *rival* must not blank the whole comparison. (Contrast: at `export`, building the trailer **does** raise `EVIDENCE_TAMPERED` — a trust-bearing action fails closed.)
- **Unranked, not "ranked last".** A headless consumer chains `compare → accept` by selecting the numeric-minimum `rank`. So a cheap-check-tampered attempt must be `rank: null` (unranked), **not** merely ranked last — otherwise an all-tampered intent group still yields a `rank: 1` a numeric-min chainer would pick. Test the all-tampered group explicitly.
- **Surface every non-green label in the human-readable field too.** A `legacy_unverified` (grandfathered, never-hash-verified) attempt is rankable, so its `rank_reason` — the string a rank-only agent reads — must name the caveat, not just the separate `integrity` field.

### 3. Recompute-don't-persist is the right resolution of a prior phase's "emit-don't-persist" — a persisted verdict is a stale cache that masks later tampering

Phase 4 deliberately emitted per-gate verdicts without persisting them, naming Phase 6 (compare/rank) as the consumer who would "add the column when it consumes it." The correct Phase-6 decision was the *opposite* of adding a column: **recompute** the per-gate outcomes live from `evidence`/`intents` each time compare runs. The reason is security, not just schema-honesty: compare must re-verify integrity live anyway, and a verdict persisted at check-time is a **stale cache** that a later tamper would not invalidate — ranking off it would reintroduce the hole the integrity model closed. Consequence: **Phase 6 added no `.sql` migration** (no `schema_head` bump, none of the head-bump literal fan-out). When an earlier phase hands you a persistence decision, "recompute from the source of truth" beats "persist a derived value" whenever the derived value's freshness is itself the security property.

### 4. The provenance digest reuses the integrity module (new domain tag, persisted bytes) — and verify-branch is local-consistency, NOT authenticity

The trailer's "content-addressed evidence digest" is a **new aggregate record kind** (it bundles proposal ids, the deciding evidence `content_hash`es, the decision digest, and gate outcomes), so it gets its own `forge.publication.v0\0` domain tag via the existing `DigestWriter` — never an ad-hoc `format!`+sha256, and pinned by a golden-vector + a no-collision-with-existing-tags test. It folds the **persisted** Phase-5 `content_hash`es (length-prefixed list), so it recomputes from the ledger by construction. The load-bearing honesty: a `verify-branch` PASS proves the published trailer is consistent with the **current local ledger** (it catches a rewritten commit message or a naively-edited row), but it is **not** an authenticity proof — an actor who rewrites the ledger rows *and* re-exports still matches, and the deciding-row check is the cheap per-row check (a fully re-hashed row is caught only by `doctor`'s op-walk). State this in the machine contract (`schema notes.provenance`), in R4/R7/R8, and in an Integrity Scope note — don't let "ranks only on verified evidence" / "the digest verifies the commit" read as an absolute the cheap check doesn't deliver. (Doc-review caught the absolute language *before* implementation by reasoning about the cheap-vs-doctor split; it would not have shown up as a failing test.)

### 5. `-z` is load-bearing on any git-diff egress that filters by path — C-quoting defeats the secret-path drop

`git diff --name-status`/`--numstat` and `git ls-tree --name-only` **C-quote** any path containing a tab/newline/non-ASCII byte (`.env\ttest` → `".env\ttest"`). A secret-path filter (`is_secret_risk_path`) then fails to match the quoted string and the secret **filename leaks** into the diff/export egress. Parse with **`-z`** (NUL-delimited, never quoted) and split on `\0` so the filter sees the real path. (Pair it with `--no-renames` so `--name-status` and `--numstat` key on the same plain path — a rename otherwise appears in numstat as `old => new`, silently dropping its line counts.) This was a code-review P1 corroborated by three personas. The general rule: **any new egress that drops/filters paths returned by a git plumbing command must use `-z`** — the default human-readable output is hostile to byte-exact path matching.

### 6. A typed error must distinguish "not our artifact" from "our artifact, but wrong" — and verify a precondition the happy path established elsewhere

`verify-branch` is marketed as an agent/CI gate, so its failure modes must be **machine-distinguishable**. Two code-review fixes:
- **"Not a Forge commit" ≠ failure.** A plain git commit (no `Forge-*` trailer) returning a bare `anyhow!` → `COMMAND_FAILED` conflates "this branch wasn't produced by Forge" with a real failure. Add a typed `MISSING_PROVENANCE_TRAILER` (full additive drift-guard fan-out) so the agent can branch: skip vs fail. An agent-facing verify/gate command should have **no untyped exit**.
- **Verify the precondition the other caller assumes.** `export` gates on `decision == accepted` *before* assembling the trailer; `verify-branch` calls the same assembly **independently** and had no such check — so a manufactured commit referencing a never-accepted revision produced a self-consistent (empty-decision) digest `verify-branch` would confirm. The fix: push the `accepted` precondition **into** the shared assembly primitive (`build_publication_trailer` → `NotAccepted` when the revision isn't accepted), so both callers are fail-closed. When two callers share a primitive but only one pre-checks an invariant, move the check into the primitive.

## Why This Matters

Compare/rank is the literal definition of agent self-selection — spawn N attempts, pick the best on tamper-evident evidence with no branch-juggling — and the published trailer is the artifact a teammate or CI sees. Both are trust surfaces, and the holes that bite are invisible to a green suite: a store API that can't host the diff it "needs" (§1), a tampered rival that blanks the comparison or a "ranked-last" tampered attempt a numeric-min agent still picks (§2), a stale persisted verdict that masks a later tamper (§3), a "verified" claim stronger than the cheap check delivers (§4), a secret filename leaking through C-quoting (§5), and a verify command that can't tell "not ours" from "broken" or confirms a hollow trailer (§6). Five of these six were caught by doc-review reasoning or a multi-persona code-review, not by the 280-test suite — which is why they are worth writing down before the Phase 8 native diff and Phase 9 signing build on this surface.

## When to Apply

- Any read-only API that **selects** among records whose trust was established elsewhere — re-verify per record, record the label (don't propagate), and make the disqualifying value un-selectable by the consumer's selection key (`null`, not "last").
- Any store/core API that "needs" an operation requiring a dependency it cannot take — invert: core exposes data + pure recompute, the adapter does the impure work.
- Any "the consumer will persist it later" decision you inherit — prefer recompute-from-source over a persisted derived value when freshness is the security property.
- Any digest carried into a published artifact and recomputed later — reuse the integrity module with a new domain tag over persisted bytes; state precisely what a PASS proves (local consistency) and does not (authenticity).
- Any new egress that filters paths from `git diff`/`ls-tree` — parse with `-z`; pair with `--no-renames` when correlating two passes.
- Any agent-facing verify/gate command — no untyped exit; distinguish "not our artifact" from "wrong"; push shared preconditions into the shared primitive.

## Scope boundaries (deferred)

LOCAL only: cross-machine provenance / ledger sync / a wire protocol / signing are Phase 9; the content diff is via the git adapter (native diff with rename detection is Phase 8). Ranking is a simple, explainable, deterministic default (gates-passing → fewer test failures → more passing → stable tiebreak) with raw evidence always returned — no configurable scoring DSL; the metric tier scores **test counts only** in v0 (clippy findings are returned but don't influence order). Deferred follow-ups (NER-137 code-review triage D1–D5): the identical pre-existing C-quote flaw in the *export* path's `filter_secret_paths_from_tree`; per-attempt connection reuse in `compare_attempts` (v0-scale perf); structural row-mapper dedup; residual test coverage (`compare_structured_metrics`, real `legacy_unverified`/`no_evidence` labels, `parse_forge_trailers`, binary-file diff); an `IntegrityStatus` closed-enum label (vs the free-form `String`).

## Related

- Plan: `docs/plans/completed/2026-05-30-011-feat-phase-6-compare-rank-provenance-plan.md`
- Code-review triage: `docs/code-reviews/2026-05-30-ner-137-phase-6.md` (the `-z` secret leak, the non-accepted-decision fail-closed gap, and the `MISSING_PROVENANCE_TRAILER` agent-gate gap were code-review findings; the Integrity Scope honesty note, tampered→`rank: null`, and the recompute-don't-persist framing were doc-review findings — none were pre-merge test failures).
- Consumes (Phase 5): `docs/solutions/architecture-patterns/tamper-evident-evidence-chain-and-failclosed-verification-2026-05-30.md` (the integrity model — verify-then-rank, `DigestWriter` discipline, the cheap-gate vs deep-`doctor` boundary §4 carries to the selection surface and the trailer; D6 `structured_failures` and D3 verify-deciding-evidence-at-export were pulled in here).
- Substrate this builds on: `docs/solutions/architecture-patterns/content-bound-gate-engine-and-failclosed-enforcement-2026-05-29.md` (§7 emit-don't-persist — this is its named consumer, resolved by recompute; §4 every-egress redaction — the compare JSON, diff hunks, and trailer are three new egresses), `docs/solutions/architecture-patterns/write-binding-verification-and-content-backend-isolation-2026-05-29.md` (the additive-error-on-every-surface discipline the two new error codes follow; the §23.4 git-leak boundary §1 honors), `docs/solutions/architecture-patterns/sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` + `crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md` (the lock-free `run` carve-out is why compare is advisory; "a lock only helps when both racers take it").
- Implementation: `crates/forge-store/src/lib.rs` (`compare_attempts`, `build_compare_row`, `aggregate_integrity`, `rank_compare_rows`, `compare_structured_metrics`, `build_publication_trailer`, `render_trailer_message`, `decision_digest_and_actor`), `crates/forge-store/src/integrity.rs` (`publication_digest`, `PUBLICATION_TAG`), `crates/forge-store/src/error.rs` (`ProvenanceMismatch`, `MissingProvenanceTrailer`), `crates/forge-policy/src/lib.rs` (`GateResult.structured_failures`, `identity_string`), `crates/forge-export-git/src/lib.rs` (`diff_trees` with `-z --no-renames`, `verify_publication_trailer`, `parse_forge_trailers`), `crates/forge-cli/src/{main.rs,schema.rs}`.
- Eval & tests: `scripts/e2e-eval.sh` (the compare/rank+provenance block), `crates/forge-cli/tests/forge_compare.rs` (the exit-criterion flow + tamper-a-winner), `crates/forge-cli/tests/forge_accept_export.rs` (trailer + verify-branch + the typed-error paths).
