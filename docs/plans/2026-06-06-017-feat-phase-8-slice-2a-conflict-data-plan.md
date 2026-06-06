---
title: "feat: Phase 8 Slice 2a - Conflict-as-data substrate + multi-parent native walkers"
type: feat
status: active
date: 2026-06-06
origin: docs/brainstorms/2026-05-31-ner-139-phase-8-requirements.md
previous_plan: docs/plans/completed/2026-05-31-016-feat-phase-8-slice-1-native-diff-plan.md
---

# feat: Phase 8 Slice 2a - Conflict-as-data substrate + multi-parent native walkers

## Summary

Build the first half of Phase 8 Slice 2: make conflict metadata first-class and prepare native history for future merge commits, without implementing the 3-way merge engine yet.

S2a adds migration 008, expands `conflict_sets`, creates per-path `path_conflicts`, upgrades stale-base accept/export rows into structured conflict-as-data, exposes read-only conflict inspection through the JSON contract, and makes `reconcile_native_head` / `native_log` traverse all parents instead of only first parents. S2b will consume this substrate to run real merges and write hunk-level conflict rows.

---

## Problem Frame

S1 shipped native structured diff, which gives Forge the data shape that a merge engine can consume. The next risk is not the merge algorithm itself; it is the substrate the algorithm must write to and the native history semantics it will depend on.

Today `conflict_sets` is still the Phase 2 metadata stub: `id`, `repo_id`, `context`, `paths_json`, and `created_at_ms`. Stale-base accept/export writes one row, but the row cannot express base/ours/theirs content refs, status, resolver backend, or per-path typed conflicts. Meanwhile, native history objects already support multiple parents, but two important readers still follow only `parents.first()`: `reconcile_native_head` and `native_log`. A future merge commit would be valid at the object layer but could still brick routine commands or disappear from log traversal.

S2a resolves those substrate problems before S2b introduces the merge engine. The slice must be independently useful: stale-base divergence becomes readable conflict-as-data, and synthetic merge commits prove the DAG readers are merge-ready.

---

## Requirements

- R10. Migration 008 makes `conflict_sets` real with base/ours/theirs content refs, generated-by operation, resolver backend, and status, while preserving existing stale-base rows.
- R11. Per-path conflict rows can represent `content`, `binary`, `delete_modify`, `rename`, `dir_file`, `mode`, and `symlink`.
- R12. Existing stale-base accept/export divergence writes real `conflict_sets` rows and `path_conflicts` rows for known, non-filtered changed paths even before the merge engine exists. Zero path rows is valid when `changed_paths` is empty or every path is filtered.
- R13. `reconcile_native_head` and `native_log` become multi-parent aware. `verify_native_history` and `gc` reachability are already multi-parent aware and should not be rewritten.
- R14. Conflict read surfaces use the existing tamper-evident operation chain and redact at the read boundary before JSON egress: refs/ids/status/kinds/counts are allowed, but raw paths and inline blob excerpts are not emitted.
- Cross-cutting: preserve S1 path-free errors, S2 policy exclusion, JSON envelope stability, `forge-store` dependency boundaries, schema-head fan-out, and typed-error drift guards.

Origin actors and flows:
- A1 coding agent consumes conflict objects.
- A2 human/reviewing agent inspects or resolves conflicts later.
- F1 conflict-resolution loop starts here with persisted readable conflict objects; S2b adds merge/resolution execution.
- AE7 is owned by this slice for multi-parent traversal.
- AE8 is partially owned here for read-boundary redaction; S2b expands it with real hunk payloads.

---

## Scope Boundaries

In scope:
- Migration 008 schema additions.
- Store APIs and CLI read-only surfaces for conflict list/show.
- Upgrading stale-base accept/export conflict persistence to the new schema.
- Multi-parent traversal for `reconcile_native_head` and `native_log`.
- Tests for migration, stale-base conflict rows, JSON read surfaces, redaction, and synthetic merge commits.

Out of scope:
- No 3-way merge algorithm.
- No conflict resolution submission or resolved-tree rebinding.
- No auto-resolution suggestions.
- No real GC deletion, worktrees, packing, indexing, or retention.
- No change to the native commit object format. Existing `parents: Vec<String>` remains the representation.

---

## Key Technical Decisions

- **Additive migration, not table replacement.** Existing repos and Phase 2 stale-base rows must continue to open. Migration 008 uses nullable columns and a new child table instead of replacing `conflict_sets`.
- **Use content refs in conflict rows.** Store `git-tree:` / `forge-tree:` content refs for base/ours/theirs rather than raw backend anchors. The merge engine consumes trees, and content refs preserve backend dispatch without teaching the store about git object layout.
- **Resolve backend anchors in the CLI, not `forge-store`.** `forge-store` must remain free of git adapter crates. The CLI already has `selected_backend` and can pass resolved content refs to the store when recording stale-base divergence.
- **Typed path conflicts are rows, summary JSON remains compatibility baggage.** `paths_json` stays for old consumers and stale-base context, but `path_conflicts` is the new structured source for S2b and agents.
- **Stale-base rows use coarse per-path kind until S2b.** For safe paths from `proposal.changed_paths`, record `kind = "content"` unless a path-level kind is not knowable. If the path list is empty after filtering, keep only the conflict-set metadata and emit no synthetic repository-level path row. S2b will replace this with real diff/merge classification. This keeps S2a useful without pretending to have a merge engine.
- **Generated operation ids are nullable for stale-base rows.** `accept_response` and `export branch` currently persist stale-base conflict data before `command_result::record_failed_operation` records the failed command operation. S2a should not refactor that operation lifecycle just to backfill the id; stale-base rows may have `generated_by_operation_id = NULL`. Future operation-owned merge writes can populate the foreign key when an operation id exists before conflict persistence.
- **Read surfaces are read-only and small.** Add `forge conflict list` and `forge conflict show <id>` so agents can consume the new rows through the JSON envelope. Do not add resolve/accept commands in S2a.
- **Internal storage and JSON egress have different secrecy rules.** SQLite may store filtered raw paths for future merge/resolution machinery, but `conflict list/show --json` must not emit raw path strings. JSON path rows expose opaque path ids or ordinals, kind/status/ref metadata, and redaction counts/warnings instead.
- **Multi-parent traversal uses stack/seen, not recursion.** Match `verify_native_history` and `NativeObjectStore::reachable_from` posture: iterative stack, deduped `seen`, typed corruption on missing commits/cycles.

---

## Context and Patterns

- `crates/forge-store/migrations/001_init.sql` defines the current `conflict_sets` stub.
- `crates/forge-store/migrations.rs` embeds migrations and pins `schema_head() == 7`; S2a bumps to 8.
- `crates/forge-store/tests/migrate.rs` builds at-head fixtures from every migration file; update this fan-out.
- `crates/forge-cli/tests/forge_migration_upgrade.rs` has the forward-version HEAD+1 fixture; update 8 -> 9.
- `crates/forge-store/src/lib.rs::record_conflict_set` writes current stale-base rows.
- `crates/forge-cli/src/main.rs::accept_response` and `export branch` call `record_conflict_set` before returning `STALE_BASE`.
- `crates/forge-store/src/lib.rs::reconcile_native_head` and `native_log` currently follow `commit.parents.first()`.
- `crates/forge-store/src/lib.rs::verify_native_history` already traverses all parents and should be mirrored, not redesigned.
- `crates/forge-cli/tests/forge_conflict_set.rs` is the current stale-base conflict test surface.
- `crates/forge-cli/tests/forge_native_history.rs` has native DAG, log, and corruption test patterns.
- `docs/solutions/architecture-patterns/commit-on-accept-ordering-ledger-authoritative-reconcile-and-the-last-hidden-git-dependency-2026-05-31.md` explains ledger-authoritative HEAD and multi-parent implications for future merge commits.
- `docs/solutions/architecture-patterns/native-commit-objects-base-anchoring-and-the-new-objectkind-gc-reachability-coupling-2026-05-30.md` carries the content-addressed `(blob, mode)` and gc-reachability coupling lessons.
- `docs/solutions/architecture-patterns/content-bound-gate-engine-and-failclosed-enforcement-2026-05-29.md` and `tamper-evident-evidence-chain-and-failclosed-verification-2026-05-30.md` carry every-egress redaction and emit-vs-persist discipline.

---

## High-Level Technical Design

Migration 008:

```text
ALTER TABLE conflict_sets ADD COLUMN base_content_ref TEXT;
ALTER TABLE conflict_sets ADD COLUMN ours_content_ref TEXT;
ALTER TABLE conflict_sets ADD COLUMN theirs_content_ref TEXT;
ALTER TABLE conflict_sets ADD COLUMN generated_by_operation_id TEXT REFERENCES operations(id);
ALTER TABLE conflict_sets ADD COLUMN resolver_backend TEXT;
ALTER TABLE conflict_sets ADD COLUMN status TEXT NOT NULL DEFAULT 'unresolved';

CREATE TABLE path_conflicts (
  id TEXT PRIMARY KEY,
  conflict_set_id TEXT NOT NULL REFERENCES conflict_sets(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  kind TEXT NOT NULL,
  base_ref TEXT,
  ours_ref TEXT,
  theirs_ref TEXT,
  resolution_ref TEXT,
  status TEXT NOT NULL DEFAULT 'unresolved',
  created_at_ms INTEGER NOT NULL
);
```

Use `CHECK` constraints for `status` and `kind` if they can be expressed without making future migrations brittle. If implementation finds SQLite ALTER/CHECK constraints too awkward for compatibility, enforce with Rust enums and tests instead.

Stale-base mapping:

- Accept stale base:
  - base = proposal base content ref
  - ours = current base content ref
  - theirs = proposal revision content ref
  - resolver backend = `stale_base`
  - status = `unresolved`
  - path rows = proposal changed paths after secret-risk filtering, with coarse `content` kind
- Export stale base:
  - base = accepted expected content ref
  - ours = current base content ref
  - theirs = proposal revision content ref
  - same resolver/status/path behavior

Read surface:

```text
forge conflict list
forge conflict show <conflict-set-id>
```

Both commands return structured JSON only through the existing envelope. `show` includes the conflict set row and redacted path-conflict summaries, not raw `path` values or inline blob content. Unknown ids return the mandatory typed error `CONFLICT_SET_NOT_FOUND`.

---

## Implementation Units

### U1. Migration 008 conflict schema

**Goal:** Add additive schema support for real conflict sets and path conflicts.

**Requirements:** R10, R11, cross-cutting schema fan-out.

**Files:**
- Create: `crates/forge-store/migrations/008_conflict_data.sql`
- Modify: `crates/forge-store/src/migrations.rs`
- Modify: `crates/forge-store/tests/migrate.rs`
- Modify: `crates/forge-cli/tests/forge_migration_upgrade.rs`
- Test: `crates/forge-store/src/migrations.rs`

**Approach:** Embed migration 008, bump schema head from 7 to 8, update at-head fixtures and forward-version tests. Keep migration SQL semicolon-safe for the naive splitter. Add tests that fresh and upgraded DBs have new `conflict_sets` columns and `path_conflicts`.

**Execution note:** Test-first on migration fan-out. The first failing test should prove `schema_head()` and at-head fixture expectations still say 7.

**Test scenarios:**
- Fresh migration reaches head 8 and includes `path_conflicts`.
- A DB stamped at 7 upgrades to 8.
- A DB stamped at 9 is refused as `SCHEMA_VERSION_UNSUPPORTED`.
- Existing conflict rows created before 008 survive and have default/null new fields.

**Verification:** migration tests pass and no schema-head literal remains stale.

### U2. Store conflict models and upgraded stale-base writer

**Goal:** Replace the loose stale-base metadata insert with a structured conflict writer that records content refs and path rows.

**Requirements:** R10, R11, R12, R14.

**Files:**
- Modify: `crates/forge-store/src/lib.rs`
- Modify: `crates/forge-cli/src/main.rs`
- Modify: `crates/forge-cli/tests/forge_conflict_set.rs`
- Test: `crates/forge-store/src/lib.rs`

**Approach:** Introduce small Rust structs/enums for conflict set status and path conflict kind. Replace or wrap `record_conflict_set` with an API that accepts base/ours/theirs content refs, generated operation if available, resolver backend, and path conflicts. Keep secret-risk path filtering before persistence. The CLI resolves backend anchors to content refs before calling store code.

**Execution note:** Characterization-first: keep current stale-base tests green, then strengthen them to assert the new columns and `path_conflicts` rows.

**Test scenarios:**
- Stale accept writes one conflict set with base/ours/theirs content refs and at least one path conflict row.
- Stale export writes the same shape.
- Secret-risk changed paths are omitted from `path_conflicts` and counted in compatibility JSON.
- Happy accept/export writes no conflict set.
- Existing `STALE_BASE` JSON envelope remains stable.

**Verification:** `forge_conflict_set` covers both git and native backend stale-base rows where practical.

### U3. Read-only conflict JSON surface

**Goal:** Let agents inspect conflict-as-data without opening SQLite directly.

**Requirements:** R4, R10, R11, R14.

**Files:**
- Modify: `crates/forge-cli/src/main.rs`
- Modify: `crates/forge-cli/src/schema.rs`
- Modify: `crates/forge-store/src/lib.rs`
- Modify: `crates/forge-store/src/error.rs` if adding `CONFLICT_SET_NOT_FOUND`
- Modify: `crates/forge-cli/tests/forge_schema.rs`
- Test: `crates/forge-cli/tests/forge_conflict_set.rs`

**Approach:** Add read-only `conflict list` and `conflict show <id>` commands. Return conflict set metadata plus redacted path-row summaries. Raw `path` values stay internal to SQLite; JSON emits opaque row ids or stable ordinals, `kind`, `status`, content refs, and `redacted_count`/`warnings[]` as needed. Add mandatory `CONFLICT_SET_NOT_FOUND` handling for missing ids, including `error.rs` and `forge_schema.rs` fan-out. S2a should already route any future inline-content-capable payload through the same redaction helper when read for JSON, even though S2a itself emits refs rather than blob excerpts.

**Execution note:** Add schema-contract tests with the command descriptions before implementing command handling.

**Test scenarios:**
- `forge conflict list --json` returns stale-base conflict ids after a stale accept/export.
- `forge conflict show <id> --json` returns base/ours/theirs refs and redacted path-conflict summaries without raw path strings.
- Missing id returns `CONFLICT_SET_NOT_FOUND`; `to_string()` and alternate debug/error output stay path-free.
- A stale-base conflict whose underlying changed file contains a secret-like value emits no inline blob content or secret value in `conflict show --json`.
- JSON envelope `schema_version` and existing fields remain unchanged.

**Verification:** agents can consume conflict rows through `forge schema`-documented commands.

### U4. Multi-parent `reconcile_native_head`

**Goal:** Make native HEAD reconciliation accept merge tips whose current HEAD is reachable through any parent.

**Requirements:** R13, AE7.

**Files:**
- Modify: `crates/forge-store/src/lib.rs`
- Test: `crates/forge-cli/tests/forge_native_history.rs` or `crates/forge-store/src/lib.rs`

**Approach:** Replace the first-parent cursor with stack-based traversal from the ledger tip. `current_head` is valid if it appears anywhere in the ancestry. Preserve typed corruption behavior: cycle -> `NativeHistoryCorruptKind::Cycle`; missing tip -> `DanglingCommitId`; missing deeper parent -> `DanglingParent`.

**Execution note:** Test-first with synthetic native commit objects and ref-store HEAD set to a second-parent ancestor.

**Test scenarios:**
- Ledger tip has parents `[other, current_head]`; reconcile succeeds and advances HEAD.
- Missing parent still raises `NATIVE_HISTORY_CORRUPT`.
- Cycle is still detected.

**Verification:** an unrelated mutating command after synthetic merge history does not brick the repo.

### U5. Multi-parent `native_log`

**Goal:** Make `forge log` traverse all parents of native merge commits.

**Requirements:** R13, AE7.

**Files:**
- Modify: `crates/forge-store/src/lib.rs`
- Test: `crates/forge-cli/tests/forge_native_history.rs`

**Approach:** Replace the first-parent cursor with stack traversal and a `seen` set. Keep tip-to-genesis-ish deterministic ordering by pushing parents in reverse so output is stable. Preserve intent filtering over every visited commit.

**Execution note:** Test-first with a synthetic merge DAG where the second parent carries a distinct intent/proposal marker.

**Test scenarios:**
- `forge log` returns commits from both parents of a merge tip.
- `forge log --intent <id>` finds a commit reachable only through the second parent.
- A cycle still surfaces `NATIVE_HISTORY_CORRUPT`.

**Verification:** `native_log` no longer has any `parents.first()` traversal.

### U6. Verification, review, and dogfood

**Goal:** Ship S2a behind the full repo gate.

**Requirements:** R5 and all cross-cutting checks.

**Files:**
- Modify or create test artifacts as needed.

**Approach:** Run the normal verification trio plus `scripts/ci.sh` if time permits, dogfood a native stale-base conflict flow, and run `ce-code-review` with plan context. Security review should focus on conflict JSON egress; reliability/adversarial review should focus on migration 008 and multi-parent reconcile.

**Test scenarios:**
- Existing native loop remains green.
- Native stale-base conflict can be listed/shown.
- Full test suite remains green.

**Verification:** `cargo fmt --all --check`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, native dogfood, and code-review gate.

---

## Risks and Mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Migration 008 breaks older stale-base rows | Existing repos fail to open | Additive nullable columns, default status, migration tests with pre-008 rows |
| Store learns backend-specific git details | Dependency boundary regression | Resolve content refs in CLI and pass them into store APIs |
| Path conflict kinds overclaim before merge engine exists | Agents trust coarse stale-base rows as true merge classification | Mark stale-base rows with resolver backend `stale_base` and only use coarse path rows until S2b |
| Multi-parent reconcile masks corruption | HEAD could advance across a fork | Require current HEAD to be reachable from ledger tip through any parent; still error if not reached |
| Conflict JSON leaks paths or inline secrets | Machine-visible egress risk | Store filtered paths internally, but emit only redacted path summaries and refs; add tests proving paths, blob excerpts, and secret-like values are absent |

---

## Verification Plan

- `cargo fmt --all --check`
- `cargo test -p forge-store`
- `cargo test -p forge-cli --test forge_conflict_set`
- `cargo test -p forge-cli --test forge_native_history`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- Native dogfood focused on stale-base conflict list/show
- `ce-code-review plan:docs/plans/2026-06-06-017-feat-phase-8-slice-2a-conflict-data-plan.md`

---

## Open Questions

- Whether `path_conflicts.kind` and `status` should be SQLite `CHECK` constraints or Rust-enforced enums only. Default: use CHECK for the known S2a vocabulary unless migration compatibility argues against it.
- Whether stale-base path rows should include one synthetic repository-level row when `changed_paths` is empty. Default: no; preserve the head-pair metadata in the conflict set and only emit path rows for known changed paths.

---

## Closeout

After merge, flip this plan to `status: completed`, move it under `docs/plans/completed/`, and write or update a solution doc for the conflict-as-data schema and multi-parent reconcile/log lessons if review or implementation surfaces non-obvious decisions.
