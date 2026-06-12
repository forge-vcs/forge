# Handoff — NER-139 Phase 8: native diff + intent-aware 3-way merge + pack/delta/GC + physical attempt isolation [XL]

**Date:** 2026-05-31 · **Milestone:** M3 — Earn native-VCS independence · **Ticket:** Linear **NER-139** (XL) · **Forge project:** id `2b5e82f7-7a78-4354-af7d-68609e6e77bc`, team **SE Engineers**, prefix **NER** · **Depends on:** Phase 7 (NER-138, **Done**).

## Where things stand

Phases 1–7 are merged. The native backend is fully git-independent for the local loop: native walker + ignore engine, content-addressed Blob/Tree/**Commit** objects with object-kind headers, a `.forge/refs/HEAD` ref store, justified commit-on-accept (`decisions.commit_id`) with a ledger-authoritative HEAD reconcile, native `log`/`checkout`/`undo`, symlink content (mode 120000), a commit-DAG `doctor` integrity walk behind the typed `NativeHistoryCorrupt`, and a **dry-run-only** gc whose reachability roots already include the ledger tip + op-log targets. The full lifecycle (`init→start→save→run→propose→check→accept→restore`, `log`, checkout, `undo`) runs **with git removed from PATH**.

`main` clean. **`schema_head` is `7`** (NER-142/NER-143 consolidation merged — migration 007 `expected_content_ref` landed; Phase 8's new migration is **008**). **`FORGE_ERROR_CODES` is `24`.** Gate: `bash scripts/ci.sh`. `gh` authenticated; remote repository configured; squash-merge `(#N)`. (The gc-malformed-view fail-closed and the undo/worktree-state model are now in `main`.)

**Read first, in order:**
- `docs/ROADMAP.md` Phase 8 section (the authoritative scope) and Phase 9 (NER-140, what Phase 8 must NOT pull in).
- The slice-1/2/3 learnings in `docs/solutions/architecture-patterns/` — especially `commit-on-accept-ordering-ledger-authoritative-reconcile-and-the-last-hidden-git-dependency-2026-05-31.md` (the **carry-over** items below come from its §6/§8 + the slice-3 code review).
- `PRD.md` §15 (conflict-as-data typed kinds) and §10.7 (retention/budget), §17.1 (anti-gaming — informs the Phase 9 boundary).
- The slice-3 code review `docs/code-reviews/2026-05-30-ner-138-phase-7-slice-3.md` and **NER-143** (the deferrals; some explicitly belong here).

## Phase 8 scope (from the ROADMAP / NER-139)

This is **XL and the riskiest leap** — a correct 3-way merge with rename/binary/mode/symlink handling is where most VCS projects stall for years. **Stage it internally like Phase 7** (slices): diff → conflict-as-data merge → GC/packing → physical worktrees. **Strongly recommend `/ce-brainstorm` then `/ce-plan` to slice it** before any `/ce-work`.

1. **Native content diff at hunk/line granularity** (working vs snapshot, snapshot vs snapshot, base vs proposal) with **rename detection**, exposed through the JSON contract — replacing the Phase 6 git-adapter diff and removing that interop dependency from the core review path. (Slice 3 left diff at name-level on purpose; this is where hunks land.)
2. **Intent-aware 3-way merge engine** (base/ours/theirs trees → merged tree), representing conflicts as **first-class, re-resolvable typed JSON per PRD §15** (content/binary/rename/dir_file/mode/symlink) — finally **writing into the `conflict_sets` table** beyond Phase 2's stale-base metadata. An agent reads the conflict object, resolves via the contract, the resolution is recorded as evidence and re-checked. **Ship conflict-as-data + manual/agent resolution FIRST; auto-resolution is a gated, never-silent, evidence-backed suggestion** (using Phase 4/5 results + intent scope) layered on top.
3. **Physical per-attempt worktrees** under `.forge/` (the work **deferred out of Phase 3**), non-destructive + concurrency-safe switching, landed here because it depends on the GC that reclaims them. Surface the per-attempt workspace path in attempt show/list JSON.
4. **Real mark-sweep GC** replacing the dry-run-only stub (`gc_dry_run` + the `--dry-run`-only bail in `main.rs`): reachability from refs + reachable snapshots/proposal_revisions/decisions **+ the ledger-tip/op-log roots slice 3 already added**, with a reflog-style safety window, mandatory dry-run preview, `--yes` (`CONFIRMATION_REQUIRED`) for real deletion **gated on `doctor` passing**, crash-safe deletion ordering.
5. **Packfile/delta/compression** (audited crate; custom compression banned) + large-file/streaming to replace whole-file `fs::read`, a **working-tree index/status cache** for fast diff/status at scale, and a retention policy + storage budget (PRD §10.7). **Packing must not break the `f1:` verify-on-read.**

## Carry-over from slice 3 that lands HERE (do not lose these)

- **gc fail-closed on a malformed op-log view (NER-143 #5):** `gc_dry_run` currently silently drops an unparseable `views.state_json` row, under-counting reachability roots. This is **harmless in dry-run but data-loss the moment real deletion lands** — fix it to fail-closed/conservative BEFORE wiring deletion.
- **The reconcile/log/doctor walkers are first-parent-only / single-parent today.** Phase 8's merge engine introduces **multi-parent commits**. `reconcile_native_head` follows `parents.first()`; `native_log`/`verify_native_history` walk all parents but assume linear ancestry for some checks. Revisit all three for true DAG (merge) ancestry — and the reconcile ancestor/fork check (slice-3 §2) must handle merge commits.
- **The gc reachability roots** (ledger `decisions.commit_id` ∪ `reachable_from_head` ∪ op-log `state_json` commit_ids, via `reachable_from(tip)`) are the **root set real deletion must use** — do not regress them; physical worktrees + checkout targets are additional roots GC must protect.
- **Conflict object integrity:** conflicts recorded as evidence must ride the existing tamper-evident chain + secret-redaction (S1/S2); a conflict payload is a machine-visible egress (no excerpts/paths).

## Invariants to NOT regress

All Phase 1–7 invariants: store-before-DB + crash-atomic (temp+fsync+rename+ancestor-fsync, propagate `sync_all`) for new object/pack/worktree writes; WAL+`IMMEDIATE`+busy/517; advisory-lock acquire-once + `run` carve-out; S1 (no fs paths in `anyhow` context, assert `to_string()` AND `{:#}`) / S2 (policy-excluded snapshot/materialize/worktree); the typed-error contract (any new code = full `error.rs` + `forge_schema.rs` fan-out, both drift guards); the schema-head fan-out discipline (grep the old head **and** the moving HEAD+1 stamp) for any new migration; `forge-store` git-free of adapter crates; native primitives in `forge-content-native`; agent-native parity (every new capability in `forge schema` with structured JSON + typed errors). **GC deletion is the dangerous one:** long protection window, mandatory dry-run diff, crash-safe deletion ordering, gate on `doctor` — a reachability bug destroys referenced content incl. in-flight attempts + the new per-attempt worktrees.

## NOT Phase 8 (Phase 9 — NER-140)

Wire protocol / clone-fetch-push-pull / ledger sync; cryptographic **signing** + the trust ladder. (Signing anchors on the slice-3 commit format, which is why `actor`/`authored_time` are already in the hashed bytes — Phase 8 must not break that.)

## Exit criteria (NER-139 → Done)

Native diff is correct with rename detection on a corpus; a 3-way merge of two attempts writes real `conflict_sets`/`PathConflict` rows with correct typed kinds, an agent resolves a conflict via the contract and it is recorded as evidence + re-checked; symlinks/mode round-trip losslessly with crash-atomic restore; `forge gc` reclaims only unreachable objects (reachability fuzz) and never deletes anything reachable from a ref/recent op/attempt-worktree; the packed+compressed store is measurably smaller than loose and than git; a multi-GB file snapshots/restores without OOM; status/diff stay fast on a large synthetic tree via the index; verify-on-read passes through the pack layer.

## Start prompt — see the chat message that accompanied this handoff for the paste-able fresh-session prompt.
