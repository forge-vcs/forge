---
title: "feat: Phase 8 Slice 5 - pack storage, retention, budget warnings, and scale benchmarks"
status: completed
date: 2026-06-08
origin: docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md
---

# Phase 8 Slice 5: Pack Storage, Retention, Budget Warnings, and Scale Benchmarks

## Problem Frame

Forge now has a native loose-object store, native hunk diff, merge/conflict resolution, real doctor-gated GC deletion, and physical attempt workspaces. The remaining Phase 8 scale gap is operational: an agent fleet can generate hundreds of snapshots, many repeated blobs, and large command outputs. Loose objects are correct and inspectable, but they do not provide the storage economics, bounded-growth signal, or large-file behavior the PRD calls out for v0.

S5 should make native storage scale without weakening the properties S1-S4 established:

1. Packed objects must still validate as the same `f1:` object IDs. Packing is a storage layout change, not a new identity scheme.
2. GC/doctor must understand packed and loose objects consistently.
3. Retention and storage-budget behavior must be visible and machine-readable, but v0 must not introduce automatic eviction.
4. Large-file handling and status/diff performance need measured gates, not vague "faster" claims.

## Source Requirements Trace

- R30: Add packfile/delta/compression with an audited crate, with a benchmark corpus of at least 500 objects / 50 MB. The packed store should be measurably smaller than loose and Git, with a target of roughly <= 60% of loose bytes.
- R31: Replace whole-file reads on the native snapshot/restore path where needed so a multi-GB file can snapshot/restore without OOM.
- R32: Add a working-tree index/status cache only if it has a concrete Phase 8 consumer and metric; otherwise explicitly defer it.
- R33: Add retention policy and storage-budget reporting. The storage budget is warning/report only, not automatic eviction.
- R34: Packed-object reads still perform `f1:` verify-on-read.
- AE6: Objects stored in a pack still validate through `f1:` verify-on-read.
- Success Criteria: A simulated fleet run followed by GC returns `.forge` to a bounded size; packed+compressed store is smaller than loose/Git; multi-GB file snapshot/restore does not OOM; status/diff stay fast on a large synthetic tree; verify-on-read passes through the pack layer.

## Scope

In scope:

- Native pack files and pack indexes under `.forge/packs/`.
- Read path support for loose and packed native objects.
- A pack creation path that safely promotes old/unreachable-or-deduplicated loose objects into pack storage without changing object IDs.
- Doctor and GC awareness of packed objects.
- Storage accounting surfaced through `doctor`, `gc --dry-run`, and command warnings.
- Retention policy configuration and visible storage-budget warnings.
- Benchmarks/dogfood harnesses for storage size, large objects, and large-tree diff/status.
- A benchmark-gated decision on whether to implement a persistent working-tree status cache in S5.

Out of scope:

- Wire protocol, clone/fetch/push/pull, remote pack negotiation, or signed exchange. Those are Phase 9.
- Native Git pack format compatibility. Forge packs may be Git-inspired but are not Git packfiles.
- Automatic background GC or automatic eviction when a budget is exceeded.
- Deleting ledger rows, evidence rows, decision rows, or proposal rows.
- A public automatic snapshot feature. Automatic snapshots remain blocked until retention behavior is proven.
- A custom compression algorithm. Compression must come from a small, maintained, audited crate.

## Design Decisions

### 1. Preserve Object Identity Through a Self-Verifying Pack Layer

Packed storage must keep the current object identity invariant: `ObjectId::new(kind, payload)` remains the address, and `read_object(id)` returns the payload only after verifying the stored preimage. The pack file should store the same framed bytes that loose slice-3 objects store today:

- `forge-object\n`
- kind
- schema version
- payload length
- payload bytes

The pack index maps `ObjectId` to `{pack_id, offset, framed_len, compressed_len, checksum}`. A read seeks to the frame, decompresses if needed, checks `sha256(frame) == id.digest`, checks the embedded kind matches `id.kind`, and returns the parsed payload.

Rationale: This satisfies R34 without a second identity system. It also lets loose and packed objects share parser and hash verification code.

### 2. Use a Forge Pack Format, Not Git Pack Compatibility

Forge should not write native Git packfiles in S5. The native store already has `f1:` object IDs and tree/commit payloads that are not Git object IDs. A Forge pack should be purpose-built for native objects and Git interop should remain in `forge-export-git`.

Recommended path:

- Pack data file: `.forge/packs/<pack_id>.fpack`
- Pack index file: `.forge/packs/<pack_id>.fidx`
- Temp files under `.forge/tmp`, then fsync and atomic rename.
- Pack index includes enough metadata to enumerate object IDs without scanning or decompressing the whole pack.

Rationale: Git compatibility at the pack layer would force identity translation and distract from the real invariant, which is `f1:` verify-on-read.

### 3. Make Packing Additive Before Deleting Loose Copies

The first implementation should support read-through packed objects before it deletes any loose copy. The safe sequence is:

1. Write pack and index durably.
2. Teach reads, doctor, and GC to see both loose and packed objects.
3. Only then allow GC/pack compaction to remove loose duplicates when the packed copy verifies.

Rationale: This keeps store-before-DB style safety. A crash after pack write but before loose delete over-retains; it never strands a reference.

Loose objects remain authoritative while they exist. If the loose path exists but fails frame/hash/kind verification, `read_object` must fail closed and surface corruption instead of silently falling back to a valid packed duplicate. A packed duplicate can be used after the corrupt loose copy is repaired or deleted by an explicit verified repair/GC path. This preserves the current corruption signal instead of hiding disk damage behind redundancy.

### 4. Retention Defines Protection, Budget Defines Warning

Retention and storage budget should be separate:

- Retention policy determines which objects/workspaces are protected from deletion.
- Storage budget reports when `.forge` exceeds a threshold.
- Budget overflow emits non-blocking `warnings[]` on mutating commands and appears in `doctor` and `gc --dry-run`.
- No command should auto-delete merely because a budget was crossed.

Rationale: Agents need a machine-readable signal that storage pressure exists, but automatic eviction is too risky before hosted policies and remote sync exist.

### 5. Gate the Working-Tree Index by a Measured Consumer

R32 is not automatically accepted as a broad persistent index. S5 has two real consumers:

- `forge save` / native snapshot changed-path computation on large trees.
- `forge diff --working` on large trees.

Unit 1 must establish benchmark baselines for these surfaces. If large-tree no-op or small-edit runs miss the thresholds below, implement a minimal native status cache. If they pass, explicitly defer a persistent working-tree index out of Phase 8 and document the measured reason.

Proposed thresholds for the benchmark corpus:

- 50k files, 1 GB total, no-op native save changed-path detection: <= 2 seconds warm.
- 50k files, one-file edit, `diff --working`: <= 2 seconds warm.
- Cache rebuild after a dirty/corrupt cache: deterministic and path-policy-correct.

These are planning targets for local benchmark evidence, not brittle CI gates. CI should run smoke-scale checks and leave full-scale timing to explicit dogfood/benchmark runs.

Rationale: A cache without a measured consumer adds invalidation risk and new on-disk state. A benchmark-gated cache keeps scope honest while still honoring the Phase 8 success criterion if the current walker is too slow.

### 6. Treat Large-File Streaming as a Separate Correctness Unit

Packing/compression reduces stored size, but it does not by itself solve whole-file memory use. The native snapshot path currently reads regular files into memory before writing blob objects, and restore reads blob payloads into memory before writing files. S5 should introduce streaming APIs only where the large-file benchmark proves current behavior cannot meet the memory target.

Rationale: Streaming touches the most durability-sensitive path in the native store. It should be tested independently from pack compaction.

## Implementation Units

### Unit 1: Storage Accounting and Scale Benchmarks

Goal:

Establish reproducible size, memory, and latency baselines before changing storage layout. This unit creates the measurement harness that later units must improve.

Files:

- Create: `scripts/bench-native-storage.sh`
- Create: `crates/forge-cli/tests/forge_storage_budget.rs`
- Modify: `scripts/ci.sh` only if a short deterministic benchmark should enter CI
- Reference: `crates/forge-content-native/src/lib.rs`
- Reference: `crates/forge-store/src/lib.rs`

Approach:

- Add a deterministic shell benchmark that creates:
  - A 500+ object / 50+ MB corpus with repeated content.
  - A large-tree corpus with 50k small files.
  - A large-file corpus using sparse files where supported, with a fallback smaller corpus for CI.
- Report loose `.forge` size, comparable Git object size, native snapshot time, native restore time, and native `diff --working` time.
- Keep heavy benchmarks opt-in for local dogfood; CI should run only smoke-scale checks unless runtime stays comfortably small.
- Add storage-accounting helpers that compute `.forge` byte usage by category: loose objects, packs, DB, temp, worktrees, evidence outputs.

Test Scenarios:

- Storage accounting excludes non-existent categories and reports zero instead of failing.
- Accounting reports loose native objects after a native save.
- Benchmark harness works on a temporary repo without network access.
- Heavy benchmark can be skipped explicitly in CI while smoke benchmark still exercises the code path.

Verification:

- `rtk bash scripts/bench-native-storage.sh --smoke`
- `rtk cargo test -p forge-cli --test forge_storage_budget`

### Unit 2: Pack File and Pack Index Read Path

Goal:

Add a native pack representation and read-through support while leaving loose objects intact.

Files:

- Modify: `crates/forge-content-native/src/lib.rs`
- Create: `crates/forge-content-native/src/pack.rs` if the implementation would otherwise bloat `lib.rs`
- Modify: `crates/forge-content-native/Cargo.toml`
- Modify: root `Cargo.toml`

Approach:

- Add a small compression dependency after checking crate maintenance and license compatibility. Prefer a widely used crate with streaming encoder/decoder support.
- Define `PackIndex` and `PackEntry` as versioned serializable structures.
- Add pack discovery under `.forge/packs`.
- Update `NativeObjectStore::read_object` to:
  1. Try the loose object path first.
  2. If the loose path exists but fails verification, fail closed and report corruption.
  3. Fall back to pack index lookup only when the loose object is absent.
  4. Verify the framed bytes hash and kind before returning payload.
- Update `all_object_ids` to enumerate both loose and packed object IDs, deduping in a `BTreeSet`.
- Keep `write_object` writing loose objects in this unit.
- Extend doctor to validate every discovered pack index entry against its pack frame, including offset bounds, compressed length, checksum, object id hash, and kind.

Test Scenarios:

- A manually written pack containing blob/tree/commit objects is readable through `read_object`.
- Corrupt compressed bytes fail closed with an opaque object-id error, not a filesystem path.
- A wrong object kind in the frame is rejected.
- `all_object_ids` returns both loose and packed objects, deduping duplicates.
- Loose object wins over pack duplicate only after verification; a corrupt loose duplicate fails closed and is reported instead of masking corruption by reading a valid pack duplicate.
- Doctor reports corrupt pack/index entries, and GC refuses pack or loose duplicate deletion while pack validation issues are present.

Verification:

- `rtk cargo test -p forge-content-native pack`
- `rtk cargo test -p forge-content-native all_object_ids`

### Unit 3: Safe Pack Creation and Loose Duplicate Reclamation

Goal:

Add a command path or internal store operation that creates packs from eligible loose native objects and later permits GC to delete loose duplicates only when a verified packed copy exists.

Files:

- Modify: `crates/forge-content-native/src/lib.rs`
- Modify: `crates/forge-store/src/lib.rs`
- Modify: `crates/forge-cli/src/main.rs`
- Modify: `crates/forge-cli/src/schema.rs`
- Create/modify: `crates/forge-cli/tests/forge_pack_gc.rs`

Approach:

- Add a pack planning operation that selects old loose objects according to retention and reachability.
- Write pack files with the same durability pattern as object writes: temp file, file fsync, atomic rename, parent directory fsync.
- Add a dry-run JSON surface. Preferred shape is additive under `gc --dry-run` unless a separate `forge pack` command is clearly cleaner after implementation discovery.
- Delete loose duplicates only after:
  - Pack index exists.
  - The packed frame verifies against the same object id.
  - The loose object is not the only verified copy.
- Keep individual object deletion out of pack files for v0. A pack can be reclaimed only when every object in it is unreachable and outside the protection window.
- Track enough pack-entry time metadata for conservative retention. Recommended shape: pack index entries include `packed_at_ms` and, when available, the source loose object's `loose_mtime_ms`. Retention uses the most conservative protected interpretation when metadata is missing or ambiguous.

Test Scenarios:

- Pack creation over-retains on crash before loose deletion.
- After pack creation, reads succeed with loose copies present.
- After deleting loose duplicates, reads still succeed from pack.
- GC never deletes a pack that contains any reachable or protected object.
- GC can delete a pack whose every object is unreachable and outside the protection window.
- Packed objects with missing or ambiguous age metadata remain protected rather than being treated as old.
- Plan digest changes when pack/loose deletion candidates change.

Verification:

- `rtk cargo test -p forge-cli --test forge_pack_gc`
- Existing `forge_doctor_gc` coverage still passes.

### Unit 4: Retention Policy and Storage-Budget Warnings

Goal:

Make retention and budget visible in machine-readable output without adding automatic eviction.

Files:

- Modify: `crates/forge-store/src/lib.rs`
- Conditional if policy is DB-backed: `crates/forge-store/src/migrations.rs`
- Conditional if policy is DB-backed: `crates/forge-store/migrations/010_storage_policy.sql`
- Modify: `crates/forge-cli/src/main.rs`
- Modify: `crates/forge-cli/src/schema.rs`
- Create/modify: `crates/forge-cli/tests/forge_storage_budget.rs`
- Modify: `scripts/e2e-eval.sh`

Approach:

- Add a small repo-local policy source. Prefer additive DB-backed defaults over ad hoc config parsing unless implementation discovery finds an existing config pattern.
- If a DB-backed policy table is added, use migration `010_storage_policy.sql` and update every schema-head assertion that currently expects the S4 head (`9`), including `scripts/e2e-eval.sh` and migration tests.
- Defaults:
  - GC protection window remains at least 7 days.
  - Storage budget warning threshold is conservative and can be overridden for tests.
  - Automatic eviction remains disabled.
- Add storage pressure to:
  - `doctor` report.
  - `gc --dry-run` data.
  - Top-level `warnings[]` for mutating commands when `.forge` exceeds budget.
- Keep warnings path-free: category names and byte counts are allowed; absolute paths are not.

Test Scenarios:

- A repo below budget emits no storage warning.
- A repo above budget emits a top-level warning on a mutating command but the command still succeeds.
- `doctor` reports storage pressure without setting corruption fields.
- `gc --dry-run` reports budget status and retention window.
- Test-only threshold override does not leak into normal schema as an unstable public contract.

Verification:

- `rtk cargo test -p forge-cli --test forge_storage_budget`
- `rtk bash scripts/e2e-eval.sh`

### Unit 5: Large-File Streaming for Native Snapshot and Restore

Goal:

Remove whole-file memory cliffs from native blob snapshot/restore where the benchmark proves they matter.

Files:

- Modify: `crates/forge-content-native/src/lib.rs`
- Modify: `crates/forge-content-native/Cargo.toml` if streaming compression requires it
- Create/modify: `crates/forge-cli/tests/forge_large_file.rs`
- Modify: `scripts/bench-native-storage.sh`

Approach:

- Add streaming write/read helpers that hash, frame, optionally compress, and persist large blob payloads without retaining the whole payload in memory.
- Preserve the existing object preimage identity. The digest still covers the full framed preimage.
- Keep small-file paths simple if doing so avoids unnecessary complexity; choose a threshold based on benchmarks.
- Restore large blobs with streaming writes and the existing crash-atomic temp/rename/fsync pattern.

Test Scenarios:

- Large blob write produces the same object id as the non-streaming reference implementation on a manageable fixture.
- Large blob restore round-trips bytes and mode.
- Crash injection before final rename leaves no referenced missing object.
- Secret-risk paths remain excluded before streaming starts.
- Memory smoke test does not allocate a buffer proportional to the large file size.

Verification:

- `rtk cargo test -p forge-cli --test forge_large_file`
- `rtk bash scripts/bench-native-storage.sh --large-file-smoke`

### Unit 6: Benchmark-Gated Working-Tree Status Cache

Goal:

Implement a minimal native status cache only if Unit 1 shows current walker/diff performance misses the S5 thresholds. Otherwise document the deferral explicitly and keep the plan's exit criteria tied to measured data.

Files if implemented:

- Modify: `crates/forge-content-native/src/lib.rs`
- Create: `crates/forge-content-native/src/status_cache.rs`
- Modify: `crates/forge-store/src/lib.rs` if cache metadata needs repo-level state
- Create/modify: `crates/forge-cli/tests/forge_native_status_cache.rs`

Approach if implemented:

- Cache path fingerprints for the effective worktree using repo-relative paths, file type, size, mtime, executable bit, and content hash where needed.
- Treat the cache as derived state: corrupt or stale cache must rebuild, never fail a user command unless the underlying filesystem read fails.
- Exclude policy-ignored paths and workspace markers exactly as snapshot does.
- Use atomic write/rename/fsync for cache files.
- Invalidate on policy/config changes, native object format changes, and worktree marker changes.

Deferral criteria:

- Unit 1 benchmark meets the large-tree thresholds without a cache.
- No user-facing command would consume the cache in S5 beyond speculative future speedups.
- The plan records the measured result and leaves a follow-up for Phase 8+.

Test Scenarios if implemented:

- No-op save uses cache and reports no changed paths.
- One-file edit invalidates only that path's fingerprint.
- Delete/add/rename-like changes are reported correctly.
- Corrupt cache rebuilds and produces the same result as a cold scan.
- Policy-excluded and secret-risk paths never enter the cache or JSON output.

Verification:

- `rtk cargo test -p forge-cli --test forge_native_status_cache`
- `rtk bash scripts/bench-native-storage.sh --large-tree-smoke`

### Unit 7: S5 Dogfood and Final Phase 8 Gate

Goal:

Prove S5 under an agent-fleet-shaped workload and keep the full native loop green.

Files:

- Modify: `scripts/dogfood-typescript-native.sh`
- Create: `scripts/dogfood-native-storage-scale.sh`
- Modify: `scripts/ci.sh` only for short smoke checks

Approach:

- Extend dogfood to:
  - Create many attempts/snapshots.
  - Pack eligible native objects.
  - Delete loose duplicates through the safe GC/pack path.
  - Confirm reads still pass through packed storage.
  - Confirm storage pressure warnings appear when a low test budget is configured.
- Add a separate storage-scale dogfood script for heavier local runs.

Test Scenarios:

- Two workspace attempts still run independently after packing.
- Accepted proposals, decisions, checkout targets, and workspace snapshots remain readable after loose duplicate deletion.
- `doctor` remains clean after pack creation and GC.
- `gc --dry-run` plus `gc --yes --plan-digest` behaves deterministically with packed objects.
- Storage budget warnings are visible and non-blocking.

Verification:

- `rtk bash scripts/dogfood-typescript-native.sh`
- `rtk bash scripts/dogfood-native-storage-scale.sh --smoke`
- Full workspace gate.

## Sequencing

1. Land Unit 1 first. It defines the benchmark corpus, storage accounting, and the R32 index/cache decision data.
2. Land Unit 2 next. Read-through packed objects must work before any loose copy is deleted.
3. Land Unit 3 after the read path is proven. This introduces pack creation and duplicate reclamation.
4. Land Unit 4 once accounting and pack visibility exist. Budget warnings should be based on real categories.
5. Land Unit 5 independently if benchmarks show memory cliffs. Do not mix streaming correctness with pack deletion in the same commit.
6. Land Unit 6 only if Unit 1 justifies it. Otherwise record the deferral and measured result.
7. Land Unit 7 last as dogfood and acceptance coverage.

## Test Strategy

Focused tests:

- `crates/forge-content-native` unit tests for pack format, frame verification, compression corruption, and packed `all_object_ids`.
- `crates/forge-cli/tests/forge_pack_gc.rs` for CLI-level pack creation, GC, and doctor integration.
- `crates/forge-cli/tests/forge_storage_budget.rs` for budget warnings and retention reporting.
- `crates/forge-cli/tests/forge_large_file.rs` for streaming round-trips and memory smoke tests.
- `crates/forge-cli/tests/forge_native_status_cache.rs` only if Unit 6 implements the cache.

System tests:

- `rtk cargo fmt --all --check`
- `rtk cargo test --workspace`
- `rtk cargo clippy --workspace --all-targets -- -D warnings`
- `rtk bash scripts/e2e-eval.sh`
- `rtk bash scripts/dogfood-typescript-native.sh`
- `rtk bash scripts/dogfood-native-storage-scale.sh --smoke`

Benchmark/dogfood:

- `rtk bash scripts/bench-native-storage.sh --smoke`
- Optional heavy local run: `rtk bash scripts/bench-native-storage.sh --full`

## Risks and Mitigations

- Pack index corruption could hide live objects. Mitigation: pack index is derived from a self-verifying pack; doctor revalidates index entries against pack frames and fails closed on mismatch.
- Compression bugs could break verify-on-read. Mitigation: decompressed frame must hash to the requested object id; corruption is detected before payload return.
- Deleting loose duplicates too early could lose data. Mitigation: pack read path lands first; duplicate deletion requires a verified packed copy and remains GC-plan/digest gated.
- Storage budget warnings could become noisy. Mitigation: warnings are category-level and only emitted when the repo exceeds a configured threshold.
- Working-tree cache invalidation could create wrong changed paths. Mitigation: implement only if benchmarks justify it; cache is derived and rebuildable, with cold-scan parity tests.
- Large-file streaming could weaken crash-atomic restore. Mitigation: preserve temp-file, fsync, rename, and parent-fsync ordering; add crash-injection tests around streaming writes.

## Deferred to Implementation

- Final compression crate selection and dependency version. The implementation must record why the crate is acceptable under the PRD's audited-dependency rule.
- Exact pack binary layout. The plan requires a versioned layout, frame verification, and random access; implementation can choose compact binary or canonical JSON index as long as tests pin it.
- Public CLI shape for pack creation. Prefer additive `gc --dry-run`/`gc --yes` integration unless implementation discovery shows a separate `forge pack` is clearer.
- Whether Unit 6 implements or defers a persistent working-tree status cache. Unit 1 benchmark results decide this.

## Acceptance

S5 is complete when:

- Packed native objects can be read with the same `f1:` verify-on-read guarantees as loose objects.
- Loose duplicate deletion after packing does not break any accepted proposal, decision, checkout, snapshot, proposal revision, or active workspace.
- `doctor` and `gc` report packed and loose objects consistently.
- Storage budget warnings appear in `warnings[]`, `doctor`, and `gc --dry-run` without blocking commands or triggering automatic deletion.
- The benchmark corpus shows packed+compressed storage is measurably smaller than loose and Git, or the PR explicitly documents why the measured corpus does not support the target.
- Large-file snapshot/restore avoids whole-file memory cliffs on the smoke benchmark.
- The R32 working-tree cache is either implemented with measured improvement and parity tests, or deferred with benchmark evidence.
- Full verification and dogfood pass.
