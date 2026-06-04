---
date: 2026-05-31
topic: ner-139-phase-8-native-diff-merge-gc-worktrees-pack
---

# NER-139 Phase 8 — Native diff, intent-aware 3-way merge, real GC, physical worktrees, pack/index/retention

## Summary

Phase 8 builds Forge's convergence + scale primitives across **6 sequential PRs in 5 danger-isolated slices**: native hunk diff → conflict-as-data + 3-way merge (staged S2a/S2b) → gated never-silent auto-resolution → real mark-sweep GC + physical per-attempt worktrees → pack/delta/compression + index/retention. Conflict-as-data and manual/agent resolution ship **before** any automation; the 3-way merge engine and GC deletion each get their own focused adversarial review.

---

## Problem Frame

After Phase 7, Forge's native backend is git-independent for the *linear* local loop (walk, snapshot, commit-on-accept, log, checkout, undo — all with git removed from PATH). But three load-bearing capabilities are still absent or fake, and they are exactly the ones an autonomous agent fleet stresses:

- **Convergence is delegated or dead.** Content diff in the core review/compare path still runs through the git-export adapter (`forge_export_git::diff_trees`, which synthesizes a git tree from native refs and shells `git write-tree`) — the native side has only a name-level changed-paths today, no hunk differ. The `conflict_sets` table is a thin stub (`id, repo_id, context, paths_json, created_at_ms`) — there is no `path_conflicts` table and no merge engine — so two agents editing the same files have no first-class, re-resolvable representation of the collision. PRD §15 names conflict-as-data a v0 requirement; today only Phase 2's stale-base *metadata* exists.
- **Storage is unbounded.** GC is a dry-run-only stub. Autonomous fleets generate orders of magnitude more speculative states than human committers, so without real reclamation `.forge` becomes "an unbounded log dump" (PRD §10.7). Loose, uncompressed objects also lose to git on storage economics, and whole-file `fs::read` cannot survive a multi-GB file.
- **Attempt isolation is logical-only.** Per-attempt physical workspaces were deferred out of Phase 3 precisely because they depend on the GC that reclaims them — they have no home until this phase.

The cost is concentrated and predictable: the moment two agents touch the same file, Forge has nothing to show; the moment a fleet runs for weeks, the store bloats; the moment a file is large, snapshot OOMs. This is also the **riskiest** phase — a correct 3-way merge with rename/binary/mode/symlink handling is where most VCS projects stall for years, and a GC reachability bug destroys referenced content including in-flight attempts.

---

## Actors

- A1. **Coding agent** — produces competing attempts; reads conflict objects and proposes resolutions through the JSON contract; consumes native diff/compare output.
- A2. **Human or reviewing agent** — accepts a resolution, invokes `forge gc`, makes the accept/reject decision.
- A3. **GC / doctor subsystem** — the integrity walk + mark-sweep that protects every reachable object and gates real deletion on a clean repo.

---

## Key Flows

- F1. **Conflict-resolution loop** (the headline)
  - **Trigger:** two attempts against one intent diverge on overlapping content.
  - **Actors:** A1, A2.
  - **Steps:** 3-way merge runs (base/ours/theirs) → clean hunks auto-merge; true conflicts are written as typed conflict-as-data (`conflict_sets` + per-path `path_conflicts`) → [when S3 auto-resolution is enabled: a ranked suggestion is emitted alongside the conflict object, never applied silently] → agent reads the conflict object via the contract → proposes (or accepts a suggested) resolution → resolution is recorded as evidence → check re-evaluates on the resolved tree.
  - **Outcome:** a merged tree exists, or the conflict is persisted as re-resolvable data; nothing is silently picked.
  - **Covered by:** R10, R11, R15, R16, R17, R20, R21.

- F2. **Safe GC**
  - **Trigger:** A2 runs `forge gc`.
  - **Actors:** A2, A3.
  - **Steps:** `gc --dry-run` reports reachable vs unreachable + the protection window → real deletion requires a doctor-clean repo and `--yes` → deletion proceeds in crash-safe order, touching only objects outside the reachability root set and the protection window.
  - **Outcome:** unreachable, out-of-window objects reclaimed; everything reachable from a ref, recent op, decision, per-attempt worktree, or checkout target is retained.
  - **Failure/escape:** a corrupt/un-parseable ledger root or a non-clean doctor → deletion refuses (fail-closed), never partial-deletes.
  - **Covered by:** R23, R24, R25, R26, R27.

- F3. **Per-attempt worktree switching**
  - **Trigger:** A1 attaches/switches attempts.
  - **Actors:** A1, A3.
  - **Steps:** the attempt's tree is materialized into a physical workspace under `.forge/` → a worktree-level advisory lock serializes concurrent materializes → the workspace path is surfaced in attempt show/list JSON → an abandoned worktree is reclaimed by GC.
  - **Outcome:** attempts have non-destructive, concurrency-safe physical isolation; GC never reclaims a live worktree.
  - **Covered by:** R27, R28, R29.

---

## Requirements

**Cross-cutting (every slice)**
- R1. Every Phase 1–7 invariant is preserved: store-before-DB + crash-atomic writes (temp + fsync + rename + ancestor-fsync, propagated `sync_all`) for all new object/pack/worktree writes; WAL + `IMMEDIATE` + busy/517; advisory-lock acquire-once + `run` carve-out.
- R2. S1 (no filesystem paths in `anyhow` context — asserted on both `to_string()` and `{:#}`) and S2 (policy-excluded snapshot/materialize/worktree) hold for all new code, including conflict payloads, pack files, and worktree paths.
- R3. Any new typed error gets the full `error.rs` + `forge_schema.rs` fan-out (both drift guards green); migration 008 gets the schema-head fan-out (grep the old head **7** AND the moving HEAD+1 stamp). `forge-store` stays git-free of adapter crates; native primitives live in `forge-content-native`.
- R4. Agent-native parity: every new capability (diff, merge, conflict read/resolve, gc, worktree show) is in `forge schema` with structured JSON + typed errors.
- R5. Slices ship **strictly sequentially** — S1 → S2a → S2b → S3 → S4 → S5 — each as one PR behind the full gate (`bash scripts/ci.sh` + `/ce-code-review` with adversarial/reliability personas; security persona on the conflict-egress + GC-deletion paths), squash-merged.

**S1 — Native content diff**
- R6. Native hunk/line-granularity diff for working-vs-snapshot, snapshot-vs-snapshot, and base-vs-proposal, exposed through the JSON contract.
- R7. Rename detection, correct on a corpus.
- R8. The native diff **replaces** the git-dependent core-path diff: S1 (a) builds a hunk-granularity differ over the native object store, and (b) rewires `forge compare --diff` off `forge_export_git::diff_trees`, leaving `diff_trees`/`synthesize_git_tree` only on the git-export *interop* path. Not a parallel second diff.
- R9. The `(blob, mode)`/symlink diff key from Phase 7 is preserved so a symlink and a same-bytes regular file stay distinct.

**S2a — Conflict-as-data + multi-parent DAG walkers** *(migration 008)*
- R10. Migration 008 makes `conflict_sets` real (PRD §15 fields: base/ours/theirs tree, generated-by op, resolver backend, status `unresolved`/`partially_resolved`/`resolved`/`abandoned`) and adds a `path_conflicts` representation (path, typed kind, base/ours/theirs ref, resolution ref).
- R11. PRD §15 typed conflict kinds are representable: `content`, `binary`, `delete_modify`, `rename`, `dir_file`, `mode`, `symlink`.
- R12. On a detected divergence, real `conflict_sets`/`path_conflicts` rows are written (extending Phase 2's stale-base metadata to per-path typed data) — independently of whether the merge engine (S2b) has run.
- R13. Make the two first-parent-only walkers merge-aware: `reconcile_native_head`'s descendant/fork check must perform a **full multi-parent reachability search** for the current HEAD (push **all** parents, dedup via the existing `seen` set), and `native_log` must traverse all parents. (`verify_native_history`/doctor and `gc`'s `reachable_from` are **already** multi-parent-aware — do not re-touch them.) This is load-bearing because `reconcile_native_head` runs on the shared command-entry path for *every* mutating command: a first-parent-only check spuriously raises `NativeHistoryCorrupt` on a legitimately-merged tip and bricks `save`/`check`/`accept`/`gc` repo-wide, not just `log`. Verified with synthetic merge-commit tests before the engine exists.
- R14. Conflict payloads ride the existing tamper-evident chain + S1/S2 redaction. Because a conflicting hunk can embed an inline secret from a file that is *not* path-excluded (a secret hardcoded in a `.rs`/`.yaml`, not a `.env`), any blob content surfaced through the JSON contract (conflict show, resolution, suggestion — R16/R20/R22) MUST pass through `redact_evidence_excerpt` at the **read boundary**, not only at evidence-capture time — a conflict payload is a machine-visible egress (no excerpts/paths).

**S2b — 3-way merge engine + manual/agent resolution**
- R15. A 3-way merge engine takes base/ours/theirs trees → a merged tree: clean, non-overlapping hunks auto-merge mechanically; overlapping/true conflicts are emitted as conflict-as-data and never silently resolved. Binary conflicts are never silently resolved. `dir_file` conflicts (one side has a directory, the other a file at the same path) are always emitted as typed conflict-as-data and never auto-merged — the engine does not restructure the tree to satisfy either side.
- R16. An agent reads a conflict object and submits a resolution through the JSON contract; the resolution is recorded as evidence.
- R17. After resolution, the check re-evaluates on the resolved tree.
- R18. The Phase 7 commit format is unchanged where it must be — `actor`/`authored_time` stay in the hashed preimage bytes so Phase 9 signing can anchor on merge commits later (genesis-hash stability preserved). A **golden-vector test** pins the merge-commit hashed preimage (a fixed merge commit → a hard-coded `f1:commit:` id, captured out-of-band) so any preimage-layout drift across S2b→S3→S4→S5 fails loudly rather than silently invalidating the Phase 9 anchor.
- R19. Symlinks and mode bits round-trip losslessly through merge + resolution with crash-atomic restore.

**S3 — Auto-resolution (gated, never-silent suggestion)**
- R20. Auto-resolution ranks/suggests resolutions using Phase 4/5 results + intent scope, layered on top of conflict-as-data. When Phase 4/5 evidence is absent for an attempt (a likely early state), auto-resolution omits evidence-based ranking and the suggestion's provenance states the evidence input was empty; it never fails silently or emits an untyped error (R4).
- R21. Auto-resolution **never auto-applies** to a true conflict — it emits a ranked, evidence-backed suggestion that an agent/human must explicitly accept; the accept is recorded as evidence and re-checked (same path as R16/R17). It is explicitly gated (off-by-default or opt-in) and never silent.
- R22. A suggestion carries its provenance (which evidence/intent inputs produced it) so the accepting actor can judge it.

**S4 — Real mark-sweep GC + physical per-attempt worktrees**
- R23. Real mark-sweep GC replaces the dry-run-only stub. Reachability reuses the slice-3 roots (ledger tip ∪ reachable-from-HEAD ∪ op-log `state_json` commit_ids) and additionally protects per-attempt worktrees + checkout targets. The merge-aware DAG walk (R13) is reused — so **S4 must not begin until R13 has passed its S2a adversarial/reliability review with no open correctness findings**. Real deletion **acquires the repo-level advisory lock** (`.forge/forge.lock`) and holds it across **both** the reachability scan and the unlink sweep, so no concurrent writer (`accept`/`save`/`checkout`) can mint a new reachable object referencing an about-to-be-deleted object between scan and sweep. (`gc` is *not* in the lock-requiring set today because the stub is read-only; real deletion must be added to it.)
- R24. Real deletion is gated on a **doctor-clean** precondition; `doctor` is extended to parse every `views.state_json` (so the gc fail-closed remedy has something to verify) and the corrupt-`state_json` shape gc already fail-closes on is a doctor finding.
- R25. Real deletion requires `--yes` (`CONFIRMATION_REQUIRED` typed error otherwise), is preceded by a mandatory dry-run diff, and honors a reflog-style protection window whose **default is no less than 7 days** (the window is the last line of defense against a reachability miss causing irreversible loss of in-flight agent state). Deletion uses **crash-safe ordering**: each unlink is crash-atomic, and a crash mid-sweep MUST leave a state that **over-protects** (a surviving object whose protection still resolves) and **never under-protects** (a referenced object already unlinked) — the inverse of store-before-DB. The existing fail-closed-on-corrupt-root behavior is preserved.
- R26. A reachability fuzz test demonstrates GC reclaims only unreachable objects and never deletes anything reachable from a ref, recent op, decision, worktree, or checkout target.
- R27. Physical per-attempt workspaces (worktree directories under `.forge/`) with non-destructive, concurrency-safe switching; GC reclaims abandoned worktrees but never a live one. Worktree materialization MUST filter secret-risk paths (`is_ignored_by_policy`/`is_secret_risk_path`) before writing content into the physical workspace — the same exclusion applied at snapshot capture — and any code path that reads content back from a worktree (diff/merge/suggestion) re-applies the filter before including it in a JSON response.
- R28. A worktree-level advisory lock closes the concurrent-materialize window (two `forge` processes materializing into one worktree outside the DB lock).
- R29. The per-attempt workspace path is surfaced in attempt show/list JSON.

**S5 — Pack/delta/compression + index + retention**
- R30. Packfile/delta/compression via an audited crate (custom compression banned); on a **defined benchmark corpus** (≥500 objects / ≥50 MB loose store) the packed + compressed store is measurably smaller than loose and than git (target ≤ ~60% of loose byte size). The pack format must preserve per-object content-addressing so each object's `f1:` hash stays independently recomputable inside a pack (see R34).
- R31. Large-file/streaming reads replace whole-file `fs::read`; a multi-GB file snapshots/restores without OOM.
- R32. A working-tree index/status cache keeps status/diff fast on a large synthetic tree.
- R33. A retention policy + storage budget per PRD §10.7 (retention for snapshots + command output; protection rules for accepted proposals / published refs / exported commits). The storage budget is a **reported/visible threshold only** (not an enforced auto-evictor): once `.forge` exceeds it, mutating commands emit a non-blocking `warnings[]` entry and `gc --dry-run`/`doctor` report it — giving an unattended fleet a machine-readable "run gc" signal without taking on automatic-deletion risk.
- R34. Packing does not break `f1:` verify-on-read — verify-on-read passes through the pack layer.

---

## Acceptance Examples

- AE1. **Covers R10, R11, R12, R15.** Given two attempts editing overlapping regions of the same file, when the 3-way merge runs, a `conflict_sets` + per-path `path_conflicts` rows are written with the correct typed kind and the merge does not pick a side.
- AE2. **Covers R15.** Given two attempts editing non-overlapping hunks of the same file, when the merge runs, a merged tree is produced with no conflict rows and no human input.
- AE3. **Covers R20, R21.** Given a true content conflict with auto-resolution enabled, when merge runs, Forge emits a ranked suggestion but does not apply it; the resolution takes effect only after an explicit accept recorded as evidence and re-checked.
- AE4. **Covers R24, R25.** Given a repo whose `doctor` reports corruption, when `forge gc --yes` is invoked, deletion refuses (doctor-clean precondition) rather than deleting anything.
- AE5. **Covers R23, R27.** Given an object that looks unreferenced but is reachable from a per-attempt worktree or a recent op-log entry, when `forge gc --yes` runs, that object is never deleted.
- AE6. **Covers R30, R34.** Given objects stored in a pack, when any read occurs, `f1:` verify-on-read still validates the object hash.
- AE7. **Covers R13.** Given a merge commit with two parents accepted as the ledger tip where the prior HEAD is reachable only via the *second* parent, when an **unrelated** command (`save` or `gc`) next runs, `reconcile_native_head` does **not** raise `NATIVE_HISTORY_CORRUPT` and the command succeeds; and `log`/`doctor` traverse all parents (no linear-only miss).
- AE8. **Covers R14.** Given a conflict payload whose ours/theirs hunk contains a secret-like `key=value`, when the conflict is read back through the JSON contract, the secret is redacted in the egress and a `warnings[]` entry is emitted.
- AE9. **Covers R27.** Given an attempt tree containing a `.env` file, when its per-attempt worktree is materialized under `.forge/`, the `.env` is not written into the physical workspace.
- AE10. **Covers R25.** Given `forge gc --yes` killed between root-enumeration and the Nth unlink, when the repo is re-opened, `doctor` reports clean (no `DanglingTree`/`DanglingParent`) and a subsequent torn-accept `--request-id` replay still resolves.
- AE11. **Covers R11, R15.** Given a `dir_file` collision between ours and theirs, when the merge engine runs, a `path_conflicts` row of kind `dir_file` is written and no merged-tree output is produced.

---

## Success Criteria

- Native diff is correct with rename detection on a corpus; the git-adapter diff is gone from the core review path.
- A 3-way merge of two attempts writes real `conflict_sets`/`path_conflicts` rows with correct typed kinds; an agent resolves a conflict via the contract, the resolution is recorded as evidence and re-checked; symlinks/mode round-trip losslessly with crash-atomic restore.
- `forge gc` reclaims only unreachable objects (reachability fuzz) and never deletes anything reachable from a ref / recent op / attempt-worktree / checkout target; real deletion is gated on doctor-clean + `--yes` + a protection window and holds the repo lock across scan+sweep.
- A simulated fleet run (many speculative attempts/snapshots over a retention window) followed by `gc` returns the store to a **bounded** size — retention + GC together (not just reachability correctness) hold the "bounded" property the Problem Frame asserts; crossing the storage budget surfaces a `warnings[]` entry.
- The packed + compressed store is measurably smaller than loose and than git; a multi-GB file snapshots/restores without OOM; status/diff stay fast on a large synthetic tree via the index; verify-on-read passes through the pack layer.
- Downstream-handoff quality: each slice merges green through `bash scripts/ci.sh`; the full native lifecycle (incl. merge + gc + worktrees) runs with git removed from PATH — this criterion **excludes** the git-export interop path (`forge_export_git` legitimately retains git); `schema_head` advances to 8 with a complete fan-out; `FORGE_ERROR_CODES` grows by exactly the new typed errors (e.g. `CONFIRMATION_REQUIRED`) with both drift guards green.

---

## Scope Boundaries

- **Phase 9 (NER-140), excluded here:** wire protocol / clone-fetch-push-pull / ledger sync; cryptographic signing + the trust ladder; merge-queue / remote-boundary conflict semantics. S2's commit format must keep `actor`/`authored_time` hashed so signing can anchor later, but signing itself is not built in Phase 8.
- **Post-Phase-8 follow-ons (per ROADMAP), excluded:** blame/annotate (intent-aware blame), bisect, general lifecycle-hook dispatch.
- **NER-143 deferrals that are NOT Phase 8:** an `--allow-nested` opt-out for a deliberately-independent nested repo; the cross-repo nested-init TOCTOU.
- **Excluded by the manual-GC decision:** background/automatic eviction on a storage-budget threshold — v0 GC is manual-invocation only; retention defines the protection window, not an auto-evictor.

---

## Key Decisions

- **5 slices, split by danger (not by feature count):** the two genuinely dangerous things — the 3-way merge engine and GC deletion — each get their own focused adversarial/reliability review; the cheaper dependency-coupled pair (GC + the worktrees it reclaims) is combined. Rationale: NER-143 showed danger-isolated review catches the design-level traps (the fifth materializing op, the non-self-healing ordering) that a fat PR dilutes.
- **S2 staged into two PRs (S2a + S2b):** migration 008 + conflict-as-data persistence + the multi-parent DAG-walker hardening land first (independently testable via synthetic merge commits, and needed by S4's GC), before the merge algorithm itself. Rationale: de-risks both S2 and S4 and keeps the hardest PR (the merge engine) focused.
- **Conflict-as-data + manual/agent resolution before any automation:** auto-resolution (S3) is a separate, gated, never-silent suggestion slice layered on top — a confidently-wrong auto-merge is the headline risk, so it never auto-applies and gets its own review.
- **Manual-only GC for v0:** `gc --dry-run` then `gc --yes`; no auto-evictor. Rationale: GC deletion is the dangerous operation; a manual, doctor-gated, windowed, confirmed path is the safe v0 shape.
- **Native diff replaces (not parallels) the git-adapter diff** in the core review path — removes the interop dependency rather than adding a second diff source.

---

## Dependencies / Assumptions

- Depends on Phase 7 (NER-138, Done): native walker/ignore engine, commit/DAG/ref objects, commit-on-accept + ledger-authoritative HEAD reconcile, the slice-3 gc reachability roots, object-kind headers.
- Builds on the NER-142/143 consolidation (merged): `schema_head` is currently **7** (migration 007 `expected_content_ref` landed); Phase 8's new migration is **008**. `FORGE_ERROR_CODES` is currently 24.
- The gc fail-closed-on-malformed-op-log-view behavior already shipped (NER-143 PR-A R6); Phase 8 inherits the *next* layer (doctor parsing `state_json` + doctor-clean deletion gate).
- Assumes the during-materialize partial-restore crash state (safe-by-refusal today) is revisited under real deletion + physical worktrees but remains refuse-never-clobber.
- Gate: `bash scripts/ci.sh`. `gh` authed `freezscholte`; squash-merge `(#N)`. Track under Linear **NER-139** (Forge project, team SE Engineers).

---

## Outstanding Questions

### Deferred to Planning

- [Affects R10][Technical] Exact `path_conflicts` schema shape (new table vs columns on `conflict_sets`; how `paths_json` migrates) — `/ce-plan` decides against the real `forge-store` migration discipline. Once chosen, re-confirm all `path_conflicts` references against the migration 008 DDL.
- [Affects R15][Technical] 3-way line-merge algorithm fidelity (diff3-style vs simpler hunk-overlap) and the binary/`dir_file`/mode/symlink conflict-classification rules.
- [Affects R7][Needs research] Rename-detection heuristic + similarity threshold; corpus to validate against. A low threshold yields false-positive rename conflicts the merge engine (R11/R15) must then handle — couple the two decisions.
- [Affects R30][Needs research] Which audited pack/delta/compression crate; delta-chain depth and pack-trigger policy; how the pack format keeps per-object content-addressing for `f1:` verify-on-read (R34).
- [Affects R32][Scope decision] R32's working-tree index/status cache has **no Phase 8 consumer** in F1/F2/F3 and no baseline metric — `/ce-plan` must justify it against a named consumer + a concrete latency/tree-size target, **or defer it out of Phase 8**. (Also covers the invalidation strategy + on-disk shape.)
- [Affects R16/R17][Technical] What rebinds the *resolved tree* to the proposal so `accept` commits it, and how that binding survives a moved base (`STALE_BASE`) or a concurrent `gc` — the F1 loop currently stops at "recorded as evidence"; "evidence" is a justification record, not a proposal-revision binding.
- [Affects R25][User decision + correctness floor] GC protection-window default (duration; time-based vs count-based vs both) — default to a conservative time window (≥7 days, R25). The floor must also cover the **torn-accept-orphan window**: a crash-after-`write_commit`-before-`decisions`-COMMIT leaves an orphan reachable by no root that a `--request-id` replay may still re-reference, so the window is a correctness bound, not only a UX default.
- [Affects R22][Security] The suggestion provenance payload must not inline un-redacted conflict/evidence content — reference content refs/evidence IDs, or pass any inline text through `redact_evidence_excerpt`.
- [Affects R27][Technical] Define "live worktree" under a crash (partially materialized, no live lock because the process died) so GC reclamation never races a recovering worktree — ties to the deferred during-materialize partial-restore crash state.
- [Affects R5][Process] S2a is migration + walker hardening; consider whether the full adversarial gate is proportionate for that PR or whether the reliability persona suffices (the merge engine S2b and GC deletion S4 are the adversarial-critical ones).
- [Affects R5] Housekeeping: move the completed NER-143 plan (`docs/plans/2026-05-31-015-...-plan.md`) into `docs/plans/completed/` — fold into S1's PR or a standalone tidy.

*(The R32 / rebind / window-floor / provenance / S2a-gate items above were surfaced by the 2026-05-31 doc-review gate and deferred from the doc body per the best-judgment route; the 15 concrete fixes were applied inline.)*
