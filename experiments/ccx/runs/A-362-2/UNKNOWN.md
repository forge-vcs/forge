# UNKNOWN — ccx-task-362-2-line-attribution (revision 1)

kind: blocking

## What I need to know

1. The contract's interface section says to **"Extend
   `crates/forge-content-native/src/provenance.rs`"**, but that file does not
   exist in this worktree. Should this task *create* the module from scratch,
   or must it wait for / build on the output of the neighbor task?
2. What API surface does `ccx-task-362-1-provenance-walk` establish? Task
   362-2's core loop ("tip→genesis walk, first-parent", resolving the blob
   for `path` at each version) is exactly a provenance walk. If 362-1 defines
   a walk function/iterator (e.g. per-path version history yielding
   `(commit_id, blob_id)` pairs), 362-2 must consume it — hand-rolling an
   independent walk here would duplicate or conflict with 362-1's public API
   when it lands, and `public_api_change_policy: contract-update-required`
   makes that a contract-level decision, not an implementation detail.

## Why the brief does not answer it

- The brief itself flags the gap: the neighbor section reads
  `NEIGHBOR CONTRACT MISSING: ccx-task-362-1-provenance-walk (surface as
  unknown, do not guess)`.
- The task contract fixes the semantics of `attribute_lines` but says
  "extend", presupposing an existing `provenance.rs` (362-1's deliverable)
  whose shape is unspecified. Nothing in the global policy or task contract
  licenses me to invent 362-1's walk API on its behalf.

## Evidence

- `crates/forge-content-native/src/` contains only `lib.rs`, `pack.rs`,
  `status_cache.rs` — no `provenance.rs`.
- `crates/forge-content-native/src/lib.rs:30-31` — the only module
  declarations are `mod pack;` and `mod status_cache;`; no `mod provenance;`.
- Workspace-wide search for "provenance" in `crates/` matches only a doc
  comment (`crates/forge-content-native/src/lib.rs:1082`); no walk code
  exists anywhere.
- Building blocks exist (`NativeObjectStore::read_head`
  `lib.rs:989`, `read_commit` `lib.rs:743`, `read_object` `lib.rs:557`), so
  the blocker is not technical feasibility — it is that the walk's owning
  contract (362-1) is absent.

## Best-guess resolution (not applied)

If 362-1 is confirmed unstarted and 362-2 is meant to be self-contained,
re-issue this contract (revision 2) stating that 362-2 creates
`provenance.rs`, adds `mod provenance;` + re-exports to `lib.rs`, and owns
an internal first-parent walk until 362-1 refactors it — or attach 362-1's
contract so its API can be consumed/stubbed correctly.
