# Code review — NER-142 (`-z`/C-quote secret leak in the git-export path)

- **base-sha:** `996c8a2` (merge-base with `main`; NER-138 Phase 7 closeout)
- **head-sha:** working tree on branch `ner-142-export-z-quote-secret-leak` (this commit)
- **scope:** `crates/forge-export-git/src/lib.rs` only (+ its tests) — the ticket's scope guard (touch only this file; do not bundle native-walker work).
- **gate:** `bash scripts/ci.sh` green (fmt `--check` · `cargo test --workspace` · clippy `-D warnings` · e2e 85/85) with the fix in place.
- **personas:** correctness, security, testing, maintainability, project-standards, adversarial. The orchestrated persona subagents did not return reliably this session; the review below was performed inline against the same persona checklists, with the security/adversarial concerns **verified empirically** in scratch git repos (not just reasoned about).

## Summary

NER-142 fixes a real secret-egress leak in `filter_secret_paths_from_tree`, the backstop that strips secret-risk-named entries from a git tree before `forge export branch` publishes it. The function read `git ls-tree -r --name-only` **without `-z`** and parsed with `.lines()`; git C-quotes any path with a tab/newline/non-ASCII byte (`.env.café` → `".env.caf\303\251"`), and the quoted form — with a leading `"` — fails `forge_content::is_secret_risk_path`, so the secret blob survived into the published branch. The fix adds `-z` (NUL-delimited, never C-quoted) and splits on `\0`.

**The adversarial pass found a second, sharper vector in the same function** (see real-actionable #2), confirmed empirically and fixed in the same PR: the write side of the drop (`git rm --cached`) used a plain pathspec, so a secret name containing a glob metacharacter was silently **reported as dropped but not removed**. Both the read side (`-z`) and the write side (`:(literal)`) are now robust to adversarial filenames.

## Real-actionable — FIXED in this PR

| # | Finding | Severity | Persona | Fix |
|---|---------|----------|---------|-----|
| 1 | **`ls-tree` without `-z` C-quotes special-byte paths → secret leak on export.** A secret-named blob with a tab/newline/non-ASCII byte is C-quoted by `ls-tree`; the quoted string fails `is_secret_risk_path` and is published. (The NER-137 D1 finding.) | P2 (security) | security, correctness | Added `-z` and `.split('\0').filter(!is_empty)`, mirroring the sibling `diff_trees` `-z` fix. Regression test `rewrite_drops_a_secret_path_with_non_ascii_bytes` (`.env.café`) — **verified it fails on the pre-fix code** (tree unchanged) **and passes after**. |
| 2 | **`rm --cached` with a plain pathspec silently fails to remove a glob-metacharacter secret name → leak persists, falsely reported as dropped.** `git rm --cached --ignore-unmatch '.env[prod]'` reads `[prod]` as a glob character class that matches nothing; `--ignore-unmatch` swallows the miss; the secret stays in the tree even though it appears in the `excluded` warnings. The `-z` fix in #1 makes this *worse* — the path now reaches `dropped` and is reported excluded while still being published. | **P1 (security)** | adversarial (empirically confirmed) | Prefixed the pathspec with `:(literal)` so the path matches byte-for-byte. **Verified in a scratch repo:** plain pathspec leaves `.env[prod]` in the tree (exit 0, no match); `:(literal)` removes it (`rm '.env[prod]'`). Regression test `rewrite_drops_a_secret_path_with_pathspec_glob_metacharacters` — **fails on the pre-fix code** (path reported dropped but physically present) **and passes after**. `:(literal)` also covers a leading-`:` filename, which a plain pathspec would misread as pathspec magic. |

Both fixes live entirely inside `filter_secret_paths_from_tree` (scope-compliant). `:(literal)` was verified to still remove the ordinary cases (`.env`, nested `certs/server.pem`) with no collateral — each secret path is enumerated individually by `ls-tree -r`, so exact-match per path is strictly more correct than glob-match (no misses, no over-removal).

## Audit of sibling path-parsing call sites (security)

Per the handoff, every `git ls-tree` / `git diff` / `git ls-files`-then-parse site in `forge-export-git` was audited for the same `.lines()`-without-`-z` class:

- **`diff_trees` (lib.rs:76-90)** — already uses `-z --no-renames` for both `--name-status` and `--numstat`, with a load-bearing comment. **Clean.**
- **`read_hunk` (lib.rs:157)** — `git diff <a> <b> -- <path>`: `path` is an **input argument** (already obtained from the `-z` parse upstream); the output is hunk text, not a path list. **Not a parse-out-paths site.** No `-z` needed.
- **`parse_forge_trailers` (lib.rs:206)** — `message.lines()` parses a **commit message** (line-oriented prose) for `Forge-*` trailers, not git plumbing path output. **Correct as-is.**
- **`synthesize_git_tree` / `remove_secret_risk_files` (lib.rs:433-487)** — the pre-`git add` defense walks the temp worktree with `read_dir` + `strip_prefix` (real OS filenames, never git-quoted) and feeds `is_secret_risk_path` the true relative path. **Not affected by C-quoting** (it never parses git output). This layer already correctly drops `.env.café` before staging; the `filter_secret_paths_from_tree` backstop is the second line of defense and is what this PR hardens.
- The remaining `ls-tree ... --name-only` calls (lib.rs:621/673/849/940) are all in `#[cfg(test)]`; the assertions that matter were updated to read back with `-z`.

Conclusion: **line 400 was the sole production parse-out-paths offender.** The fix brings the git-export egress to parity with the native walker (which never C-quotes because it passes real filenames).

## Defense-in-depth — noted, not blocking

- **NUL-split idiom duplication (maintainability, P3).** `.split('\0').filter(|p| !p.is_empty())` now appears in `filter_secret_paths_from_tree`, the two new tests, and (in a `splitn`-with-`\t` shape) in `diff_trees`. A `split_nul` helper would dedup ~2 production sites, but the three shapes differ (the `name_status` parse is alternating status/path, not a uniform field list) and extracting it now would touch `diff_trees`, which is outside this PR's tight scope guard. Left inline deliberately; revisit if a third uniform consumer appears.
- **End-to-end `export_branch` coverage for a non-ASCII / glob secret (testing, P3).** The two new tests drive `filter_secret_paths_from_tree` directly; `export_branch_reports_dropped_secret_in_excluded` covers the full export path for a plain `.env`. A full-path test for a special-character secret is marginal additional value (the unit covers the changed logic and `diff_trees` has its own non-ASCII test). Not added to keep the diff minimal.

## Reviewed-and-rejected — false positives / verified-safe

- **`-z` doesn't fully disable quoting (`core.quotePath`)** — verified empirically: `ls-tree -z` emits raw NUL-delimited bytes regardless of `core.quotePath`; the `-z` machine format is unconditional. **Rejected.**
- **`:(literal)` could miss a path the plain pathspec removed** — verified false: every secret path is independently enumerated by `ls-tree -r` and removed by its own exact-match pathspec; `:(literal)` removes a strict superset (it adds the glob-metachar and leading-`:` cases) with zero collateral. **Rejected.**
- **Empty-tree / trailing-NUL handling** — `"".split('\0')` yields `[""]`, filtered out by `!is_empty()`; the trailing NUL git emits is likewise dropped. The empty-tree fast path (`dropped.is_empty()` → return unchanged) is preserved. **Verified safe.**

## Verdict

**Ready.** Two real secret-egress findings (one P2 read-side, the ticket's stated bug; one P1 write-side, found by the adversarial pass) fixed and **proven** with fail-before/pass-after regression tests and empirical git probes. The sibling-call-site audit confirms no other production leak of this class. CI gate green (fmt · clippy `-D warnings` · tests 25/25 in-crate · e2e 85/85). Scope honored — `crates/forge-export-git/src/lib.rs` only.
