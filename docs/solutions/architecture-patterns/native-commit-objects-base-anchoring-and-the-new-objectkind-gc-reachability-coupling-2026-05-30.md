---
title: "Native commit objects + backend-agnostic base anchoring: cut the slice where the reader lives, the new-ObjectKind↔gc-reachability coupling, and the mode-blind content-addressed diff"
date: 2026-05-30
category: architecture-patterns
module: forge-content-native
problem_type: architecture_pattern
component: native-commit-objects-ref-store-and-base-anchoring
severity: high
applies_when:
  - Adding a new content-addressed `ObjectKind` to a store that already has a garbage-collector / reachability scan
  - Reversing a backend's base anchor from a foreign id (git SHA) to a native id, with a downstream interop adapter that still needs the foreign id
  - A lazily-created "genesis" object is written as a side effect of a method that reads like a pure query (`current_base`)
  - Deciding whether to write a substrate object during the lifecycle now, or only define its format and defer writing to the slice that reads it
  - Bumping a schema-migration head and fanning out the version literal across a test tree (including a HEAD+1 stamp literal that moves)
  - A name-level diff is computed from content-addressed trees (blob ids are mode-blind)
  - A git plumbing command must be reproducible across machines (synthesized commit/tree SHAs)
tags: [native-commit-objects, ref-store, base-anchoring, objectkind, gc-reachability, content-addressing, mode-bit-diff, lazy-genesis, lock-invariant, slice-cut, descope, schema-head-fanout, grep-gate, deterministic-git-parent, migration-semicolon, ner-138]
---

# Native commit objects + backend-agnostic base anchoring (NER-138 Phase 7 slice 2)

Slice 2 gave the native content backend its own history substrate (a `Commit` `ObjectKind` + a ref store under `.forge/`), made `base_head` a native commit id instead of a git SHA, and turned `changed_paths` into a native tree diff — so a native `forge save` is git-binary-free for snapshot + base + changed_paths. These are the non-obvious learnings the doc-review and code-review gates surfaced.

## 1. Cut the slice where the *reader* lives, not where the format is defined

The instinct was "the slice that introduces the Commit object should also write commits during the lifecycle (commit-on-accept)." The doc-review gate killed that, and the reasoning generalizes:

- **A substrate object's *format* belongs in the slice that needs its anchor; *writing* it during the lifecycle belongs in the slice that reads/rewinds it.** Slice 2 needs a stable base anchor, which a single **genesis** commit provides. Writing *justified* commits at `accept` is only meaningful once `log`/checkout/`undo` exist to walk and rewind the DAG — that is slice 3. So slice 2 defines + unit-tests the full commit format (all justification fields) but only ever *writes* the genesis shape (empty parents, null justification).
- **The descope eliminated the slice's hardest correctness problem.** Commit-on-accept hashes the commit over a `decision_id` that `decide()` mints *inside* its `IMMEDIATE` transaction — so the object cannot be hashed before the txn it must precede (store-before-DB), and "re-run re-derives the identical content-addressed commit" is false because `decision_id`/timestamps are non-deterministic per retry. Genesis-only sidesteps all of it.
- **Two specialist reviewers independently flagging scope creep is a strong descope signal.** The adversarial and scope-guardian personas both concluded commit-on-accept's *only* slice-2 consumer would have been stale-base-after-accept (not a named exit criterion). When the personas whose job is exactly "is this in scope / will this break" converge from different angles, believe them.
- **The descope also resolved a forward-compat trap for free.** Genesis commits have no decider, so omitting `actor`/authored-time from the v1 hashed bytes creates no Phase-9-signing gap (a signature attests exact bytes; a later registry bump cannot retroactively bring earlier *justified* commits under signed/decider-bound provenance). Because slice 2 writes only genesis, there are no justified-but-unsigned-decider commits. The slice-3 design note now requires actor + authored-time in the first justified-commit format.

## 2. A new `ObjectKind` couples to the garbage collector's reachability roots

The sharpest bug the code-review gate caught: adding `Commit` to `all_object_ids` (the gc/doctor object enumeration) **without** teaching `gc_dry_run` about the ref-store HEAD made `forge gc --dry-run` report the genesis commit — which every attempt's `base_head` points at — as unreachable garbage.

**The principle: when you add an object kind reachable from a *new root*, the reachability scan's root set must grow to match.** gc seeded `reachable` only from DB `content_ref`s (snapshot/proposal trees). Commit objects are reachable from the **ref store HEAD**, a root gc didn't know existed. The fix is a `reachable_from_head()` that walks the HEAD commit DAG (commit → parents, plus each commit's tree) and seeds it into gc's reachable set.

It was inert *only* because gc is dry-run-only today — a latent data-loss trap that would have armed the moment slice-3+ grants gc deletion authority. **An "enumerate all objects" scan and a "compute reachable objects" scan are two halves of one invariant; a new kind that lands in the first must land in the second in the same change.**

## 3. A "read" that lazily writes is safe only behind a *verified* lock invariant

`current_base` (a method that reads like a pure query) lazily creates the genesis commit + sets HEAD on first call. That side-effect-in-a-reader is safe here, but only because of an invariant that had to be *verified*, not assumed:

- Every `current_base` caller (`start`, `attempt start/attach`, `accept`, `export branch`) is a mutating command holding the advisory lock (acquire-once); the lock-free `run` carve-out never reaches it — so two genesis commits cannot race.
- `save` (which also reaches the genesis path via `changed_paths`) requires an active attempt that only `start` creates, and `start` calls `current_base` first — so genesis is always captured at *start-time*, never over a mid-`save` dirty tree.

The fragility: this rests on **data dependencies, not an explicit guard**. A future command that calls `current_base`/`snapshot_worktree` without first requiring a start-created row would silently move genesis-establishment to an arbitrary worktree state. Surface invariants like this explicitly (doc comment + a test asserting the lock is held), because the next implementer won't re-derive them.

## 4. `base_head` must anchor on a commit, not a tree

The ROADMAP said "anchor on a native tree/snapshot id." A **tree** anchor is wrong: it changes on every worktree edit, so stale-base detection (which compares `current_base()` at accept-time against the stored `base_head`) would fire spuriously after any file change. A **commit** anchor is set at genesis and advances only when Forge records history (slice 3) — stable across edits, matching git HEAD semantics, and it gives slice 3 a real DAG to walk instead of retrofitting commits onto trees. `base_head` stays opaque `TEXT`; native-vs-git is detected by routing through the canonical `ObjectId::parse` (not a hard-coded `f1:commit:` string literal that a future format bump would silently desync).

## 5. A content-addressed name-level diff is mode-blind unless mode is in the key

`changed_paths` diffed the base tree against the worktree tree by comparing blob object ids. Blob ids are **content-only** — a `chmod +x` with unchanged content produces the *same* blob id, so an executable-bit change was invisible in `changed_paths` (a parity regression vs `git diff --name-only HEAD`, which lists a chmod). `changed_paths` is the per-attempt diff summary surfaced to reviewers, so a security-relevant mode change vanished from provenance. **Fix: key the flatten map on `(blob_id, mode)`, not `blob_id`.** Any diff built from content-addressed objects must fold in the metadata (mode, and later symlink-ness) the content hash deliberately excludes.

## 6. The schema-head fan-out grep gate catches what the enumeration misses

Bumping `schema_head` 4→5 ripples to every hard-coded version literal across the test tree. Two traps:

- **The enumeration is never complete.** The feasibility reviewer listed `forge_init.rs`/`forge_concurrency.rs`/`forge_migration_upgrade.rs` but missed `forge-store/tests/migrate.rs` entirely. A repo-wide grep gate is the backstop — trust the grep, not the list.
- **The HEAD+1 stamp literal *moves* and a grep-for-the-old-head can't find it.** "DB ahead of binary refuses" tests stamp `schema_head + 1` as a literal (`5` when head was 4). After the bump, version 5 is a *valid current* version the runner accepts, so the refusal test inverts and fails — and grepping for the old head literal `4` structurally cannot catch the literal `5`. **The grep gate must cover both the old head AND the prior HEAD+1 literal.**

(Related: the migration applier splits SQL naively on `;`, and the rule extends to **comments** — a semicolon inside a `--` comment breaks the split mid-statement. Reproduced live this slice.)

## 7. Git-export interop with a native base = a *deterministic* synthesized git parent

When `base_head` became a native commit id, `export branch` could no longer pass it to `git commit-tree -p <parent>`. The fix: resolve a native base to a synthesized git parent commit built from the base tree, with a **fully fixed environment** — fixed author/committer identity, `@0 +0000` date, fixed message, and `core.autocrlf=false` on *both* the `commit-tree` and the `add`/`write-tree` that builds the tree. Determinism is load-bearing: idempotent re-export reconciles by comparing the existing branch's parent against the freshly-synthesized one, so a non-deterministic SHA would turn a benign re-export into a spurious `BRANCH_EXISTS`. Within-environment determinism (the reconciliation case) is guaranteed; cross-machine determinism additionally depends on identical blob bytes (the autocrlf pin closes the line-ending vector; a future git default change remains a documented residual). Test determinism across **two fresh repos**, not two calls in one process, or the test can't see the drift it's guarding against.

## See also

- [[native-worktree-walker-ignore-engine-and-index-vs-filesystem-divergence-2026-05-30]] — slice 1: the walker this slice's base anchoring + changed_paths build on; §6 predicted that native base anchoring and native changed_paths are one unit (this slice).
- [[write-binding-verification-and-content-backend-isolation-2026-05-29]] — Phase 3: the confined git delegation this slice was allowed to reverse; S1 (no fs paths in `anyhow` context) and S2 (base_content_ref → a policy-excluded tree), both preserved here.
- [[schema-migration-reconciliation-and-typed-error-contract-2026-05-29]] — the split-on-`;` migration applier and the head-bump version-literal fan-out discipline this slice extended (semicolon-in-comment; the moving HEAD+1 literal).
- [[crash-correctness-advisory-lock-and-atomic-restore-2026-05-29]] — the store-before-DB + crash-atomic + acquire-once discipline the new commit objects and ref store inherit verbatim.
