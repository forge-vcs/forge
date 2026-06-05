---
title: "feat: Phase 8 Slice 1 — Native hunk/line diff + rename detection (replace the git-adapter core-path diff)"
type: feat
status: completed
date: 2026-05-31
origin: docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md
---

# feat: Phase 8 Slice 1 — Native content diff

## Summary

S1 is the first of NER-139 Phase 8's six PRs. It builds a **native hunk/line-granularity content diff with rename detection over the content-addressed object store** and uses it to **replace the git-dependent diff on Forge's core compare path**. Today `forge compare --diff` calls `forge_export_git::diff_trees`, which materializes each native tree into a temp worktree and shells `git write-tree`/`git diff`. S1 adds a native engine (`forge-content-native`) that walks the two trees in-process, diffs changed blobs with the `similar` crate (Patience, structured hunks), detects renames (exact-by-blob-id, free, then a capped inexact pass), and routes `forge-tree:` refs to it — leaving `git` only on the git-backend and git-export interop paths. The diff result types move to the shared `forge-content` crate so both backends emit the same JSON; the contract is enriched **additively** (the existing `hunk` text string stays; structured `hunks` + rename fields are added) so the existing consumer stays green while agents get structured output. No migration; no new typed error.

---

## Problem Frame

After Phase 7 the native backend is git-free for the *linear* local loop, but content diff in the review/compare path still round-trips through the git binary (`synthesize_git_tree` → temp worktree → `git write-tree` → `git diff`). That is an environment-dependent dependency on the exact path Phase 8's merge engine (S2b) will consume, and it blocks the "git removed from PATH" independence claim for `compare`. The merge engine needs a native, structured, mode-aware diff to compute base/ours/theirs hunks; S1 is its foundation. (Full motivation in origin: `docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md` Problem Frame.)

---

## Requirements

- R6. Native hunk/line-granularity diff for **working-vs-snapshot, snapshot-vs-snapshot, and base-vs-proposal**, exposed through the JSON contract. *(origin R6)*
- R7. **Rename detection**, correct on a corpus. *(origin R7)*
- R8. The native diff **replaces** the git-dependent core-path diff: (a) build a hunk differ over the native object store, (b) rewire `forge compare --diff` off `forge_export_git::diff_trees`, leaving `diff_trees`/`synthesize_git_tree` only on the git-export interop path. Not a parallel second diff. *(origin R8)*
- R9. Preserve the `(blob, mode)`/symlink diff key from Phase 7 — a symlink (mode `120000`) and a same-bytes regular file stay distinct; a `chmod`-only change surfaces. *(origin R9)*
- R1–R5 (cross-cutting, every slice): store-before-DB crash-atomic discipline (read-only here, so n/a beyond not regressing); **S1 path-free errors** (no fs paths in `anyhow` context — assert on both `to_string()` and `{:#}`) + **S2 policy exclusion** (secret/`.forge` paths never surface in a diff); **agent-native parity** (the diff in `forge schema` with structured JSON); **typed-error + drift-guard discipline** (S1 adds none — assert `FORGE_ERROR_CODES` stays 24); ship behind the full gate. *(origin R1–R5)*

**Origin actors:** A1 (coding agent — consumes native diff/compare output), A2 (human/reviewing agent).
**Origin flows:** F1 (conflict-resolution loop — S2+ consumes this diff; S1 is its substrate).
**Origin acceptance examples:** none of origin AE1–AE11 are S1-owned (they cover S2a+); S1 introduces its own diff-parity scenarios below.

---

## Scope Boundaries

- **No 3-way merge, no conflict-as-data, no `conflict_sets`/`path_conflicts`, no migration 008** — that is S2a/S2b. S1 is diff only.
- **No auto-resolution, GC, worktrees, pack, or index** — S3/S4/S5.
- The diff engine does **not** mutate the store or worktree — it is a read-only computation (no new object writes, no restore).
- **Git is NOT removed from the repo.** It legitimately remains on (a) git-backend repos' diff path and (b) the export/commit-synthesis interop path (`synthesize_git_tree`/`synthesize_deterministic_commit`/`resolve_git_base_commit`). S1 removes git only from the **native** compare-path diff.
- Intra-line / token-level highlight spans (difftastic-style) are **not** in S1 — the 3-tag line model (`context`/`delete`/`insert`) is the v0 shape.

### Deferred to Follow-Up Work

- **Histogram algorithm** (cleanest code hunks): ship on `similar` + Patience; an `imara-diff` histogram swap behind the pluggable algorithm seam is a measured, reversible follow-up if hunk grouping proves materially worse on real code (origin R30-adjacent; pre-1.0 churn risk noted). Not in S1.
- **`.forge`-level binary/text override attribute** (analogous to `.gitattributes text`): the NUL-byte heuristic is the v0 default; an override is post-S1.

---

## Context & Research

### Relevant Code and Patterns

- `crates/forge-export-git/src/lib.rs` — `diff_trees(repo_root, ref_a, ref_b, include_hunks) -> Result<TreeDiff>` and the structs `TreeDiff { files: Vec<FileDiff>, dropped_secret_paths }`, `FileDiff { path, status, insertions, deletions, binary, hunk: Option<String>, truncated }`. The git path shells `git diff -z --no-renames --name-status`/`--numstat` + `git diff <a> <b> -- <path>`, after `synthesize_git_tree` materializes a `forge-tree:` ref. `HUNK_LIMIT = 4096`, `redact_evidence_excerpt`, `is_secret_risk_path` drop loop. **Also consumed by the export path** (`resolve_git_base_commit` → `synthesize_git_tree`) — leave that intact.
- `crates/forge-cli/src/main.rs` — `compare_response` resolves each attempt via `forge_store::attempt_proposal_content_ref` (→ a `forge-tree:` ref for native repos), calls `forge_export_git::diff_trees(&cwd, ref_a, ref_b, true)`, sets `data["diff"]`, and feeds `dropped_secret_paths` into `secret_export_warnings`. `CompareArgs.diff: Option<Vec<String>>` (clap `num_args = 2`).
- `crates/forge-content-native/src/lib.rs` — `NativeObjectStore::{new, read_object, verify_content_ref}`, `ObjectId::{parse, kind, digest}`, `ObjectKind::{Blob,Tree,Commit}`; private `TreeObject`/`TreeEntry { name, kind, mode, object }`/`TreeEntryKind`; **private `flatten_tree` → `BTreeMap<String, FileFingerprint=(String,u32)>`** keyed by repo-relative path → `(blob-id, mode)` (the diff key to reuse). `read_object` is **path-free + verify-on-read** (re-hashes). Guard test `native_production_paths_shell_no_git` forbids `Command::new` outside `#[cfg(test)]` in this crate — the native engine must be subprocess-free.
- `crates/forge-content/src/lib.rs` — `is_secret_risk_path`, `is_ignored_by_policy`, `redact_evidence_excerpt`, the `forge-tree:`/`git-tree:` prefixes + `classify_content_ref`. Natural home for the shared diff types.
- `crates/forge-cli/src/schema.rs` — `command_shapes()` (the `compare` entry currently says "via the git adapter"); `error_registry`/`CLI_LEVEL_CODES`. `crates/forge-cli/tests/forge_schema.rs` — `FORGE_ERROR_CODES` (24).
- Tests: `crates/forge-cli/tests/forge_compare.rs` (`compare_diff_emits_file_hunk_diff_between_two_attempts` is the contract pin), `crates/forge-cli/tests/forge_native_history.rs::prepare_native_proposal` (native-init pattern), `crates/forge-content-native/src/lib.rs` `#[cfg(test)]` (the `symlink_round_trips_…`, `native_changed_paths_reports_executable_bit_change`, path-free proof patterns).

### Institutional Learnings

- **`(blob_id, mode)`-with-symlink-ness diff key** (`docs/solutions/architecture-patterns/native-commit-objects-…-2026-05-30.md` §5, extended `…commit-on-accept-…-2026-05-31.md` §7): content-addressing deliberately excludes mode, so a blob-id-only diff misses `chmod` and conflates symlinks with same-bytes files. The slice-2 `changed_paths` shipped this bug; only the adversarial persona caught it. **S1's status keying must be `(blob-id, mode)` at hunk granularity, and a rename that also flips mode/symlink-ness must not be reported as a pure rename.**
- **Read through verify-on-read** (`…commit-on-accept-…-2026-05-31.md` §4): disambiguate by hash, never by format-guess. S1 reads both diff sides via `read_object` — no raw-bytes fast path that skips `hash(file)==id`.
- **Walker / policy backstop / `is_file()` symlink trap** (`…native-worktree-walker-…-2026-05-30.md` §1/§4/§5): the working-vs-snapshot side enumerates via the native walker; exclude via the shared `is_ignored_by_policy` (reuse, never fork); yield symlinks (don't filter on `is_file()`). A `git diff` differential corpus must restrict to identical-membership paths and assert each index-vs-filesystem divergence class explicitly.
- **Path-free errors + "grep every git call site, prove with a PATH-only-`sh` test"** (`…walker-…-2026-05-30.md` §3; `…commit-on-accept-…-2026-05-31.md` §5): the load-bearing git call hides in the shared path. Prove S1's independence with a runtime git-removed test on the native compare path, not just a grep.

### External References

- `similar` crate (docs.rs/similar): `TextDiff::configure().algorithm(Algorithm::Patience).diff_slices(...)` → `grouped_ops(context)` → `DiffOp::iter_changes()` → `ChangeTag`. `bytes` feature for non-UTF-8 line content. Mature (137M downloads, 5-yr history) — satisfies the 4-day min-release-age gate. (Chosen over `imara-diff` — pre-1.0 + breaking 0.2.0 rewrite — and over hand-rolling; user-confirmed.)
- Rename detection (git `diffcore-rename`): exact-by-SHA first, then inexact similarity = shared-material / size-of-larger, default `-M` 50%, capped by `diff.renameLimit` with a fail-soft warning.
- Binary detection (git `buffer_is_binary`): NUL byte in first ~8000 bytes ⇒ binary.

---

## Key Technical Decisions

- **`similar` crate, Patience default, pluggable algorithm seam** *(user-confirmed)*: structured `grouped_ops`/`iter_changes` hunks for free; byte-slice safe; gate-friendly. The algorithm is a single internal seam so a histogram swap (`imara-diff`) is a later measured change.
- **Additive contract enrichment, not replacement**: keep `FileDiff.hunk: Option<String>` (the text unified-diff body — the existing `compare_diff_…` test pins it) and **add** `hunks: Vec<HunkDiff>` (structured), `old_path: Option<String>`, `similarity: Option<u8>`, with `status` keeping its existing git name-status letter encoding (`A`/`M`/`D`, extended to `R<score>` for renames — **not** full words like `renamed`/`modified`, so the shared `FileDiff` and the pinned `status == "M"` test stay consistent across both backends). Additive fields are backward-compatible; agents get structured data, the existing consumer stays green. Dropping the text `hunk` is a deferred cleanup, not S1.
- **Diff result types live in `forge-content` (shared), native engine in `forge-content-native`, router in the CLI.** `forge-content` is the backend-abstraction crate, so the wire-contract types belong there; both backends emit them. The router (classify ref → native vs git) sits in `forge-cli` because it must reach *up* to both `forge-content-native` and `forge-export-git` (a lower crate can't call the git crate).
- **Status keyed on `(blob-id, mode)`** including symlink-ness (R9). Symlink (`120000`) and binary blobs are reported with status + counts but **no line hunks** (symlink → optional target-string note; binary → `binary: true`). Rename pairing happens on `(blob-id)` for the exact pass but a mode/symlink flip on an otherwise-identical blob is surfaced, never collapsed into a clean rename.
- **Rename heuristic (v0, cheap):** exact-by-blob-id first (free — content-addressed), then size-prefilter, then line-hash-multiset similarity on the residual adds×deletes, `rename_limit` cap (default ~1000) with a fail-soft `warnings[]` entry, 50% default threshold exposed as a config/flag knob; exact matches are always 100%.
- **Secret/policy parity:** even though both proposal trees were built by the policy-filtered walker, the engine still runs `is_secret_risk_path` over the diffed path set and populates `dropped_secret_paths` (so `secret_export_warnings` keeps firing and a hand-built/adversarial tree can't leak); hunk bodies pass through `redact_evidence_excerpt` and the 4096 cap.

---

## Open Questions

### Resolved During Planning

- **Diff algorithm/crate** → `similar` + Patience, pluggable (user-confirmed).
- **Rename heuristic + threshold + corpus** (origin OQ, R7) → exact-by-blob-id then capped inexact line-hash similarity, 50% default (config knob), fail-soft on `rename_limit`; corpus = a curated set of move/rename/edit/binary/symlink cases plus a native-vs-git differential on identical-membership paths (U3/U5 tests).
- **Contract shape** → additive enrichment (keep text `hunk`, add structured `hunks` + rename fields).
- **"git removed from PATH" scope** (origin OQ) → excludes the export interop path; proven by a native-compare-path runtime test with `PATH` = only `sh` (U5).
- **Wire `status` encoding** (doc-review F3) → the shared `FileDiff.status` keeps git name-status **letters** (`A`/`M`/`D`, `R<score>` for renames) on both backends — not full words — so the pinned `status == "M"` test and the shared type stay consistent (U1).
- **Native-vs-git parity oracle** (doc-review F9) → structural equivalence (status + counts + paths) for the differential; `git diff --diff-algorithm=patience --unified=3` for hunk-body parity; `git diff -M` for rename cases (U5).
- **Structured-line secret egress** (doc-review F1/F2/F7/F8) → redact every `DiffLine.content` (not just the text hunk), bound both forms to 4096, use `is_ignored_by_policy` for the drop, exclude secret sources from rename pairing and policy-check `old_path` (U2/U3).
- **`forge diff` command scope** (doc-review F4) → **kept** — agent-native parity (every capability in `forge schema` with structured JSON) and working-vs-snapshot has no natural `compare` home (compare is attempt-vs-attempt); `forge diff` is the clean entry point the S2b merge engine reuses (U5).
- **Working-vs-snapshot integration test placement** (doc-review F11) → **created in U4** (ships the mode with its policy/symlink-backstop proof and survives a U4/U5 split); U5 extends it for router/no-git/parity.

### Deferred to Implementation

- Exact `similar` API surface (`diff_slices` vs `diff_lines`, the `bytes`-feature byte-slice path for non-UTF-8) — pick at code time against the pinned version.
- Whether the working-vs-snapshot map is best produced by a new read-only "dry walk" helper or by reusing the existing snapshot-build walk in a non-persisting mode (U4) — decide against the real walker code.
- Final `forge diff` flag spelling (`--from/--to/--working`, `--find-renames[=<n>]`) — converge with existing CLI conventions at code time.

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

Data flow (native path), and the routing seam:

```
forge compare --diff A B
  └─ compare_response (forge-cli)
       ├─ attempt_proposal_content_ref(A) -> "forge-tree:f1:tree:sha256:…"   (ref_a)
       ├─ attempt_proposal_content_ref(B) -> "forge-tree:f1:tree:sha256:…"   (ref_b)
       └─ diff_content_refs(cwd, ref_a, ref_b, opts)            ← NEW router (forge-cli)
            ├─ classify_content_ref(ref) == ForgeTree  → forge_content_native::diff_native_trees(...)   [no git]
            └─ classify_content_ref(ref) == GitTree    → forge_export_git::diff_trees(...)              [git, unchanged]
                                   │
                                   ▼  both return forge_content::TreeDiff
       data["diff"] = TreeDiff ; warnings += secret_export_warnings(dropped_secret_paths) + truncation

diff_native_trees(store, root_a, root_b, opts):
   flatten_tree(root_a) , flatten_tree(root_b)   →  BTreeMap<path, (blob_id, mode)>   (the Phase-7 key)
   for each path: classify by (blob,mode) equality → Added / Removed / Modified(+mode flip) / Unchanged
   rename pass (opts.detect_renames):  exact-by-blob-id(free) → size-prefilter → line-hash similarity → cap+fail-soft
   for each changed text file:  read_object(blob) (verify-on-read) → NUL-scan(8000B)?binary
                                : similar Patience grouped_ops(ctx) → hunks[] + text hunk → redact → 4096 cap
   drop is_secret_risk_path → dropped_secret_paths
```

JSON shape (`data.diff`, additive over today's shape):

```jsonc
{ "files": [
    { "path": "src/new.rs", "status": "R87", "old_path": "src/old.rs", "similarity": 87,   // git name-status letter; R<score> for renames
      "binary": false, "insertions": 12, "deletions": 4, "truncated": false,
      "hunk": "@@ -40,6 +40,8 @@ …",                         // existing text field (kept)
      "hunks": [ { "old_start": 40, "old_lines": 6, "new_start": 40, "new_lines": 8,
                   "lines": [ {"tag":"context","content":"fn foo() {"},
                              {"tag":"delete","content":"  let x = 1;"},
                              {"tag":"insert","content":"  let x = 2;"} ] } ] },
    { "path": "logo.png", "status": "M", "binary": true,
      "insertions": null, "deletions": null, "hunk": null, "hunks": [] }
  ],
  "dropped_secret_paths": [] }
```

---

## Implementation Units

```
U1 ──▶ U2 ──▶ U3 ──▶ U5
        └────▶ U4 ──┘
```
U1 (types + dep + tree-walk exposure) → U2 (core tree-vs-tree engine) → U3 (rename) and U4 (working-vs-snapshot / base-vs-proposal modes) both build on U2; U5 (router + CLI rewire + schema + parity/no-git proof) depends on U2/U3 (and U4 if its mode ships). U4 is the internal trim/split candidate if review deems S1 too large.

### U1. Shared diff contract types, `similar` dependency, native tree-walk exposure

**Goal:** Land the backend-neutral diff data types in `forge-content`, add the `similar` dependency, and expose a public tree-fingerprint walk from `forge-content-native` — the scaffolding U2 builds on, with no behavior change yet.

**Requirements:** R6, R8, R9, R4

**Dependencies:** None

**Files:**
- Modify: `Cargo.toml` (workspace deps — add `similar` with the `bytes` feature), `crates/forge-content/Cargo.toml`, `crates/forge-content-native/Cargo.toml`
- Modify: `crates/forge-content/src/lib.rs` (define `TreeDiff`, `FileDiff`, `HunkDiff`, `DiffLine`, `DiffLineTag`, `RenameInfo`/fields)
- Modify: `crates/forge-export-git/src/lib.rs` (use the relocated types instead of locally-defined `TreeDiff`/`FileDiff`; behavior unchanged), `crates/forge-cli/src/main.rs` (import path)
- Modify: `crates/forge-content-native/src/lib.rs` (add `pub(crate) fn tree_fingerprints(&self, root: &ObjectId) -> Result<BTreeMap<String,(String,u32)>>` wrapping the existing private `flatten_tree` — `pub(crate)`, not `pub`, because the only consumer (U2's engine) is in-crate; the CLI reaches the diff through `diff_native_trees`, never the fingerprint map directly)
- Test: `crates/forge-content/src/lib.rs` (`#[cfg(test)]`), `crates/forge-content-native/src/lib.rs` (`#[cfg(test)]`)

**Approach:**
- Move `TreeDiff`/`FileDiff` from `forge-export-git` to `forge-content`; keep every existing field byte-identical in serde output (snake_case), then **add** `hunks: Vec<HunkDiff>` (default empty), `old_path: Option<String>`, `similarity: Option<u8>`. `DiffLineTag` is a 3-variant enum (`context`/`delete`/`insert`, `#[serde(rename_all="snake_case")]`). Every new type (`HunkDiff`, `DiffLine`, `DiffLineTag`) derives `Debug, Clone, Serialize, PartialEq, Eq` to match the existing `FileDiff`/`TreeDiff` derives (`forge-export-git/src/lib.rs:28`/`:43`), and `similarity` stays `Option<u8>` (an integer, never a float ratio) — both are load-bearing so the relocated `FileDiff`'s `Eq` derive keeps compiling. `status` stays the existing git name-status string (`A`/`M`/`D`, extended to `R<score>` for renames) — both backends emit **letters, not words**, so the shared type and the pinned `compare_diff_…` assertion (`status == "M"`, `forge_compare.rs:136`) stay consistent.
- `forge-export-git` re-imports the types and continues to populate only the existing fields (new fields default) — so the git path is contract-identical until U5.
- Expose the native tree walk so U2 can flatten both roots without duplicating `flatten_tree`.

**Patterns to follow:** existing `FileDiff`/`TreeDiff` serde in `forge-export-git`; `flatten_tree`/`FileFingerprint` in `forge-content-native`.

**Test scenarios:**
- Happy path: `TreeDiff`/`FileDiff` with the new fields serialize to snake_case JSON; an all-empty `hunks`/`None` rename fields round-trip and a legacy-shaped value (no new fields) still deserializes.
- Happy path: `tree_fingerprints` on a known small tree returns the expected `path → (blob-id, mode)` map including a nested dir, an executable (`100755`), and a symlink (`120000`).
- Edge case: `tree_fingerprints` on an empty tree → empty map; on a tree with only a subdir → flattened leaf paths.
- Integration: `forge-export-git` compiles against the relocated types and `compare_diff_emits_file_hunk_diff_between_two_attempts` (git backend) stays green (proves the move is behavior-neutral).

**Verification:** workspace builds; `similar` resolves under the min-release-age gate; the existing git-backend `compare --diff` test passes unchanged; `tree_fingerprints` is `pub(crate)` and covered.

---

### U2. Native tree-vs-tree hunk diff engine

**Goal:** `diff_native_trees(store, root_a, root_b, opts) -> Result<TreeDiff>` — the core engine: `(blob,mode)`-keyed status, blob reads via verify-on-read, binary + symlink handling, `similar` structured hunks + bounded redacted text hunk, secret-path drop. No rename pairing yet (adds/deletes reported plainly).

**Requirements:** R6 (snapshot-vs-snapshot), R8, R9, R1 (S1 path-free), R2 (S2 policy)

**Dependencies:** U1

**Files:**
- Modify: `crates/forge-content-native/src/lib.rs` (add the engine + a private `diff_blobs` helper)
- Test: `crates/forge-content-native/src/lib.rs` (`#[cfg(test)]`)

**Approach:**
- Flatten both roots via `tree_fingerprints`. Classify each path by `(blob-id, mode)` equality: present-only-in-B → Added; only-in-A → Removed; both with differing key → Modified (a mode/symlink flip with identical blob is still Modified, carrying the mode change).
- For each changed path needing a body: `read_object` both blobs (path-free, verify-on-read). Binary check = NUL byte in first 8000 bytes of either side → `binary: true`, `insertions/deletions = None`, no hunks. Symlink (`mode == 0o120000`) → no line hunks (the blob is a link target string; optionally surface old/new target as a single-line note, never a multi-line diff).
- Text path: `similar` `TextDiff::configure().algorithm(Patience)` over line slices (use the `bytes` path so non-UTF-8 lines don't panic) → `grouped_ops(ctx)` → build structured `hunks` (1-based `old_start/old_lines/new_start/new_lines` from the `DiffOp` ranges) and the derived text `hunk` via `iter_changes()`/`ChangeTag`. **Redact every line's content through `forge_content::redact_evidence_excerpt` before it lands in either `hunks[].lines[].content` or the derived text `hunk`** — one redaction pass over the line slices feeds both forms, so the structured field can never leak a secret the text field hides. Count insertions/deletions. Bound the per-file diff body to 4096 bytes with `truncated`; the cap covers the structured `hunks` total serialized size **as well as** the text `hunk` (mirror `HUNK_LIMIT`), so neither form emits unbounded content.
- Run `is_ignored_by_policy` over the changed-path set; matches go to `dropped_secret_paths`, not `files`. Use the **shared policy predicate**, not `is_secret_risk_path` alone — `is_ignored_by_policy` also covers `.forge/` and `.forge-restore-` internal paths, so a hand-built or adversarial tree carrying a `.forge/` entry cannot surface in `files` (and the working side in U4 uses the same predicate, so the two cannot drift).
- **All errors path-free**: only `read_object` (already path-free) and pure computation; never interpolate a path into `anyhow` context.

**Execution note:** Build the engine test-first against hand-constructed trees; add the native-vs-git differential corpus in U5 once the router exists.

**Technical design:** see High-Level Technical Design (the `diff_native_trees` block).

**Patterns to follow:** `changed_paths`/`flatten_tree` `(blob,mode)` keying; `forge-export-git`'s `read_hunk` redaction + `HUNK_LIMIT` truncation; the path-free `read_object` reader.

**Test scenarios:**
- Happy path: pure add / pure delete / content-modify between two trees → correct status + counts + a structured hunk whose `lines` tags match the change; text `hunk` contains the changed token.
- Edge case (R9): `chmod`-only change (same blob, `100644`→`100755`, `#[cfg(unix)]`) → Modified, no spurious hunk content; a symlink (`120000`) whose target bytes equal a sibling regular file's bytes → the two stay distinct, symlink reported without a line diff.
- Edge case: binary blob (embedded NUL in first 8000 B) → `binary: true`, `hunks: []`, counts `null`; a >8000-byte all-text file with a NUL at byte 9000 → still text (NUL outside the window).
- Edge case: non-UTF-8 line content does not panic (bytes path); empty file vs non-empty; trailing-newline differences.
- Error/secret path: a tree containing `.env`/`*.pem`/`credentials*` **or a `.forge/`/`.forge-restore-` path** → those paths land in `dropped_secret_paths`, never in `files`; a `key=value` secret-like line is redacted in **both** `hunks[].lines[].content` and the text `hunk` (assert the structured field, not just the text form); an oversize diff body is truncated with `truncated: true` and the structured `hunks` is bounded too (assert `hunks` size, not only `hunk`).
- Error path (S1): a missing/corrupt blob object surfaces an error that contains **no filesystem path** in either `to_string()` or `{:#}` (mirror `ref_store_corrupt_head_error_is_path_free`).

**Verification:** `diff_native_trees` produces a `TreeDiff` matching hand-computed expectations across the scenarios; the native crate's `native_production_paths_shell_no_git` guard still passes (engine is subprocess-free); path-free assertions hold.

---

### U3. Rename detection

**Goal:** Add rename/copy detection to `diff_native_trees`: exact-by-blob-id (free), then a capped inexact similarity pass, producing `status` rename, `old_path`, `similarity`, with a fail-soft warning when the candidate pool exceeds `rename_limit`.

**Requirements:** R7, R9

**Dependencies:** U2

**Files:**
- Modify: `crates/forge-content-native/src/lib.rs` (rename pass + a `DiffOptions { detect_renames, rename_threshold, rename_limit }` input)
- Test: `crates/forge-content-native/src/lib.rs` (`#[cfg(test)]`)

**Approach:**
- After U2's add/delete classification, when `detect_renames`: build maps of removed-blob-id → path and added-blob-id → path, **excluding any `is_ignored_by_policy` path from both maps** so a secret source (e.g. `.env`) can never be paired into a rename. After pairing, run the policy check over **both** `path` and `old_path` on every emitted entry — if either matches, the entry goes to `dropped_secret_paths`, never to `files` with the secret filename exposed in `old_path`. **Exact pass:** an id on both sides pairs as a rename (similarity 100), or a copy if the source path still exists in B. Remove paired paths from the add/delete sets.
- **Inexact pass** on the residual: size-prefilter (`min/max < threshold` ⇒ skip), then line-hash-multiset similarity = `common-line-hashes / lines-of-larger` for surviving pairs; pair the best match ≥ `rename_threshold` (default 50). Cap: if `residual_adds × residual_deletes > rename_limit` (default ~1000), **skip the inexact pass**, leave adds/deletes unpaired, and push a structured `warnings[]` entry (`rename_detection_skipped`).
- **Mode/symlink guard (R9):** a pair whose blobs match but whose modes differ is reported as a rename **plus** a mode change, never a silent pure-rename; a rename target that becomes/ceases to be a symlink is surfaced.

**Patterns to follow:** git `diffcore-rename` phases (exact → basename → inexact) collapsed to v0 exact + inexact; the `(blob,mode)` key discipline from U2.

**Test scenarios:**
- Happy path (exact): a file moved with identical content → single `renamed` entry, `old_path` set, `similarity: 100`, no add/delete pair.
- Happy path (inexact): a moved-and-lightly-edited file above 50% → `renamed` with a `similarity` in (50,100); a heavily-rewritten move below threshold → stays separate add + delete.
- Edge case (R9): a rename that also flips the executable bit or symlink-ness → reported as rename + mode change, not a pure rename.
- Edge case (copy): source path still present in B and its blob also appears at a new path → `copied` (or rename per the chosen copy semantics), documented either way.
- Edge/secret path (R2): a `.env` deleted from tree A that the inexact pass would otherwise pair with a new non-secret file → no `old_path: ".env"` in `files`; the secret path appears only in `dropped_secret_paths`.
- Error/scale path: a synthetic changeset whose `adds × deletes` exceeds `rename_limit` → inexact pass skipped, adds/deletes reported unpaired, a `rename_detection_skipped` warning emitted (fail-soft, no hang).
- `Covers R7.` A small curated rename corpus (move, move+edit, rename+chmod, below-threshold) asserts the expected pairings.

**Verification:** rename pairings match expectations across the corpus; the cap fails soft with a warning; threshold is a parameter; mode/symlink flips are never swallowed into a clean rename.

---

### U4. Working-vs-snapshot and base-vs-proposal diff modes

**Goal:** Complete R6's three modes. base-vs-proposal is tree-vs-tree (resolve two stored refs → U2 engine). working-vs-snapshot produces the working-side `(path → (blob,mode))` map via the native walker (policy-excluded, symlink-yielding) and diffs it against a stored snapshot tree.

**Requirements:** R6 (working-vs-snapshot, base-vs-proposal), R2 (S2 policy on the working side), R9

**Dependencies:** U2 (engine). Independent of U3 (renames compose if present).

**Files:**
- Modify: `crates/forge-content-native/src/lib.rs` (a read-only "dry" worktree-fingerprint helper + a `diff_working_vs_tree` entry; a `diff_content_refs`-style resolver that maps two `forge-tree:` refs to roots)
- Test: `crates/forge-content-native/src/lib.rs` (`#[cfg(test)]`), `crates/forge-cli/tests/forge_native_diff.rs` (**created here** — working-vs-snapshot integration in a real temp repo: policy/`.forge` exclusion + symlink-yielding + `chmod`; U5 extends the same file for router/no-git/parity, so the mode ships with its R2/R9 backstop proof and survives a U4/U5 split)

**Approach:**
- base-vs-proposal: resolve both content_refs to `ObjectId` roots (`classify_content_ref` → `ObjectId::parse`) and call `diff_native_trees` — no new engine logic.
- working-vs-snapshot: walk the worktree with the native walker, excluding via the shared `is_ignored_by_policy` (reuse, never fork), **yielding symlinks** (don't filter on `is_file()`), hashing each file's bytes to a blob-id and capturing its mode → the working-side fingerprint map; diff vs the snapshot tree map. Read-only — persists nothing.
- This is the internal **trim point**: if review deems S1 too large, base-vs-proposal (free) stays and working-vs-snapshot + its `forge diff` surface (U5) can split to a fast-follow within S1's milestone. If exercised, the working-vs-snapshot follow-on is **mandatory, not optional** — R6 lists the mode unconditionally — and a Forge Linear (NER) ticket for it must be filed before the first S1 PR merges, so the R6 gap cannot silently persist into S2a (which depends on S1 being a complete diff substrate).

**Patterns to follow:** the native walker + `is_ignored_by_policy` backstop and the symlink/`is_file()` lessons (`…walker-…-2026-05-30.md` §4/§5); the snapshot-build walk that already hashes worktree files.

**Test scenarios:**
- Happy path: a dirty worktree (one edited, one new, one deleted file) vs its HEAD snapshot → correct add/modify/delete diff.
- Happy path: base-vs-proposal between two stored refs → identical result to calling the engine directly on the two roots.
- Edge case (R2): a worktree containing `.env`/a secret-risk file and a `.forge/` entry → neither surfaces in the diff (policy backstop on the working side).
- Edge case (R9): a worktree symlink → yielded and diffed as a symlink, not dropped by an `is_file()` filter; a `chmod`-only worktree change surfaces.
- Edge case: clean worktree (matches snapshot) → empty diff.

**Verification:** all three R6 modes produce correct diffs; the working side never leaks policy-excluded paths; symlinks/mode survive; nothing is persisted.

---

### U5. Backend-neutral router, CLI rewire, `forge schema`, and the git-removed-from-PATH proof

**Goal:** Route the compare path through a native-vs-git selector, rewire `compare_response`, expose the diff (incl. working-vs-snapshot) via the agent-native contract, update `forge schema`, and **prove** git is gone from the native compare-path diff.

**Requirements:** R8, R6, R4, R3 (assert `FORGE_ERROR_CODES` unchanged)

**Dependencies:** U2, U3 (and U4 if its mode ships in S1)

**Files:**
- Modify: `crates/forge-cli/src/main.rs` (a `diff_content_refs(cwd, ref_a, ref_b, opts)` router: `classify_content_ref` → ForgeTree ⇒ `diff_native_trees` (resolve roots via `NativeObjectStore`), GitTree ⇒ `forge_export_git::diff_trees`; rewire `compare_response` to call it; add a `forge diff` subcommand exposing tree-vs-tree + working-vs-snapshot with `--find-renames[=<n>]`)
- Modify: `crates/forge-cli/src/schema.rs` (`command_shapes()`: drop "via the git adapter" from `compare`; document the `diff` command + the structured `hunks` sub-shape)
- Test: `crates/forge-cli/tests/forge_native_diff.rs` (**extend** — created in U4), `crates/forge-cli/tests/forge_compare.rs` (extend), `crates/forge-cli/tests/forge_schema.rs` (assert updates)

**Approach:**
- The router lives in the CLI (only the CLI can reach both `forge-content-native` and `forge-export-git`). For a native repo every ref is `ForgeTree` → the native engine, **zero git**. `GitTree` refs (git-backend repos) keep the git path unchanged. **Leave `diff_trees`/`synthesize_git_tree` intact** for the export/commit-synthesis consumers — only `compare_response`'s diff call moves to the router.
- `compare_response` keeps building `data["diff"]` from the returned `TreeDiff` and feeding `dropped_secret_paths` to `secret_export_warnings` — wire shape stable, now with `hunks`/rename fields populated. **Preserve the existing per-file truncation-warning loop (`main.rs:490-495`) unchanged** so a `truncated` native `FileDiff` still emits the envelope-level `warnings[]` entry that an agent reading only `warnings[]` (not `data.diff.files[].truncated`) depends on.
- Add a `forge diff` command for agent-native parity (R4): tree-vs-tree (two refs/attempts) and working-vs-snapshot, JSON-first, with rename flags.
- Update the `compare` schema string and add the `diff` shape; the static-contract tests pin presence.

**Test scenarios:**
- Integration (native, the headline): native-backend `compare --diff A B` (mirror `prepare_native_proposal`) → `data.diff.files[]` with `status`, the text `hunk` containing the change, **and** populated structured `hunks`; a renamed file shows `status: renamed` + `old_path` + `similarity`.
- `Covers R8.` Run the native-backend `compare --diff` with `PATH` containing only `sh` (git removed) → succeeds and emits the diff (proves the native compare path shells no git). Mirror `native_lifecycle_runs_with_git_removed_from_path`.
- Integration (git backend): `compare_diff_emits_file_hunk_diff_between_two_attempts` (git backend, GitTree path) stays green — the router's git branch is unchanged.
- Integration (parity, `Covers R7/R8`): a curated native-vs-git differential corpus over identical-membership paths (move/edit/binary/symlink/chmod). Assert **structural** equivalence — status letter + insertion/deletion counts + affected paths — between the native engine and `git diff`. For **hunk-body** parity, align the oracle's algorithm and context radius (`git diff --diff-algorithm=patience --unified=3`) so a Patience-vs-Myers-default or context-radius mismatch cannot cause a false failure. For **rename** cases, compare against a renames-on oracle (`git diff -M`) — the standing compare-path git diff stays `--no-renames` and cannot emit `R<score>`/`old_path` to validate native rename pairings, so a `-M` oracle is required to check them. Each index-vs-filesystem divergence class asserted explicitly; corpus never silently narrowed.
- Integration (`forge diff`): working-vs-snapshot via `forge diff` returns the expected JSON; `--find-renames` toggles rename pairing.
- Schema/contract: `forge schema` no longer says "via the git adapter" for `compare` and lists the `diff` command + `hunks` shape; `FORGE_ERROR_CODES` is still **24** (S1 adds no typed error); the static-contract test (`schema_emits_versioned_contract_without_a_repo`) stays green.

**Verification:** `forge compare --diff` and `forge diff` work on the native backend with git removed from PATH; the git-backend path and the export interop path are unchanged; `forge schema` reflects the native diff + structured shape; the full gate (`bash scripts/ci.sh`) is green; `FORGE_ERROR_CODES` unchanged.

---

## System-Wide Impact

- **Interaction graph:** `compare_response` is the only changed call site; the new router is additive. `attempt_proposal_content_ref` (ref resolution) is reused unchanged. `forge diff` is a new read-only command.
- **Error propagation:** diff errors are `anyhow`, path-free (S1), surfaced through the existing envelope `errors[]`; no new typed code. `rename_detection_skipped` and secret-drop/truncation are `warnings[]`, not errors.
- **State lifecycle risks:** none — the diff is read-only (no object writes, no restore, no lock). It must not regress verify-on-read (reads go through `read_object`).
- **API surface parity (R4):** the structured diff is exposed via both `compare --diff` and the new `forge diff`, and declared in `forge schema`. The `--json` envelope (`schema_version: forge.cli.v0`) is unchanged; `data.diff` gains additive fields only.
- **Unchanged invariants:** `forge_export_git::diff_trees`/`synthesize_git_tree`/`resolve_git_base_commit` remain for the export/commit-synthesis path and the git backend; the genesis-hash / object framing is untouched (S1 only reads); `FORGE_ERROR_CODES` stays 24.

---

## Risks & Dependencies

| Risk | Mitigation |
|------|------------|
| Blob-id-only diff key reintroduces the slice-2 mode-blind bug (chmod/symlink invisible). | Key status + rename on `(blob-id, mode)` incl. symlink-ness; unix `chmod`-only test + symlink-vs-same-bytes test (U2); rename+mode-flip test (U3). |
| A residual git call hides on the compare path (the slice-3 `git rev-parse` lesson). | Grep every compare-path git call site AND prove with a `PATH`-only-`sh` native `compare --diff` runtime test (U5), not just a grep. |
| Path leaks into a diff error (S1 regression — the path the NER-143 fix just closed). | Read via the path-free `read_object`; never `.context("…path…")`; assert path-freeness on both `to_string()` and `{:#}` (U2). |
| Secret content leaks through a hunk, a structured `hunks[].lines[].content`, a rename `old_path`, or a hand-built `.forge/` tree. | `is_ignored_by_policy` drop loop (covers `.forge/`, broader than `is_secret_risk_path`) + per-line `redact_evidence_excerpt` on **both** the structured lines and the text hunk + 4096 cap on **both** forms; rename source paths excluded from pairing and `old_path` policy-checked before emit; secret-drop + structured-line-redaction tests (U2/U3). |
| Rename inexact pass is O(adds×deletes) → pathological hang. | `rename_limit` cap with fail-soft `warnings[]`; size-prefilter; cheap line-hash metric, not a per-pair real diff (U3). |
| `similar` is a new runtime dependency. | Mature (137M dl, 5-yr history), satisfies the 4-day min-release-age gate; pinned; isolated behind a pluggable algorithm seam (user-confirmed). |
| S1 balloons past ~M (rename + 3 modes + parity corpus). | U4 (working-vs-snapshot) is the explicit internal trim/split point; base-vs-proposal + `compare --diff` replacement + rename are the load-bearing core. |
| Differential corpus silently narrowed to pass (the walker §1 trap). | Restrict corpus to identical-membership paths; assert each index-vs-filesystem divergence class explicitly; never auto-narrow (U5). |

---

## Sources & References

- **Origin document:** `docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md` (S1 = R6–R9; cross-cutting R1–R5).
- Related code: `crates/forge-export-git/src/lib.rs` (`diff_trees`/`synthesize_git_tree`), `crates/forge-content-native/src/lib.rs` (`flatten_tree`/`read_object`/`classify_content_ref`), `crates/forge-cli/src/main.rs` (`compare_response`), `crates/forge-cli/src/schema.rs`.
- Learnings: `docs/solutions/architecture-patterns/native-commit-objects-base-anchoring-and-the-new-objectkind-gc-reachability-coupling-2026-05-30.md` (§5 mode key), `…commit-on-accept-ledger-authoritative-reconcile-and-the-last-hidden-git-dependency-2026-05-31.md` (§4 verify-on-read, §5 grep-every-git-call, §7 symlink key), `…native-worktree-walker-ignore-engine-and-index-vs-filesystem-divergence-2026-05-30.md` (§1/§4/§5).
- External: `similar` crate (docs.rs/similar); git `diffcore-rename` (similarity index / `diff.renameLimit`); git `buffer_is_binary` (NUL/8000-byte heuristic).
- Phase 7 per-slice plan precedent: `docs/plans/completed/2026-05-30-012…/013…/014-feat-phase-7-slice-*-plan.md`.
