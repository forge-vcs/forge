# Handoff — NER-138 Phase 7 **slice 3** (final): justified commits + native log/checkout/undo + symlink content + object-kind headers + git-export demotion

**Date:** 2026-05-30 · **Milestone:** M3 — Earn native-VCS independence · **Ticket:** Linear **NER-138** (umbrella for all of Phase 7; this is the **final** slice — when it lands, NER-138 → Done) · **Forge project:** id `2b5e82f7-7a78-4354-af7d-68609e6e77bc`, team **SE Engineers**, prefix **NER**

## Where things stand

Phase 7 is staged into 3 slices. **Slices 1 and 2 are merged:**

- **Slice 1 (walker):** native worktree walker + ignore engine (`ignore` 0.4.25), replacing the `git ls-files` shell-out in the snapshot path. PR #22 (`cedc351`), closeout #23 (`af7e403`).
- **Slice 2 (history substrate):** native `Commit` `ObjectKind` + commit-object format, a ref store under `.forge/refs/HEAD`, **backend-agnostic `base_head`** (a native `f1:commit:` **genesis** id, not a git SHA — the Phase-3 git delegation is gone), native name-level `changed_paths` (tree diff, mode-aware), git-export interop (deterministic synthesized git parent), and the `005` migration (`native_object_format` registry; `schema_head` 4→5). **Genesis-only:** the ref-store HEAD is set at first `start` but **does not advance** — writing *justified* commits at `accept` was deliberately deferred here. PR **#24** (`95429c4`).

`main` is clean and synced. **Read first:**
- Slice-2 plan (completed): `docs/plans/completed/2026-05-30-013-feat-phase-7-slice-2-commit-objects-plan.md`
- Slice-2 learnings: `docs/solutions/architecture-patterns/native-commit-objects-base-anchoring-and-the-new-objectkind-gc-reachability-coupling-2026-05-30.md` — pins the invariants slice 3 must not regress.
- Slice-2 code review: `docs/code-reviews/2026-05-30-ner-138-phase-7-slice-2.md` — its "Defer-able" section is slice-3's to-do list.
- Slice-1 learnings: `docs/solutions/architecture-patterns/native-worktree-walker-ignore-engine-and-index-vs-filesystem-divergence-2026-05-30.md`
- `docs/ROADMAP.md` (Phase 7 section).

Gate: `bash scripts/ci.sh` (fmt `--check` · `cargo test --workspace` · clippy `-D warnings` · `scripts/e2e-eval.sh`). `gh` authed as `freezscholte`; remote `freezscholte/forge`; squash-merge convention `(#N)`. **`schema_head` is now `5`. `FORGE_ERROR_CODES` is `23`.**

**Adjacent ticket still open:** **NER-142** — the NER-137 D1 `-z`/C-quote secret-leak in the *export* path's `filter_secret_paths_from_tree` (`crates/forge-export-git/src/lib.rs`). Slice 2's native-base synthesis reuses that path (covered by a non-ASCII-secret test) but did **not** fix the structural `-z` flaw. Its own minimal PR; can land anytime.

## What's next — slice 3 (navigable native history + full git independence)

Run the full lifecycle: branch off `main` (e.g. `ner-138-phase-7-slice-3-history-nav`) → `/ce-plan NER-138` (scoped to slice 3) → **doc-review gate** → `/ce-work` → **code-review gate** (`/ce-code-review plan:<path>`) + `bash scripts/ci.sh` → `/ce-commit-push-pr` referencing NER-138. On merge: flip the slice-3 plan to `completed`, `/ce-compound`, and **set NER-138 → Done** (all 3 slices landed).

### Slice-3 scope

1. **Justified commit-on-accept + HEAD advancement + base progression** (the deferred slice-2 unit — design it properly here):
   - Accepting a proposal in a native repo writes a `Commit` referencing {accepted snapshot tree, parents=[prior HEAD], intent_id, proposal_revision_id, decision_id, **actor**, **authored_time**, evidence_digest}, advances the ref-store HEAD, and records the commit id in **`decisions.commit_id`** (new `006` migration column).
   - **Resolve the ordering/determinism conflict slice 2 flagged:** the commit payload references `decision_id`, but `decide()` mints `decision_id` *inside* its `IMMEDIATE` txn. Either (a) pre-mint `decision_id` in `main.rs` and thread it into `decide()`, or (b) write the commit object before the txn (orphan-safe) and advance HEAD *after* the decision row commits, healing torn state via a HEAD-from-ledger reconcile on the next `current_base`. Pick one and prove crash-retry convergence for BOTH replay paths (same `request_id` → replay guard; new `request_id` → fresh accept).
   - **Include `actor` (already in `decisions.actor`) + an authored-time in the HASHED commit bytes** — Phase 9 signs these exact bytes; a later registry bump cannot retroactively bring earlier justified commits under signed/decider-bound provenance (slice-2 product-lens finding). This is an `f1`→`f2` (or registry-bumped) commit-format change; `native_object_format` exists to record it.
   - **Populate `evidence_digest`** from the deciding evidence `content_hash` (opaque lowercase-hex only — never excerpt text; consider a `Hex64` newtype / `debug_assert` guard, slice-2 security finding).
   - Then base progression is real: the next attempt's `base_head` = the accepted commit, and stale-base-after-accept becomes meaningful.
2. **Native `log`** — walk the commit DAG from HEAD via the JSON contract ("show every change under this intent and the evidence that justified it").
3. **Historical checkout** — check out any past commit's tree into the worktree.
4. **`forge undo` / op-restore** — surface the existing operations/views op-log (`001_init.sql:15-48`, a strong unused seed).
5. **Symlink content round-trip** (mode 120000) so symlinks survive snapshot→restore (today symlinks-to-files are captured by content via the `fs::metadata` gate; symlink *content* is not).
6. **Object-kind headers** — store each object's kind in its own header to **kill the `all_object_ids` triple-hash scan** (`[Blob, Tree, Commit]` re-hash, `forge-content-native` — slice 2 extended it to 3 kinds; slice 3 removes the scan).
7. **Commit-DAG `doctor` integrity** — cycle / dangling-parent detection (`verify_reachable` traverses only trees today; add a commit-DAG walk). Whole-phase exit criterion. Introduce a typed **`NativeHistoryCorrupt`** error code for corruption distinguishable from transient IO (full additive fan-out — see below).
8. **Demote git export to one optional interop adapter** — the whole-phase exit criterion is the full native lifecycle with **git removed from PATH**; git export stays as interop only.

### The schema + error fan-out slice 3 owns

- A numbered **`006_*.sql`** migration (`decisions.commit_id TEXT`, and any commit-format-version bump recorded in `native_object_format`) with the **full `schema_head` 5 → 6 fan-out**: grep the WHOLE test tree + `scripts/e2e-eval.sh` for the literal `5` (the `migrations.rs` `schema_head_is_max_version`/`fresh_apply` tests, the `migrate.rs` integration fixtures incl. the at-head + **HEAD+1 stamp which moves `6`→`7`**, `forge_init.rs`, `forge_concurrency.rs`, the e2e `versions 1,2,3,4,5` + `doctor schema_version=5` + the HEAD+1 insert). **The grep gate must cover both the old head `5` AND the prior HEAD+1 literal `6`** — a grep-for-`5` cannot find the moving HEAD+1 stamp (slice-2 lesson; it is the exact site the enumeration missed and the grep caught).
- The new **`NativeHistoryCorrupt`** code = typed `ForgeError` variant + `code()`/`details()`/`retryable()`/`Display` + `error_registry()` `ErrorCodeSpec` + **both** `error.rs` drift-guard tests (the `all` array AND the exhaustive match) + the `FORGE_ERROR_CODES` list in `tests/forge_schema.rs` (currently **23** → **24**) — all in one change, or the contract drifts.

## Carry-over invariants slice 3 must honor (do NOT regress)

- **Durability:** store-before-DB ordering, crash-atomic temp+fsync+rename+parent-dir fsync (incl. newly-created ancestors), propagate-never-swallow `sync_all`, WAL + `IMMEDIATE` + busy/517 retry, advisory-lock **acquire-once-never-nested** (+ the lock-free `run` carve-out). Commit-on-accept's HEAD advance + `decisions.commit_id` write is the delicate new ordering — see scope item 1.
- **The lazy-genesis lock invariant** (slice-2 §3): every `current_base`/`snapshot_worktree` caller holds the lock; the genesis is taken at start-time. If slice 3 adds a command that reaches `current_base`/`snapshot_worktree`, re-verify it holds the lock and runs after a start.
- **The new-ObjectKind↔gc-reachability coupling** (slice-2 §2): `gc_dry_run` now seeds reachability from `reachable_from_head()`. If slice 3 adds object kinds or new roots (e.g. an op-log-referenced commit), grow gc's reachable roots in the SAME change. (Real GC deletion is Phase 8 — keep gc dry-run-only, but its *report* must stay honest.)
- **Diff must fold in metadata the content hash excludes** (slice-2 §5): `changed_paths` keys on `(blob, mode)`; symlink content (item 5) adds symlink-ness — fold it into the diff key too.
- **Secret hygiene (S1/S2):** no fs paths in `anyhow` context (assert `to_string()` AND `{:#}`); `base_content_ref` / any new materialization names a policy-excluded tree; the `.forge/` ref store + objects stay excluded; `evidence_digest` stays opaque-hex (no excerpt).
- **Boundary:** `forge-store` stays git-free; all native-history code in `forge-content-native`; git export interop stays in `forge-export-git`.
- **Differential-harness discipline:** prove equivalence before deleting any remaining git call; after slice 3, native `save`+`log`+checkout+`undo` should be **fully git-free** (git removed from PATH).

## NOT slice 3 (scope discipline)

- Native **content** diff at hunk/line granularity, the 3-way merge engine, real mark-sweep GC (deletion), pack/delta/compression, working-tree index/status cache, physical per-attempt worktrees → **Phase 8 (NER-139)**.
- Wire protocol / ledger sync / **signing** → **Phase 9** (signing anchors on the commit-object format — which is why slice 3 must get `actor`/authored-time into the hashed bytes now).

## Whole-phase exit criteria (NER-138 → Done when ALL hold)

A native-backend repo completes `init → start → save → run → propose → check → accept → restore`, **walks its own history** (`log`), **checks out any past commit**, and **`forge undo` restores a prior operation** — **all with git removed from PATH**; the differential test proves snapshot-set equality (incl. secret-risk exclusion); no `git ls-files`/`git diff`/`git rev-parse` in native paths (grep — the slice-2 `native_production_paths_shell_no_git` gate extends to the new paths); symlinks + object-kind headers round-trip; the **DAG has no cycles/dangling parents** (doctor verifies); git export still works as **interop**, not a core dependency.

## Start prompt for the next session

> Pick up NER-138 Phase 7 **slice 3** (final): justified commit-on-accept (with `actor`/authored-time in the hashed bytes + `decisions.commit_id`, resolving the `decide()`-mints-`decision_id`-in-txn ordering) + HEAD advancement + base progression + native `log`/checkout/`forge undo` + symlink content (mode 120000) + object-kind headers (kill the `all_object_ids` triple-hash scan) + commit-DAG `doctor` integrity (+ typed `NativeHistoryCorrupt`) + demote git export to optional interop. Read `docs/handoffs/2026-05-30-ner-138-phase-7-slice-3-kickoff.md`, the slice-2 learnings `docs/solutions/architecture-patterns/native-commit-objects-base-anchoring-and-the-new-objectkind-gc-reachability-coupling-2026-05-30.md`, and the slice-2 completed plan first. Owns a `006_*.sql` migration + full `schema_head` 5→6 fan-out (grep for `5` AND the moving HEAD+1 `6`) + the `NativeHistoryCorrupt` error fan-out (23→24). Run the lifecycle: branch off main → `/ce-plan NER-138` (scoped to slice 3) → `/ce-doc-review` gate → `/ce-work` → `/ce-code-review plan:<path>` + `bash scripts/ci.sh` → `/ce-commit-push-pr` referencing NER-138. On merge: flip the slice-3 plan to completed, `/ce-compound`, and set NER-138 → Done. NOT slice 3: content diff/merge/GC-deletion/packing/per-attempt worktrees (Phase 8); wire/sync/signing (Phase 9). Exit criterion: the full native lifecycle + history nav with **git removed from PATH**.
