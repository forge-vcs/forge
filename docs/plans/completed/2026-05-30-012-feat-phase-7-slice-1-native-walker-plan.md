---
title: "feat: Phase 7 Slice 1 — Native worktree walker + ignore engine + differential harness"
type: feat
status: completed
date: 2026-05-30
deepened: 2026-05-30
origin: docs/ROADMAP.md  # Phase 7 section; ticket NER-138. Slice 1 of 3 (XL phase, staged internally). No dedicated *-requirements.md — the ROADMAP Phase 7 entry + the NER-134 Phase-3 boundary solution doc are the source.
---

# feat: Phase 7 Slice 1 — Native worktree walker + ignore engine + differential harness

## Summary

Replace the native content backend's `git ls-files` / `--exclude-standard` shell-out (the worktree enumerator behind `snapshot_worktree`) with a native, git-binary-free, **environment-independent** worktree walker built on the audited `ignore` crate. The walker honors repo-local `.gitignore` + `.forgeignore` with documented precedence and preserves the shared `is_ignored_by_policy` / `is_secret_risk_path` exclusion *exactly* as an always-wins security backstop. A differential test harness proves the native-walked snapshot set equals the prior git-based set across a deliberately adversarial corpus (including secret-risk and special-byte-path exclusion) **before** the `git ls-files` call is removed — with all index-vs-filesystem divergence classes enumerated and asserted explicitly rather than papered over. This is the first and riskiest slice of NER-138 (Phase 7); native `changed_paths` and backend-agnostic `base_head` are deferred to slice 2 because they are technically coupled to native base anchoring.

---

## Problem Frame

Forge's "native" content backend is not yet independent of git: `snapshot_worktree` shells out to `git ls-files` and `git ls-files --others --exclude-standard` to enumerate worktree paths (`crates/forge-content-native/src/lib.rs`, `snapshot_candidate_paths`). This is the deliberately-confined git leak that Phase 3 isolated behind the `ContentBackend` trait and explicitly left for Phase 7 to reverse (see origin: `docs/solutions/architecture-patterns/write-binding-verification-and-content-backend-isolation-2026-05-29.md`, §4). Until the worktree walk is native, the "native" backend is a git wrapper, a class of environment-dependent failures persists, and Forge cannot honestly claim to be a git *alternative*. The riskiest part of cutting this dependency is the ignore engine: a subtle divergence from git's path set could **leak a `.env`/private key** or **silently drop a tracked file**. The differential harness is the safety net that makes the reversal safe.

---

## Requirements

- R1. The native backend enumerates worktree paths for snapshotting **without invoking the `git` binary** — the `git ls-files` / `--others --exclude-standard` shell-out in the native snapshot path is replaced by an `ignore`-crate walk.
- R2. The native walker honors repo-local `.gitignore` (nested, with negation) so that, **in a clean environment** (no `.git/info/exclude`, no global `core.excludesfile`), the walker's path set matches the prior git-based set for all paths that are not index-only (see R5 divergence classes).
- R3. The native walker honors a `.forgeignore` file with **documented precedence** (`is_ignored_by_policy` > `.forgeignore` > `.gitignore` > defaults), establishing the net-new Forge-specific ignore contract the PRD left as an open question (`PRD.md:545`).
- R4. The shared `forge_content::is_ignored_by_policy` / `is_secret_risk_path` exclusion is preserved **exactly** as an always-wins backstop applied to the walker's output — no secret-hygiene regression. `.env`, `.env.*`, `*.pem/.key/.p12/.pfx`, `id_rsa`/etc., `*credential*`/`*secret*`-named files, `.forge/`, `.git/`, and `.forge-restore-*` temps (at any depth) are excluded identically to today. A `.forgeignore` `!`-negation can never re-include an `is_ignored_by_policy` path.
- R5. A differential test harness proves the native-walked snapshot set **equals** the prior git-based set across a parity corpus that deliberately excludes index-only paths, AND **separately enumerates and asserts each index-vs-filesystem divergence class** (force-added-ignored, tracked-then-later-ignored, tracked-but-deleted-from-disk, tracked symlink-to-file [see R8], submodule gitlink, case-folded `.gitignore`, and global/`.git/info/exclude`). The `git ls-files` call is **not removed until that harness is green**.
- R6. No filesystem path (especially a secret-named one) leaks into `anyhow` error context from walk errors (security invariant S1) — including the error's full source chain (`{:#}`), not only its top-level `to_string()`. A walk failure surfaces a path-free message; the path-bearing `ignore::Error` payload is stripped, not merely wrapped.
- R7. The walker's output for a base/snapshot tree continues to satisfy S2 (the set excludes `is_ignored_by_policy` paths) — preserved by routing the walk output through the policy backstop.
- R8. No regression to existing native-backend behavior: `forge save` still produces a `forge-tree:` content ref, restore still round-trips, **tracked symlinks-to-files are still captured by content** (the walker yields symlink entries; the existing `fs::metadata`/`is_file` gate remains the capture authority), and the `fmt/test/clippy -D warnings` + e2e CI gate stays green.

**Origin actors:** N/A (infrastructure/substrate work; no end-user actor change).
**Origin flows:** F — `save` (snapshot worktree), `restore` (dirty-worktree guard reads the same snapshot set).
**Origin acceptance examples:** Exit criteria from NER-138 (whole-phase, partially advanced here): "the differential test proves snapshot-set equality (including secret-risk exclusion); no `git ls-files` … in native paths (grep)" — slice 1 delivers the differential proof and removes `git ls-files` from the native *snapshot* path.

---

## Scope Boundaries

- **No native `changed_paths`.** `changed_paths` ("what differs from git HEAD") is technically coupled to having the base commit's tree as a *native* tree. It stays shelling `git diff --name-only` behind the trait, unchanged.
- **No backend-agnostic `base_head`.** `current_base` / `base_content_ref` keep returning git-derived `git-tree:` refs. The `// Phase 7 (NER-138): replace with native base anchoring` marker comments stay in place. Phase 3 §4 **explicitly forbids** emitting `forge-tree:` base refs before the native base anchoring exists.
- **Half-native save is intentional at this checkpoint.** After slice 1, a single native-backend `forge save` walks the snapshot *natively* but still calls `changed_paths`, `current_base`, and `base_content_ref` which *shell git* in the same operation. So a "git removed from PATH" run will partially fail until slice 2 (`current_base`/`base_content_ref` hard-error; `changed_paths` degrades to empty via its existing `if let Ok` swallow). This is a deliberate intermediate state — **do not test this checkpoint for full native independence**; that exit criterion is whole-phase (slices 2+3). U6's e2e block therefore keeps git in PATH.
- **No commit/Change `ObjectKind`, no native ref store, no `log`/checkout/`undo`** (slices 2 and 3).
- **No symlink *content* support** (mode 120000 round-trip is slice 3). Slice 1 preserves today's behavior: symlinks-to-files are captured by content via the existing `fs::metadata`/`is_file` gate; the walk does not follow symlinks *into* directories.
- **No object-kind headers / `all_object_ids` double-hash-scan removal** (slice 3).
- **No native content diff (hunk-level), no 3-way merge, no real GC, no packing, no per-attempt worktrees** (Phase 8).
- **No schema migration.** Slice 1 adds no tables; `schema_head` stays `4`. The `4` literals in `scripts/e2e-eval.sh` (lines 61, 122) and `migrations.rs` are untouched.

### Deferred to Follow-Up Work

- Native `changed_paths` + backend-agnostic `base_head` (native base anchoring): **slice 2** (follow-on PR under NER-138).
- Native `log` / historical checkout / `forge undo` / symlink round-trip / object-kind headers / git-export-as-interop demotion / full `git-removed-from-PATH` e2e block: **slice 3**.
- **NER-142** (filed): fix the NER-137 **D1** `-z`/C-quote secret-leak in the *export* path's `filter_secret_paths_from_tree` (`crates/forge-export-git/src/lib.rs`). Same `is_secret_risk_path`-on-quoted-git-output bug class as the walker work, but a separate crate with no dependency on U1–U4. Done as its **own minimal PR** (not bundled here) so the riskiest walker reversal keeps an undiluted security review — committed via NER-142, not left optional.

---

## Context & Research

### Relevant Code and Patterns

- `crates/forge-content-native/src/lib.rs` — `NativeContentBackend::snapshot_worktree` → `scan_worktree` → `snapshot_candidate_paths` (the `git ls-files` union, the function slice 1 replaces) → `is_ignored_by_policy` post-filter + `fs::metadata`/`is_file` gate in `scan_worktree`. The `git()` shell-out helper. The existing test module (`#[cfg(test)] mod tests`) already `git init`s a tempdir and snapshots it (`restore_roundtrips_atomically_and_leaves_no_temp`) — the pattern the differential test follows.
- `crates/forge-content/src/lib.rs` — the shared `is_ignored_by_policy` (= `.forge` ∪ `.git` ∪ `is_restore_temp_path` ∪ `is_secret_risk_path`) and `is_secret_risk_path` (matches on the *filename component* via `rsplit('/')`, **case-insensitively** via `to_ascii_lowercase()` — so a case-variant secret name is caught regardless of `.gitignore` casing). The single predicate both backends must consult so the exclusion set cannot drift (NER-133 U6). `.gitignore`/`.gitattributes`/`.github/` are NOT under `.git/` and remain eligible.
- `crates/forge-content-git/src/lib.rs` — the **reference** walker: `all_tracked_paths` (`git ls-files`), `untracked_paths` (`git ls-files --others --exclude-standard`, filtered by `is_ignored_by_policy`). Neither uses `-z`; both parse with `.lines()`. The differential harness reproduces `set(ls-files) ∪ set(ls-files --others --exclude-standard)`, policy-filtered, deduped, sorted — in a clean environment.
- `crates/forge-content-native/Cargo.toml` + root `Cargo.toml` `[workspace.dependencies]` — dependency declaration convention (`foo.workspace = true`). `Cargo.lock` is committed.
- `crates/forge-cli/tests/common/mod.rs` — `TestRepo::new_git()` (the only constructor); native tests start from it and pass `--content-backend native`.
- `scripts/e2e-eval.sh` — git-backed `LIFECYCLE` block at lines 57–71; debug binary at `target/debug/forge`; `mkrepo` always `git init`s. The `4` schema literals at lines 61, 122 (untouched — no migration).

### Institutional Learnings

- **Phase 3 (`write-binding-verification-and-content-backend-isolation-2026-05-29.md`):** §4 "confine a leaking adapter behind a trait before you replace it — and forbid the premature fix": slice 1 replaces *only* the walker; it must NOT touch `current_base`/`base_content_ref`/`git-tree:` emission. §5 S1 (no fs paths in anyhow context — they bypass typed-error redaction) and S2 (base/snapshot set must stay policy-excluded). The pre-registered Phase-3 deferred follow-ups — the **S1 path-free-error test** and the **S2 cross-backend planted-secret round-trip** — are slice-1 deliverables (see `docs/code-reviews/2026-05-29-ner-134-phase-3.md`).
- **Phase 6 (`compare-rank-on-verified-evidence-and-self-verifying-provenance-trailer-2026-05-30.md`):** §1 git-adapter boundary — `forge-store` stays git-free; the walk/ignore work lives in the `forge-content-native` adapter, and the `ignore` dependency must NOT be added to `forge-store` or `forge-content`. §5 the `-z`/C-quote class: `git ls-files`/`ls-tree` C-quote paths with tab/newline/non-ASCII bytes by default, so a quoted `.env`-with-special-byte slips `is_secret_risk_path` and leaks. The `ignore`-crate walker is the **structural cure** — it yields real `PathBuf`s, never quoted strings. NER-137 **D1** (the same flaw still open in the export path) is filed as **NER-142**.
- **Durability (Phases 1a/1b):** the walker writes no objects and no DB rows, so store-before-DB ordering, crash-atomic restore, and propagate-`sync_all` do not apply. The walker must be **lock-agnostic** (never acquire `.forge/forge.lock` — the nested-acquire deadlock footgun) and treats its output as an advisory point-in-time snapshot. The one durability-adjacent rule that *does* apply: the `.forge-restore-*` exclusion (at any depth) is enforced by `is_ignored_by_policy`, not by `.gitignore`.
- **Secret hygiene (Phases 4/5):** a read-only walker hashes nothing, so the `DigestWriter` discipline is a guardrail (use plain sorted-vector set equality, no ad-hoc fingerprinting). The additive-error drift-guard fan-out applies *only if* slice 1 introduces a new typed error code — it does not (see Key Technical Decisions).

### External References

- `ignore` crate 0.4.25 — `WalkBuilder` (https://docs.rs/ignore/0.4.25/ignore/struct.WalkBuilder.html). Confirmed defaults: `hidden`=enabled (skips dotfiles), `ignore`=enabled (reads `.ignore`), `git_ignore`/`git_global`/`git_exclude`/`parents`/`require_git`=enabled, `follow_links`=disabled, `ignore_case_insensitive`=disabled. `add_custom_ignore_filename(".forgeignore")` exists and is **higher precedence** than `.gitignore`. `sort_by_file_name` gives deterministic ordering for the **serial** `Walk` (not the parallel walker). `build()` yields `Walk` with `Item = Result<DirEntry, ignore::Error>`; per-entry errors surface inline. `filter_entry(pred)` prunes a directory's entire subtree when `pred` returns false (avoids descending into it). **`ignore::Error::WithPath`/`Loop` embed the filesystem path in their `Display`** — so the error payload must be stripped, not just wrapped (R6).

---

## Key Technical Decisions

- **Use the `ignore` crate (0.4.25), single workspace dependency.** It bundles `walkdir` + `globset` + correct nested-gitignore/negation semantics — the "deceptively hard" part the ROADMAP warns against re-implementing. Added to `[workspace.dependencies]` and referenced from `forge-content-native` only (keeps the dependency graph acyclic; honors §23.4). Gate-safe (published 2025-10-30; Cargo has no min-release-age gate configured, but it clears 4 days regardless).
- **`WalkBuilder` configuration — environment-independent by design.** The native walker depends ONLY on repo-local ignore state, which is Phase 7's whole point (reproducible runs free of environment-dependent failures):
  - `hidden(false)` — git lists dotfiles (`.gitignore`, `.github/`); the default would wrongly drop them.
  - `git_ignore(true)` — honor repo-local `.gitignore` (nested, with negation).
  - `git_exclude(false)`, `git_global(false)` — do **not** consult `.git/info/exclude` or the user's global `core.excludesfile`. These are git-private / machine-specific and would reintroduce environment-dependence; excluding them makes a repo's native snapshot set reproducible across machines. (Intentional, enumerated divergence from `git ls-files --exclude-standard` — see R5.)
  - `parents(false)` — the repo root is the ignore boundary (matches git, which stops at the worktree root); prevents reading parent-directory `.gitignore` above the repo, which `require_git(false)` would otherwise enable and which is both a parity divergence and a production-surprise.
  - `ignore(false)` — git does not honor the crate's own `.ignore` file; disabling preserves parity.
  - `require_git(false)` — honor `.gitignore` even without a `.git` dir (native independence).
  - `follow_links(false)` — do not traverse *into* symlinked directories.
  - `add_custom_ignore_filename(".forgeignore")` — higher precedence than `.gitignore`.
  - `sort_by_file_name(..)` — deterministic ordering on the serial walker.
- **Prune `.git`/`.forge` at the walk layer.** Apply `filter_entry(|e| !is_ignored_by_policy(rel_path(e)))` so the walk never recurses into `.git/` (≈2170 internal files in this repo) or `.forge/`. This **reuses** the shared predicate (it is not a fork — the post-walk `is_ignored_by_policy` filter remains the authoritative backstop; `filter_entry` is a descent-pruning optimization layered on the same rule). Without it, every `save` stats thousands of `.git` internals only to discard them.
- **Yield files AND symlinks at the walk layer; let the metadata gate decide capture.** Do **not** filter on `file_type().is_file()` in the walk loop — a symlink's `file_type()` is `is_symlink`, so that would drop a tracked symlink-to-file that today's `fs::metadata` (which follows the link) captures, regressing R8. The walker skips directories (handled by recursion) and yields regular files and symlinks; the existing downstream `fs::metadata`/`is_file` gate in `scan_worktree` stays the capture authority.
- **Layered exclusion, policy-as-backstop.** The `ignore` engine handles `.gitignore` + `.forgeignore`; `forge_content::is_ignored_by_policy` is applied to the engine's output as the **authoritative, always-wins** secret/internal backstop. A `!`-negation in `.forgeignore` can re-include a `.gitignore`-excluded path, but never an `is_ignored_by_policy` path.
- **Documented precedence (net-new contract):** `is_ignored_by_policy` (always wins, non-negotiable) > `.forgeignore` > `.gitignore` > built-in defaults. Recorded in the walker module doc; pinned by tests (U4).
- **S1 error handling — strip the path, do not just wrap it.** `ignore::Error`'s own `Display` embeds the offending path (`WithPath`/`Loop`). A naive `entry_err.context("failed to walk worktree")` would carry that path into the anyhow source chain, visible under `{:#}` and any chain-rendering surface. Instead, map a walk error to a fresh, path-free `anyhow` error built from only the `io::ErrorKind` (path-free), discarding the `ignore::Error` payload. The S1 test asserts no path bytes appear in **both** `error.to_string()` (the envelope's rendering) and `format!("{:#}", error)` (the full chain).
- **Walk-error policy (resolved):** a per-entry `NotFound`/ENOENT (a file that vanished between enumeration and read — realistic under a concurrent agent fleet) is **skipped-and-continued** (benign; mirrors the existing `fs::metadata` `_ => continue` arm — a deleted file simply isn't snapshotted). Any other walk error (permission denied, other IO) is **fail-closed** with the path-free message above. This is documented inline so the `fs::metadata` silent-skip is recognizably intentional, not an accidental swallow.
- **No new typed error code.** Walk failures use the path-free `anyhow` mapping above — no filesystem path enters the envelope. This avoids the additive drift-guard fan-out (`FORGE_ERROR_CODES` stays at 23; both `error.rs` drift-guard tests and the `forge_schema` list are untouched). If a typed walk-failure code later proves warranted, the full additive fan-out applies then.
- **Differential parity excludes `.forgeignore` and index-only paths.** Git knows nothing of `.forgeignore`, so the native-vs-git set-equality corpus contains no `.forgeignore`; `.forgeignore` behavior is proven by native-only assertions (U4). Index-only divergence classes are asserted separately, not folded into the equality corpus (U3).
- **Set comparison, not hashing.** The harness compares sorted `Vec<String>` path sets (final `scan_worktree`-level output) — no fingerprinting.
- **Path normalization:** the walker emits repo-relative, `/`-separated `String` paths (stripping `repo_root`), matching `is_secret_risk_path`'s `rsplit('/')` and the existing tree builder. (Targets macOS/Linux CI; Windows `\`-separator normalization is out of slice-1 scope and the differential test is `#[cfg(not(windows))]`-guarded so a future Windows CI does not silently fail.)
- **Case-folding:** the walker uses the crate's default case-sensitive `.gitignore` matching (`ignore_case_insensitive(false)`) for cross-platform determinism. On a case-insensitive filesystem (macOS) this can differ from git for a case-variant `.gitignore` rule — an enumerated, accepted divergence (R5). The **secret-leak** angle is unaffected: `is_secret_risk_path` lowercases the filename, so a case-variant secret name is still excluded by the backstop.

---

## Open Questions

### Resolved During Planning

- *Should native `changed_paths` ship in slice 1?* No — coupled to native base anchoring; deferred to slice 2 (confirmed with user).
- *Which crate?* `ignore` 0.4.25 (confirmed with user; clears the supply-chain gate).
- *`.gitignore` vs `.forgeignore` precedence?* `.forgeignore` higher (via `add_custom_ignore_filename`); policy backstop always wins. Documented.
- *Does slice 1 need a schema migration / error-code fan-out?* No to both.
- *Should the walker honor `.git/info/exclude` / global gitignore?* No — `git_exclude(false)`, `git_global(false)`, `parents(false)` for environment-independent reproducibility (the Phase 7 goal). Divergence from `git ls-files --exclude-standard` on those is intentional and enumerated.
- *Walk-error policy?* `NotFound` → skip-and-continue (benign mid-walk delete); other errors → fail-closed, path-free. Resolved above.
- *Symlink-to-file capture?* Walker yields symlink entries; the downstream `fs::metadata`/`is_file` gate decides — preserves today's behavior (R8).

### Deferred to Implementation

- The exact module shape (a new private `walk_worktree` fn in `lib.rs` vs. a small `walk.rs` submodule) — decide during U2 based on size; the differential test calls it directly, so it stays in-crate.
- The precise `filter_entry` rel-path computation (stripping `repo_root`, `/`-joining) — mechanical; mirror the existing path handling in `scan_worktree`.

---

## High-Level Technical Design

> *This illustrates the intended approach and is directional guidance for review, not implementation specification. The implementing agent should treat it as context, not code to reproduce.*

Snapshot path enumeration, before vs. after:

```
BEFORE (current state):
  scan_worktree(repo_root)
    └─ snapshot_candidate_paths(repo_root)
         ├─ git ls-files                              ← shell-out
         └─ git ls-files --others --exclude-standard  ← shell-out
    └─ filter: !is_ignored_by_policy(path)
    └─ filter: fs::metadata(path).is_file()           (follows symlinks)

AFTER (slice 1):
  scan_worktree(repo_root)
    └─ walk_worktree(repo_root)                       ← native, no git binary
         WalkBuilder::new(repo_root)
           .hidden(false).ignore(false)
           .git_ignore(true)
           .git_exclude(false).git_global(false)      // environment-independent
           .parents(false).require_git(false)
           .follow_links(false)
           .add_custom_ignore_filename(".forgeignore")   // higher precedence than .gitignore
           .filter_entry(|e| !is_ignored_by_policy(rel))  // prune .git/.forge descent (reuses predicate)
           .sort_by_file_name(..)
           .build()  →  yields Result<DirEntry, ignore::Error>
                         (Err: NotFound → skip; else → fresh PATH-FREE anyhow error, S1)
                         (yields files AND symlinks; only dirs skipped at walk layer)
    └─ filter: !is_ignored_by_policy(path)            ← unchanged backstop (always wins)
    └─ filter: fs::metadata(path).is_file()           ← unchanged (symlink-to-file still captured)
```

Exclusion precedence (highest wins):

```
is_ignored_by_policy   (.forge/, .git/, .forge-restore-*, secret-risk)   ← ALWAYS wins; not negatable
  >  .forgeignore      (Forge-specific; ! can re-include a .gitignore drop)
  >  .gitignore        (repo-local, nested, with negation)
  >  built-in defaults
```

Index-vs-filesystem divergence classes (U3 asserts each explicitly; the parity-equality corpus excludes them):

```
git ls-files (index-based)        native walk (filesystem)      handled by
─────────────────────────────     ────────────────────────      ──────────────────────
force-added-ignored: LISTS         drops                          assert-and-annotate
tracked-then-later-ignored: LISTS  drops                          assert-and-annotate
tracked-but-deleted-on-disk: LISTS drops                          assert-and-annotate
submodule gitlink: LISTS           descends/skips differently     assert-and-annotate (or scope-out w/ comment)
case-folded .gitignore (macOS):    may keep (case-sensitive)      enumerate; secrets covered by backstop
.git/info/exclude + global:        honored                        intentionally NOT honored (reproducibility)
```

Differential harness shape (U3): build a tempdir corpus with `git init`, run BOTH the retained git-based `snapshot_candidate_paths`→`scan_worktree` path and the new native `scan_worktree`→`walk_worktree` path, assert the sorted path sets are **equal** for the parity corpus (no index-only paths, no `.forgeignore`, clean environment), and assert each divergence class above with its own annotated case.

---

## Implementation Units

### U1. Add the `ignore` crate dependency

**Goal:** Make `ignore = "0.4.25"` available to `forge-content-native` via the workspace dependency convention, with `Cargo.lock` updated and committed.

**Requirements:** R1 (enabler)

**Dependencies:** None

**Files:**
- Modify: `Cargo.toml` (add `ignore = "0.4.25"` to `[workspace.dependencies]`, alphabetically)
- Modify: `crates/forge-content-native/Cargo.toml` (add `ignore.workspace = true` to `[dependencies]`)
- Modify: `Cargo.lock` (regenerated by the build; commit the change)

**Approach:**
- Mirror the existing `foo.workspace = true` pattern. Do **not** add `ignore` to `forge-content` or `forge-store`.
- Verify the dependency graph stays acyclic and the workspace builds.

**Patterns to follow:** existing `[workspace.dependencies]` block and `crates/forge-content-native/Cargo.toml`.

**Test scenarios:** Test expectation: none — dependency scaffolding, no behavioral change. Verified by `cargo build --workspace` and `Cargo.lock` containing `ignore` + its transitive deps (`globset`, `walkdir`, …).

**Verification:** `cargo build --workspace` succeeds; `ignore` resolves to a 0.4.x version in `Cargo.lock`.

---

### U2. Native worktree walker + ignore engine

**Goal:** Replace the native snapshot path's `git ls-files` union with a native `ignore`-crate walk honoring repo-local `.gitignore` + `.forgeignore` (documented precedence) and the `is_ignored_by_policy` backstop, with S1-safe (path-stripped) error handling, symlink-yielding, and `.git`/`.forge` walk-layer pruning. Land together with U3; the git call is removed only after U3 is green.

**Requirements:** R1, R2, R3, R4, R6, R7, R8

**Dependencies:** U1

**Files:**
- Modify: `crates/forge-content-native/src/lib.rs` — add a private `walk_worktree(repo_root) -> Result<Vec<String>>`; rewire `scan_worktree` to call `walk_worktree` **directly**, removing `snapshot_candidate_paths` (its `git ls-files` arm) once U3 passes. Keep the `git()` helper (still used by `changed_paths`/`current_base`/`base_content_ref` — slice 2).
- Test: `crates/forge-content-native/src/lib.rs` (`#[cfg(test)] mod tests` — see U3 for the differential corpus and U4 for `.forgeignore`).

**Approach:**
- Configure `WalkBuilder` per Key Technical Decisions (environment-independent toggles; `filter_entry` pruning of `is_ignored_by_policy` directories; `add_custom_ignore_filename(".forgeignore")`; `sort_by_file_name`).
- Yield regular files **and symlinks** from the walk; do not filter on `file_type().is_file()` at the walk layer. Emit repo-relative `/`-separated paths.
- Apply `is_ignored_by_policy` to the walk output as the always-wins backstop (preserve the existing post-filter and the `fs::metadata`/`is_file` capture gate).
- On a walk `Err`: `NotFound` → skip-and-continue; otherwise map to a **fresh path-free** `anyhow` error built from `io::ErrorKind` only (never interpolate the entry path, never carry the `ignore::Error` as a path-bearing source).
- Leave `changed_paths`, `current_base`, `base_content_ref`, and the `// Phase 7 (NER-138)` markers **untouched**.
- **Sequencing/clippy:** U2 and U3 land in **one commit/PR**. While both paths coexist, the retained git-based helper keeps a `#[cfg(test)]` caller (the U3 differential test) so `clippy -D warnings` does not flag it as dead code; the git arm is deleted in U2's final step after U3 is green.

**Execution note:** Do not delete the `git ls-files` arm until the differential harness (U3) is green — sequencing is the safety net.

**Patterns to follow:** `crates/forge-content-git/src/lib.rs` `untracked_paths` (policy-filter shape); the existing `scan_worktree` structure; the Phase 3 §5 S1 path-free error discipline.

**Test scenarios:**
- Happy path: a worktree with `src/a.rs`, `README.md`, `.gitignore`, `.github/workflows/ci.yml` → walker yields all four (dotfiles via `hidden(false)`).
- Edge case: nested dirs — `a/b/c/deep.txt` yielded with the correct `/`-joined relative path.
- Edge case: `.gitignore` with `ignored.log` excludes it; `!important.log` negation re-includes.
- Edge case: `.forgeignore` `build/` excludes `build/out.bin` not in `.gitignore`; `.forgeignore` `!keep.tmp` re-includes a `.gitignore`-excluded `keep.tmp`.
- Edge case (symlink, R8): a tracked symlink-to-file is yielded by the walk and captured by content (regression guard for the `file_type` trap).
- Edge case (`.git` prune, perf/correctness): the walk does not recurse into `.git/` or `.forge/` (no internal objects in the candidate set; `filter_entry` prunes descent).
- Error path (S1): a walk error on a secret-named path surfaces an error whose `to_string()` **and** `{:#}` contain no path bytes.
- Error path: a file removed mid-walk (`NotFound`) is skipped without aborting the snapshot.
- Edge case (R4 backstop): `.env`, `certs/server.pem`, `id_rsa`, `app-secret.txt`, `.forge/forge.db`, `.git/config`, `src/nested/.forge-restore-xyz` all absent from the final output even when not listed in `.gitignore`.

**Verification:** `forge save` on a native-backend repo still produces a `forge-tree:` content ref and restores byte-identically; the snapshot path set no longer depends on `git ls-files` (`grep`); symlink-to-file capture preserved.

---

### U3. Differential test harness (native set == prior git-based set)

**Goal:** Prove the native walker's snapshot set equals the prior git-based set across a clean-environment parity corpus, and assert each index-vs-filesystem divergence class explicitly. Subsume the pre-registered Phase-3 S1/S2 tests. **Gates** removal of the `git ls-files` arm in U2.

**Requirements:** R5, R4, R6, R7, R8

**Dependencies:** U1, U2

**Files:**
- Test: `crates/forge-content-native/src/lib.rs` (`#[cfg(test)] mod tests` — co-located so it can call the private `walk_worktree` and the retained git-based helper directly; `#[cfg(not(windows))]`-guarded).

**Approach:**
- Build a corpus tempdir (`git init`, clean environment) containing: tracked files, untracked-non-ignored files, a `.gitignore` (with a negation), gitignored files, `.env` and other secret-risk names (incl. one with a **non-ASCII/tab byte** — the C-quote leak shape), a `.forge/` dir, a `.git/` dir, and an orphaned `src/nested/.forge-restore-*` temp at depth.
- Assert `sorted(native_set) == sorted(git_based_set)` for this parity corpus (no `.forgeignore`, no index-only paths).
- **Commit to assert-explicitly** for each divergence class (remove the "or keep out of corpus" option): a separate annotated case per class — force-added-ignored, tracked-then-later-ignored, tracked-but-deleted-on-disk, submodule gitlink (assert-and-annotate, or scope-out with an explicit comment if a real submodule fixture is impractical), case-folded `.gitignore` (note macOS behavior), and global/`.git/info/exclude` intentionally-not-honored. Each asserts the index-based git set and the filesystem native set with a comment explaining the no-index semantic.
- Assert (S2 cross-backend planted-secret round-trip): every planted secret-risk path is **absent from both** sets.
- Assert (S1): a forced walk error on a secret-named path yields a path-free error in both `to_string()` and `{:#}` (may be a sibling test).

**Patterns to follow:** the existing `restore_roundtrips_atomically_and_leaves_no_temp` test (git-init-a-tempdir-in-a-native-unit-test); `crates/forge-cli/tests/forge_start_save.rs` `.gitignore` exclusion test.

**Test scenarios:**
- Happy path: mixed corpus (tracked + untracked + nested) → native set == git set.
- Edge case: gitignored files (incl. `!` negation) → excluded identically.
- Edge case: secret-risk paths incl. a non-ASCII/tab name → absent from both.
- Edge case: `.forge-restore-*` at depth → excluded by the backstop in both.
- Integration / divergence (each its own annotated case): force-added-ignored, tracked-then-later-ignored, tracked-deleted-on-disk, submodule gitlink, case-fold — asserted with the index-vs-filesystem expectation.

**Verification:** the differential test passes; only **after** it is green does U2 delete the `git ls-files` arm. `cargo test --workspace` green.

---

### U4. `.forgeignore` precedence — native-only assertions + documentation

**Goal:** Pin the documented `.forgeignore` > `.gitignore` precedence and the policy-always-wins ordering with native-only tests (separate from the differential parity corpus, since git knows nothing of `.forgeignore`), and document the precedence where a future implementer will find it.

**Requirements:** R3, R4

**Dependencies:** U2

**Files:**
- Modify: `crates/forge-content-native/src/lib.rs` (module/trait-adjacent doc comment recording the precedence contract)
- Test: `crates/forge-content-native/src/lib.rs` (`#[cfg(test)] mod tests` — `forgeignore_*` cases)

**Approach:**
- Native-only assertions: `.forgeignore` excludes a path `.gitignore` does not; `.forgeignore` `!`-negation re-includes a `.gitignore`-excluded path; an `is_ignored_by_policy` path (e.g., `.env`) is **never** re-includable by a `.forgeignore` `!`-negation (backstop wins).
- Document the four-level precedence in the walker doc comment (policy > `.forgeignore` > `.gitignore` > defaults), citing `PRD.md:545` as the resolved open question.

**Test scenarios:**
- Happy path: `.forgeignore` with `*.tmp` excludes `scratch.tmp` not in `.gitignore`.
- Edge case: `.forgeignore` `!keep.log` re-includes a path `.gitignore` excluded (precedence proof).
- Error/security path: `.forgeignore` `!.env` does **not** re-include `.env` (backstop not negatable).

**Verification:** the precedence tests pass; the doc comment states the precedence unambiguously.

---

### U6. e2e native-backend snapshot block

**Goal:** Exercise the native walker through the real `forge` binary in `scripts/e2e-eval.sh` — a native-backend `init → start → save → run → propose → check → restore` block proving the walker works end-to-end. Git stays in PATH (native `changed_paths`/`base_head` still shell git until slice 2; the full `git-removed-from-PATH` block is slice 3).

**Requirements:** R8

**Dependencies:** U2

**Files:**
- Modify: `scripts/e2e-eval.sh` (new native-backend section after the git LIFECYCLE block, ~line 72)

**Approach:**
- `mkrepo` + `F init --content-backend native`, then walk `start <intent> → save → run -- true → propose → check → accept/restore`, asserting `save`'s `content_ref` is a `forge-tree:` ref and the lifecycle succeeds.
- Add a `.gitignore` + a planted `.env` and assert the **saved snapshot set** (the `forge-tree:` ref's file list) excludes them — snapshot-scope secret-hygiene smoke confirming U3's unit-level proof through the binary. (Export-path secret hygiene is NER-142's territory, not U6's.)
- Do **not** touch the `4` schema literals (lines 61, 122) — no migration.

**Test scenarios:** Test expectation: e2e smoke (observable-behavior). Happy path: native lifecycle succeeds; `save` yields `forge-tree:`; planted `.env` absent from the saved snapshot.

**Verification:** `bash scripts/e2e-eval.sh` passes including the new native block; `bash scripts/ci.sh` green.

---

## System-Wide Impact

- **Interaction graph:** `snapshot_worktree` (called by `save`, and by the dirty-worktree guard in `restore` / `attempt attach`) now enumerates via the native walker. The output path set must be identical to today's for these consumers (clean environment); the differential harness is the guarantee.
- **Error propagation:** walk errors travel as path-stripped `anyhow` errors through the existing CLI envelope; no new typed code. Neither the top context nor the source chain (`{:#}`) carries a path (S1).
- **State lifecycle risks:** none new — the walker writes nothing (no objects, no DB rows, no fsync, no lock).
- **API surface parity:** the `git` content backend is unaffected (keeps its own `git ls-files` walker). The native and git backends now have intentionally different ignore *mechanism* and slightly different ignore *semantics* (native ignores global/info-excludes); production cross-backend snapshot equality is not required (backend is chosen at init), and the differential harness pins native==git only in a clean environment.
- **Half-native checkpoint:** after slice 1 a native `save` is snapshot-native but base-anchoring/changed_paths-git (see Scope Boundaries) — documented so it is not mistaken for a regression and is not tested for full git-independence.
- **Integration coverage:** the differential harness (U3) plus the e2e native block (U6) cover what unit mocks cannot — that the real `ignore`-crate walk over a real filesystem reproduces git's set.
- **Unchanged invariants:** `current_base`/`base_content_ref`/`changed_paths` still shell git behind the trait (slice 2); the `// Phase 7 (NER-138)` markers stay; `forge-store`/`forge-content` stay git-free and `ignore`-free; `schema_head` stays 4; `FORGE_ERROR_CODES` stays 23; `is_ignored_by_policy`/`is_secret_risk_path` are consumed, not forked (the `filter_entry` prune reuses the predicate).

---

## Risk Analysis & Mitigation

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Ignore engine diverges and **leaks a secret** (`.env`/key) | Med | Critical | `is_ignored_by_policy` always-wins backstop after the engine; corpus includes secret-risk + special-byte names; walker sees real `PathBuf`s (cure for the C-quote class); backstop lowercases names (covers case-variant secrets). |
| Ignore engine diverges and **drops a tracked file** | Med | High | Differential harness asserts set-equality before the git call is removed; every index-only divergence class enumerated and asserted, not silently masked. |
| **Symlink-to-file dropped** by a walk-layer `file_type` filter | High (if missed) | High | Walker yields symlink entries; downstream `fs::metadata`/`is_file` gate is the capture authority; explicit U2 + U3 regression case. |
| **Walk recurses into `.git`** (≈2170 files/save) | High (if missed) | Med | `filter_entry` prunes `is_ignored_by_policy` directories at the walk layer (reuses the predicate); U2 perf/correctness case. |
| Path leaks into anyhow chain via `ignore::Error` Display (S1) | Med | High | Strip the `ignore::Error` payload, build a fresh path-free error from `io::ErrorKind`; S1 test asserts both `to_string()` and `{:#}`. |
| `hidden(true)`/`ignore(true)` defaults drop dotfiles / honor `.ignore` | High (if missed) | High | Explicit `hidden(false)`, `ignore(false)`; tests assert dotfiles yielded, `.ignore` not honored. |
| Environment-dependent set via global/info-excludes / parent `.gitignore` | Med | Med | `git_exclude(false)`, `git_global(false)`, `parents(false)` — repo-local-only, reproducible; divergence from git enumerated. |
| Case-fold `.gitignore` divergence on macOS | Med | Low | Enumerated divergence; deterministic case-sensitive matching; secrets covered by the case-insensitive backstop. |
| Mid-walk `NotFound` aborts `save` under agent fleet | Med | Med | `NotFound` → skip-and-continue (resolved walk-error policy); other errors fail-closed. |
| `clippy -D warnings` dead-code during U2/U3 coexistence | Med | Low | U2+U3 land in one PR; retained git helper keeps a `#[cfg(test)]` caller until the arm is deleted. |
| Scope creep into slice 2/3 (base anchoring, undo) | Med | Med | Scope Boundaries explicit; `// Phase 7` markers left in place; code-review gate with `plan:<path>`. |

---

## Documentation / Operational Notes

- Record the `.gitignore`/`.forgeignore`/policy precedence and the intentional environment-independence (no global/info-excludes) in the walker module doc — resolves `PRD.md:545` for the native backend. A user-facing changelog/docs pass for `.forgeignore` rides with slice 3 once it is a stable public contract.
- The security persona of `/ce-code-review` fires on snapshot-exclusion paths — expect scrutiny that the `ignore`-crate set still excludes everything `is_ignored_by_policy` does, especially the symlink, `.git`-prune, special-byte, and S1-chain cases.
- No migration, no schema-head bump, no error-code fan-out — slice 1 is behavior-preserving for the published contract.

---

## Sources & References

- **Origin document:** `docs/ROADMAP.md` (Phase 7 section); ticket **NER-138** (Linear, Forge project, milestone "M3 — Earn native-VCS independence").
- Related code: `crates/forge-content-native/src/lib.rs` (`snapshot_candidate_paths`, `scan_worktree`, `git()`); `crates/forge-content/src/lib.rs` (`is_ignored_by_policy`, `is_secret_risk_path`); `crates/forge-content-git/src/lib.rs` (reference walker); `crates/forge-export-git/src/lib.rs` (`filter_secret_paths_from_tree` — NER-142).
- Related tickets: **NER-138** (this phase), **NER-142** (deferred D1 export-path `-z` fix).
- Institutional learnings: Phase 3 boundary doc (§4, §5 S1/S2); Phase 6 compare/rank doc (§1, §5; NER-137 D1); the two durability docs (Phases 1a/1b); the secret-hygiene docs (Phases 4/5).
- Prior code-review triage: `docs/code-reviews/2026-05-29-ner-134-phase-3.md` (deferred S1/S2 follow-ups realized here).
- External docs: `ignore` crate 0.4.25 — https://docs.rs/ignore/0.4.25/ignore/struct.WalkBuilder.html
