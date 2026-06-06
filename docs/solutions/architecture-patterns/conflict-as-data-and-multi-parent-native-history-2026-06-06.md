---
title: "Conflict-as-data and multi-parent native history: the S2a substrate for Forge merges"
date: 2026-06-06
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: conflict-data-native-history
severity: high
applies_when:
  - Persisting merge or stale-base conflicts for agent consumption
  - Adding JSON read surfaces for conflict payloads
  - Making native history readers safe for merge commits and diamond ancestry
  - Extending operation integrity digests with child records
symptoms:
  - Stale-base or merge conflicts are only visible as command failures
  - Native history traversal rejects valid merge commits or diamond ancestry
  - Conflict JSON risks exposing raw paths or inline content
tags: [conflict-as-data, path-conflicts, native-history, multi-parent, redaction, integrity, phase-8, ner-139]
---

# Conflict-as-data + multi-parent native history

Phase 8 Slice 2a turned stale-base divergence into durable conflict data and made native history readers merge-aware before the merge engine existed. The key lesson is that conflict storage and history traversal are prerequisites, not cleanup tasks: if either is vague, a later merge engine has nowhere safe to write and valid merge commits can brick normal commands.

## 1. Conflict rows need refs and child records, not only compatibility JSON

The original `conflict_sets` table was a metadata stub. S2a kept that compatibility surface but made the real model explicit:

- `conflict_sets` carries base, ours, theirs content refs, resolver backend, status, generated operation, and an integrity hash.
- `path_conflicts` carries per-path kind, side paths/statuses/modes/refs, resolution ref, status, and an opaque path fingerprint.
- `paths_json` remains compatibility baggage, not the source of truth for future merge code.

This lets S2b write real merge classifications without changing the outer shape again.

## 2. Internal storage and JSON egress have different secrecy rules

SQLite may need raw paths so a future resolver can update the right rows. JSON read surfaces do not. `forge conflict list/show` returns ids, refs, kinds, statuses, fingerprints, counts, and warnings, but not raw paths or blob excerpts. The `CONFLICT_SET_NOT_FOUND` typed error also avoids echoing a path-like selector in either details or display text.

The durable rule for future conflict work: treat conflict hunks as machine-visible egress. Redact at the read boundary even if the content was not captured as evidence.

## 3. Operation-owned conflicts must hash child rows

S2a's failed-operation path inserts the operation, view, conflict set, and path conflicts in one `IMMEDIATE` transaction. The operation hash links to the conflict-set digest, and the conflict-set digest includes ordered path-conflict digest inputs. That makes tampering with a child path row visible to `doctor`, not only tampering with the parent row.

The pattern generalizes to merge-generated conflicts: prepare the complete parent-plus-children digest before insert, then insert every row in one transaction.

## 4. Multi-parent traversal needs active-path cycle detection, not repeat equals cycle

Diamond ancestry revisits a commit through two valid paths. A simple visited-set-as-cycle-detector falsely marks that as corruption. S2a fixed native history verification by separating traversal state:

- `visiting` or active-path state detects a true cycle.
- `visited` deduplicates already-verified commits and accepts diamonds.

Use the same distinction anywhere future merge commits are walked.

## 5. Reconcile and log must consume all parents before merge commits ship

Native commit objects already had `parents: Vec<String>`, but a reader that follows only `parents.first()` is still linear-history code. S2a updated `reconcile_native_head` and `native_log` so a merge tip whose prior HEAD is reachable only through the second parent is valid. This matters because reconcile runs on the shared mutating command path; a first-parent-only bug can brick unrelated commands after the first merge commit.

## See also

- `docs/plans/completed/2026-06-06-017-feat-phase-8-slice-2a-conflict-data-plan.md`
- `docs/plans/2026-06-06-018-feat-phase-8-slice-2b-native-merge-resolution-plan.md`
- `docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md`
- `docs/solutions/architecture-patterns/commit-on-accept-ordering-ledger-authoritative-reconcile-and-the-last-hidden-git-dependency-2026-05-31.md`
