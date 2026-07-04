# ADR-0001: Domain-Oriented Module Structure

Status: Accepted

Date: 2026-07-04

Deciders: Jan Skolte

Related issue: NER-366

## Context

Forge is architected well by layer: core types, store, content backends, evidence, policy, protocol, sync, export, and CLI live in separate crates.
Inside the two largest crates, the code has not stayed separated by domain.

Hotspots at the start of this refactor:

| File | Current size | Problem |
| --- | ---: | --- |
| `crates/forge-store/src/lib.rs` | 14,163 lines | Repository lifecycle, attempts, snapshots, proposals, evidence, trust, visibility, embargo, private overlays, sync, conflict handling, publication, storage, and inline tests share one file. |
| `crates/forge-cli/src/main.rs` | 4,839 lines | Argument parsing, response envelopes, dispatch, command handlers, sync/export handling, and worktree healing share one file. |

That shape is especially costly for Forge because Forge is agent-native.
Agents work with finite context windows, anchor-based edits, and branch/attempt parallelism.
A large mixed-domain file makes agents load irrelevant code, makes edit anchors less unique, makes file paths less useful during review, and concentrates parallel-attempt conflicts in the same files.

The immediate pressure point is NER-360.
Resumable sync and lazy hydration should land in a sync domain module, not deepen the current store and CLI monoliths.

NER-370 audit update: the storage and store-sync slices created `crates/forge-store/src/storage.rs` and `crates/forge-store/src/sync.rs`; the CLI sync/export slice created `crates/forge-cli/src/commands/sync.rs` and `crates/forge-cli/src/commands/export.rs`.
`crates/forge-cli/src/main.rs` is now at the 3,000-line ceiling, but it still contains shared replay, locking, worktree, and remaining command-family wiring.
`crates/forge-store/src/lib.rs` remains the primary monolith at 13,576 lines and is not yet a facade.

NER-372 audit update: the visibility policy/grant/projection surface moved into `crates/forge-store/src/visibility.rs`.
`crates/forge-store/src/lib.rs` is now capped at 12,917 lines and remains the primary store monolith until the remaining domains move.

## Decision

Forge crates should be organized by domain modules as well as by layer.
Layer separation remains valuable, but it is not sufficient once one crate contains several independent lifecycle concerns.

`lib.rs` and `main.rs` are facade files.
They may contain crate or binary documentation, module declarations, public re-exports, top-level dispatch, and narrowly shared wiring.
They should not accumulate new domain behavior.

The first refactor slice records this decision and agent guidance only.
It does not move Rust domain logic.
Future refactor slices move one domain at a time.

Public API paths must remain stable during moves.
If a public item moves from `lib.rs` into a domain module, `lib.rs` re-exports it so callers do not chase internal file layout changes.

Structural move slices are behavior-preserving.
They do not rename functions, change signatures, alter CLI output, change schema, or mix in cleanup.
Cleanups and behavior changes happen in separate proposals.

Empty module scaffolding is not the default.
Create module files when they receive real code or when a reviewed scaffolding slice has a concrete benefit.

## Store Module Map

This map is the target ownership model.
Extraction slices may adjust ownership when code inspection proves a different boundary is stronger.

| Module | Owns |
| --- | --- |
| `repository.rs` | init/open/migrate/root/backend/lock helpers, request-id operation lookup, central repository context helpers. |
| `attempts.rs` | start/list/show/attach/detach attempts, attempt workspace paths and markers, attempt materialization helpers. |
| `snapshots.rs` | save, restore, checkout, expected content refs, snapshot content refs, and snapshot restore helpers. |
| `proposals.rs` | propose/check/accept/reject metadata, proposal review records, decision lookup, and proposal readiness helpers. |
| `evidence.rs` | evidence recording, structured run capture summaries, and integrity verification entry points that delegate to `integrity.rs`. |
| `trust.rs` | trust policy, enforcement, local key status, hosted-runner and third-party attestation policy calls, and trust-rank helpers. |
| `org.rs` | organization governance status and initialization when extraction shows it is clearer than folding org into trust. |
| `visibility.rs` | visibility policy, grants/revocations, projection decisions, and work-package visibility state. |
| `embargo.rs` | embargo mark/grant/revoke/release/reveal/publish/close workflows and publishability guards. |
| `private_overlay.rs` | private path hashes, private labels/exclusions, encryption key binding, private payload transport, and materialized overlays. |
| `sync.rs` | native/projected clone setup, sync fetch/pull/push bookkeeping, sync merge markers, and `SYNC_MERGED_OP_KIND_SQL_IN`. |
| `storage.rs` | storage accounting, storage budget status, and pack/GC accounting helpers that remain in store. |
| `conflict.rs` | conflict set, failed operations with conflict, merge conflict recording, and preflight conflict resolution. |
| `publication.rs` | publication trailers, exportable proposal metadata, publication records, and branch publication checks. |
| `internal.rs` | shared row mappers, transaction helpers, canonical JSON helpers, and small utilities used by several domains. |

Existing modules `error.rs`, `integrity.rs`, `migrations.rs`, `repo_lock.rs`, and `signing.rs` stay module-owned.
Attestation policy belongs in `trust.rs` or `org.rs`, while Ed25519 mechanics remain in `signing.rs`.
`projection_decision` starts in `visibility.rs` unless extraction shows sync or proposals is the stronger owner.

## CLI Module Map

| Module | Owns |
| --- | --- |
| `args.rs` | clap derive structs, command parsing, request-id extraction, and parser error conversion. |
| `envelope.rs` | response envelope helpers and exit-code mapping not already owned by `schema.rs`. |
| `worktree.rs` | clean-worktree checks, expected-ref healing, sync import materialization guards, and native content-ref helpers. |
| `commands/attempt.rs` | start, attempt, save, restore, checkout, show, undo, and log where attempt lifecycle is dominant. |
| `commands/intent.rs` | intent list/detail command handling. |
| `commands/proposal.rs` | propose, check, accept, reject, proposal list, and review handlers. |
| `commands/diffmerge.rs` | compare, diff, merge, conflict, native diff options, and diff warnings. |
| `commands/run.rs` | run and evidence capture response handling. |
| `commands/trust.rs` | trust, key, org, hosted-runner, and third-party attestation handlers. |
| `commands/visibility.rs` | visibility and embargo handlers, split to `commands/embargo.rs` if the file grows. |
| `commands/sync.rs` | sync clone/fetch/pull/push/serve handlers. |
| `commands/export.rs` | export branch/pr/body handlers. |
| `main.rs` | main entrypoint, top-level dispatch, and minimal shared wiring only. |

## Sequencing

Refactor in small, reviewable slices:

1. Slice 0: land this ADR and agent-facing guidance.
2. Storage proving slice.
3. Sync-adjacent store extraction before NER-360 adds resumable sync or lazy hydration.
4. CLI sync/export extraction after store sync has a home.
5. Visibility, embargo, private overlay, trust/org, publication, conflict, evidence, proposals, snapshots, attempts, and repository extraction.
6. Repeat facade audits as large domains move, removing line-count allowlist entries as they fall under the ceiling.

Repository lifecycle moves last because it owns central open/init/migration/lock helpers.

Each later slice should have its own Forge intent/attempt and Linear tracking.
Each slice should move code in original order where practical, preserve public re-exports, move tests only when ownership is clear, and run the standard Forge verification gates.

## File-Size Rule

The soft ceiling for Rust source files is 3,000 lines, including inline tests.
Crossing it requires either an immediate split or a short top-of-file justification explaining why cohesion beats size.

CI now enforces this rule with an allowlist in `scripts/check-rust-line-count.sh`.
Non-allowlisted Rust files over 3,000 lines fail CI.
Allowlisted files may shrink, but they may not grow past their recorded cap.

Current allowlisted breaches are known exceptions while this refactor is underway:

- `crates/forge-store/src/lib.rs` at 12,917 lines.
- `crates/forge-content-native/src/lib.rs` at 4,721 lines. This predates ADR-0001 and should be split or justified in a later content-native follow-up; it is not part of the store/CLI facade slice.
- `crates/forge-cli/tests/forge_sync.rs` at 4,683 lines. This is integration coverage, not a facade, but it should split by sync scenario group in a later test-maintenance slice.

`crates/forge-cli/src/main.rs` is no longer allowlisted because it is exactly at the ceiling.
Any growth above 3,000 lines should move into an existing command module or a new domain module.

## Test Relocation

Domain-local unit tests should move with their domain module when ownership is clear.
Cross-domain lifecycle tests should live in integration tests or in a small facade-level test module.
Do not relocate tests only to make line counts look better.

## Consequences

Positive consequences:

- Agents load less irrelevant context for scoped edits.
- Reviewers regain file-path signal.
- Parallel attempts conflict less often in the same files.
- New domain behavior has an obvious home.
- NER-360 can grow sync behavior in a sync module instead of the store monolith.

Accepted costs:

- Some helpers may need `pub(crate)` visibility or `internal.rs` ownership.
- Git blame for moved code requires history-aware inspection.
- Refactor slices will spend engineering time without user-visible behavior changes.
- A thin `pub use` facade adds a small indirection layer.

## Alternatives Considered

Do nothing and rely on grep.
Rejected because the cost compounds as Forge becomes more agent-authored.

Split into many crates.
Rejected as the default because modules deliver most of the agent-workability benefit without Cargo churn, cross-crate visibility friction, or premature versioning boundaries.

Split by CLI command in the store.
Rejected for store logic because several commands share lifecycle domains.
Command-shaped modules remain appropriate inside `forge-cli`.

Enforce the 3,000-line rule in CI without an allowlist.
Rejected because it would fail the current baseline before the remaining store/content-native refactors can proceed.
The accepted compromise is hard enforcement for new or growing oversized files with explicit caps for known exceptions.
