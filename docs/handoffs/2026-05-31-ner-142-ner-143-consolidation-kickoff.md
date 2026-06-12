# Handoff — NER-142 (`-z`/C-quote export secret leak) + NER-143 (forge undo/op-restore hardening)

**Date:** 2026-05-31 · **Project:** Forge (Linear project id `2b5e82f7-7a78-4354-af7d-68609e6e77bc`, team **SE Engineers**, prefix **NER**) · **Why now:** consolidate/harden the local loop before opening the XL **Phase 8 (NER-139)** front — the ROADMAP's own "finish and prove the wedge before opening an XL greenfield front" philosophy.

## Where things stand

**Phase 7 (NER-138) is Done** — all 3 slices merged: native walker (slice 1), commit objects + ref store + git-free base/changed-paths (slice 2), and navigable history + full git independence (slice 3, PR #26 / closeout #27). `main` is clean at `996c8a2`. **`schema_head` is now `6`. `FORGE_ERROR_CODES` is `24`** (`NATIVE_HISTORY_CORRUPT` added). Gate: `bash scripts/ci.sh` (fmt `--check` · `cargo test --workspace` · clippy `-D warnings` · `scripts/e2e-eval.sh` 85/85). `gh` authenticated; remote repository configured; squash-merge convention `(#N)`.

These two tickets are **independent small/medium PRs** — do them in **either order, as two separate PRs** (NER-142 is the quicker, fully self-contained one; NER-143 is the larger cluster). Both must clear the doc-review (if you write a plan) and code-review gates + `bash scripts/ci.sh` before merge.

---

## NER-142 — `-z`/C-quote secret leak in the export path (fix; small, self-contained, own PR)

**The bug.** `filter_secret_paths_from_tree` (`crates/forge-export-git/src/lib.rs:399`) runs `git ls-tree -r --name-only <tree>` (line 400) **without `-z`** and parses with `.lines()`. `git ls-tree` **C-quotes** any path containing a tab/newline/non-ASCII byte (e.g. `.env\u{a0}prod` → `".env\u{a0}prod"`). The quoted/escaped string then fails `forge_content::is_secret_risk_path`'s prefix match, so a **secret-named file with a special byte escapes the export secret filter** and lands in the published git branch tree. This is the NER-137 D1 finding, deferred at the time.

**The fix.** Parse with `-z` (NUL-delimited, no C-quoting): `git ls-tree -r -z --name-only <tree>` and split on `\0` instead of `.lines()`. The native walker already cures this class structurally (it passes the real filename to `is_secret_risk_path`, never a git-C-quoted string — see `native_walk_excludes_secret_with_special_byte_in_name`); this fix brings the **git-export interop egress** to parity. **Audit every other `git ls-tree`/`git diff`/`git ls-files`-`.lines()` parse in `forge-export-git`** (there are sibling `ls-tree` calls at lib.rs:621/673/849/940 in tests, but check production paths like `diff_trees`/`synthesize_git_tree` for the same `.lines()`-without-`-z` pattern and fix any that filter or compare paths).

**Test.** Add a regression test: a secret-named path with a non-ASCII/tab/newline byte (e.g. `.env.café`, or `.env\u{a0}prod`) in the exported tree is **absent** from the published branch. Mirror the existing `diff_trees_drops_a_secret_path_with_non_ascii_bytes` pattern in `crates/forge-export-git/src/lib.rs` tests.

**Scope guard.** Export stays an optional interop adapter (Phase 7 demoted it). Do not pull native-history changes in. `forge-store` must stay free of git-adapter crates (`crates/forge-store/tests/dependency_boundary.rs` pins this).

---

## NER-143 — forge undo/op-restore hardening + code-review defer-ables

**Read first:** `docs/code-reviews/2026-05-30-ner-138-phase-7-slice-3.md` (its "Defer-able" + "Defense-in-depth" sections ARE this ticket's to-do list, with repros) and the slice-3 learnings `docs/solutions/architecture-patterns/commit-on-accept-ordering-ledger-authoritative-reconcile-and-the-last-hidden-git-dependency-2026-05-31.md` (§8 frames the dirty-check design issue). Surfaces: `crates/forge-cli/src/main.rs` (`checkout_response`/`undo_response`/`restore_response`), `crates/forge-store/src/lib.rs` (`undo_target`/`record_undo`/`checkout_target_content_ref`/`record_checkout`/`reconcile_native_head`/`native_tip`/`native_log`/`verify_native_history`/`gc_dry_run`), `crates/forge-content-native/src/lib.rs` (`scan_worktree` symlink capture, `materialize_symlink`/`validate_symlink_target`, `sync_dir`/`materialize_tree`). Tests in `crates/forge-cli/tests/forge_native_history.rs`.

**The main cluster (worktree-state semantics):**
1. **P1 — undo/checkout permanent-dirty-worktree chaining.** After `undo` restores snapshot A, the latest snapshot is still B, so the next `undo`/`checkout`/`restore` dirty-check (worktree-vs-**latest-snapshot**) spuriously fails `DIRTY_WORKTREE` — "undo twice" is impossible without an intervening `save`. Root is pre-existing in `restore`'s baseline, now inherited by the new commands. Safe (refuses, never clobbers) but a real limitation. **Fix: track "expected current worktree content" in `current_state` separately from "latest snapshot"** — a dirty-check is only meaningful against the content the worktree is *expected* to hold, not the most-recently-*saved* content. This is the headline NER-143 design change; the others below can ride alongside or in follow-up PRs.
2. **P2 — undo semantics divergence + misleading `undone_operation_id`.** `undo` uses the snapshot `parent_snapshot_id` chain (undo-the-last-save), not the op-log `current_state` rewind R7/U8 described; after a non-save head op (accept/checkout/run) it restores the prior save and mislabels `undone_operation_id` as the op-log head. Either align to op-log rewind, or keep the snapshot-chain model but set `undone_operation_id` to the op that produced the restored snapshot + document the "undoes the last save" semantics.
3. **P2 — undo cross-attempt scope.** `undo_target` selects the repo-wide latest snapshot while the dirty-check resolves the attached attempt's latest; in a multi-attempt repo `undo` could restore attempt X's content into attempt Y's worktree. Adopt `restore`'s `snapshot_owner_attempt_id == bound_attempt` binding check (`main.rs` restore_response).

**Other defer-ables:**
4. **P2 — checkout/undo materialize-before-op-log-record ordering.** `restore_snapshot` clobbers the worktree, then `record_checkout`/`record_undo` runs in a separate txn; a crash or `CurrentStateChanged` CAS-loss between them leaves the worktree mutated with no op-log record. Materialize is idempotent (bounded residual); fix is an op-log "intent" row before materialize (the worktree-vs-ledger analog of store-before-DB).
5. **P3 — gc swallows malformed view `state_json` — LATENT FOR PHASE 8.** `gc_dry_run`'s op-log-root scan (`if let Ok(value) = serde_json::from_str(...)`) drops an unparseable `views.state_json` row silently, under-counting roots. Dry-run-only today; **MUST fail-closed (conservative) before Phase 8 (NER-139) grants real mark-sweep deletion** — surface/propagate the parse failure instead of shrinking the root set.
6. **P2 — R15 re-capture symlink validation (defense-in-depth) + e2e.** `validate_symlink_target` runs only at materialize (the real security boundary); the plan's R15 also specified re-capture. Not a live leak (capture stores only the read-link target string, never follows), so this is defense-in-depth + an end-to-end escaping-symlink round-trip test (snapshot a worktree with an absolute/`../../` symlink, assert checkout/restore rejects it path-free). Caution: capture-side *rejection* would break `forge save` on a legitimate absolute symlink — prefer the e2e + a documented decision over hard capture-side rejection.
7. **P2 — extract `ensure_clean_worktree` helper.** The byte-identical 7-line dirty-check block is triplicated across `restore_response`/`checkout_response`/`undo_response`; extract one helper so the refuse-before-materialize safety invariant has a single definition. (Folds naturally into #1.)
8. **P3 — maintainability dedup.** The redundant `decisions.commit_id` query in `gc_dry_run` vs `native_tip`; the DanglingCommitId/DanglingParent tip-classification idiom duplicated across `reconcile_native_head`/`native_log`/`verify_native_history` (the walkers legitimately differ in raise-vs-report policy — a shared `dangling_kind(cid, tip)` helper is the safe extraction).

**Defense-in-depth (lower priority, can defer further):**
9. Pre-existing **S1 path-in-context** in `materialize_tree`/`sync_dir` (`path.display()` in `.with_context`) reaches `COMMAND_FAILED` on IO failure — leaks only the repo's own `.forge`/worktree paths (not secrets). Replace with the path-free `io::ErrorKind` pattern already used by `read_head`/`map_walk_error`. Assert path-freeness against both `to_string()` and `{:#}`.
10. **Native-init-in-subdir nested `.forge`** — native `init` anchors root at `cwd` with no ancestor-`.forge` guard; add one (refuse/warn), mirroring git init's toplevel snap.
11. **`export branch` `STALE_BASE.expected_head`** now carries the accepted `commit_id` for native repos (correct) — add a schema-doc/CHANGELOG note.

## Invariants to NOT regress (both tickets)

Durability (store-before-DB, crash-atomic temp+fsync+rename+ancestor-fsync, propagate `sync_all`, WAL+`IMMEDIATE`+busy/517, advisory-lock acquire-once + `run` carve-out); the dirty-check **safety property** (refuse, never clobber — don't trade the brick for a data-loss bug); secret hygiene S1 (no fs paths in `anyhow` context — assert `to_string()` AND `{:#}`) / S2 (policy-excluded materialization); gc stays **dry-run-only** (Phase 8 owns deletion); `forge-store` git-free of adapter crates; native history in `forge-content-native`; the ledger-authoritative-reconcile / HEAD-lags-never-leads model from slice 3.

## NOT in scope (Phase 8 — NER-139)

Real mark-sweep GC deletion, native hunk-level diff, the 3-way merge engine, pack/delta/compression, physical per-attempt worktrees, the working-tree index/status cache. (Several of these are where the slice-3 deferrals "fully belong" — see the Phase 8 handoff.)

## Start prompt — see the chat message that accompanied this handoff for the paste-able fresh-session prompt.
