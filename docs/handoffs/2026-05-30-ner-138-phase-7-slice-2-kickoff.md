# Handoff — NER-138 Phase 7 **slice 2**: native commit/Change objects + ref store + backend-agnostic `base_head` + native `changed_paths`

**Date:** 2026-05-30 · **Milestone:** M3 — Earn native-VCS independence · **Ticket:** Linear **NER-138** (umbrella for all of Phase 7; stays **In Progress** until all 3 slices land) · **Forge project:** id `2b5e82f7-7a78-4354-af7d-68609e6e77bc`, team **SE Engineers**, prefix **NER**

## Where things stand

Phase 7 is staged internally into 3 slices. **Slice 1 is merged:**

- **Slice 1 (walker):** native worktree walker + ignore engine on the `ignore` crate (0.4.25), replacing the `git ls-files`/`--exclude-standard` shell-out in `forge-content-native`'s snapshot path; `.gitignore` + `.forgeignore` (precedence `policy > .forgeignore > .gitignore > defaults`); environment-independent (`git_exclude(false)`/`git_global(false)`/`parents(false)`); S1 path-stripped walk errors; `.git`/`.forge` pruned at the walk layer; symlinks yielded so the `fs::metadata` gate preserves symlink-to-file capture; a differential harness proving native-set == prior git-set with each index-vs-filesystem divergence class asserted. **Merged as PR #22 (`cedc351` on `main`).**

`main` is clean and synced. Slice-1 plan: `docs/plans/completed/2026-05-30-012-feat-phase-7-slice-1-native-walker-plan.md`. Code review: `docs/code-reviews/2026-05-30-ner-138-phase-7-slice-1.md`. **Read the slice-1 learnings** — they pin invariants slice 2 must not regress: `docs/solutions/architecture-patterns/native-worktree-walker-ignore-engine-and-index-vs-filesystem-divergence-2026-05-30.md`.

Gate: `bash scripts/ci.sh` (fmt `--check` · `cargo test --workspace` · clippy `-D warnings` · `scripts/e2e-eval.sh`) — **CI runs this on every PR and it passed on #22** (verify ~1m46s). Toolchain pinned `1.92.0`. `gh` authed as `freezscholte`; remote `freezscholte/forge`; squash-merge convention `(#N)`. `schema_head` is **4** (slice 1 added no migration). `FORGE_ERROR_CODES` is **23**.

**Adjacent ticket already filed:** **NER-142** — the NER-137 D1 `-z`/C-quote secret-leak in the *export* path's `filter_secret_paths_from_tree` (`crates/forge-export-git/src/lib.rs`). Its own minimal PR; **not** slice-2 scope, but cheap and can land anytime.

## What's next — slice 2 (native history substrate)

**No plan doc exists yet.** Run the full lifecycle: branch off `main` (e.g. `ner-138-phase-7-slice-2-commit-objects`) → `/ce-plan NER-138` (scoped to slice 2) → **doc-review gate** (`/ce-doc-review`, apply `safe_auto`, fold the rest in) → `/ce-work` → **code-review gate** (`/ce-code-review plan:<path>`) + `bash scripts/ci.sh` → `/ce-commit-push-pr` referencing NER-138. On merge: flip the slice-2 plan to `completed` + move to `docs/plans/completed/`, `/ce-compound`, write the **slice-3** handoff.

### Slice-2 scope (from the ROADMAP / ticket)

1. **Native Commit/Change `ObjectKind`** (today `ObjectKind` is `Blob` + `Tree` only, in `forge-content-native/src/lib.rs`). Content-addressed, domain-separated, referencing **tree + parent(s) + the proposal_revision/decision/intent that justified it + an evidence digest** — so history is *intent-aware from the first commit* (the thing git's commit graph cannot represent). Reuse the existing **`f1:` versioned object tag** and the length-prefixed, domain-separated `ObjectId::new` hashing (custom crypto banned — extend the `ObjectKind::as_str` domain set).
2. **Native ref store under `.forge`** + a **hash-registry / object-format migration table from day one** — the commit-object schema is a near-permanent format commitment; version it from the start so the format can evolve.
3. **Backend-agnostic `base_head`** — this is the load-bearing reversal. Replace the **deliberately-confined git delegation** in `NativeContentBackend::current_base` / `base_content_ref` (they currently shell `git rev-parse` and return `git-tree:` refs; both carry a `// Phase 7 (NER-138): replace with native base anchoring` marker and a "do NOT emit `forge-tree:` here yet" guard — **slice 2 is where you ARE allowed and required to do what Phase 3 forbade**). Anchor native repos on a native tree/snapshot id, not a git commit. Honor S1 (no fs paths in `anyhow` context) and S2 (`base_content_ref` → a policy-excluded tree — `.env`/keys never materialized).
4. **Native `changed_paths`** — replace the `git diff --name-only HEAD` + `git ls-files --others` shell-out in `changed_paths` (`forge-content-native/src/lib.rs`) by diffing the **prior snapshot/base tree (now a native tree, per #3) against the walked tree** — *name-level only*. This is coupled to #3 (that's why slice 1 deferred it). **Hunk-level native content diff is PHASE 8 — do NOT build it.**

### The schema fan-out slice 2 owns (slice 1 deliberately had none)

A numbered **`005_*.sql`** migration (native ref store + Commit/Change object metadata + backend-agnostic `base_head` column changes) with the **full `schema_head` 4 → 5 head-bump fan-out**:

- `crates/forge-store/src/migrations.rs`: add `005` to `MIGRATIONS`; `schema_head()` returns 5; the `schema_head_is_max_version` test asserts `== 5`; the HEAD+1 fixtures bump to 6; the convergence/at-head fixtures must **stub every table 005 touches**.
- **Grep the WHOLE test tree + `scripts/e2e-eval.sh` for the literal `4`** and update each: `e2e-eval.sh` has it at the `doctor schema_version=4` check and `schema_migrations has versions 1,2,3,4` check (and the HEAD+1 insert uses `5` → bump to `6`).
- Any **new error code** (e.g. base-anchoring/ref-store IO failures) = typed `ForgeError` + `code()`/`details()`/`retryable()`/`Display` + `error_registry()` `ErrorCodeSpec` + **both** `error.rs` drift-guard tests (the `all` array AND the exhaustive match) + the `FORGE_ERROR_CODES` list in `tests/forge_schema.rs` (currently **23**) — all in one change, or the contract drifts. Prefer mapping onto existing codes where honest.

## Carry-over invariants slice 2 must honor (do NOT regress)

- **Durability (Phases 1a/1b)** — the new commit/ref objects inherit slice-1's `NativeObjectStore::write_object` discipline verbatim: **store-before-DB ordering** (object file + its dir entry durable BEFORE the SQLite txn that commits the referencing row), crash-atomic writes (temp + `sync_all` + atomic rename + parent-dir fsync incl. newly-created ancestors), propagate-never-swallow `sync_all`, WAL + `IMMEDIATE` + busy/517 retry, advisory-lock **acquire-once-never-nested** (+ the lock-free `run` carve-out), in-txn determining read. The ref store under `.forge` is mutable state — it needs the same crash-atomic + store-before-DB treatment as objects.
- **Boundary (Phase 3 / Phase 6 §1)** — everything through the `ContentBackend` trait; `forge-store` stays **git-free** (the `ignore` crate and any native-history code live in `forge-content-native`, not `forge-store`/`forge-content`); replace the git-tree delegation; `base_head` backend-agnostic.
- **Secret hygiene (Phases 4/5 + slice 1)** — the policy backstop (`is_ignored_by_policy`/`is_secret_risk_path`) stays always-wins; `base_content_ref` must reference a **policy-excluded** tree (S2); new content-addressing reuses the length-prefixed domain-separated `DigestWriter`/`ObjectId` hashing.
- **Compare/export (Phase 6)** — backend-agnostic `base_head` (native tree id) must keep `compare_attempts` / `export branch` / `export verify-branch` working. The git-adapter diff (`diff_trees` in `forge-export-git`) operates on `base_head` and attempt-bound snapshot `content_ref`s — making `base_head` a native id must not break it (git export stays as **interop**).
- **The differential-harness discipline** — if slice 2 changes the snapshot/base set semantics, extend the harness; prove equivalence before deleting any remaining git call. After slice 2, native `save` should be **fully git-free** for snapshot + base + changed_paths (only `log`/checkout/`undo`/symlink-content/object-kind-headers remain for slice 3).

## NOT slice 2 (scope discipline)

- `log` / historical checkout / `forge undo` / op-restore, symlink **content** round-trip (mode 120000), object-kind headers (killing the `all_object_ids` double-hash scan), git-export demotion to optional interop → **slice 3**.
- Native **content** diff at hunk granularity, the 3-way merge engine, real mark-sweep GC, pack/delta/compression, working-tree index/status cache, physical per-attempt worktrees → **Phase 8 (NER-139)**.
- Wire protocol / ledger sync / signing → **Phase 9**.

## Whole-phase exit criteria (achieved across all 3 slices — do NOT set NER-138 → Done until all land)

A native-backend repo completes init→save→run→propose→check→restore, walks its own history, checks out any past commit, and `forge undo` restores a prior operation — **all with git removed from PATH**; the differential test proves snapshot-set equality (incl. secret-risk exclusion); no `git ls-files`/`git diff` in native paths (grep); symlinks + object-kind headers round-trip; the DAG has no cycles/dangling parents (doctor verifies); git export still works as interop.

## Start prompt for the next session

> Pick up NER-138 Phase 7 **slice 2**: native Commit/Change `ObjectKind` + native ref store under `.forge` + backend-agnostic `base_head` + native `changed_paths` (name-level). Read `docs/handoffs/2026-05-30-ner-138-phase-7-slice-2-kickoff.md` and the slice-1 learnings `docs/solutions/architecture-patterns/native-worktree-walker-ignore-engine-and-index-vs-filesystem-divergence-2026-05-30.md` first. This slice replaces the Phase-3-marked git-tree delegation in `NativeContentBackend::current_base`/`base_content_ref` (the one place you ARE allowed to emit `forge-tree:` base refs), and owns the numbered `005_*.sql` migration + full `schema_head` 4→5 fan-out. Run the lifecycle: branch `ner-138-phase-7-slice-2-commit-objects` off main → `/ce-plan NER-138` (scoped to slice 2) → `/ce-doc-review` gate → `/ce-work` → `/ce-code-review plan:<path>` + `bash scripts/ci.sh` → `/ce-commit-push-pr` referencing NER-138. NOT slice 2: log/checkout/undo/symlink-content/object-kind-headers (slice 3); content diff/merge/GC (Phase 8).
