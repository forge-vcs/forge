---
title: "NER-143 — forge undo/op-restore hardening (expected-current-content dirty-check + cluster)"
type: fix
status: completed
date: 2026-05-31
origin: docs/code-reviews/2026-05-30-ner-138-phase-7-slice-3.md
---

# NER-143 — forge undo/op-restore hardening

> **Doc-review gate run 2026-05-31** (feasibility · coherence · scope-guardian · security-lens · adversarial,
> against the Explore code map). All findings folded in below; the headline change is the **crash-safe
> ordering** for the dirty-check baseline (F1) and the **PR split** (scope). See § "Doc-review resolutions".

## Context & problem

NER-138 Phase 7 slice 3 made native history navigable (`log`/`checkout`/`undo`) and earned full git
independence. Its code-review gate (11 personas, base `8aea21d` → head `0257b9f`,
`docs/code-reviews/2026-05-30-ner-138-phase-7-slice-3.md`) deferred a cluster of real-but-not-blocking
findings into **NER-143**. The slice-3 learnings doc
(`docs/solutions/architecture-patterns/commit-on-accept-ordering-ledger-authoritative-reconcile-and-the-last-hidden-git-dependency-2026-05-31.md`,
§8) frames the headline design fix.

**State going in:** `main` clean; schema head `6` (derived by `migrations::schema_head()` from the
`MIGRATIONS` array — there is **no** `SCHEMA_HEAD` constant); `FORGE_ERROR_CODES = 24`; gate
`bash scripts/ci.sh` (fmt `--check` · `cargo test --workspace` · clippy `-D warnings` · e2e 85/85).
Native history lives in `forge-content-native`; `forge-store` is git-free of adapter crates
(`crates/forge-store/tests/dependency_boundary.rs` pins this). NER-142 (the export `-z` leak) ships
separately (PR #29) — **not** part of this work.

### The headline bug (P1, adversarial conf 90)

`restore_response` (main.rs:540), `checkout_response` (main.rs:751) and `undo_response` (main.rs:784)
each refuse a dirty worktree **before** materializing, by comparing the worktree's content ref against
`latest_snapshot_content_ref(&cwd, None)` — the latest *saved* snapshot for the attached attempt. The
three blocks are byte-identical.

After `undo`/`checkout`/`restore` materializes snapshot **A**, the latest *saved* snapshot is still
**B**. The next navigation command recomputes `current` (worktree = A) vs `latest` (= B), they differ,
and it spuriously returns `DIRTY_WORKTREE`. So **"undo twice" is impossible without an intervening
`save`** — chained navigation bricks. Safe (refuses, never clobbers) but a real usability limitation.
Root cause (§8): **a dirty-check is only meaningful against the content the worktree is *expected* to
hold, not the most-recently-*saved* content** — they diverge the moment a non-save op materializes a
different tree.

## Goals / non-goals

**Goals:** fix the P1 dirty-check baseline via a **crash-safely-tracked** "expected current worktree
content"; extract the triplicated dirty-check into one helper; bind `undo` to the attached attempt;
correct `undone_operation_id`; make `gc` fail closed on every malformed/unreadable root-scan input;
close the residual S1 path-in-context leak; add the native-init nested-`.forge` guard; add the R15
escaping-symlink e2e; document the `export branch` `STALE_BASE.expected_head` shift; extend the
git-removed e2e block.

**Non-goals (Phase 8 / NER-139):** real mark-sweep GC deletion, native hunk-level diff, the 3-way merge
engine, pack/delta/compression, physical per-attempt worktrees, the working-tree index/status cache,
moving the base==tip fork validation into `decide()`'s txn, the reconcile first-parent-only-walk rework.
The full op-log-rewind undo model (vs the deliberate snapshot-chain v0 cut) stays deferred — this work
aligns the *audit field + semantics docs*, not the undo model. A **two-phase op-log "intent" row** with a
reconcilable status is **out** (it fights the NER-136 tamper-evident `content_hash` chain — see DR-F1);
crash-safety is achieved with the existing `current_state` column instead. The `dangling_kind`
maintainability dedup is **deferred** (maintainability-only, no current correctness cost).

## Invariants to NOT regress

Durability (store-before-DB; crash-atomic temp+fsync+rename+ancestor-fsync; propagate `sync_all`;
WAL+`IMMEDIATE`+busy/517; advisory-lock acquire-once + `run` carve-out); the dirty-check **safety
property** (refuse, never clobber — must not trade the brick for a data-loss bug); secret hygiene **S1**
(no fs paths in `anyhow` context — assert against `to_string()` AND `{:#}`) / **S2** (policy-excluded
materialization); the NER-136 tamper-evident `content_hash` chain (no mutate-after-commit of an op row);
`gc` stays **dry-run-only** (Phase 8 owns deletion); `forge-store` git-free of adapter crates; the
ledger-authoritative-reconcile / HEAD-lags-never-leads model from slice 3; genesis-hash stability (no
commit-format change here).

## Doc-review resolutions (the design decisions the gate forced)

- **DR-F1 (adversarial+security, the crux): set `expected_content_ref` in the record txn (atomic with the
  op-log advance), and make the dirty-check accept `worktree == expected OR worktree == target`.**
  *(Corrected during PR-B implementation from the gate's first sketch of "write before materialize" — that
  sketch doesn't actually self-heal: a re-run hits the dirty-check FIRST, sees worktree=old vs expected=new,
  and refuses, so it never reaches the "set-expected no-op, materialize overwrites" step. Both naive
  single-orderings have a brick window; the dirty-check's OR-target clause is what closes it.)*
  - **Ordering** for `restore`/`checkout`/`undo`: dirty-check → `restore_snapshot` (materialize) →
    `record_*` (which, in its `IMMEDIATE` txn, both advances the op-log AND sets
    `expected_content_ref = target` — one atomic unit; on a `CurrentStateChanged` CAS-loss the whole txn
    rolls back, leaving expected unchanged). `save` sets `expected_content_ref = new snapshot ref` in its
    own record txn (the worktree already holds that content — no window).
  - **Dirty-check** (`ensure_clean_worktree`): pass iff `worktree == expected_content_ref`
    (fallback `latest_snapshot_content_ref` when expected IS NULL — pre-007 / pre-first-materialize, DR-F4)
    **OR** `worktree == target`. Else `DIRTY_WORKTREE`.
  - **Crash/CAS-loss analysis:**
    - Normal: dirty-check (worktree==expected) ✓ → materialize → txn sets expected=target. Consistent.
    - Crash/CAS-loss after materialize, before/at the txn: worktree=target, expected=old. **Re-running the
      same command heals** — the dirty-check passes via `worktree == target`, re-materialize is a no-op, and
      the txn sets expected=target. A *different* command meanwhile refuses safely until the interrupted op
      is re-run (never clobbers). This is the bounded, self-healing residual (materialize is idempotent).
    - Chained `undo` (the headline bug): undo₁ sets expected=A; undo₂ dirty-check worktree(A)==expected(A) ✓.
    - Genuine unsaved edit: worktree == neither expected nor target → refuse. **Safety property preserved.**
    - `worktree == target` by coincidence (user edited to exactly the target): materialize is a no-op, nothing
      is lost — safe.
  This **unifies R1 and R5**: the atomic record-txn write + the OR-target dirty-check give crash-safety
  WITHOUT a separate two-phase op-log "intent" row (which would fight the NER-136 tamper-chain). `accept`/`run`
  don't materialize, so they leave `expected_content_ref` untouched (worktree unchanged → still correct).
- **DR-F2 (feasibility): the column write has no existing per-command `current_state` UPDATE to extend.**
  The only `current_state` UPDATE is the shared `insert_operation_view_chained` CAS (lib.rs:4134), hit by
  **every** mutating op. Do **NOT** add `expected_content_ref` to that shared UPDATE (it would null/clobber
  it on `accept`/`run`/`propose`/`check`). Instead issue a **dedicated** `UPDATE current_state SET
  expected_content_ref = ?1 WHERE singleton = 1` — pre-materialize for restore/checkout/undo, post-snapshot
  for save.
- **DR-F3 (feasibility): there is no `SCHEMA_HEAD` const.** `schema_head()` (migrations.rs:64) returns
  `MIGRATIONS.iter().map(|(v,_,_)| *v).max()`. Adding `(7, "007_…", include_str!(…))` to `MIGRATIONS`
  (migrations.rs:34) makes head=7 automatically. The `6`-pinned fan-out to update: `migrations.rs`
  tests `schema_head_is_max_version` (:410), `fresh_apply_reaches_head_with_checksums` (:423-440, asserts
  each of 1..6 + the `006` checksum), the convergence test (:703-704), and `scripts/e2e-eval.sh:179`
  (`1,2,3,4,5,6` → `…,7`; the `headplus1` future-version insert at :184 already uses 7 → bump to 8).
- **DR-F4 (adversarial+security): post-upgrade NULL window is an accepted limitation.** Migration 007 is
  `ADD COLUMN … TEXT` (NULL, no backfill). Every existing repo has `expected_content_ref = NULL` until its
  first post-007 materialize, so a user parked on a non-latest tree who runs `undo` first still hits the
  old brick once. **Decision:** document this as an accepted v0 limitation (the fallback to
  `latest_snapshot_content_ref` keeps pre-007 behavior; `save`/any materialize populates it). A
  derive-at-migration backfill is possible (DR-F6) but deferred — solo-dev v0, self-heals on first save.
- **DR-F5 (all): split the PR.** Ship the independent, low-blast-radius hardening (R6 gc fail-closed, R8
  S1, R9 init-guard, R10 symlink e2e, R11 docs/e2e) as **PR-A first** (no schema dependency), then the
  focused dirty-check change (R1/R2/R3/R4 + migration 007) as **PR-B**, so the data-loss-adjacent schema
  change gets concentrated review. (Pending user confirmation — the kickoff framed NER-143 as one PR.)
- **DR-F6 (scope+adversarial): keep the column, justified as a deliberate denormalization.** Expected
  content is *derivable* from `current_state.current_view_id` → its view `state_json` (save/restore →
  `snapshot_id`→`content_ref`; checkout → `commit_id`→tree; undo → `restored_snapshot_id`) with a
  walk-back over non-materializing `accept`/`run`/`propose`/`check` views. The column denormalizes that
  walk to one read; the cost is the DR-F2 write discipline. Recorded so the trade-off is explicit.
- **DR-F7 (coherence): R6 is NOT deferred** — drop the misleading "[latent for Phase 8]" heading label;
  it **must ship before** Phase 8 grants deletion. R9 resolves to a **hard refusal** (not warn), matching
  the codebase's refuse-convention and the exit criterion. "R15" in R10 is an **external code-review
  finding id**, not a requirement of this plan.

## Requirements

### R1 — Crash-safe expected-current-content dirty-check baseline (the P1 fix) [headline, PR-B]
Track "expected current worktree content" in `current_state`, written **before** materialize (DR-F1).
- **Migration 007** (additive, NULL sentinel, mirroring 002/005): `ALTER TABLE current_state ADD COLUMN
  expected_content_ref TEXT;`. Register `(7, …)` in `MIGRATIONS`; update the `6`-pins (DR-F3).
- `restore`/`checkout`/`undo`: a dedicated `UPDATE current_state SET expected_content_ref = <target>`
  **before** `restore_snapshot` (DR-F1, DR-F2). `save`: set it to the new snapshot's ref post-snapshot.
- The dirty-check compares the worktree against `expected_content_ref`, falling back to
  `latest_snapshot_content_ref` when NULL (pre-007 / first-materialize; DR-F4).
- **Acceptance:** after `save B; restore A; undo` (or `checkout X; checkout Y`), a *second* nav command
  does not spuriously fail `DIRTY_WORKTREE`; a real unsaved edit between two nav commands still refuses;
  the refuse-before-materialize safety property holds in every case.

### R2 — `ensure_clean_worktree` helper (single definition) [PR-B]
Extract the triplicated dirty-check (now expected-vs-worktree with the NULL fallback) into one helper;
all three commands call it. **Acceptance:** one definition; restore/checkout/undo all call it; a test or
structural check pins that the three callers share it.

### R3 — `undo` cross-attempt binding [PR-B]
`undo_target` (lib.rs:1053) selects the **repo-wide** latest snapshot (no attempt filter), while the
dirty-check resolves the *attached* attempt's latest — so in a multi-attempt repo `undo` could restore
attempt X's content into attempt Y's worktree. **Resolution (DR-F2/coherence):** filter the
latest-snapshot selection in `undo_target` to the attached attempt (`WHERE attempt_id = <attached>`); the
`parent_snapshot_id` chain already stays within an attempt (save chains per-attempt), so this both fixes
the contamination and makes "undo the last save" mean "this attempt's last save." **Acceptance:** a
two-attempt repo test where `undo` on attempt Y never restores X's content.

### R4 — `undone_operation_id` correctness + semantics doc [PR-B]
`undo_target` sets `undone_operation_id = context.current_operation_id` (lib.rs:1076) — the op-log head,
which after a non-save head op (accept/checkout/run) is *not* the save being undone. Keep the snapshot-chain
model; set `undone_operation_id` to the operation that produced the restored snapshot **if** the
`views.state_json.snapshot_id → operations` lookup is unambiguous (note: `snapshots` has no
`operation_id` column — confirm the view→op linkage), else fall back to documenting the "undoes the last
save" semantics in the command/struct docs. **Acceptance:** the field references the correct op for a
non-save head op, or the semantics are documented.

### R6 — `gc` fail-closed on unreadable root-scan inputs [PR-A, must ship before Phase 8]
`gc_dry_run` silently drops inputs in **three** places, each under-counting the reachable/root set
(security DR finding): `if let Ok(value) = serde_json::from_str(...)` for `views.state_json` (lib.rs:3037),
`if let Ok(ids) = native_store.verify_content_ref(...)` (lib.rs:3015), and `if let Ok(...) =
…reachable_from(...)` (lib.rs:3043-3047). Today dry-run-only and harmless; **all three MUST fail closed
before Phase 8 grants real mark-sweep deletion** (an under-counted root → deleting a live object).
Propagate the failure as a typed error. **Decision (DR-feasibility F5):** reuse `NativeHistoryCorrupt`;
its `NativeHistoryCorruptKind` enum needs a `MalformedView` (or `UnreadableObject`) variant since a
parse-failed view has no `commit_id` — that variant add is a smaller fan-out than a new top-level code
(`FORGE_ERROR_CODES` stays 24). **Acceptance:** a repo with a corrupt `views.state_json` (and one with an
unreadable object) makes `gc --dry-run` error typed, not silently under-report.

### R8 — Residual S1 path-free errors [PR-A, defense-in-depth, pre-existing]
Replace `path.display()`/`full.display()`/`parent.display()` in `.with_context(...)` with the path-free
`io::ErrorKind` pattern (`read_head`/`map_walk_error`) at **all** sites (security DR enumerated them):
`materialize_tree` arm (content-native lib.rs:825, 829, 844, 850, 877), `restore_snapshot` cleanup
(lib.rs:192), and the shared `sync_dir` (lib.rs:1282, 1284). **Acceptance:** induced IO failures —
including one that reaches `sync_dir` (e.g. make the parent dir un-fsyncable) and one that reaches the
`remove_file` cleanup — surface path-free errors, asserted vs both `to_string()` AND `{:#}`.

### R9 — Native-init nested-`.forge` guard (hard refusal) [PR-A, defense-in-depth]
`init_repository` anchors root at `cwd` with no ancestor-`.forge` guard; `forge_root`'s nearest-ancestor
walk then routes commands to whichever `.forge` is closer up-tree — and a nested native repo's objects
look unreachable to the outer repo's gc (a Phase-8 deletion hazard, per security DR). Add a guard that
**refuses** `init` when any ancestor dir already holds `.forge/forge.db` (DR-F7: refuse, not warn).
**Acceptance:** `init` inside an existing repo's subtree is refused typed.

### R10 — R15 re-capture symlink validation + escaping-symlink e2e [PR-A, defense-in-depth]
`validate_symlink_target` runs only at materialize (the real boundary). Add the **e2e escaping-symlink
round-trip test** (snapshot a worktree with an absolute / `../../` symlink, assert checkout/restore
*rejects* it path-free). Caution: capture-side *rejection* would break `forge save` on a legitimate
absolute symlink — prefer the e2e + documented decision. Note (security DR): this control is `#[cfg(unix)]`;
document the non-Unix fall-through (materializes target bytes as a regular file) as out-of-scope.
**Acceptance:** an escaping symlink round-trips capture but is rejected at materialize, proven by a test.

### R11 — Docs + e2e parity [PR-A]
- Schema-doc/CHANGELOG note: native `export branch` `STALE_BASE.expected_head` now carries the accepted
  `commit_id`.
- Extend the shell e2e git-removed block to exercise `restore`/`checkout`/`undo` (Rust tests already
  cover these git-free; this is parity). **Acceptance:** the note exists; the e2e block runs the three.

### Deferred (tracked, not in this work)
- **R5 full op-log "intent" row** — superseded by DR-F1 (the `expected_content_ref`-before-materialize
  marker gives the crash-safety without a two-phase op row, which would fight the tamper-chain).
- **R7 `dangling_kind` helper + gc/native_tip dedup** — maintainability-only; defer. **Caution:** when
  done, do NOT collapse gc's all-decisions root loop (lib.rs:3030) into the tip-only `native_tip` — that
  reintroduces the under-count R6 fixes (feasibility DR-F9).

## Implementation Units

**PR-A (independent hardening — no schema dependency):**
- **U-A1 — gc fail-closed (R6).** Propagate all three root-scan swallow sites as typed errors; add the
  `NativeHistoryCorruptKind` variant + its drift-guard fan-out (both `error.rs` tests).
- **U-A2 — S1 path-free errors (R8).** `io::ErrorKind` pattern at the 8 sites; path-free tests.
- **U-A3 — native-init guard (R9).** Ancestor-`.forge` refusal + test.
- **U-A4 — symlink e2e + docs + e2e parity (R10, R11).**

**PR-B (the dirty-check change — schema 007):**
- **U-B1 — Migration 007 + schema fan-out (R1).** `007_*.sql`; `MIGRATIONS` entry; the `6`-pins (DR-F3).
- **U-B2 — `expected_content_ref` writes (R1).** Dedicated pre-materialize UPDATE in restore/checkout/undo;
  post-snapshot UPDATE in save (DR-F1, DR-F2). Store accessor `expected_content_ref(cwd) -> Option<String>`.
- **U-B3 — `ensure_clean_worktree` helper + dirty-check rewire (R1, R2).** Expected-vs-worktree with the
  NULL fallback; all three commands call it.
- **U-B4 — `undo_target` attempt binding + `undone_operation_id` (R3, R4).**

## Exit criteria

- `bash scripts/ci.sh` green (count grows from 85 by the new e2e assertions).
- New tests: `undo_twice_without_intervening_save_succeeds` (R1); `nav_then_unsaved_edit_still_refuses`
  (R1 safety); a **materialize-then-record-crash** test proving the worktree stays recoverable / nav
  self-heals (R1/DR-F1 — the window the original plan missed); `undo_does_not_cross_attempts` (R3);
  `undo_labels_the_correct_operation` (R4); `ensure_clean_worktree` single-definition check (R2);
  `gc_fails_closed_on_corrupt_view_state_json` + an unreadable-object case (R6);
  `materialize_tree_io_failure_is_path_free` incl. a `sync_dir` failure, vs `to_string()` AND `{:#}` (R8);
  `native_init_in_existing_repo_subtree_is_refused` (R9); `escaping_symlink_is_rejected_at_materialize`
  (R10); extended `native_lifecycle_runs_with_git_removed_from_path` parity (R11).
- schema head `7`; migration vector 1..7; the convergence test still passes.
- `forge-store` dependency-boundary test green; safety property preserved (every nav command refuses a
  genuinely dirty worktree before materializing).

## Risks

- **Crash window (DR-F1).** The materialize-then-record split is inherent (two stores). The
  pre-materialize expected write makes the common interrupted case clean and the rest self-healing; the
  materialize-then-record-crash test pins it. Never trade the brick for a clobber.
- **Schema migration.** Additive `ADD COLUMN … TEXT` (NULL) is safe; convergence test covers it. NULL
  window is an accepted v0 limitation (DR-F4).
- **Shared-UPDATE trap (DR-F2).** The dedicated UPDATE must NOT land in `insert_operation_view_chained`.
- **gc under-count (DR-F9).** R7 dedup, if ever done, must not collapse the all-decisions root loop.

## Open questions

1. **PR structure** — one PR (kickoff framing) vs PR-A then PR-B (DR-F5 recommendation). **User decision.**
2. **R4 view→op linkage** — confirm `views.state_json.snapshot_id → operations` is unambiguous; else R4
   is doc-only.
3. **R6 error shape** — `MalformedView` kind variant vs synthesized placeholder `commit_id`; lean variant.
