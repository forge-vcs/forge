---
title: "feat: Phase 6 — Evidence-based attempt compare/rank + provenance-carrying publication"
type: feat
status: active
date: 2026-05-30
origin: docs/ROADMAP.md  # Phase 6 section; ticket NER-137. Wedge requirements: docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md (F4 / R18–R20). No dedicated *-requirements.md — the ROADMAP Phase 6 entry + the competing-attempts brainstorm are the source.
---

# feat: Phase 6 — Evidence-based attempt compare/rank + provenance-carrying publication

## Summary

Ship the headline wedge surface: a **first-class compare/rank API** that returns, per competing attempt, the changed-paths/diff summary, the per-gate check results, and the structured metrics (test/lint counts) — replacing `list_attempts`' id/intent/base_head/status-only output — so a human or agent can select a winner from **verified** data and chain `compare → accept` headlessly. Then carry a **self-verifying provenance trailer** into the published git commit: replace the constant `"Forge accepted proposal"` message with a structured trailer carrying `proposal_id`, `proposal_revision_id`, a content-addressed evidence digest (derived from the Phase 5 per-evidence `content_hash`es), the deciding actor, and the gate outcomes — plus a verification step that recomputes the digest from the **local** ledger and confirms the published trailer matches.

The load-bearing integration: compare/rank ranks on **Phase 5-verified** evidence. An attempt whose deciding evidence fails the cheap per-row hash-verification is surfaced as `tampered`/`untrusted` and is **unranked** (never a green/ranked winner). The trailer's provenance digest reuses the `integrity.rs` digest discipline (length-prefixed, domain-separated; custom crypto banned), so the verification step recomputes it from the same ledger rows and confirms a match by construction. Phase 6 **inherits** Phase 5's integrity — including its honest boundary (see the **Integrity Scope** note below): the cheap per-row check that compare/trailer/verify-branch use catches the naive/interior edit (the literal Phase 4 hole), while a fully re-hashed row is caught only by `doctor`'s op-walk; Phase 6 does not re-derive a stronger guarantee than Phase 5 ships.

Everything is **additive** over the M1/M2 substrate and — by a deliberate decision (Key Technical Decisions) to *recompute* per-gate outcomes rather than persist them — Phase 6 introduces **no schema migration** (no `schema_head` bump). One new typed error code (`PROVENANCE_MISMATCH`) lands on every contract surface; the envelope stays `forge.cli.v0`.

**Scope honesty (positioning checkpoint):** at the end of Phase 6 this is **LOCAL** compare/rank + a self-verifying **LOCAL** trailer — **NOT** cross-machine provenance. Ledger sync / a wire protocol / signed attestation are Phase 9. The content diff is produced **via the git adapter** only; native diff with rename detection is Phase 8. This phase fully delivers the honest near-term claim: *an agent-native, trust-anchored review/evidence ledger that interoperates with git.*

---

## Problem Frame

The competing-attempts wedge (`docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md`) is "multiple agents attempt, the best is selected." Phases 3–5 made each attempt's evidence **bound** (Phase 3 write-binding), **gated** (Phase 4 multi-gate check engine), and **tamper-evident** (Phase 5 hash-chain). But the selection surface itself — F4 "Human compares candidates", R18–R20 "metadata-first comparison" — is the piece the strategy assessment flagged as having "no API at all" today: `list_attempts` returns only `{attempt_id, intent_id, intent, base_head, status, attached}`, and `attempt show`/`proposal list` are single-attempt, single-latest views. There is no surface that lays competing attempts side by side with their diffs, gate verdicts, and metrics, and no way to chain `compare → accept` from verified data.

Separately, every published branch today carries the constant commit message `"Forge accepted proposal"` (CLI export handler → `forge_export_git::export_branch`). The published git artifact therefore carries **none** of the ledger provenance — which proposal/revision, which evidence, which decider, which gates passed — even though all of it exists locally and is tamper-evident. PR-body generation under-reports for the same reason (`pr_body_for` reads a single latest evidence row).

Phase 6 closes both gaps **locally**: a compare/rank API that ranks on verified evidence, and a provenance trailer that makes the published commit a self-verifying pointer back into the local ledger.

---

## Requirements

- **R1.** A store-level compare/rank API returns, **per attempt under one intent**: `attempt_id`, `intent`, `status`; the latest proposal's `proposal_id`/`proposal_revision_id`/`snapshot_id`/`base_head`; the changed-paths/diff summary; the per-gate check results (verdict + deciding `evidence_id` + `exit_code` + structured failure count); the structured metrics (parsed test/lint counts); the **integrity status** of the deciding evidence; the **`decision_status`** and **`publication_status`** of the proposal (origin R19 — "already accepted? already exported?" is selection-relevant, and the existing `proposal_metadata_for_attempt` rollup already computes both); and a deterministic rank. Raw per-attempt evidence is **always** returned alongside the rank (ranking advisory, evidence authoritative).
- **R2.** A CLI surface `forge attempt compare` **and** the top-level alias `forge compare` emit the comparison as a stable `--json` envelope keyed by `attempt_id` / `proposal_id`, so an agent can chain `compare → accept` headlessly. With no selector, compare returns every intent that has ≥1 attempt, each as a ranked group; `--intent <id>` filters to one; `--attempt <id>` scopes to that attempt's intent. (No new ambiguity error code — multiple intents are *grouped*, not an error.)
- **R3.** The default ranking is **deterministic and explainable**: all-required-gates-passing attempts rank ahead of any non-passing attempt; within each tier, fewer parsed test failures rank ahead, then more parsed passing, then a stable tiebreak (`base_head` recency / `attempt_id`). The ranking is a simple total order with a human `rank_reason`. **v0 narrowing (deliberate):** the metric tier orders on **parsed test counts only** — clippy/lint findings are captured into the per-attempt `metrics` and returned as raw evidence, but do **not** influence the order (keeping the default order simple and explainable per the ROADMAP's "keep the default ranking simple"); a caller wanting lint-weighted order re-ranks on the always-returned raw metrics. No configurable scoring DSL.
- **R4 (load-bearing).** Compare/rank ranks on **cheaply-verified** evidence. For each attempt, the deciding evidence rows are re-verified with the Phase 5 **per-row** check (`verify_evidence_integrity`); an attempt whose deciding evidence is `Tampered` is surfaced with `integrity: "tampered"` and is **unranked** (`rank: null`, never in the passing tier and never assigned a numeric rank — so a headless consumer that selects by numeric-minimum rank cannot pick a tampered attempt), **regardless** of its stored exit code or gate verdict. This holds even in any future `--unverified` mode (a policy bypass is never an integrity bypass). **Boundary (see Integrity Scope):** the per-row check catches the naive/interior edit but a fully re-hashed row is caught only by `doctor`'s op-walk; compare therefore does not claim doctor-grade verification, and `accept`/`decide` remain the authoritative in-txn gate (which independently re-verifies, also at the per-row level).
- **R5.** A content-level diff between two competing proposals at **file/hunk granularity** is produced **via the git adapter** (`diff_trees`), resolving each proposal's `content_ref` through the existing `git_tree_for_content_ref` chokepoint (so git-tree and forge-tree refs both diff uniformly). The per-attempt compare summary is **file-level** (name-status + numstat) by default; the **hunk-level** body is produced only on an explicit pairwise/`--diff` path (not for every attempt on every `forge compare` — see the per-call-cost note in Key Technical Decisions). Secret-risk-named paths are dropped from the diff (via `is_secret_risk_path`), binary files emit no hunk body, and any emitted hunk body is redacted (via the evidence redactor) and bounded. Native diff with rename detection is **out of scope** (Phase 8).
- **R6.** The constant `"Forge accepted proposal"` commit message is replaced with a **structured provenance trailer** carrying `proposal_id`, `proposal_revision_id`, a single content-addressed **provenance digest** (the `Forge-Provenance-Digest` trailer line), the **deciding actor**, and the **gate outcomes**. The provenance digest is built with the `integrity.rs` `DigestWriter` discipline (a new `forge.publication.v0\0` domain tag, length-prefixed, injective) folding the deciding evidence rows' Phase 5 `content_hash`es plus the decision digest plus the canonical gate outcomes. There is exactly **one** digest trailer line (no separate `Forge-Evidence-Digest`/`Forge-Publication-Digest` split — they would carry the same value). Gate identities encoded into the `Forge-Gates` line are secret-redacted per-token (`redact_secret_like_text`) before being written, since the commit message is a published egress.
- **R7.** A verification step **recomputes the digest from the local ledger** and confirms the published trailer matches: a `forge export verify-branch <name>` command (and a store helper) parses the trailer from the published commit message, re-reads the deciding evidence/decision rows, recomputes the provenance digest, and **fails closed** with a new typed `PROVENANCE_MISMATCH` error (non-zero exit) when the recomputed digest does not match the published one. A clean match returns a success envelope. **What a PASS proves (and does not):** a match confirms the published commit's trailer is consistent with the **current local ledger** — it detects a rewritten commit message, or a ledger row edited (naively) without re-export. It does **not** prove ledger authenticity: an attacker with DB write access who rewrites the deciding rows (re-hashing them) **and** re-exports still passes (same cheap-check boundary as R4; cross-machine/authenticity is Phase 9 signing). The Phase-6 consumer is therefore **producer-side**: the same machine re-verifies its own export before/after opening a PR, and a human reading the published commit gets a forward-compatible breadcrumb (proposal id, gates, actor); a cross-machine verifier is Phase 9.
- **R8.** Assembling the trailer **re-verifies the deciding evidence** (pulling Phase 5 deferred D3 forward, lightly): a deciding evidence row that fails the per-row check is refused at export (`EVIDENCE_TAMPERED`) **before** the branch/trailer is created, so a published "passing-check" provenance cannot rest on a **naively-tampered** row (the fully-re-hashed-row boundary of R4 still applies — that is `doctor`'s op-walk / Phase 9's job).
- **R9.** PR-body generation cites the **competing attempts against the declared intent** (using the compare output), fixing the single-latest-evidence under-reporting in `pr_body_for`. All changed paths and identities in the PR body remain secret-redacted; export stays one-way to a local git branch (no remote).
- **R10 (D6 — observability).** `forge_policy::GateResult` serializes a `structured_failures: Option<u64>` field (the parsed failure count of the deciding evidence), so a consumer can distinguish "failed on exit code" from "failed on parsed count" from `check --json` and from the compare output. Additive serde field; not a DB column.
- **R11 (per-gate persistence decision).** Phase 6 **recomputes** per-gate outcomes in the compare/trailer paths via the policy engine over verified evidence; it does **not** persist a per-gate-verdict column. This is the deliberate resolution of the Phase 4 "emit-don't-persist" decision (see Key Technical Decisions): a persisted verdict is a stale cache that could mask post-check tampering, and recompute keeps the schema honest (no migration).
- **R12 (carry-over invariants — additive only).** Preserve WAL + `BEGIN IMMEDIATE` + busy/517 retry, advisory-lock acquire-once-never-nested + the lock-free `run` carve-out, store-before-DB ordering, the in-txn determining read, the additive-error drift-guard discipline, the Phase 4 verdict rule + fail-closed enforcement, the Phase 5 tamper-evidence, per-token/every-egress secret redaction, the 4096-byte excerpt cap, the shared `forge_content::is_ignored_by_policy` in both backends, and the export secret-deny default. The new error code rides the full drift-guard fan-out; `schema_version` stays `forge.cli.v0`.
- **R13 (scope).** LOCAL only: no cross-machine provenance, no ledger sync, no wire protocol, no signing (Phase 9). No native diff (Phase 8). No 3-way merge, real GC, or physical per-attempt worktrees (Phase 8). No configurable scoring DSL.

**Origin actors (wedge brainstorm):** A1 Human developer (compares candidates, chooses a winner — F4), A2 Local coding agent (chains `compare → accept` headlessly on verified data — F3), A3 Forge CLI (resolves context, returns stable JSON).
**Origin flows honored:** F4 (human compares candidates — R18/R19/R20), F3 (agent acts with explicit context — `--attempt`/`--intent`, echoed IDs). The lifecycle `compare` slots between `check` and `accept`; the trailer hardens `export`.

### Integrity Scope (Honesty Note — mirrors Phase 5's tamper-evident-not-proof boundary)

Compare/rank, the trailer assembly, and `verify-branch` all use the **cheap per-row** integrity check (`verify_evidence_integrity`: recompute the row's digest from its columns, compare to the stored `content_hash`). This catches the **naive and interior** edit — the literal Phase 4 honesty-note hole (`exit_code 7→0` leaving the hash stale), the exact attack the wedge must defeat. It does **not** catch a DB-write attacker who edits a row **and recomputes that row's own `content_hash`** to match — that "recompute-row-hash" tamper is caught only by `doctor`'s full op-log re-walk (which folds the *old* domain digest into each op link) and, ultimately, by Phase 9 signing. So the honest guarantee Phase 6 ships, stated without overclaim:

- A tampered attempt detected by the cheap check is **never** ranked/green and is **refused** at export — fail-closed (R4/R8).
- A `verify-branch` PASS confirms the published trailer is consistent with the **current local ledger**; it is **not** an authenticity proof against a co-rewritten ledger+re-export (R7).
- For the **deep** check (recompute-row-hash, deletion, reorder, head-truncation), the contract directs the operator to `doctor` (the existing Phase 5 op-walk). Adding a doctor-grade op-link verification to the compare/accept *path* (vs. relying on a separate `doctor` run) is the Phase 5 deferred follow-up **D2**, intentionally **not** pulled into Phase 6 — Phase 6 keeps Phase 5's deliberate cheap-gate / deep-doctor split rather than re-deriving a stronger guarantee at the selection surface. This boundary is surfaced in the machine contract (`forge schema` `notes.provenance`).

---

## Scope Boundaries

### In Scope

- A store-level compare/rank API + `forge attempt compare` / `forge compare` CLI surface (R1–R3).
- Verify-then-rank: ranking on Phase 5 cheaply-verified evidence; cheap-check-tampered attempts flagged `tampered` and unranked (`rank: null`) (R4; deep recompute-row-hash is `doctor`'s — Integrity Scope).
- A git-adapter file/hunk content diff between two competing proposals (R5).
- A structured provenance trailer on the published commit + a local recompute-and-confirm verification step + `PROVENANCE_MISMATCH` (R6, R7).
- Re-verify deciding evidence at export (R8, pulls Phase 5 D3 forward lightly).
- Compare-driven PR-body generation (R9).
- `GateResult.structured_failures` (R10, Phase 5 D6).

### Deferred for Later (not Phase 6)

- **Cross-machine provenance / ledger sync / wire protocol / signed attestation → Phase 9.** The trailer is a self-verifying *local* pointer; team-scale trust requires the wire protocol and signing.
- **Native content diff with rename detection → Phase 8.** Phase 6 uses the pragmatic git-adapter diff via `git_tree_for_content_ref`; native diff removes the git interop dependency but is not needed for the compare surface.
- **3-way merge engine, real mark-sweep GC, physical per-attempt worktrees → Phase 8.**
- **A configurable scoring DSL / weighted ranking.** The default ranking is a simple, explainable total order; raw evidence is always returned so a caller can re-rank itself.
- **Persisted per-gate-verdict history column.** Recompute is the chosen path (R11); a persisted column with a `gc`-aware retention story is a future option, not Phase 6.

### Deferred to Follow-Up Work (Phase 6-adjacent, not this PR)

- Phase 5 D1 (`integrity_marker` hardening), D2 (`accept` walks the deciding evidence op-link in-txn), D4/D5 residuals — remain Phase 9 companions; Phase 6 pulls in **only** D3 (verify deciding evidence at export, via R8) and D6 (R10).
- A pairwise N-way diff matrix (all attempt pairs) — Phase 6 ships per-attempt-vs-base diff summaries + a two-proposal pairwise diff; an all-pairs matrix is additive later.

---

## Context & Research

### Relevant Code and Patterns

- **Listing/metadata surface (what compare replaces/extends):** `crates/forge-store/src/lib.rs` — `list_attempts` (returns `AttemptSummary`: id/intent/base_head/status/attached, scoped to the whole repo), `proposals_for_attempt` (the `ProposalSummary` builder with `content_ref`/`changed_paths` from `proposal_revisions`), `proposal_metadata_for_attempt` (builds `ProposalMetadata` with `check_status`/`decision_status`/`publication_status` strings — the nearest existing per-proposal rollup), `list_proposals`, `show_attempt`. `AttemptRecord` carries `intent_id` (the grouping key compare needs).
- **Gate engine (per-gate verdicts + structured metrics):** `crates/forge-policy/src/lib.rs` — `evaluate(spec, snapshot_id, &facts) -> CheckOutcome`, `EvidenceFact` (already carries `structured_failures: Option<u64>`), `GateResult` (program/args/verdict/evidence_id/exit_code — **missing** `structured_failures`, this is D6/R10), `CheckOutcome { status, reason, gates }`, `GateVerdict`. Pure function, zero IO — re-runnable in the compare path.
- **Check evaluation + integrity verification (what compare reuses for verify-then-rank):** `crates/forge-store/src/lib.rs` — `evaluate_check_on(conn, attempt, proposal)` (loads spec via `intent_check_spec`, projects facts via `evidence_facts_on`, runs `forge_policy::evaluate`, then runs the **integrity gate** that calls `verify_evidence_integrity` per deciding gate and returns `EvidenceTampered` fail-closed); `evidence_facts_on` (projects `structured_failures` from `structured_json`); `verify_evidence_integrity(conn, evidence_id, marker) -> IntegrityStatus {Verified | LegacyUnverified | Tampered(kind)}`; `evidence_high_water`. Compare runs the **same** verification on a read connection (no lock — it does not write).
- **Decision + decision digest:** `decide` (persists `decisions.content_hash` = `integrity::decision_digest(...)` + actor), `DecisionRecord`, `verify_decision_integrity(cwd, proposal_revision_id)` (the verifying read pattern the trailer verify mirrors), `decision_for_proposal_revision`, `decision_high_water`.
- **Integrity digest module (reuse for the trailer):** `crates/forge-store/src/integrity.rs` — `DigestWriter` (`.str()/.i64()/.bool()/.str_slice()/.opt_str()/.finish()`, length-prefixed injective), domain tags `EVIDENCE_TAG`/`OPERATION_TAG`/`DECISION_TAG` (`b"forge.<kind>.v0\0"`), `GENESIS_PARENT_HASH`, `evidence_digest`/`decision_digest`/`operation_link_hash`, the golden-vector + `domain_tags_separate_record_kinds` tests.
- **Export path (where the trailer goes):** `crates/forge-cli/src/main.rs` `export_response` → `ExportCommand::Branch` arm: resolves the proposal, checks `decision_for_proposal_revision == "accepted"`, calls `verify_decision_integrity`, stale-base pre-check, then `forge_export_git::export_branch(cwd, name, base_head, current_head, content_ref, "Forge accepted proposal")` and `record_publication`. `crates/forge-export-git/src/lib.rs` `export_branch(..., message)` → `git_tree_for_content_ref` (the git/forge-tree resolver), `filter_secret_paths_from_tree`, `forge_content_git::create_branch_from_git_tree(repo_root, branch, base_commit, tree, message)` (`git commit-tree tree -p base -m message`). The `git(repo_root, &[&str])` helper is the runner to build `diff_trees` on.
- **PR body:** `pr_body_for(cwd, attempt_id, proposal_id) -> (String, Vec<String>)` — renders intent + single latest evidence + changed paths (filtered via `forge_content::filter_secret_risk`) + check/decision/publication. R9 feeds the compare output here.
- **CLI command plumbing:** `crates/forge-cli/src/main.rs` — `Command` enum + `main` match; `AttemptCommand` enum (`Start`/`List`/`Show`/`Attach`) + `attempt_response`; `command_result(command, request_id, |cwd, request_id| -> (Option<op_id>, Value, warnings))` (the read-only template is `attempt list` / `show`); `resolve_actor`; `command_from_args` two-word set `matches!(.., "export"|"attempt"|"proposal")` — `forge attempt compare` is already covered by `"attempt"`; the top-level `forge compare` is a **leaf** command (no subcommand) and must **not** be added to this set, or its error envelope would mislabel the command as `"compare <arg>"`; `requires_repo_lock`/`is_mutating_command` (compare is **neither** — read-only); `schema.rs` `command_shapes()` registry.
- **Typed-error contract:** `crates/forge-store/src/error.rs` — `ForgeError`, `code()/details()/retryable()/after_ms()/Display`, `error_registry()` + `ErrorCodeSpec`, drift-guard tests `registry_covers_every_variant` (the `all` array **and** the exhaustive `match`) + `codes_match_the_pre_change_registry` + `attempt_worktree_mismatch_details_carry_only_ids`. Mirror in `crates/forge-cli/tests/forge_schema.rs` `FORGE_ERROR_CODES` (21 codes today → 22 after adding `PROVENANCE_MISMATCH`). `schema.rs` `notes.integrity`/`notes.secret_protection`.
- **Tests + eval:** `crates/forge-cli/tests/forge_attempts.rs` (`competing_attempt_loop_exports_selected_proposal` — the canonical two-attempts-under-one-intent fixture; `attempt_start_lists_and_shows_competing_attempts`), `forge_accept_export.rs` (`prepare_proposal`; the `content_ref.strip_prefix("git-tree:")` assertion), `common/mod.rs` (`TestRepo::new_git`, `repo.forge()`). `scripts/e2e-eval.sh` (`ck`/`ckc`/`pg`/`F`/`mkrepo`/`db` helpers; blocks slot after the LIFECYCLE / CHECK-GATES blocks). `scripts/ci.sh` runs fmt/test/clippy then the eval.

### Institutional Learnings (carry-over invariants honored)

- **`tamper-evident-evidence-chain-and-failclosed-verification-2026-05-30.md` (Phase 5)** — §3/§4: rank only on verified evidence; the cheap per-row determining check (`verify_evidence_integrity`) catches the naive edit, `doctor`'s op-walk catches the recompute. Compare is **read-only**, so the in-txn rule does not mechanically apply (no write to bind to) and it takes **no** lock — but its ranking is a snapshot a lock-free `run` can invalidate, so it stays **advisory**; `accept`/`decide` remain the authoritative in-txn re-verified gate. The trailer digest reuses `DigestWriter` + a **new** domain tag (§4 + the implementation note); golden-vector + no-collision tests pin it. Recompute from the **persisted** (redacted) bytes so the trailer recomputes by construction.
- **`content-bound-gate-engine-and-failclosed-enforcement-2026-05-29.md` (Phase 4)** — §7 "emit speculative downstream data; do not persist it until a consumer exists" **names Phase 6 as the consumer**: the decision is explicitly handed to Phase 6, and the doc leans recompute over a speculative persisted column (R11). §4 redact per-token on **every** egress, success paths included — the compare JSON, the diff body, and the PR body are three new success-path egresses. §1 the verdict rule the compare per-gate results surface.
- **`write-binding-verification-and-content-backend-isolation-2026-05-29.md` (Phase 3)** — §6 the additive-error-on-every-surface checklist (`PROVENANCE_MISMATCH` follows it; `details` carries only opaque ids); §4 confine the git diff behind the export-git adapter (don't leak git into core); the compare operates on attempt-bound snapshot `content_ref`s — the binding Phase 3 made sound.
- **`schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md` (Phase 2)** — §4 typed error via `anyhow` + `downcast`, no writer signature changes, converted at the CLI layer; §5 "verify the production caller graph." (No migration this phase — but if a future reviewer argues for persistence, the head-bump fan-out discipline applies.)
- **`sqlite-multiprocess-concurrency-...` + `crash-correctness-advisory-lock-...` (Phase 1a/1b)** — the lock-free `run` carve-out is why compare's ranking is advisory; "a lock only helps when both racers take it" is why a read-only compare needs no lock and ranking is non-authoritative.

### External References

None required — this is internal Rust/SQLite work on mature local patterns (5 in-repo solution docs, `sha2` in-tree, git plumbing already used by the export path). `git diff --name-status` / `--numstat` / `git diff <treeA> <treeB>` are standard plumbing already adjacent to the export-git crate's existing `git()` calls. No new crates (the 4-day min-release-age gate is respected by adding nothing).

---

## Key Technical Decisions

- **Compare is read-only and advisory; `accept` stays authoritative.** Compare opens a throwaway connection, takes **no** advisory lock (it never writes), and re-verifies each attempt's deciding evidence with the Phase 5 per-row check. Its ranking is a snapshot a concurrent lock-free `run` can invalidate, so it is explicitly **advisory** — it informs selection, it does not gate it. The authoritative gate stays `decide`/`accept`'s existing in-txn re-evaluation (NER-132 U2 TOCTOU closure). A green compare ranking must never substitute for that.
- **Verify-then-rank, fail-closed (R4).** For each attempt, recompute the check via the same `evaluate_check_on`-style path on the read connection; that path **already** raises `EvidenceTampered` when a deciding row fails the per-row check. Compare catches that and records `integrity: "tampered"` for the attempt instead of propagating the error (one bad attempt must not blank the whole comparison), and the tampered attempt is **unranked** (`rank: null`) — *not* merely "ranked last" — so a headless consumer that selects by numeric-minimum `rank` cannot accidentally pick a tampered attempt even within an all-tampered group. `rank_reason` names the tamper, and `accept`/`decide` independently re-verify in-txn (the authoritative backstop). `legacy_unverified` rows rank normally but carry a warning, matching the gate's grandfathering. (Scope of "tampered" here is the cheap per-row check — see Integrity Scope.)
- **No migration — recompute, don't persist (R11).** This is the deliberate resolution of Phase 4 §7. Per-gate outcomes and structured metrics are recomputed from existing `evidence`/`intents` rows each time compare runs, **because** compare must re-verify integrity live anyway — a persisted verdict computed at check-time is a stale cache that a later tamper would not invalidate. Consequence: Phase 6 adds **no** `.sql` migration, no `schema_head` bump, and therefore none of the head-bump literal fan-out. (Stated explicitly so a reviewer does not expect a `005_*.sql`.)
- **D6 / `GateResult.structured_failures` is an in-memory serde field (R10), not a column.** `verdict_for` already reads `fact.structured_failures` to decide a structured gate; it just doesn't carry it onto the `GateResult`. Adding the field is additive to the emit-only struct and surfaces on both `check --json gates[]` and the compare output.
- **Trailer digest = a new `forge.publication.v0\0` domain tag (R6).** The trailer is a **new aggregate record kind** (it bundles proposal_id, proposal_revision_id, the deciding evidence `content_hash`es, the decision digest, and the gate outcomes), so it gets its own domain tag per the `integrity.rs` "an evidence digest can never be confused with a decision/op digest" rule — not a reuse of an existing tag, and not an ad-hoc `format!`+sha256. The "content-addressed evidence digest" specifically folds the deciding evidence rows' **Phase 5 `content_hash`es** (length-prefixed list) so it is recomputable from the ledger and changes if any deciding evidence row is edited. Pinned by a golden-vector test and a no-collision-with-existing-tags test.
- **Trailer encoding in the commit message = git trailer lines, ONE digest line (R6/R7).** The commit message body carries human text plus machine trailer lines: `Forge-Proposal-Id:`, `Forge-Proposal-Revision-Id:`, `Forge-Provenance-Digest:` (the single content-addressed digest), `Forge-Decision-Actor:`, and `Forge-Gates:`. There is exactly **one** digest line — an earlier draft listed both `Forge-Evidence-Digest` and `Forge-Publication-Digest` carrying the same value, which is dead/ambiguous output; the struct field, the `publication_digest()` function, the rendered key, and the `verify-branch` parse all use the single name `Forge-Provenance-Digest` (`provenance_digest`). Verification re-parses these from `git show -s --format=%B`. Git trailers are a stable, greppable convention and survive `commit-tree` verbatim. `Forge-Gates` is encoded so it parses single-pass even with multi-word argv (settle the exact encoding in U5 against the round-trip test).
- **One new typed error code: `PROVENANCE_MISMATCH` (R7).** Raised by `verify-branch` when the recomputed provenance digest ≠ the published `Forge-Provenance-Digest`. Distinct from `EVIDENCE_TAMPERED` (which is "a specific ledger row failed its per-row check") because a trailer mismatch is "the published artifact's provenance no longer matches the local ledger" — a different machine-actionable fact (the commit may have been rewritten, or a ledger row edited without re-export). `details` carries only opaque ids + the published and recomputed digests (no excerpts/paths), pinned by a `provenance_mismatch_details_carry_only_ids` test. Full additive drift-guard fan-out (the `FORGE_ERROR_CODES` count goes 21 → 22). Compare itself needs **no** new error code (multiple intents are grouped, not an error; unknown selectors reuse `UNKNOWN_INTENT`/`UNKNOWN_ATTEMPT`).
- **Compare's per-call cost is bounded by default (R5/R11).** Recompute-don't-persist means each `forge compare` re-runs, per attempt: the policy evaluation, the per-row integrity recompute(s), and a **file-level** git diff (`--name-status`/`--numstat`) of the proposal tree vs base. The expensive per-file **hunk body** is therefore **not** emitted by default — it is produced only on an explicit pairwise/`--diff` path — so a bare `forge compare` over many intents does not fan out into an unbounded per-file `git diff` subprocess storm. A scale test (a repo with several intents × attempts) asserts the default output stays bounded; if measured cost warrants, scoping the no-selector default to active intents is a cheap follow-up (not v0).
- **Diff via the existing `git_tree_for_content_ref` chokepoint (R5).** `diff_trees` resolves both proposals' `content_ref`s to git tree hashes through the one resolver the export path already uses (handles `git-tree:` directly and `forge-tree:` via `synthesize_git_tree`), then runs `git diff --name-status` + `--numstat` (file granularity, authoritative structured output) and, for the bounded hunk body, `git diff <treeA> <treeB> -- <path>`. Secret-risk paths are dropped before any hunk is read (`is_secret_risk_path`), and hunk text is redacted (`redact_evidence_excerpt`) and bounded — the diff body is captured-output-shaped and must not leak.
- **Re-verify deciding evidence at export (R8).** Trailer assembly recomputes the check (via the integrity-verifying path), so a tampered deciding evidence row raises `EVIDENCE_TAMPERED` **before** the branch is created — pulling Phase 5 D3 forward, so a published "passing-check" provenance can never rest on tampered evidence.

---

## Open Questions

### Resolved During Planning

- **Compare scope unit?** → Per-intent groups. `forge compare` returns every intent with ≥1 attempt as a ranked group; `--intent`/`--attempt` filter. No ambiguity error code.
- **Persist per-gate verdicts or recompute?** → Recompute (no migration). A persisted verdict is a stale cache that defeats verify-then-rank.
- **New domain tag for the trailer, or reuse?** → New `forge.publication.v0\0` (a new aggregate kind), with golden-vector + no-collision tests.
- **Trailer verify: error or diagnostic boolean?** → Fail-closed typed error (`PROVENANCE_MISMATCH`, non-zero exit) so `verify-branch` is usable as a **producer-side** gate; clean match → success envelope. A PASS proves trailer↔current-ledger consistency, not authenticity (Integrity Scope).
- **Does compare take the advisory lock?** → No. It is read-only; ranking is advisory; `accept` stays authoritative.
- **Diff granularity / source?** → Default per-attempt summary is **file-level** `name-status` + `numstat` (structured, authoritative, no hunk subprocess); the bounded redacted **hunk body** is produced only on an explicit pairwise/`--diff` path — all via the git adapter through `git_tree_for_content_ref`. Native diff is Phase 8.
- **Tampered/no-proposal rank value?** → `rank: null` (unranked), not "ranked last", so a numeric-min consumer cannot select it.
- **Ranking metric inputs (v0)?** → Test counts only in the order; clippy/lint findings returned in `metrics` but not weighted (R3 narrowing; callers re-rank on raw metrics).

### Deferred to Implementation

- **Exact ranking tiebreak fields** beyond the primary tiers (gates-passing → fewer failures → more passing): settle the final tiebreak (`base_head` recency vs `attempt_id` lexical) in U3 against a deterministic-order test; it must be total and stable.
- **Exact git trailer key names + multi-value encoding for `Forge-Gates`** (one line vs repeated lines per gate): settle in U6 against the parse/recompute round-trip test; keep it greppable and `%(trailers)`-compatible.
- **How `diff_trees` bounds a large hunk body** (per-file byte cap vs total cap; whether to emit hunks at all when a file is binary): settle in U2; default to a per-file cap reusing `EXCERPT_LIMIT` semantics and a `truncated` flag.
- **Whether compare's per-attempt diff summary is vs `base_head` or vs the prior snapshot:** default to vs `base_head` (the proposal's declared base) so the summary is "what this attempt changes against the intent's baseline"; confirm in U3.

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

**Compare/rank (read-only, verify-then-rank):**

```
compare(cwd, selector) -> AttemptComparison:
  conn = open_connection(db)                      # no lock; read-only
  intents = resolve_intents(selector)             # all, or filtered by --intent/--attempt
  for intent in intents:
    attempts = attempts_for_intent(conn, intent)
    rows = []
    for a in attempts:
      proposal = latest_proposal(conn, a)          # may be None -> attempt has no proposal yet
      facts    = evidence_facts_on(conn, a)        # existing projection (carries structured_failures)
      spec     = intent_check_spec(conn, a.intent_id)
      outcome  = forge_policy::evaluate(spec, proposal.snapshot_id, facts)   # per-gate verdicts
      integrity = verify_each_deciding_row(conn, outcome, marker)            # Phase 5 per-row check
        # -> Verified | LegacyUnverified | Tampered(kind)   (Tampered is recorded, NOT propagated)
      metrics  = structured_metrics(facts, proposal.snapshot_id)             # parsed test/lint counts
      diff_sum = diff_summary(content_ref(proposal) vs base_head)            # FILE-LEVEL (name-status+numstat); no hunk body by default (U2)
      rows.push(AttemptCompareRow { a, proposal, outcome.gates(+structured_failures),
                                    metrics, integrity, decision_status, publication_status,
                                    changed_paths(redacted), diff_sum })
    rank(rows)                                      # deterministic total order; tampered rows get rank: null (R3/R4)
  return AttemptComparison { intents: [...] }       # raw evidence always present alongside rank
```

**Default ranking (deterministic, explainable):**

| tier key (in order)                       | direction        |
|-------------------------------------------|------------------|
| integrity tampered (cheap check)          | **unranked (`rank: null`)** — never assigned a numeric rank |
| integrity verified AND all required gates passing | true first |
| parsed test failures                      | fewer first      |
| parsed test passing                       | more first       |
| stable tiebreak (base_head recency, then attempt_id) | deterministic |

**Provenance trailer (assemble at export, recompute to verify):**

```
build_publication_trailer(cwd, proposal) -> Trailer:           # at export, before the branch
  outcome   = evaluate_check_on(conn, attempt, proposal)        # raises EVIDENCE_TAMPERED (R8) fail-closed
  deciding  = [g.evidence_id for g in outcome.gates if g.evidence_id]   # the gate-deciding rows
  ev_hashes = [content_hash(conn, id) for id in deciding]       # Phase 5 per-evidence hashes
  dec_digest = decision_digest_of(conn, proposal.revision)
  prov_digest = integrity::publication_digest(                 # new forge.publication.v0\0 tag
                 proposal_id, revision_id, ev_hashes(len-prefixed list),
                 dec_digest, gate_outcomes, actor)
  -> Trailer { proposal_id, revision_id, provenance_digest = prov_digest, actor, gates }

# commit message body (passed to export_branch instead of "Forge accepted proposal"):
#   Forge accepted proposal <proposal_id>
#   <intent text>
#
#   Forge-Proposal-Id: <id>
#   Forge-Proposal-Revision-Id: <id>
#   Forge-Provenance-Digest: <prov_digest>          # ONE digest line (no Evidence/Publication split)
#   Forge-Decision-Actor: <actor>
#   Forge-Gates: cargo test=passed; cargo clippy=passed   # gate identities redacted per-token (R6)

verify_branch(cwd, name):                                       # recompute from the LOCAL ledger
  published = parse_trailers(git show -s --format=%B <name>)
  recomputed = build_publication_trailer(cwd, resolve(published.proposal_id)).provenance_digest
  if recomputed != published.provenance_digest:
      Err(PROVENANCE_MISMATCH { proposal_id, published, recomputed })   # fail-closed, non-zero
  else Ok({ verified: true, proposal_id, provenance_digest })
  # A PASS = "published trailer consistent with the CURRENT local ledger" — NOT authenticity (R7 / Integrity Scope)
```

---

## Implementation Units

Grouped into four phases. U-IDs are stable.

### U1. `GateResult.structured_failures` (D6 / R10)

**Goal:** Carry the deciding evidence's parsed failure count onto the per-gate result so consumers can distinguish an exit-code failure from a parsed-count failure, on both `check --json` and the compare output.

**Requirements:** R10.

**Dependencies:** None.

**Files:**
- Modify: `crates/forge-policy/src/lib.rs` — add `structured_failures: Option<u64>` to `GateResult`; set it in `verdict_for` from the deciding `fact.structured_failures` (the latest-on-snapshot fact, else `None`).
- Test: `crates/forge-policy/src/lib.rs` unit tests.

**Approach:** Additive serde field (already `#[derive(Serialize)]`). Populate from the same `latest_on_snapshot` fact the verdict is decided from; `None` when the gate is `missing`/`stale` or no parser matched. Default-mode synthesized gates also carry it. No behavior change to verdicts or rollup.

**Patterns to follow:** the existing `GateResult` construction in `verdict_for`; the `fact_with_failures` test helper.

**Test scenarios:**
- Happy path: a passing structured gate (`Some(0)`) → `GateResult.structured_failures == Some(0)`.
- Edge case: a failing structured gate (`Some(2)`) → `Some(2)`; an exit-code-only gate over evidence with parsed failures → still carries the parsed count for observability.
- Edge case: `missing`/`stale` gate → `None`; no-parser evidence → `None`.
- Edge case: serde round-trip — `GateResult` serializes the new field; existing `check --json gates[]` shape is a superset (additive).

**Verification:** `forge_policy` unit tests green; `GateResult` JSON carries `structured_failures`; no verdict/rollup change.

---

### U2. Git-adapter tree-vs-tree content diff (`diff_trees`)

**Goal:** A file/hunk-granularity content diff between two proposals' content_refs, produced via the git adapter, secret-safe and bounded — the diff primitive compare and the pairwise diff both use.

**Requirements:** R5, R12.

**Dependencies:** None (uses existing git plumbing).

**Files:**
- Modify: `crates/forge-export-git/src/lib.rs` — add `diff_trees(repo_root, content_ref_a, content_ref_b) -> Result<TreeDiff>` plus the `TreeDiff`/`FileDiff` serde structs; resolve each ref via the existing `git_tree_for_content_ref`; build on the existing `git(repo_root, &[&str])` runner.
- Test: `crates/forge-export-git/src/lib.rs` unit tests (build two trees via the existing `build_tree` test helper, diff them).

**Approach:** Resolve both refs to git tree hashes through `git_tree_for_content_ref` (git-tree direct, forge-tree via `synthesize_git_tree`). Run `git diff --name-status <a> <b>` (per-file status: A/M/D/R) and `git diff --numstat <a> <b>` (insertions/deletions) — this **file-level** result is the default compare summary. The **hunk body** (`git diff <a> <b> -- <path>`) is produced only when the caller requests it (the pairwise/`--diff` path), not for every file on every compare (cost — see Key Technical Decisions). Drop secret-risk paths (`forge_content::is_secret_risk_path`) **before** reading any hunk; **binary files emit no hunk body** (a `binary` flag instead — the line-oriented redactor can't sanitize binary bytes). When a hunk is read, redact via `forge_content::redact_evidence_excerpt` and bound to a per-file cap (reuse the `EXCERPT_LIMIT` value — `diff_trees` lives in `forge-export-git`, which does not depend on `forge-evidence`, so define a local `const` mirroring it rather than importing) with a `truncated` flag. `TreeDiff` carries the ordered `FileDiff` list (`{path, status, insertions, deletions, binary, hunk: Option<String>, truncated}`) + the dropped-secret list (becomes a warning). Keep all of it inside the export-git adapter (no git leak into core). (Note: the ROADMAP Phase 6 feature list explicitly asks for "file/**hunk** granularity"; this **overrides** the wedge brainstorm's R20 "must not require file-level diff" deferral — the brainstorm scoped the *first* slice, Phase 6 is the diff slice.)

**Patterns to follow:** `git()` runner and `git_tree_for_content_ref`/`filter_secret_paths_from_tree` in the same file; the `is_secret_risk_path` drop idiom; the `EXCERPT_LIMIT` cap + `*_truncated` flags from evidence capture.

**Test scenarios:**
- Happy path: two trees differing in one file → one `FileDiff` with status `M`, correct insertions/deletions, a non-empty hunk.
- Happy path: a file added in B / deleted in B → status `A` / `D`.
- Edge case: identical trees → empty `FileDiff` list.
- Error/secret path: a tree containing `.env`/`certs/server.pem` → those paths are **dropped** from the diff and reported in the dropped list; no hunk is read for them.
- Edge case: a large file change → hunk bounded to the cap with `truncated: true`.
- Edge case: a secret-shaped token inside a hunk body → redacted in the emitted hunk; Forge's own 40/64-hex SHAs are not mangled (the evidence redactor's allowlist).

**Verification:** `diff_trees` returns correct per-file status + line counts + bounded redacted hunks for two real trees; secret-risk paths never appear; the function lives entirely in the export-git adapter.

---

### U3. Store-level compare/rank API (`compare_attempts`)

**Goal:** The read-only compare/rank engine: per attempt under one intent, return changed paths/diff summary, per-gate verified check results, structured metrics, integrity status, and a deterministic rank — ranking on cheaply-verified evidence (tampered → unranked), raw evidence always alongside.

**Requirements:** R1, R3, R4, R11, R12.

**Dependencies:** U1 (structured_failures), U2 (diff summary).

**Files:**
- Modify: `crates/forge-store/src/lib.rs` — add `compare_attempts(cwd, selector: CompareSelector) -> Result<AttemptComparison>` and the serde result structs (`AttemptComparison { intents: Vec<IntentComparison> }`, `IntentComparison { intent_id, intent, attempts: Vec<AttemptCompareRow> }`, `AttemptCompareRow { attempt_id, status, proposal: Option<{proposal_id, proposal_revision_id, snapshot_id, base_head}>, changed_paths, diff_summary, gates: Vec<GateResultView>, metrics: StructuredMetrics, integrity: IntegrityLabel, decision_status: Option<String>, publication_status: Option<String>, rank: Option<u32>, rank_reason }`); helpers `attempts_for_intent`, `intents_with_attempts`, `structured_metrics_for`, `rank_rows`, and a read-only `verify_each_deciding_row` wrapper over `verify_evidence_integrity`. `rank` is `Option<u32>` so a tampered (or no-proposal) row is `None` — unrankable, not numerically selectable. `decision_status`/`publication_status` come from the same per-proposal rollup `proposal_metadata_for_attempt` already computes.
- Test: `crates/forge-store/src/lib.rs` unit tests for `rank_rows` (pure ranking) + `crates/forge-cli/tests/forge_compare.rs` (integration, U7-adjacent).

**Approach:** Open a throwaway read connection (no lock). Resolve intents from the selector (all intents with ≥1 attempt; `--intent` filters; `--attempt` maps to its intent). For each attempt: resolve its latest proposal (`proposals_for_attempt().pop()`; `None` → an attempt with no proposal, rendered with empty gates/`unranked`); project facts via `evidence_facts_on`; load the spec via `intent_check_spec`; run `forge_policy::evaluate` to get per-gate verdicts (now carrying `structured_failures`); run `verify_each_deciding_row` and label the attempt `verified`/`legacy_unverified`/`tampered` (Tampered is **recorded, not propagated** — a bad attempt must not blank the comparison); compute `structured_metrics` (sum/extract parsed test passed/failed/ignored + clippy findings from the deciding-snapshot evidence); compute the file-level `diff_summary` via U2 (`content_ref` vs `base_head`'s tree, no hunk bodies); redact `changed_paths` via `filter_secret_risk`. Then `rank_rows`: a deterministic total order over the **rankable** rows — verified-and-all-required-gates-passing first; within a tier, fewer parsed failures, then more parsed passing, then a stable tiebreak; **tampered rows (and no-proposal rows) get `rank: null`** (unranked, never assigned a number) with a `rank_reason` naming why. The metric tiebreak uses **test counts only** (clippy findings stay in `metrics`, not the order — R3 v0 narrowing). Raw per-gate + metrics are always present.

**Execution note:** Start with a failing `rank_rows` unit test that pins the exit-criterion ordering (a passing-gates verified attempt ranks above a failing one; a tampered attempt — even with stored exit 0 — is `rank: null`, never numerically below-but-selectable) before wiring the IO.

**Patterns to follow:** `proposal_metadata_for_attempt` (the per-proposal rollup it generalizes); `evaluate_check_on`'s spec+facts+evaluate+verify sequence (compare runs the same on a read conn, swallowing the tamper into a label); `forge-policy`'s pure-function + exhaustive-unit-test style for `rank_rows`; `filter_secret_risk` for changed paths.

**Test scenarios:**
- Covers AE7. Happy path: two attempts under one intent, both proposed, one with a passing `cargo test` gate and one with a failing one → both rows returned with correct gates/metrics/changed_paths; the passing attempt ranks first; `rank_reason` explains both.
- Happy path (metrics): an attempt whose evidence parsed `{passed:50, failed:0}` outranks one with `{passed:48, failed:2}` (both gates otherwise equal).
- Happy path (origin R19): an already-accepted / already-exported attempt surfaces `decision_status: "accepted"` / `publication_status: "published"` so a selector can see decided work.
- Load-bearing (R4): an attempt whose deciding evidence row is tampered (mutated exit_code) → `integrity: "tampered"`, `rank: null` — never in the passing tier and never numerically selectable, even though its stored exit_code is 0. A second untampered failing attempt is the rank-1 winner.
- Load-bearing (R4, all-tampered group): a group whose **only** proposed attempt is tampered yields **no** `rank: 1` row — a numeric-min chainer cannot select a tampered attempt.
- Edge case: an attempt with no proposal → rendered with empty gates, `rank: null`, no diff; does not error the comparison.
- Edge case: a `legacy_unverified` deciding row → ranked normally with an `integrity: "legacy_unverified"` label (no brick).
- Edge case: secret-risk changed path (`.env`) → dropped from `changed_paths`, surfaced as a warning count.
- Scale (cost): a repo with several intents × attempts returns a bounded default comparison with no per-file hunk subprocess fan-out (file-level summary only); compare completes without a hunk-body `git diff` per file.
- Determinism: the same repo state yields byte-identical compare JSON across runs (stable ordering); `rank_rows` is a total order over rankable rows (no ties left unbroken).

**Verification:** Compare returns per-attempt diffs + per-gate results (with structured_failures) + structured metrics + decision/publication status + a deterministic ranking; cheap-check-tampered attempts are unranked (`rank: null`); raw evidence is always present; output is stable and secret-redacted.

---

### U4. CLI `forge attempt compare` / `forge compare`

**Goal:** Expose the compare/rank API as a stable read-only `--json` surface, keyed by `attempt_id`/`proposal_id`, for headless `compare → accept` chaining.

**Requirements:** R2, R12.

**Dependencies:** U3.

**Files:**
- Modify: `crates/forge-cli/src/main.rs` — add `AttemptCommand::Compare(CompareArgs)` + a top-level `Command::Compare(CompareArgs)` alias; `CompareArgs { intent: Option<String>, attempt: Option<String> }`; dispatch both to a shared `compare_response` calling `forge_store::compare_attempts`; add `"compare"` to the `command_from_args` two-word set if top-level-with-subcommand (here a flat `forge compare` + `forge attempt compare`, both leaf commands); surface dropped-secret warnings.
- Modify: `crates/forge-cli/src/schema.rs` — add `attempt compare` / `compare` to `command_shapes()`.
- Test: `crates/forge-cli/tests/forge_compare.rs` (drives the binary).

**Approach:** Read-only — use the `attempt list`/`show` `command_result` template (returns `None` operation_id, warnings for dropped secrets); **not** in `is_mutating_command`/`requires_repo_lock`. `forge compare` and `forge attempt compare` share one handler. Echo resolved `attempt_id`/`proposal_id` in the JSON so an agent can chain. The envelope stays `forge.cli.v0` (additive command).

**Patterns to follow:** `attempt_response`'s `AttemptCommand::List`/`Show` arms; `command_result` read-only usage; `secret_export_warnings` for the dropped-path warnings.

**Test scenarios:**
- Covers AE6/AE7. Happy path: two competing attempts → `forge compare --json` returns a grouped, ranked comparison; `forge attempt compare --intent <id>` filters to one intent; both forms produce the same data for one intent.
- Happy path: the JSON echoes `attempt_id` and `proposal_id` for each row (chainable to `accept --attempt --proposal`).
- Edge case: an unknown `--intent`/`--attempt` selector → the existing `UNKNOWN_INTENT`/`UNKNOWN_ATTEMPT` typed error (no new code).
- Edge case: a repo with one intent and no `--intent` → that intent's group; multiple intents and no selector → all groups (not an error).
- Contract: `forge schema --json` lists the new command; `schema_version` stays `forge.cli.v0`.

**Verification:** `forge compare` / `forge attempt compare` emit a stable, redacted, chainable comparison; read-only (no lock); schema lists it.

---

### U5. Provenance trailer digest (`integrity.rs`) + structured commit trailer at export

**Goal:** A new domain-separated publication digest folding the deciding evidence `content_hash`es + decision digest + gate outcomes, and a structured commit trailer that replaces the `"Forge accepted proposal"` constant — assembled at export and refusing tampered deciding evidence first.

**Requirements:** R6, R8, R12.

**Dependencies:** U1 (gate outcomes), U3 (the verified per-gate path it reuses).

**Files:**
- Modify: `crates/forge-store/src/integrity.rs` — add `const PUBLICATION_TAG: &[u8] = b"forge.publication.v0\0";`, `PublicationDigestInput`, and `publication_digest(input) -> String` (folds `proposal_id`, `proposal_revision_id`, a length-prefixed list of deciding evidence `content_hash`es, the decision digest, and a canonical gate-outcomes encoding); golden-vector + no-collision unit tests.
- Modify: `crates/forge-store/src/lib.rs` — add `build_publication_trailer(cwd, proposal_revision_id) -> Result<PublicationTrailer>` that re-runs the integrity-verifying check (raising `EvidenceTampered` for a tampered deciding row, R8), reads the deciding evidence `content_hash`es and the decision digest, and returns the `PublicationTrailer { proposal_id, proposal_revision_id, provenance_digest, actor, gates }` (one digest field, named `provenance_digest`); a `render_trailer_message(&PublicationTrailer, intent) -> String` producing the commit body with the `Forge-*` trailer lines — **one** `Forge-Provenance-Digest` line, and the `Forge-Gates` line with each gate's program/args run through `forge_content::redact_secret_like_text` per-token before encoding (R6 — the commit is a published egress).
- Modify: `crates/forge-cli/src/main.rs` — `export_response` Branch arm: build the trailer (after the existing decision-integrity verify, before `export_branch`), pass `render_trailer_message(...)` as the commit message instead of `"Forge accepted proposal"`.
- Test: `crates/forge-store/src/integrity.rs` unit tests; `crates/forge-cli/tests/forge_accept_export.rs` (the published commit message carries the trailer lines).

**Approach:** Reuse `DigestWriter` with the new tag; the evidence digest is a function of the **persisted** Phase 5 `content_hash`es (so it recomputes by construction). The gate-outcomes encoding is canonical (sorted `(program, args, verdict)` length-prefixed). The commit body keeps a human first line + intent text, then machine `Forge-*` trailer lines (git-trailer format, `%(trailers)`-compatible). Assembling the trailer re-verifies the deciding evidence (R8) — a tamper fails closed before the branch exists.

**Patterns to follow:** `integrity.rs` `decision_digest`/`evidence_digest` + the golden-vector and `domain_tags_separate_record_kinds` tests; `verify_decision_integrity`'s ledger-read shape; `export_branch`'s `message` parameter.

**Test scenarios:**
- Happy path: `publication_digest` is deterministic, 64-hex, and a pinned golden vector; changing any folded field (an evidence hash, the actor, a gate verdict) changes it.
- No-collision: `publication_digest` over comparable inputs differs from `evidence_digest`/`decision_digest`/`operation_link_hash` (domain-tag separation).
- Happy path: after `accept` + `export branch`, `git show -s --format=%B <branch>` contains `Forge-Proposal-Id`, `Forge-Proposal-Revision-Id`, a single `Forge-Provenance-Digest`, `Forge-Decision-Actor`, and `Forge-Gates`, and the digest matches the locally recomputed one (there is exactly one digest line).
- Error path (R8): a tampered deciding evidence row → `export branch` returns `EVIDENCE_TAMPERED` and **no** branch is created (assert the ref does not exist).
- Security (R6): a gate declared with a secret-shaped argv token (`--token=ghp_x`) renders that token as `[REDACTED]` in the `Forge-Gates` line (no secret in the committed message).
- Edge case: the actor recorded at accept flows into `Forge-Decision-Actor`; a default-mode intent (no declared gates) still produces a well-formed trailer (synthesized gate outcomes).

**Verification:** The published commit carries a structured, ledger-derived trailer instead of the constant message; the digest is domain-separated, golden-pinned, and recomputable; a tampered deciding row blocks export before the branch.

---

### U6. Trailer verification (`verify-branch`) + `PROVENANCE_MISMATCH`

**Goal:** A local verification step that parses the published trailer, recomputes the publication digest from the ledger, and confirms a match — fail-closed with a new typed error on mismatch.

**Requirements:** R7, R12.

**Dependencies:** U5.

**Files:**
- Modify: `crates/forge-store/src/error.rs` — add `ForgeError::ProvenanceMismatch { proposal_id, published_digest, recomputed_digest }`; `code()="PROVENANCE_MISMATCH"`, `retryable()=false`, `after_ms()=None`, `details()` carries only `{proposal_id, published_digest, recomputed_digest}` (opaque ids/hashes, no excerpts/paths), `Display`; append the `ErrorCodeSpec`; extend **both** drift-guard tests (`registry_covers_every_variant` `all` array **and** exhaustive match; `codes_match_the_pre_change_registry`); add a `provenance_mismatch_details_carry_only_ids` test.
- Modify: `crates/forge-cli/tests/forge_schema.rs` — `FORGE_ERROR_CODES += "PROVENANCE_MISMATCH"`.
- Modify: `crates/forge-store/src/lib.rs` — add `verify_publication_trailer(cwd, branch_or_commit) -> Result<TrailerVerification>` that reads the commit message (via the export-git adapter), parses the `Forge-*` trailers, recomputes `build_publication_trailer(...).provenance_digest` for the named proposal revision, and returns `Ok(TrailerVerification { verified: true, proposal_id, provenance_digest })` on match or `Err(ProvenanceMismatch{..})` on mismatch (and `EvidenceTampered` if a deciding row fails the per-row check, inherited from `build_publication_trailer`). A PASS proves trailer↔current-ledger consistency, **not** authenticity (R7 / Integrity Scope).
- Modify: `crates/forge-export-git/src/lib.rs` — a small `read_commit_message(repo_root, branch_or_commit) -> Result<String>` + `parse_forge_trailers(message) -> Trailers` helper (keeps git invocation in the adapter).
- Modify: `crates/forge-cli/src/main.rs` — `ExportCommand::VerifyBranch(args)` → `forge export verify-branch <name>` calling `verify_publication_trailer`; map `ProvenanceMismatch` via the existing `error_to_object` downcast (no new wiring).
- Modify: `crates/forge-cli/src/schema.rs` — add `notes.provenance` stating: the trailer is a **self-verifying LOCAL** pointer; a `verify-branch` PASS confirms the published trailer is consistent with the current local ledger (catches a rewritten commit message or a naively-edited ledger row) but is **not** an authenticity proof against a co-rewritten ledger+re-export (cross-machine verification + signing are Phase 9); the Phase-6 consumer is producer-side (re-verify own export, human-readable breadcrumb). Add `export verify-branch` to `command_shapes()`.
- Test: `crates/forge-store/src/error.rs` (drift + details), `crates/forge-cli/tests/forge_accept_export.rs`.

**Approach:** Pure read + recompute (no lock). The verification recomputes from the **local** ledger and compares to the published trailer — a self-verifying local pointer, **not** cross-machine transport, and **not** authenticity (state both in `notes.provenance`). Mismatch is fail-closed (non-zero exit) so the command is usable as a producer-side gate. `details` carries only ids + the two digests. Adding `PROVENANCE_MISMATCH` takes `FORGE_ERROR_CODES` from 21 to 22.

**Patterns to follow:** `ForgeError::EvidenceTampered`/`AttemptWorktreeMismatch` full additive-surface landing + `details_carry_only_ids`; `verify_decision_integrity`'s verifying-read shape; `error_to_object` downcast.

**Test scenarios:**
- Happy path: after `accept` + `export branch`, `forge export verify-branch <name> --json` → `verified: true`, `provenance_digest` matches the trailer; exit 0.
- Error path (exit criterion): mutate a deciding `evidence` row via raw SQLite after export, then `verify-branch` → `EVIDENCE_TAMPERED` (the recompute re-verifies the row, per-row check) — fail-closed, non-zero exit.
- Error path: rewrite the published commit's `Forge-Provenance-Digest` trailer to a wrong value (or recompute against a different proposal) → `PROVENANCE_MISMATCH` with `{proposal_id, published_digest, recomputed_digest}`; non-zero exit.
- Contract: `forge schema --json` lists `PROVENANCE_MISMATCH` and `export verify-branch`; `details` carries only ids/digests; both drift tests + `FORGE_ERROR_CODES` pass; `schema_version` stays `forge.cli.v0`.

**Verification:** `verify-branch` confirms a clean trailer and fails closed (typed, non-zero) on a tampered ledger row or a mismatched trailer; the new code lands on every contract surface.

---

### U7. Compare-driven PR-body + e2e eval + integration coverage

**Goal:** Feed the comparison into PR-body generation so it cites the competing attempts against the declared intent; extend the e2e eval with a compare/rank+provenance block; land the end-to-end integration tests proving the exit criteria.

**Requirements:** R9, R12, and the NER-137 exit criteria.

**Dependencies:** U3, U4, U5, U6.

**Files:**
- Modify: `crates/forge-store/src/lib.rs` — `pr_body_for` renders a "Competing Attempts" section from `compare_attempts` (scoped to the proposal's intent): each attempt's id, gate verdicts, metrics, integrity label, and rank, against the declared intent — replacing the single-latest-evidence under-report. Changed paths and identities stay secret-redacted.
- Modify: `scripts/e2e-eval.sh` — a `=== ATTEMPT COMPARE/RANK + PROVENANCE (NER-137) ===` block: init → start with a `--require-tests-pass "cargo test"` gate (the existing NER-136 `IntentArgs` structured-gate flag) → two rival attempts under one intent (mirror the `competing_attempt_loop` fixture), verify each, `forge compare`, assert per-attempt diffs + per-gate results + structured metrics + a deterministic ranking; `accept` + `export branch` the ranked winner headlessly; `verify-branch` recomputes the trailer; a `sqlite3`-gated tamper sub-check (mutate a deciding row → compare flags `tampered`/`rank: null` / `verify-branch` fails closed). Gate on `have_sqlite` where DB mutation is used.
- Test: new `crates/forge-cli/tests/forge_compare.rs` (the headline e2e: 2+ rival attempts, verify each, compare, assert diffs/gates/metrics/rank, export winner, verify trailer recomputes; the tamper-a-winner case); extend `crates/forge-cli/tests/forge_pr_body.rs` (PR body cites competing attempts).
- Modify: `crates/forge-cli/src/schema.rs` — keep `notes`/`command_shapes` consistent (already touched in U4/U6).

**Approach:** The e2e block is the binary-level proof of the full exit criteria; the `forge_compare.rs` integration test is the assert-rich Rust-level proof. Reuse the canonical two-attempts-under-one-intent fixture from `forge_attempts.rs`. The PR-body change reuses `compare_attempts` (no new query path) and keeps the existing secret-redaction.

**Execution note:** Start with the `forge_compare.rs` exit-criterion test (2+ rival attempts → compare asserts per-attempt diffs + per-gate results + metrics + deterministic ranking → export ranked winner → `verify-branch` recomputes) as a failing test driving U3–U6.

**Patterns to follow:** `competing_attempt_loop_exports_selected_proposal` (fixture); `forge_tamper.rs` (the `sqlite3` mutate-then-assert idiom); the e2e `ck`/`ckc`/`pg`/`F`/`db` helpers; `forge_pr_body.rs` assertion style.

**Test scenarios:**
- Covers AE7 (exit criterion): two rival attempts under one intent, each verified, `compare` returns per-attempt diffs + per-gate results + structured metrics + a deterministic ranking; the ranked winner `accept`+`export`s headlessly; `verify-branch` confirms the trailer recomputes from the ledger.
- Load-bearing: tamper a deciding evidence row of the would-be winner → compare surfaces it as `tampered` with `rank: null` (the honest attempt becomes the rank-1 winner); `verify-branch` of an exported tampered-ledger branch fails closed.
- Happy path (R9): the PR body for a proposal lists the competing attempts against the declared intent (not just one latest evidence row); secret-risk paths excluded.
- Integration (e2e eval): the new block passes end-to-end against the real `forge` binary inside `scripts/ci.sh`.
- Edge case: a single-attempt intent → compare returns one ranked row (rank 1), PR body still well-formed (no "competing" noise beyond the one attempt).

**Verification:** The e2e eval and `forge_compare.rs` prove every NER-137 exit criterion end-to-end; the PR body cites competing attempts; `bash scripts/ci.sh` is green.

---

## System-Wide Impact

- **Interaction graph:** compare/rank is a new **read-only** consumer of the existing spec+facts+evaluate+verify path (`intent_check_spec` → `evidence_facts_on` → `forge_policy::evaluate` → `verify_evidence_integrity`) and of the U2 git-adapter diff. The export path gains trailer assembly (re-using that same verifying check, R8) and a new verify command. No mutating command changes behavior; no new write path; no lock acquired by any new command.
- **Error propagation:** `ProvenanceMismatch` rides `anyhow::Error`, recovered by `downcast_ref` at the CLI — no writer signature changes; fail-closed, non-retryable. Compare **swallows** a per-attempt `EvidenceTampered` into an `integrity: "tampered"` + `rank: null` label (a bad attempt must not blank the comparison, and `rank: null` keeps it unselectable by a numeric-min consumer), while export/verify **propagate** it (a tampered deciding row must block the trust-bearing action). The tamper detected here is the cheap per-row check; the deep recompute-row-hash case is `doctor`'s op-walk (Integrity Scope).
- **API surface parity:** envelope stays `forge.cli.v0` (additive command + additive `GateResult` field + one additive error code). `forge schema` gains the new command + `notes.provenance`.
- **Schema:** **no migration** — Phase 6 reads existing `attempts`/`proposals`/`proposal_revisions`/`evidence`/`decisions` rows and embeds the trailer in the git commit (not the DB). `schema_head` stays 4; no head-bump fan-out.
- **Security:** three new success-path egresses (compare JSON, diff hunks, PR body) all run through the established redactors (`filter_secret_risk` on paths, `redact_evidence_excerpt` on hunks, per-token `redact_secret_like_text` on emitted identities); paths stay out of `anyhow` context; the export secret-deny default and the 4096 cap are preserved.
- **Unchanged invariants:** the Phase 4 verdict rule + fail-closed enforcement, the Phase 5 tamper-evidence + `--allow-unverified`-never-bypasses-tamper rule, the in-txn authoritative `accept` gate, WAL/IMMEDIATE/advisory-lock carve-outs, and both backends' `is_ignored_by_policy` are all preserved; compare/rank/trailer are strictly additive.

---

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Compare ranks a tampered attempt green (reintroduces the Phase 4 hole on the selection surface) | Verify-then-rank: re-run `verify_evidence_integrity` per deciding row; a tampered row → `integrity: "tampered"` + `rank: null` (unranked, not numerically selectable); load-bearing tests mutate a winner's exit_code and assert it is unranked, incl. an all-tampered group yielding no rank-1 (U3/U7) |
| Honesty gap: "ranks only on verified" / "never rests on tampered" overclaims the cheap check (a re-hashed row passes it) | Scope the claim (Integrity Scope note + R4/R7/R8): the cheap per-row check catches naive/interior edits; the recompute-row-hash case is `doctor`'s op-walk + Phase 9 signing; `verify-branch` PASS = trailer↔current-ledger consistency, not authenticity. Documented in `notes.provenance` (U6) |
| Compare's per-call cost unbounded (O(intents×attempts) recompute + git diff on every `forge compare`) | Default per-attempt summary is file-level (name-status+numstat) — **no** per-file hunk subprocess; hunk body only on explicit `--diff`; scale test asserts the bounded default (U2/U3); scoping no-selector to active intents is a cheap follow-up if measured cost warrants |
| A green compare ranking is treated as authoritative and back-doors the NER-132 TOCTOU | Compare is read-only and explicitly **advisory**; `accept`/`decide` keep the in-txn re-verified gate; documented in Key Decisions; no lock taken by compare |
| Trailer digest built with ad-hoc `format!`+sha256 (drift, collision) | Reuse `integrity.rs` `DigestWriter` + a **new** `forge.publication.v0\0` tag; golden-vector + no-collision tests (U5) |
| Trailer doesn't recompute (hash over raw vs persisted bytes) | Fold the **persisted** Phase 5 evidence `content_hash`es; recompute from the same stored rows; round-trip test export→verify (U5/U6) |
| A published "passing" provenance rests on tampered evidence | Trailer assembly re-verifies the deciding evidence (R8); `EVIDENCE_TAMPERED` before the branch; assert no branch on refusal (U5) |
| New egresses (compare JSON / diff / PR body) leak a secret path or token | `filter_secret_risk` + `is_ignored_by_policy` on paths; `redact_evidence_excerpt` on hunks; per-token redaction on identities; paths out of `anyhow` context (U2/U3/U7) |
| Diff body unbounded / leaks via git | Bound per-file to `EXCERPT_LIMIT` with a `truncated` flag; drop secret-risk paths before reading hunks; keep git in the export-git adapter (U2) |
| `PROVENANCE_MISMATCH` lands on only some contract surfaces → `forge.cli.v0` drift | Full additive fan-out + both `error.rs` drift-guard tests + `FORGE_ERROR_CODES` + `details_carry_only_ids` (U6) |
| Ranking non-deterministic (unstable tiebreak) → flaky compare JSON | `rank_rows` is a pure total order with a stable final tiebreak; determinism unit test asserts byte-identical output (U3) |
| Scope creep into native diff / per-gate persistence / a scoring DSL | Explicit Key Decisions: diff via git adapter only, recompute (no column, no migration), simple total-order ranking; deferred items listed in Scope Boundaries |

---

## Phased Delivery

### Phase A — Inputs (U1, U2)
`GateResult.structured_failures` and the git-adapter `diff_trees` — the two primitives compare consumes. Independently testable; no IO into the store yet.

### Phase B — Compare surface (U3, U4)
The store-level `compare_attempts` (verify-then-rank) and the `forge compare` / `forge attempt compare` CLI. The headline read surface goes green.

### Phase C — Provenance (U5, U6)
The publication digest + structured commit trailer (replacing the constant message, re-verifying deciding evidence) and the `verify-branch` recompute + `PROVENANCE_MISMATCH`. Where the published artifact becomes self-verifying.

### Phase D — Integration (U7)
Compare-driven PR body, the e2e eval block, and the assert-rich `forge_compare.rs` proving every exit criterion end-to-end.

---

## Alternative Approaches Considered

- **Persist per-gate verdicts in a new column (the literal Phase 4 §7 prediction) instead of recomputing.** Rejected: a persisted verdict is a stale cache that a post-check tamper would not invalidate, defeating verify-then-rank; it would also force a migration + head-bump fan-out for no behavioral gain. Recompute keeps the schema honest and the ranking verified. (The doc itself leans recompute.)
- **Make compare authoritative / take the advisory lock.** Rejected: compare never writes; a lock can't serialize against the lock-free `run` anyway; the authoritative gate already lives in-txn at `accept`. Compare stays read-only and advisory.
- **Reuse `EVIDENCE_TAMPERED` for a trailer mismatch.** Rejected: a trailer mismatch is "the published artifact's provenance no longer matches the ledger," distinct from "this row was edited," and it would need a new `TamperKind` variant anyway (same drift-guard cost). A dedicated `PROVENANCE_MISMATCH` is clearer for a branching agent.
- **Build a native tree diff now.** Rejected: explicitly Phase 8; the git adapter via `git_tree_for_content_ref` diffs both backends' refs uniformly today with zero new dependencies.
- **A compare-time ambiguity error (`AMBIGUOUS_INTENT`).** Rejected: returning per-intent groups (filtered by `--intent`/`--attempt`) is a cleaner agent-native surface and avoids a contract addition; unknown selectors reuse existing typed errors.

---

## Documentation / Operational Notes

- Update `crates/forge-cli/src/schema.rs`: add the `attempt compare` / `compare` / `export verify-branch` command shapes and a `notes.provenance` describing the self-verifying **local** trailer boundary (recompute from the local ledger; NOT cross-machine — Phase 9).
- Post-merge: `/ce-compound` a solution doc capturing the non-obvious learnings (verify-before-rank as a determining read that feeds a *selection* not a *write* — a new wrinkle not fully covered by the in-txn docs; the new publication domain tag; the recompute-don't-persist resolution of Phase 4 §7; the three-new-egress redaction sweep). Flip this plan to `status: completed` + move to `docs/plans/completed/`; set NER-137 → Done.
- CI: `bash scripts/ci.sh` is the gate; the eval now includes the compare/rank + provenance block.

---

## Sources & References

- **Origin:** `docs/ROADMAP.md` (Phase 6), ticket NER-137, `docs/brainstorms/2026-05-28-competing-local-attempts-requirements.md` (F4 / R18–R20 — the comparison surface; the metadata-first decision).
- **Carry-over solution docs:** `docs/solutions/architecture-patterns/tamper-evident-evidence-chain-and-failclosed-verification-2026-05-30.md` (Phase 5 — the integrity model Phase 6 consumes; D3/D6), `content-bound-gate-engine-and-failclosed-enforcement-2026-05-29.md` (Phase 4 — the per-gate verdicts + the emit-don't-persist decision naming Phase 6), `write-binding-verification-and-content-backend-isolation-2026-05-29.md` (Phase 3 — attempt-bound content_refs + additive-error discipline), `schema-migration-reconciliation-and-typed-error-contract-2026-05-29.md` (Phase 2 — typed error contract), `sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29.md` + `crash-correctness-advisory-lock-and-atomic-restore-2026-05-29.md` (Phase 1a/1b — the lock-free `run` carve-out, why compare is advisory).
- **Phase 5 context (D-list inputs):** `docs/plans/completed/2026-05-30-010-feat-phase-5-tamper-evident-evidence-plan.md`, `docs/code-reviews/2026-05-30-ner-136-phase-5.md` (D3 → R8, D6 → R10).
- **Related code:** `crates/forge-store/src/{lib.rs,integrity.rs,error.rs}`, `crates/forge-policy/src/lib.rs`, `crates/forge-export-git/src/lib.rs`, `crates/forge-content-git/src/lib.rs`, `crates/forge-content/src/lib.rs`, `crates/forge-cli/src/{main.rs,schema.rs}`, `crates/forge-cli/tests/{forge_attempts.rs,forge_accept_export.rs,forge_pr_body.rs,forge_schema.rs}`, `scripts/e2e-eval.sh`, `scripts/ci.sh`.
- **External:** NIST SP 800-185 (TupleHash length-prefix principle — already applied in `integrity.rs`); git trailer conventions (`%(trailers)`).
