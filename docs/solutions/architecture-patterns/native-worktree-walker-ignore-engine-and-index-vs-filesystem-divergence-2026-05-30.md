---
title: "Replace a git-binary worktree walk with a native ignore-engine: the index-vs-filesystem divergence class, reproducibility-over-parity, and S1 error stripping"
date: 2026-05-30
category: architecture-patterns
module: forge-content-native
problem_type: architecture_pattern
component: native-worktree-walker-and-ignore-engine
severity: high
applies_when:
  - Replacing a `git ls-files` / `git diff` shell-out with a native filesystem walk (an `ignore`-crate walker)
  - A differential harness must prove a new enumerator reproduces an old one before the old one is deleted
  - A "native"/"reproducible" backend is intentionally allowed to diverge from git's index-based view
  - A walk/IO error type embeds a filesystem path in its `Display` and must not leak into an untyped error envelope
  - Layering a new ignore engine on top of an existing always-wins secret/internal exclusion predicate
  - Splitting an XL reversal into slices and deciding where the clean cut is
tags: [native-walker, ignore-crate, gitignore, forgeignore, index-vs-filesystem-divergence, differential-harness, reproducibility-over-parity, environment-independence, s1-path-free-error, policy-backstop, symlink-file_type-trap, filter_entry-prune, slice-coupling, half-native-checkpoint, prove-before-delete, ner-138]
---

# Native worktree walker + ignore engine: index-vs-filesystem divergence, reproducibility-over-parity, and S1 error stripping

NER-138 Phase 7 slice 1 replaced the native content backend's `git ls-files` / `--others --exclude-standard` shell-out (the worktree enumerator behind `snapshot_worktree`) with a git-binary-free walker built on the audited `ignore` crate (0.4.25). These are the non-obvious learnings — the traps that a differential harness, a doc-review gate, and a 5-persona code-review gate surfaced.

## 1. A filesystem walk does NOT reproduce `git ls-files` — the gap is a *class*, not one exception

`git ls-files` is **index-based**: it lists what git *tracks*. An `ignore`-crate walk is **filesystem-based**: it lists what's on disk minus ignore rules. They agree on the common case but diverge in a whole family of cases the native backend has no way to see (it has no index concept):

- **force-added-ignored** — `git add -f` a `.gitignore`-matched file: git lists it, the walk drops it.
- **tracked-then-later-ignored** — commit a file, then add a `.gitignore` rule matching it: git's `ls-files` still lists it (tracked), the walk drops it.
- **tracked-but-deleted-from-disk** — git's raw `ls-files` lists it from the index; the walk can't see a nonexistent file. (This one *converges* once the downstream `fs::metadata`/`is_file` gate drops the missing path from the git reference too.)
- **submodule gitlinks** — git lists the gitlink path; a filesystem walk descends/skips differently.
- **case-folded `.gitignore`** on a case-insensitive filesystem (macOS) — git honors `core.ignorecase`; the crate matches case-sensitively by default.

**The trap:** writing a differential test that asserts `native == git` over a corpus that happens to include an index-only path makes the harness fail for a *correct* walker, which then pressures you to either narrow the corpus dishonestly or balloon scope mid-implementation. **The fix:** make the parity-equality corpus contain *only* paths whose membership is identical in both views, and assert each index-only divergence class as its own annotated test (`git_set.contains(p) && !native_set.contains(p)`). Where a real fixture is impractical (submodule) or platform-dependent (case-fold), scope it out with an **explicit comment naming the class and why** — never silently omit it. The plan committed to "assert-explicitly OR scope-out by comment"; honoring that is what keeps the safety net honest.

## 2. Reproducibility actively argues *against* git parity — and the divergence direction is the load-bearing test

Phase 7's stated goal is to remove "environment-dependent, non-deterministic failures hostile to reproducible runs." That goal **conflicts** with faithfully matching `git ls-files --others --exclude-standard`, because `--exclude-standard` honors `.git/info/exclude` (git-private) and the user's global `core.excludesfile` (machine-specific). Honoring those would reintroduce exactly the machine-dependence Phase 7 exists to kill.

So the correct `WalkBuilder` config makes the native walker **intentionally more inclusive than git**: `git_exclude(false)`, `git_global(false)`, `parents(false)` — depend only on repo-local `.gitignore` + `.forgeignore`. The native snapshot set is then reproducible across machines.

**The subtle test gap:** every parity and "native-drops-X" test would still pass if a toggle were mis-set to `true`, because a clean CI environment has no `.git/info/exclude` and no global excludes — native would just match git and stay green. The **only** assertion that proves the toggles are real is the *more-inclusive direction*: write `.git/info/exclude` with a rule, assert git's `--exclude-standard` drops the file while the native walk **keeps** it. Without that test, a crate upgrade or an edit could silently flip a toggle and re-introduce environment-dependence undetected.

## 3. `ignore::Error`'s `Display` embeds the path — S1 requires *stripping* the payload, not wrapping it

Security invariant S1 (from Phase 3): no filesystem path may reach the untyped `anyhow` envelope `message`, because that bypasses the typed-error secret-path redaction. The trap: `ignore::Error::WithPath`/`Loop` render the offending path in their own `Display`, so a naive `entry_err.context("failed to walk worktree")` chains the path-bearing `Display` into the anyhow source chain — invisible in `to_string()` (top context only) but exposed by `{:#}` and any chain-rendering surface.

**The fix:** map a walk error to a *fresh* path-free `anyhow` error built from only the `io::ErrorKind` (which is path-free), discarding the `ignore::Error` payload. Extract the mapping into a pure `map_walk_error(&ignore::Error) -> Option<anyhow::Error>` so it is unit-testable by hand-constructing the exact `WithPath{ err: Io(...) }` shape the crate produces — and **assert path-freeness against both `to_string()` and `{:#}`**, not just the top context. (`io_error()` recurses through `WithPath`/`WithDepth`/`WithLineNumber` to the inner `Io` and returns `None` for `Loop`/`Glob`/`Partial`, so the `None` arm must also be path-free.)

A related decision: a per-entry `NotFound` (a file that vanished between enumeration and read — realistic under a concurrent agent fleet) is benign → skip-and-continue, mirroring the existing `fs::metadata` `_ => continue`. Any *other* walk error fails closed (a snapshot tool must refuse to record a silently-incomplete set rather than guess an unknown error is benign).

## 4. The policy backstop runs AFTER the engine and REUSES the shared predicate — it never forks it

The single shared `forge_content::is_ignored_by_policy` (= `.forge` ∪ `.git` ∪ restore-temp ∪ `is_secret_risk_path`) stays the **authoritative, always-wins** secret/internal exclusion. The `ignore` engine (`.gitignore` + `.forgeignore`) is *additive* on top of it, applied first; the policy filter runs on the engine's output. Consequences:

- A `.forgeignore` `!`-negation can re-include a `.gitignore`-dropped path, but **never** an `is_ignored_by_policy` path (the backstop runs after and is not negatable). Documented precedence: `policy > .forgeignore > .gitignore > defaults` (`.forgeignore` gets higher precedence than `.gitignore` via `add_custom_ignore_filename`).
- Pruning `.git`/`.forge` descent at the walk layer via `filter_entry` is a **performance optimization that reuses** `is_ignored_by_policy` — it is *not* a fork (the post-walk filter remains authoritative). Without it the walk recurses through thousands of `.git` internals every save just to discard them. Re-encoding the secret rules as `ignore`-crate override globs *would* be a fork and would reopen the drift the shared predicate closed (NER-133).
- The walker yields real `PathBuf`s, never git-C-quoted strings, so `is_secret_risk_path` (which keys on the lowercased filename) sees the true name — structurally curing the `-z`/C-quote leak class on the snapshot path.

## 5. The `file_type().is_file()` walk-layer filter silently drops tracked symlinks-to-files

A symlink's own `file_type()` is `is_symlink`, never `is_file`, and `follow_links(false)` means the walker doesn't resolve it. Filtering on `is_file()` *at the walk layer* therefore drops a tracked symlink-to-file that today's `fs::metadata` (which follows the link) captures — a silent regression a parity corpus without a symlink case would miss. **The fix:** yield files *and* symlinks from the walk (skip only directories); let the existing downstream `fs::metadata`/`is_file` gate decide capture. This preserves prior behavior exactly (symlink-to-file captured by content; symlink-to-dir dropped at the gate; never traversed into).

## 6. Slice coupling: native `changed_paths` is bound to native base anchoring — cut the slice there

The instinct is "walker + changed-paths together." But `changed_paths` means "what differs from the base (git HEAD)", which requires the base commit's tree as a **native** tree — i.e., the backend-agnostic `base_head` / native-base-anchoring work. So the clean cut is: **slice 1 = walker only** (replace the snapshot enumerator); **slice 2 = native base anchoring + native `changed_paths` together** (they're one unit). Removing the snapshot-path git call while `changed_paths`/`current_base`/`base_content_ref` still shell git is a deliberate, documented **half-native checkpoint** — a native `save` is snapshot-native but base-anchoring-git, so "git removed from PATH" partially fails until slice 2. Surface that explicitly so the checkpoint isn't mistaken for a regression or tested for full independence prematurely. (Phase 3 §4's "don't emit `forge-tree:` base refs before the native walker exists" still holds in reverse here: don't touch the base methods in slice 1.)

## 7. Process learnings

- **Prove-before-delete with both paths alive.** Keep the old git enumeration as a `#[cfg(test)]` reference (`git_based_scan`) and assert `native == git` on the parity corpus *before* deleting the production git call. Land the walker (U2) and the harness (U3) in **one PR** so `clippy -D warnings` never flags the retained reference as dead code in an intermediate state. The harness must be a genuine *independent* differential (real `git` reference vs production `scan_worktree`), not native-vs-native.
- **A silently-failed `Edit` can drop a whole deliverable.** An `Edit` whose `old_string` doesn't match returns an error but does not halt the run; the planned e2e block (U6) went missing because its anchor guess was wrong, and the error scrolled by amid parallel tool results. Three code-review personas (correctness, testing, reliability) independently caught it. Lesson: when an `Edit` errors, treat it as blocking and re-verify the change landed — and the code-review gate earns its cost precisely on this class of "looks done, isn't" miss.
- **Surface the dependency-crate choice and let the differential corpus drive precision.** The `ignore` crate (BurntSushi, powers ripgrep) bundles `walkdir` + `globset` + correct nested-gitignore/negation — the "deceptively hard" part not to reimplement. Added to the adapter crate only (`forge-content-native`), never to `forge-store`/`forge-content`, keeping the git/walk work behind the boundary (PRD §23.4).

## See also

- [[write-binding-verification-and-content-backend-isolation-2026-05-29]] — Phase 3: the confined seam this slice reverses; S1/S2 invariants; "confine before you replace, and forbid the premature fix."
- [[compare-rank-on-verified-evidence-and-self-verifying-provenance-trailer-2026-05-30]] — Phase 6 §5: the `-z`/C-quote secret-path-quoting class the native walker structurally cures on the snapshot path (the export-path twin is filed as NER-142).
