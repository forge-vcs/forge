---
title: "The expected-content dirty-check, its OR-target crash-safety hinge, and the git-plumbing egress double-vector (NER-142 + NER-143)"
date: 2026-05-31
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: worktree-dirty-check-crash-safety-and-git-egress-path-parsing
severity: high
applies_when:
  - A materialize-then-record op (restore/checkout/undo) clobbers the worktree in one step and records the op in a separate transaction — the worktree-vs-ledger analog of store-before-DB
  - A worktree dirty-check must refuse genuine unsaved work but allow chained navigation — comparing against "latest saved" instead of "expected current content" breaks the chain
  - Adding a tracked-state column whose write must be crash-consistent with an op-log advance under a CAS that can lose the race
  - Parsing git plumbing output (ls-tree/diff/ls-files) for PATHS that are then security-filtered — C-quoting and pathspec-magic are two distinct escape vectors
  - Enumerating "every op that does X" (every materializing op, every secret-egress parse site) where missing one silently reintroduces the bug
tags: [worktree-dirty-check, expected-content-ref, materialize-then-record, crash-safety, refuse-never-clobber, or-target-heal, cas-loss, git-c-quote, pathspec-literal, secret-egress, enumerate-every-op, ner-142, ner-143]
---

# Expected-content dirty-check crash-safety + the git-plumbing egress double-vector (NER-142 + NER-143)

Two consolidation tickets after Phase 7. NER-142 closed a secret leak in the git-export path; NER-143
made the worktree dirty-check meaningful for chained navigation. The non-obvious learnings, the kind the
doc-review and code-review gates surfaced before they shipped.

## 1. A dirty-check is only meaningful against EXPECTED content, never latest-SAVED content

`restore`/`checkout`/`undo` refused a dirty worktree by comparing it against the latest *saved* snapshot.
After `undo` materialized snapshot A, the latest saved snapshot was still B — so the next nav command saw
`worktree(A) != latest-saved(B)` and spuriously failed `DIRTY_WORKTREE`. "undo twice" was impossible
without an intervening `save`. The fix is a new `current_state.expected_content_ref` (the tree the last
materializing op put in the worktree), tracked separately from the latest snapshot. The general rule:
**a dirty-check refuses against the content the worktree is *expected* to hold — they diverge the moment a
non-save op (restore/checkout/undo/attach) materializes a different tree than was last saved.**

## 2. The crash-safety hinge is the OR-target clause, NOT "write expected before materialize"

The materialize (clobber the worktree) and the record (advance the op-log + set `expected_content_ref`)
are two transactions — the worktree-vs-ledger analog of store-before-DB. The doc-review's first sketch was
"write `expected_content_ref` BEFORE materialize so a re-run is idempotent." **That sketch does not
self-heal**, and catching it required walking the re-run by hand: a re-run hits the *dirty-check first*,
sees `worktree(old) != expected(new)`, and refuses — it never reaches the "materialize overwrites" step.
Both naive single-orderings have a brick window.

What actually closes it: set `expected_content_ref` in the **record txn** (atomic with the op-log advance),
and make the dirty-check pass on **`worktree == expected` OR `worktree == target`**. The OR-target clause
is the hinge: after a materialize-then-crash before the record commits, the worktree holds `target` while
`expected` is the stale prior ref — the re-run heals via `worktree == target` (re-materialize is a no-op),
while a genuine unsaved edit matches neither and is still refused. This unifies the fix with the deferred
"op-log intent row" (R5): `expected_content_ref` IS the pre-materialize intent marker, so no two-phase
op-log row is needed — which matters because a mutable-status op row would break the NER-136 tamper-evident
`content_hash` chain. **Lesson: when a fix's correctness depends on a re-run healing, trace the re-run's
FIRST gate, not just its happy path — the gate that refuses before the heal is where naive orderings die.**

## 3. "Refuse, never clobber" is the load-bearing invariant — verify it survives every fix

The dirty-check's value is that it is safe-by-refusal: it never clobbers unsaved work. Every change had to
preserve that. The code-review adversarial + reliability personas attacked "can the new baseline ever PASS
while genuine unsaved work would be clobbered?" and could not break it (`matches_target` is safe because
materialize-over-target is a no-op over exactly the path set restore touches; `matches_expected` only
passes on a saved tree; the NULL fallback is the stricter latest-snapshot baseline). The found bugs were
all the *safe* direction — a **false-refuse** (spurious `DIRTY_WORKTREE`), an availability bug, never a
clobber. That distinction set their severity: a false-refuse is P1, a clobber would be P0. **When hardening
a safety gate, classify each new failure as refuse-side (annoying, safe) vs clobber-side (data loss) — they
are not the same severity even when they share a root cause.**

## 4. "Every op that materializes" is an enumeration — the fifth one is the one you miss

R1 set `expected_content_ref` in the four obvious recorders (save/restore/checkout/undo). The code-review
(correctness + reliability, independently) found a **fifth**: `attempt attach` also restores a tree into the
worktree, but its recorder didn't set the baseline — so an attach-then-nav spuriously refused. It was masked
because every existing attach test inserted a `restore` between attach and undo, which repaired `expected`.
This is the same shape as NER-142's audit ("every `ls-tree`/`diff`/`ls-files`-then-parse site") and the
slice-3 "grep every git call site" lesson: **when a fix is "do X in every op that does Y," the enumeration
is never complete on the first pass — grep for the action (here: `restore_snapshot` call sites), not the
ops you already listed.** And: **a test that inserts a repair step between the trigger and the assertion can
hide the very bug it appears to cover** — the masking `restore` made the cross-attempt test pass for the
wrong reason.

## 5. The git-plumbing egress has TWO path-escape vectors: C-quoting (read) AND pathspec-magic (write)

NER-142's stated bug was the read side: `git ls-tree --name-only` without `-z` C-quotes a special-byte path
(`.env.café` → `".env.caf\303\251"`), and the quoted form slips `is_secret_risk_path`, leaking the secret
into the published tree. Fix: add `-z`, split on NUL. But the adversarial pass found the `-z` fix made a
*second, latent* vector reachable on the **write** side: the drop loop fed the now-correctly-decoded path to
`git rm --cached --ignore-unmatch <path>` as a **pathspec**, and a name with a glob metacharacter
(`.env[prod]`) or a leading `:` is parsed as magic/glob that matches nothing — `--ignore-unmatch` swallows
the miss, so the secret was *reported excluded but still published*. Fix: `:(literal)` pathspec. **Lesson:
correctly decoding a path on the read side can ARM a previously-unreachable write-side vector — when you fix
"the path reaches the filter," also check "the path reaches the mutation," because git treats argv paths as
pathspecs (magic + glob), not literals, unless you say `:(literal)`.** Both vectors verified empirically in
scratch repos, not just reasoned about.

## See also

- [[commit-on-accept-ordering-ledger-authoritative-reconcile-and-the-last-hidden-git-dependency-2026-05-31]] — §8 framed the expected-content baseline (resolved here); the "grep every call site of the tool you claim independence from" lesson (§5) generalizes to §4 above.
- [[crash-correctness-advisory-lock-and-atomic-restore-2026-05-29]] — the store-before-DB + crash-atomic discipline the materialize-then-record split (§2) is the worktree analog of; per-file-atomic restore means a during-materialize crash leaves a partial tree that refuses safely (the deferred fourth crash point).
- [[sqlite-multiprocess-concurrency-and-idempotent-replay-2026-05-29]] — the `FnMut`-re-runs-per-retry / CAS-loss-rolls-back mechanics §2's atomic record-txn write depends on (set_expected_content_ref composes inside the same IMMEDIATE txn as the op-log CAS).
- [[write-binding-verification-and-content-backend-isolation-2026-05-29]] — S1 path-free errors (the materialize/sync `io::ErrorKind` rewrite in PR-A) and the refuse-before-materialize safety property continued here.
