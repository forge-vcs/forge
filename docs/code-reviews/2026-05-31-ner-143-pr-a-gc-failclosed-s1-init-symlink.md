# Code review — NER-143 PR-A (gc fail-closed + S1 path-free errors + native-init guard + escaping-symlink e2e + docs/e2e parity)

- **base-sha:** `996c8a2` (merge-base with `main`)
- **head-sha:** `9fb45c4` (the impl/test commits + this review-fix commit) on branch `ner-143a-gc-failclosed-s1-init-symlink`
- **plan:** `docs/plans/completed/2026-05-31-015-fix-ner-143-undo-op-restore-hardening-plan.md` (the independent-hardening half; R1–R4 + migration are PR-B)
- **gate:** `bash scripts/ci.sh` green (fmt `--check` · `cargo test --workspace` · clippy `-D warnings` · e2e **88/88**) before and after the fixes.
- **personas (6):** correctness, security, reliability, testing, maintainability, adversarial (the orchestrated subagents ran inline against the committed diff; conclusions cross-checked against the Explore code map).

## Summary

PR-A ships the five independent, low-blast-radius items from the NER-138 slice-3 deferred cluster, split out (per the doc-review gate) from the data-loss-adjacent dirty-check + migration change (PR-B): **R6** (gc fails closed on a corrupt ledger root-determinacy input), **R8** (S1 path-free errors in the native materialize/restore/sync paths), **R9** (native-init nested-`.forge` guard), **R10** (escaping-symlink capture/reject e2e), **R11** (STALE_BASE doc note + git-removed e2e parity for restore/checkout/undo). No schema/migration change. `FORGE_ERROR_CODES` stays 24.

**correctness, security, and maintainability returned clean** (0 findings ≥70); security confirmed the S1 hardening is complete (all 8 `path.display()` sites closed, the tempfile-persist case correctly reaches `error.error.kind()` without the path). reliability, testing, and adversarial surfaced real-but-not-blocking findings, fixed below.

## Real-actionable — FIXED in this PR (commit `9fb45c4`)

| # | Finding | Severity | Persona(s) | Fix |
|---|---------|----------|------------|-----|
| 1 | The R6 gc fail-closed test corrupted **all** view rows, so it couldn't prove the failure came from a root-**determining** row; and the `ObjectId::parse(root)` fail-closed branch was **untested**. | P2 (testing 85, adversarial 80) | testing, adversarial | Added a shared `native_repo_with_a_checkout` helper that builds a real `commit_checked_out` view row (a root reachable only through the op-log). `gc_fails_closed_on_a_corrupt_ledger_view_row` now corrupts **only** that row (`WHERE state_json LIKE '%commit_checked_out%'`, asserting exactly 1 row) — surviving even a future scan narrowed to commit-bearing views. New `gc_fails_closed_on_an_unparseable_reachability_root` plants a valid-JSON row with a garbage `commit_id` to exercise the second (parse) guard distinctly. Both proven to pass. |
| 2 | The gc fail-closed messages said "run forge doctor", but `doctor` does **not** parse every `views.state_json`, so it would report `ok: true` on the very repo gc just declared corrupt — a dead-end diagnostic. | P2 (reliability 80) | reliability | Reworded both messages to "the ledger is damaged" (still path-free, still `COMMAND_FAILED`); dropped the hollow doctor pointer. |
| 3 | The R9 nested-`.forge` guard was gated on `content_backend == "native"`, but `forge_root`'s nearest-ancestor routing is **backend-agnostic** — a git inner repo whose own toplevel sits below an outer repo can nest too (and gc would see its objects as unreachable). | P2 (adversarial 80) | adversarial | Made the ancestor check **unconditional**. Verified the 12 `forge_init` tests (incl. git-backend init + idempotent re-init) still pass — they init in tempdirs with no ancestor `.forge`, and the guard checks ancestors only so re-init of the same root never trips it. |
| 4 | The R6 root-id parse-guard silently widened to cover `decisions.commit_id` roots (not only view-derived), undocumented. | P3 (reliability 85) | reliability | Documented in the `for root in &roots` comment that the guard covers **every** root source. |
| 5 | STALE_BASE.expected_head semantic shift (native = accepted `commit_id`) had no schema-doc note. | P3 (api-contract) | (R11) | Added the note to the `StaleBase` variant doc in `error.rs` (no CHANGELOG file exists in the repo). |

## Defer-able — filed / tracked, not PR-A scope

- **A doctor pass that parses every `views.state_json` + the Phase-8 doctor-clean deletion precondition** (reliability 80). gc's reachability **walk** (`verify_content_ref`/`reachable_from`) stays best-effort by design — a determined-but-dangling *object* is doctor's domain, pinned by the existing `doctor_reports_corrupt_native_content_and_gc_reports_unreachable_objects` contract. Before Phase 8 (NER-139) wires real mark-sweep deletion to this report, deletion must additionally require a doctor-clean repo (so a transiently-unreadable object isn't misclassified as garbage), and doctor should detect the corrupt-`state_json` shape this PR fail-closes on. **Belongs with NER-139** — added to the Phase-8 scope.
- **Cross-repo nested-init TOCTOU** (adversarial 75). The R9 guard runs before the per-repo init lock; two inits racing in ancestor/descendant dirs could both pass. This is an inherent two-store / cross-repo race (the locks are per-`.forge`, so they can't serialize an ancestor that doesn't exist yet) and is a narrow edge for a solo-dev v0 tool. Documented as an accepted v0 limitation in the guard comment.
- **`--allow-nested` opt-out** for a deliberately-independent nested repo (adversarial). Not a v0 use case; the unconditional guard refuses by default. Future work.
- **R10 relative-escape e2e** (adversarial 85). The e2e exercises the absolute-target branch; the **relative `../../` escape branch is already unit-covered** by `materializing_an_escaping_symlink_is_rejected` (which tests absolute + relative-escape + safe-relative + sibling at the `validate_symlink_target` level). An e2e relative-escape case is marginal additional value.
- **A materialize-path IO-failure unit test** (testing 80). The R8 path-free assertion directly covers `sync_dir`; the four `materialize_tree`/restore conversions use the identical `error.kind()` pattern with no path token, so the residual regression risk is a copy-paste slip rather than a present leak. Left to the identical-pattern argument.

## Reviewed-and-rejected — verified-safe / not reachable

- **R6 over-reach bricks a normal repo** (adversarial, verified false). Every legitimate `views.state_json` writer emits a JSON **object** via `json!({...})`; `serde_json::from_str::<Value>` succeeds on any well-formed JSON and `.get("commit_id")` returns `None` harmlessly for rows without a commit. The column is `NOT NULL`. Only genuinely byte-malformed JSON trips the guard — no op produces that. **Rejected.**
- **R8 error-swallowing / tempfile leak** (adversarial, verified false). Every rewritten site maps the io error to a path-free `anyhow!` and still propagates via `?`; the consumed `NamedTempFile` inside `PersistError` drops on the error path, and a crash-orphaned `.forge-restore-` temp is reclaimed by doctor's sweep. **Rejected.**
- **S1 test needle insufficiency** (security, verified-safe-by-nesting). The envelope tests check the repo-root path as the needle; because every leaf path under the repo is a superstring of the root, the canonical absolute-path leak shape **is** caught. The production code never emits a bare relative sub-path. **Rejected** as a live gap.
- **R9 false-positive on re-init** (verified false). The guard walks `root.parent()` upward only, never `root` itself, so re-init of the same root passes the guard and reaches `already_initialized`. **Rejected.**

## Verdict

**Ready.** Five real-actionable findings fixed on-branch (two test-integrity, one diagnostic-honesty, one guard-completeness, one doc); the deferred items are correctly Phase-8-scoped or below the v0 bar. CI gate green (fmt · clippy `-D warnings` · workspace tests · e2e 88/88). Scope honored — no schema/migration change; PR-B (the crash-safe dirty-check + migration 007) follows separately.
