---
title: "Native storage scale: pack after proof, stream at the loose boundary, and cache only measured diff pressure"
date: 2026-06-08
category: architecture-patterns
module: forge-store
problem_type: architecture_pattern
component: tooling
severity: high
applies_when:
  - Scaling the native Forge object store for many agent attempts
  - Adding pack, GC, retention, or budget behavior to content-addressed storage
  - Optimizing native diff/status for large working trees
  - Streaming large snapshot or restore payloads without changing object identity
symptoms:
  - Loose native objects grow without a bounded-storage signal
  - Large files require whole-file memory reads on snapshot or restore
  - Diff/status latency scales with total tree size after one small edit
  - Packed reads risk becoming a second object identity system
tags: [native-storage, packs, gc, retention, status-cache, streaming, phase-8]
---

# Native Storage Scale

Phase 8 Slice 5 made native storage scale for agent fleets without weakening the invariants established by loose native objects. The durable lesson is that packing, retention, streaming, and status caching are separate layers: each can improve operational behavior, but none should change the JSON envelope, object identity, or Git-backed compatibility path.

## 1. Forge Packs Are a Layout Change, Not a New Identity Scheme

Packed storage keeps the same framed native preimage used by loose objects. `ObjectId::new(kind, payload)` remains the address, and reads verify the stored frame before returning a payload. Git interoperability stays in `forge-export-git`; native packs are purpose-built Forge storage, not Git packfiles with identity translation hidden underneath.

The rule for future pack work is simple: if packing changes the object id, reader contract, or JSON envelope, it is the wrong layer.

## 2. GC Deletes Loose Duplicates Only After Packed Reads Are Proven

Real deletion is safe only when the replacement read path has already been written, verified, and indexed. S5 keeps deletion digest-gated and doctor-aware: build the pack, verify reads, then remove duplicate loose files. Retention and budget reporting are visible signals, not automatic eviction.

This keeps the high-risk operation local and auditable. A low-budget warning can tell an agent fleet to compact or clean up; it must not silently remove content outside the explicit GC path.

## 3. Stream Large Files at the Snapshot/Restore Boundary First

The first useful large-file win is on the loose snapshot/restore path, where whole-file reads create immediate memory pressure. Streaming still hashes the same framed preimage, so object ids remain stable. Small-file paths can stay straightforward, and legacy raw loose objects should continue to stream on restore so old repositories are not forced through a migration before they benefit.

Do not introduce a second large-object object kind until the existing boundary is proven insufficient.

## 4. Treat Status Cache as Rebuildable Derived State

The persistent status cache was justified only after a benchmark exposed the bottleneck. The cache lives under `.forge/status-cache.json`, can be rebuilt after corruption, and write failures do not fail the command. Signatures include size and mtime, plus Unix ctime/dev/inode where available. Policy-excluded paths never enter the cache, and blob overlays are hydrated only for paths that diff actually needs.

That keeps the cache a performance tool, not a source of truth. The source of truth remains the native tree plus working-tree bytes.

## 5. Dogfood the Failure Modes, Not Just the Happy Path

The storage dogfood harness needs to prove packed-read-after-loose-delete, retention behavior, low-budget warnings, and large-tree diff/status latency. A unit suite that only reads a packed object while its loose copy still exists is not enough.

Measured S5 evidence:

- 10k-file one-edit `diff --working` before cache: 3146 ms.
- 10k-file one-edit `diff --working` after cache: 361 ms.
- Default status-cache smoke after tuning: seed 123 ms, one-file diff 70 ms.
- Storage dogfood smoke: PASS=30 FAIL=0.
- Full S5 verification: workspace tests 410 passed, e2e PASS=95 FAIL=0, TypeScript dogfood PASS=44 FAIL=0, storage dogfood PASS=30 FAIL=0.

## See Also

- `docs/plans/completed/2026-06-08-020-feat-phase-8-slice-5-pack-index-retention-plan.md`
- `docs/plans/completed/2026-06-07-019-feat-phase-8-slice-4-gc-worktrees-plan.md`
- `docs/plans/completed/2026-06-06-018-feat-phase-8-slice-2b-native-merge-resolution-plan.md`
- `docs/solutions/architecture-patterns/native-commit-objects-base-anchoring-and-the-new-objectkind-gc-reachability-coupling-2026-05-30.md`
- `docs/solutions/architecture-patterns/commit-on-accept-ordering-ledger-authoritative-reconcile-and-the-last-hidden-git-dependency-2026-05-31.md`
- `docs/solutions/architecture-patterns/conflict-as-data-and-multi-parent-native-history-2026-06-06.md`
