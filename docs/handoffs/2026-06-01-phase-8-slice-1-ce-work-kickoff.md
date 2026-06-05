# Handoff — NER-139 Phase 8 · Slice S1 (native content diff) · `/ce-work` kickoff

**Date:** 2026-06-01 · **Ticket:** Linear **NER-139** (XL), Forge project `2b5e82f7-7a78-4354-af7d-68609e6e77bc`, team **SE Engineers**, prefix **NER** · **Plan (authoritative):** `docs/plans/completed/2026-05-31-016-feat-phase-8-slice-1-native-diff-plan.md` · **Origin requirements:** `docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md`

## Where things stand

Phase 8 is pre-sliced **S1 / S2a / S2b / S3 / S4 / S5** (6 PRs). **S1 is planned and the doc-review gate is complete** — the gate applied 1 silent fix + 10 best-judgment fixes into the plan and resolved 2 judgment calls. The plan is ready to implement; this session begins **`/ce-work` on the S1 plan**. Later slices each get their own plan + doc-review gate when reached.

**Gate state:** `main` clean. `schema_head` is **7**. `FORGE_ERROR_CODES` is **24** (S1 adds **no** typed error and no migration). Gate = `bash scripts/ci.sh` (fmt `--check` · `cargo test --workspace` · clippy `-D warnings` · e2e). `gh` authed `freezscholte`; squash-merge `(#N)`.

**New dependency:** add `similar` (with the `bytes` feature) to the workspace in U1. It satisfies the global **4-day min-release-age supply-chain gate** (mature, 137M downloads) — **do not bypass/lower that gate**; if an install is blocked, surface it, don't work around it.

## S1 scope (R6–R9, cross-cutting R1–R5)

Native hunk/line diff over the content-addressed object store, with rename detection, exposed through the JSON contract; it **replaces** the git-dependent diff on the core `compare` path (leaving `git` only on the git-backend diff and the export/commit-synthesis interop path). Five units, dependency order:

```
U1 ──▶ U2 ──▶ U3 ──▶ U5
        └────▶ U4 ──┘
```

- **U1** — shared diff types in `forge-content` (move `TreeDiff`/`FileDiff` from forge-export-git, add `HunkDiff`/`DiffLine`/`DiffLineTag` + rename fields, additive); add `similar`; expose `pub(crate) tree_fingerprints` in forge-content-native.
- **U2** — `diff_native_trees` engine: `(blob,mode)`-keyed status, verify-on-read blob reads, binary/symlink handling, `similar` Patience structured hunks + redacted bounded text hunk, policy drop.
- **U3** — rename detection: exact-by-blob-id → capped inexact line-hash similarity, fail-soft on `rename_limit`, mode/symlink-flip never collapsed into a pure rename.
- **U4** — working-vs-snapshot (native walker, policy-excluded, symlink-yielding) + base-vs-proposal; **its integration test (`forge_native_diff.rs`) is created here.** (Declared internal trim point, but working-vs-snapshot is **kept** this slice.)
- **U5** — backend-neutral router in the CLI, rewire `compare_response`, **new `forge diff` command** (kept — agent-native parity), `forge schema` update, native-vs-git parity corpus, **git-removed-from-PATH proof**, assert `FORGE_ERROR_CODES` stays 24.

## Locked decisions (do NOT re-derive — they're in the plan)

- `similar` + **Patience**, behind a single pluggable algorithm seam (histogram/`imara-diff` swap is a later measured change).
- Types in **forge-content** (shared) · engine in **forge-content-native** · router in **forge-cli** (only the CLI reaches both forge-content-native and forge-export-git).
- **Additive** contract: keep the text `hunk` string, **add** structured `hunks` + `old_path`/`similarity`.
- `status` keeps git **letter** encoding (`A`/`M`/`D`, `R<score>`), **not** words — the shared type + the pinned `compare_diff_…` test (`status == "M"`, `forge_compare.rs:136`) stay consistent.
- **`forge diff` command kept** (working-vs-snapshot has no natural `compare` home; agent-native parity). Working-vs-snapshot **integration test lives in U4**.

## Security invariants the doc-review baked in (do NOT weaken)

These came out of the gate and are now in the plan — implement them, don't re-litigate:
- **Redact every `DiffLine.content`** through `redact_evidence_excerpt`, not just the text hunk (one pass over the line slices feeds both forms). The **4096 cap bounds the structured `hunks` total too**, not only the text hunk.
- **Drop via `is_ignored_by_policy`** (covers `.forge/`/`.forge-restore-`), broader than `is_secret_risk_path` alone — a hand-built `.forge/` tree must not surface in `files`.
- **Rename `old_path` cannot leak a secret filename:** exclude `is_ignored_by_policy` paths from the rename candidate maps, and policy-check **both** `path` and `old_path` before emit.
- **Path-free errors (S1):** read via the path-free `read_object` (verify-on-read); never interpolate a path into `anyhow` context; assert path-freeness on **both** `to_string()` and `{:#}` (mirror `ref_store_corrupt_head_error_is_path_free`, which lives at `forge-content-native/src/lib.rs:2363`).
- Preserve the **per-file truncation-warning loop** (`main.rs:490-495`) across the `compare_response` rewire.

## Parity corpus (U5) — must be sound, not silently narrowed

Assert **structural** equivalence (status letter + ins/del counts + paths). For **hunk-body** parity run `git diff --diff-algorithm=patience --unified=3` (the native engine is Patience; the standing compare path's git diff is Myers-default + `--no-renames`). For **rename** cases use a `git diff -M` oracle (the `--no-renames` standing path can't emit `R<score>`/`old_path`). Each index-vs-filesystem divergence class asserted explicitly.

## FYI to keep in mind (not blocking — from the gate's report)

- Symlink target: surfacing old/new target needs a `read_object` on the link blob; the target string must pass redaction.
- Copy detection by content-address can pair unrelated identical blobs (e.g. two empty files) — consider defaulting copy detection off in v0.
- `forge diff` flag spelling — pin the final names in the schema test before the PR opens.

## NOT S1 (later slices / Phase 9)

No 3-way merge / conflict-as-data / `conflict_sets` / migration 008 (S2a/S2b); no auto-resolution (S3); no GC/worktrees (S4); no pack/index (S5). The two carried P0s land later: GC repo-lock (S4), reconcile multi-parent (S2a). Phase 9 (NER-140) = wire protocol + signing — keep `actor`/`authored_time` in the hashed commit bytes.

## Exit criteria for the S1 PR

`bash scripts/ci.sh` green; native-backend `compare --diff` and `forge diff` work **with git removed from PATH** (PATH-only-`sh` runtime test); git-backend diff and the export interop path unchanged; `forge schema` reflects the native diff + structured `hunks` and no longer says "via the git adapter"; `FORGE_ERROR_CODES` still 24; native-vs-git parity corpus passes.

## Start prompt

Paste this into a fresh Codex session from `/Users/skolte/Github-Private/forge`:

```text
Pick up Forge NER-139 Phase 8 Slice S1: native hunk/line diff + rename detection.

Use Compound Engineering only. Superpowers has been disabled in Codex config; do not invoke Superpowers workflows. Follow repo `AGENTS.md` / `CLAUDE.md` and keep using `rtk` for shell commands.

Current expected repo context:
- Repo: /Users/skolte/Github-Private/forge
- Branch may still be `main`; check `git status --short --branch` first.
- Preserve existing uncommitted setup/planning files unless explicitly asked otherwise: `AGENTS.md`, `.agents/`, `.compound-engineering/config.local.example.yaml`, the Phase 8 brainstorm/plan/handoff docs, and the schema-head update in `docs/handoffs/2026-05-31-ner-139-phase-8-kickoff.md`.
- Prefer creating/switching to a feature branch before code changes: `codex/ner-139-phase-8-s1-native-diff`.

Read first:
1. `docs/handoffs/2026-06-01-phase-8-slice-1-ce-work-kickoff.md`
2. `docs/plans/completed/2026-05-31-016-feat-phase-8-slice-1-native-diff-plan.md`
3. `docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md`
4. `docs/ROADMAP.md` Phase 8 and Phase 9 boundary
5. Relevant learnings in `docs/solutions/architecture-patterns/`, especially native walker / commit-on-accept / mode+symlink keying notes

Then run the Compound Engineering work flow against the authoritative plan:
`compound-engineering:ce-work docs/plans/completed/2026-05-31-016-feat-phase-8-slice-1-native-diff-plan.md`

Scope to implement:
- U1: move shared diff types into `forge-content`, add `similar` with `bytes`, expose native tree fingerprints.
- U2: native tree-vs-tree diff engine with `(blob,mode)` keying, verify-on-read, binary/symlink handling, structured redacted bounded hunks, policy drop.
- U3: rename detection, exact then capped inexact similarity, no secret-path leak through `old_path`.
- U4: working-vs-snapshot and base-vs-proposal native diff modes with integration coverage.
- U5: CLI router, rewire native `compare --diff`, add `forge diff`, update `forge schema`, add native-vs-git parity and git-removed-from-PATH proof.

Do not implement S2+ scope: no 3-way merge, no conflict-as-data migration 008, no auto-resolution, no real GC/worktrees, no pack/index, no Phase 9 sync/signing.

Security invariants are load-bearing: redact every structured diff line and text hunk, bound both forms, drop via `is_ignored_by_policy`, never leak secret paths via rename `old_path`, keep S1 path-free error assertions, and preserve diff truncation warnings.

Final gate before shipping: `bash scripts/ci.sh`, plus CE review/commit/PR flow per `CLAUDE.md`.
```
