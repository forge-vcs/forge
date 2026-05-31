---
title: "Commit-on-accept ordering, the ledger-authoritative HEAD reconcile, and the last hidden git dependency: how Forge earned native-VCS independence (NER-138 Phase 7 slice 3)"
date: 2026-05-31
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: commit-on-accept-ordering-head-reconcile-and-git-free-root-resolution
severity: high
applies_when:
  - Writing a content-addressed object whose id depends on a value minted INSIDE the DB transaction it must precede (store-before-DB vs decision-id-in-txn)
  - A non-transactional ref/pointer (a HEAD file) must stay consistent with a transactional ledger (SQLite) across crashes — two stores, no distributed txn
  - A lock-free reader (gc) seeds reachability from a cache (ref-store HEAD) that a never-reconciled reader can read stale
  - Pinning a content-addressed format against regression (genesis hash stability) — and why "reconstruct it in-test" is not a pin
  - Adding a self-describing header to a content-addressed object store WITHOUT changing the id or breaking legacy objects
  - Claiming "git removed from PATH" independence while a root-resolution helper silently shells `git rev-parse` on every command
  - A materialize-then-record command (checkout/undo) whose worktree clobber and op-log write are separate transactions
  - A worktree dirty-check that compares against "latest snapshot" rather than "expected current content" — breaking chained navigation
tags: [commit-on-accept, store-before-db, ledger-authoritative-reconcile, head-lags-never-leads, decision-id-in-txn, genesis-hash-stability, golden-vector, object-kind-headers, hash-resolved-disambiguation, git-free-root-resolution, gc-reachability-roots, symlink-r15-traversal, native-vcs-independence, ner-138, ner-143]
---

# Commit-on-accept ordering + ledger-authoritative reconcile + the last hidden git dependency (NER-138 Phase 7 slice 3)

Slice 3 made Forge's native history *navigable* (justified commit-on-accept, `log`/checkout/`undo`) and earned **full git independence** — the whole native lifecycle runs with git removed from PATH. These are the non-obvious learnings the doc-review and code-review gates surfaced, the headline ones being the commit-on-accept ordering resolution (which slice 2 deliberately deferred) and the discovery that root resolution was the last hidden git dependency.

## 1. The decision-id-in-txn / store-before-DB conflict: write inside the txn, advance HEAD after, reconcile from the ledger

Slice 2 deferred commit-on-accept precisely because of a contradiction: the commit object content-addresses over a `decision_id` that `decide()` mints *inside* its `IMMEDIATE` transaction, while store-before-DB durability wants the object durable *before* the row that references it. You cannot satisfy both naively, and "re-run re-derives the identical commit" is false (the id is non-deterministic per retry). The resolution that actually works:

- **Mint `decision_id` + build + durably `write_commit` the object INSIDE the `FnMut`, before the `INSERT decisions(commit_id)`/COMMIT.** The fsync-inside-the-write-lock cost is acceptable because `accept` is low-frequency and the advisory lock already serializes writers. A busy/517 retry re-runs the closure, mints a *new* `decision_id` → a *new* commit object; the loser is an unreferenced orphan (gc-collectible). This is the SQLite-concurrency doc's "mint inside, the loser's object is unreferenced" made concrete.
- **Advance HEAD (`set_head`) AFTER the txn commits — and read the committed `commit_id` from the closure's return, never an outer captured mut** (a retried closure leaves the outer var holding the last *attempted* value, which is the winner only by accident).
- **The crash window is inherent** (two stores, no distributed txn) and is healed by treating the **SQLite ledger as authoritative and the ref-store HEAD as a reconcilable cache**. A `reconcile_native_head` at the command boundary advances HEAD to the latest accepted `decisions.commit_id` when it lags. The invariant is **HEAD lags, never leads (and never forks)**: advancing HEAD *before* COMMIT would leave HEAD ahead of the ledger on a crash — the unhealable direction (you cannot know the prior HEAD to roll back to). Lagging is always healable forward.

The crash states this produces: crash-before-COMMIT ⇒ orphan object + HEAD unadvanced (consistent); crash-after-COMMIT-before-`set_head` ⇒ `commit_id` durable + HEAD stale (reconciled on the next command); crash-after-`set_head` ⇒ consistent.

## 2. Reconcile must run BEFORE the preflight-replay short-circuit (the adversarial catch)

The torn window (committed-but-HEAD-stale) is only healed if the reconcile actually runs. The sharp ordering bug the adversarial reviewer constructed: `command_result` has an early-return preflight replay check (`operation_for_request` → `replay_response`) that fires for a same-`request_id` retry. If reconcile ran *after* that short-circuit, a retry of a *torn* accept would replay the idempotent stub and return **without healing HEAD**, leaving it stale for the next command. **The reconcile call must sit after lock acquisition but before the preflight-replay short-circuit.** "Place the heal before any early-return that the to-be-healed state can trigger" is the general rule.

A second adversarial catch: reconcile must verify the ledger tip is a **descendant of the current HEAD** before advancing (a non-descendant tip is a fork → `NativeHistoryCorrupt`, not a silent overwrite). Lock-serialized accepts can't fork in practice, but the guard is what makes "HEAD lags, never leads" provable rather than assumed.

## 3. A golden vector you reconstruct in-test is not a pin

Genesis-hash stability is the highest-impact regression vector: if adding `actor`/`authored_time` changed the genesis hash, every existing native repo's `base_head` desyncs into spurious `STALE_BASE`. The fix is `#[serde(skip_serializing_if = "Option::is_none")]` on the new fields (a `None` genesis serializes byte-identically). But the *test* matters as much as the fix:

- The first implementation asserted byte-equality against an **in-test reconstructed** JSON string and only checked the id `starts_with("f1:commit:sha256:")`. The doc-review and code-review both flagged this: a reconstruction can drift from what was actually shipped in lockstep with the struct, passing green while real repos brick; and a prefix check catches a `CommitObject`-shape change but **not a change to the preimage framing** (`object_preimage`/`ObjectId::new`).
- The fix: pin the **full hard-coded id literal** for a fixed input (computed independently: `f1:commit:sha256:cf31029e…` for the all-zeros-tree genesis). Now any change to either the struct *or* the hashing preimage fails loudly. **A content-address pin must assert the actual digest, captured out-of-band, not recomputed from the same code path under test.**

## 4. Object-kind headers: store the self-describing preimage; disambiguate by hash, not by format-guess

To kill the `all_object_ids` triple-hash scan, store each object as its **full domain-separated preimage** (`b"forge-object\n" + kind + … + payload`) rather than the raw payload. Key properties: the id is **unchanged** (it was already `hash(preimage)`, so no re-addressing, dedup/path stable), the file becomes self-verifying (`hash(file) == id`) and self-describing (kind is a parsed header field). Legacy slice-1/2 raw-payload objects coexist via a fallback. The adversarial trap: a legacy *blob* whose content coincidentally starts with the magic. The robust rule — **disambiguate by hash, never by format-guess**: parse the preimage *and* verify `hash(file) == id`; a legacy blob fails that check (its id is `hash(preimage(payload))`, not `hash(payload)`) and falls through to the triple-probe. Keep both paths alive in one PR (prove-before-delete) with a mixed-store differential test.

## 5. The last hidden git dependency: root resolution

The whole-phase exit criterion is "git removed from PATH." After slices 1+2 made snapshot/base/diff git-free, the criterion was *still* unachievable — because **`open_repository` (every command) resolved the repo root via `git rev-parse --show-toplevel`**. This is the dependency that's easiest to miss: it's not in the content path, it's in the plumbing every command silently calls. The fix: a git-free `forge_root` that walks up for the `.forge/forge.db` marker, used by `open_repository`/`migrate`/`acquire_repo_lock`; native `init` anchors root at `cwd` with no git (a git-backed repo still snaps to the git toplevel, since Forge layers on an existing git repo). **When claiming independence from a tool, grep for every call site of that tool — the load-bearing one is usually in the shared entry path, not the feature you were focused on.** Prove it with a test that runs the lifecycle with the tool removed from PATH (a `PATH` containing only `sh`), not just a source grep.

## 6. The new-root↔gc-reachability coupling, extended: seed from the ledger tip, not the cache

Slice 2's §2 lesson (a new object kind reachable from a new root must grow gc's reachable set in the same change) extends sharply here. gc is **lock-free and never reconciles**, so seeding reachability from the ref-store HEAD would read a *stale* HEAD (missing the latest accepted commit after a torn window) and report a live commit as garbage. The fix: seed gc from the **authoritative ledger tip** (`native_tip`: latest `decisions.commit_id`, else HEAD) — shared with reconcile so they can never disagree — plus **every op-log-referenced commit** (a `checkout` target writes no `decisions` row, so it's reachable only through the op-log `state_json`). gc stays dry-run-only; only its report must be honest. The latent hazard the reliability reviewer flagged: gc silently drops an unparseable `views.state_json` row, which would under-count roots — harmless in dry-run but **must fail-closed before Phase 8 grants deletion authority** (filed NER-143).

## 7. Symlink R15: the materialize boundary is the security control, not the capture boundary

Symlink content (mode `120000`) round-trips by capturing the link target via `read_link` (the target *string* only — **never followed**) and recreating a symlink on materialize. The security boundary is **materialize-time**: `validate_symlink_target` rejects absolute and worktree-escaping targets (`../../etc/passwd`) before creating the link, so a malicious commit can't materialize an escaping symlink. The doc-review asked for validation at *both* capture and materialize; the code-review correctly concluded capture-side rejection is **not** needed for the leak (capture stores only the string, never resolves it) and would **break `forge save` on a legitimate absolute symlink** in the user's repo. Lesson: **put the containment check where the dangerous action is (creating a link that can be followed out), not where the inert data is captured.** Fold symlink-ness into the `(blob, mode)` diff key (the slice-2 mode-blind-diff lesson) so a symlink and a same-bytes regular file stay distinct.

## 8. A materialize-then-record op needs the "expected current content" baseline (deferred, NER-143)

`checkout`/`undo` reuse `restore`'s worktree dirty-check, which compares the worktree against the **latest saved snapshot**. After `undo` restores snapshot A, the latest snapshot is still B, so the *next* navigation command spuriously fails `DIRTY_WORKTREE` — "undo twice" is impossible without an intervening `save`. The root is pre-existing (`restore`'s baseline), inherited by the new commands; it's safe (refuses, never clobbers) but a real usability limitation. The proper fix tracks **"expected current worktree content"** in `current_state` separately from "latest snapshot." Recorded because the lesson generalizes: **a dirty-check is only meaningful against the content the worktree is *expected* to hold, not the most-recently-*saved* content** — they diverge the moment a non-save op (restore/checkout/undo) materializes a different tree. Also deferred: materialize-before-op-log-record ordering (the worktree-vs-ledger analog of store-before-DB), and undo's snapshot-chain-vs-op-log-rewind semantics. See [[native-commit-objects-base-anchoring-and-the-new-objectkind-gc-reachability-coupling-2026-05-30]] §1's "cut the slice where the reader lives" — undo is where op-restore semantics get load-bearing, and slice 3's v0 cut (single-step, snapshot-chain) is honest but leaves the op-log-rewind model for a follow-up.

## See also

- [[native-commit-objects-base-anchoring-and-the-new-objectkind-gc-reachability-coupling-2026-05-30]] — slice 2: defined the commit format + ref store this slice writes to; §1 deferred commit-on-accept (resolved here in §1–2), §2 the gc-reachability coupling (extended here in §6), §5 the mode-blind diff (extended to symlinks here in §7), §6 the schema-head grep gate (the 5→6 fan-out + moving HEAD+1 stamp, executed cleanly this slice).
- [[crash-correctness-advisory-lock-and-atomic-restore-2026-05-29]] — the store-before-DB + crash-atomic + acquire-once discipline §1's commit write, HEAD advance, and symlink materialize inherit verbatim.
- [[sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29]] — the `FnMut`-re-runs-per-retry / mint-inside / loser-is-orphan mechanics §1 depends on.
- [[schema-migration-reconciliation-and-typed-error-contract-2026-05-29]] — the `006` additive-ALTER tolerance + the `NativeHistoryCorrupt` 23→24 typed-error fan-out; the "fix wired to dead code" trap (the code-review caught the inverse — *correct* code wired to no test: doctor's dangling-parent/tree + `evidence_digest` were untested until the gate added tests that confirmed they work).
- [[native-worktree-walker-ignore-engine-and-index-vs-filesystem-divergence-2026-05-30]] — slice 1: the symlink `file_type` trap §7's capture path resolves, and prove-before-delete §4 reused for object-kind headers.
- [[write-binding-verification-and-content-backend-isolation-2026-05-29]] — S1 (path-free errors; the one residual is the pre-existing `materialize_tree`/`sync_dir` path-in-context, NER-143) and S2 (policy-excluded materialization) preserved across the new commands.
