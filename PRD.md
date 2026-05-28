# Forge PRD: Agent-Native Version Control and Collaboration Platform

## 1. Executive Summary

Forge is a Rust-native change-control system designed for the era where AI agents and humans both produce code. It is not a Git clone with friendlier commands. Its first product is an agent-native review ledger over existing Git repositories; its long-term architecture now includes an explicit native Forge content backend.

Forge's reviewable lifecycle is:

Intent -> Attempt -> Snapshot -> Proposal -> Evidence -> Check Result -> Decision -> Publication

The product starts as a local CLI, metadata store, evidence ledger, and Git-compatible content boundary. Git-backed snapshots remain the default adapter, and native Forge loose-object snapshots are available behind an explicit repository setting. It must later be strong enough to support a hosted collaboration platform: the next GitHub built around intents, agent attempts, evidence, policy, review, and safe publication.

Forge v0 is not an independent VCS. It validates the agent-native workflow and begins proving native snapshot storage without replacing Git as the collaboration/export protocol.

The current IDEA.md has the right strategic direction, but it under-specifies the dangerous parts:

- What invariants make the repository safe under crash, concurrency, and agent-heavy use.
- How conflicts, rebases, accept races, and publication races work.
- What is content-addressed, what is mutable metadata, and how migrations happen.
- How evidence avoids becoming a secret-leaking liability.
- How "no dependencies" interacts with hashing, compression, serialization, and Git interoperability.
- What makes Forge meaningfully better than Git plus scripts, Jujutsu plus CI, or Sapling-style stacked review.

This PRD makes those constraints explicit so a deeper design review can challenge them.

## 2. Product Thesis

Git was optimized for human-authored commits, branch-based collaboration, and diff-centric review. AI agents change the shape of work. They create many intermediate states, run exploratory commands, inspect many files, and often need rollback, comparison, provenance, and check results before a human trusts the output.

Forge should optimize for this reality:

- Agents need stable machine-readable commands, durable IDs, no required prompts, and safe rollback.
- Humans need to understand why a change exists, what was tried, what passed, what failed, and what risk remains.
- Teams need clean accepted history without preserving every exploratory state as durable project history.
- Future hosted Forge needs first-class objects for evidence, policy, review, identity, and permissions, not a Git commit graph with side tables.

The key bet: the durable reviewable unit should be a proposal with evidence, check state, and decision state, not a raw branch or commit.

## 3. Sharp Product Boundary

Forge v0 must prove one wedge:

A developer can let one or more agents make isolated attempts, review their resulting proposals with captured evidence, and accept or reject the best proposal without manually managing Git branches, worktrees, snapshot-style commits, and PR-body archaeology.

If v0 does not make that workflow obviously better than Git plus shell scripts, the product has failed.

## 4. Critical Review of IDEA.md

### 4.1 Strong Ideas Worth Keeping

- Intent/attempt/snapshot/proposal/evidence/check/decision/publication is the right workflow vocabulary.
- Git should be an adapter, not the core data model.
- CLI-first is correct because agents and power users can adopt it without waiting for UI integration.
- Local-first is essential for trust, offline use, and adoption.
- Evidence-first review is the main differentiator from Git, Jujutsu, and normal PR workflows.
- Decision and publication as clean durable boundaries are the right answer to noisy agent exploration.

### 4.2 Missing Load-Bearing Decisions

The current idea document names major components but leaves too many foundation choices unresolved:

- No repository invariant model: what must always be true after every command.
- No transaction protocol: how writes survive crash, partial fsync, interrupted commands, and concurrent agents.
- No conflict model: how Forge represents conflicts without relying only on file conflict markers.
- No storage format decision: text manifests, binary records, SQLite, object files, packs, indexes, or hybrid.
- No schema migration plan: every persisted object needs versioning and forward/backward compatibility rules.
- No JSON/structured CLI contract: agent-safe output must be versioned and stable.
- No evidence sensitivity model: command output can contain secrets and PII.
- No scale model: status scans, snapshot frequency, large files, ignored files, generated directories, and monorepos can destroy performance.
- No hosted-platform model: local design decisions may block permissions, review state, remote synchronization, and server-side checks later.

### 4.3 Strategic Tensions

The document currently asks for "minimal and simple" and "maximum feature parity" at the same time. These conflict unless parity is defined narrowly.

Forge should not target Git command parity in v0. It should target workflow parity where it matters:

- Preserve user work.
- Compare changes.
- Restore earlier states.
- Exchange accepted work through Git.
- Support branch/PR workflows through export.
- Maintain enough internal model purity to grow beyond Git later.

The product should be brutally scoped: do less than Git at first, but do the agent workflow far better.

### 4.4 The "No Dependencies" Trap

Rust-native does not mean "std-only at all costs." A dependency ban can reduce supply-chain risk, but it can also force Forge to hand-roll dangerous infrastructure.

Rust's standard library does not provide a stable content-addressing hash suitable for repository object IDs. `std::collections::DefaultHasher` is not a persistent object ID format and its exact algorithm is not the right foundation for repository integrity. The standard library gives useful filesystem primitives such as `File::sync_all`, `File::create_new`, file locks, and `rename`, but not a complete database, compression engine, JSON parser, Git protocol implementation, or cryptographic hash suite.

Forge should use a dependency policy, not a dependency ban:

- Core domain model: no unnecessary dependencies.
- Storage safety: prefer simple code, explicit formats, and exhaustive tests.
- Cryptographic hash, compression, serialization, and Git compatibility: allow small, audited, well-maintained crates when reimplementation would be riskier.
- Optional integrations: isolate behind feature flags.
- Build policy: pin versions, audit licenses, review transitive dependencies, and keep the runtime binary small.

Hard rule: do not implement custom cryptography or custom compression for production storage unless the project is willing to maintain it as a security-critical subsystem.

## 5. Goals

### 5.1 Product Goals

- Make agent-generated code reviewable through intent, provenance, evidence, and check results.
- Replace manual snapshot-style commits with lightweight snapshots.
- Allow multiple agent or human attempts to compete safely.
- Accept only curated proposals into durable project history.
- Keep the CLI predictable for both humans and agents.
- Keep local repositories usable offline.
- Preserve Git interoperability without making Git the internal model.
- Establish object and metadata concepts that can power a hosted Forge platform later.

### 5.2 Engineering Goals

- Rust-native implementation.
- Small, explicit core with clear invariants.
- Crash-safe writes and recoverable metadata.
- Content-addressed immutable objects.
- Append-only operation log for audit and recovery.
- Deterministic structured output for agents.
- Scalable status and snapshot operations.
- Versioned persistent schemas from the first commit.
- Minimal dependency surface, not dependency theater.

## 6. Non-Goals

### 6.1 v0 Non-Goals

- Full Git command replacement.
- Full Git protocol implementation.
- Outperforming Git, Jujutsu, or Sapling as a general-purpose VCS.
- Hosted Forge service.
- Web review UI.
- Native IDE plugin.
- Semantic merge engine.
- Custom distributed synchronization protocol.
- Enterprise auth, orgs, billing, or permissions.
- Perfect secret redaction.
- Large-scale monorepo virtualization.
- GitHub feature parity.

### 6.2 Permanent Non-Goals Unless Revisited

- Treating every exploratory snapshot as public project history.
- Requiring agents to understand Git internals.
- Trusting self-reported agent provenance without later attestation.
- Letting Git branch semantics dictate the Forge domain model.
- Treating v0 native loose objects as proof that Forge has solved complete native VCS storage.

## 7. Target Users

### 7.1 AI Coding Agents

Agents need:

- Non-interactive commands.
- Stable JSON output.
- Idempotent operations where possible.
- Durable IDs for intents, attempts, snapshots, proposals, evidence, check results, decisions, and publications.
- Clear conflict/error codes.
- Safe restore and cleanup operations.
- Evidence capture that can be attached to work.

### 7.2 Human Developers

Developers need:

- A simple default workflow.
- Confidence that user work will not be lost.
- Fast status and diff commands.
- Easy comparison between agent attempts.
- Evidence summaries that are useful without reading raw logs.
- Git export for existing PR workflows.

### 7.3 Reviewers and Technical Leads

Reviewers need:

- Intent and scope context.
- Changed-file and inspected-file context.
- Commands run, exit codes, and check results.
- Risk notes and unresolved questions.
- Confidence that evidence was captured by Forge, not merely written by an agent.

### 7.4 Future Hosted Platform Users

Hosted Forge users will need:

- Remote repositories.
- Proposal review.
- Evidence review.
- Policy gates.
- Identity and permissions.
- CI and agent-run integration.
- Server-side checks and tamper evidence.

These are future product surfaces, but the local object model must not make them impossible.

## 8. Core Domain Model

### 8.1 Repository

A Forge repository is a local workspace with a `.forge` control directory. It may coexist with `.git`, but `.git` is not the source of truth for Forge lifecycle state.

Repository records must include:

- Repository ID.
- Storage format version.
- Hash algorithm version.
- Default line of development.
- Git adapter state when applicable.
- Feature flags enabled for this repository.

### 8.2 Intent

An intent describes the purpose and boundary of work.

Required fields:

- Intent ID.
- Title.
- Problem statement.
- Desired outcome.
- Status: `draft`, `active`, `paused`, `completed`, `abandoned`, `accepted`.
- Creation actor.
- Created/updated timestamps.

Optional fields:

- Scope.
- Constraints.
- Success criteria.
- Linked issue/ticket/PR.
- Risk notes.

### 8.3 Attempt

An attempt is one human or agent try against an intent and a base revision.

Required fields:

- Attempt ID.
- Intent ID.
- Base revision ID.
- Workspace ID.
- Actor ID.
- Agent ID when applicable.
- Status: `created`, `active`, `suspended`, `finished`, `failed`, `abandoned`, `proposal_created`.
- Current snapshot ID.

Important rule: an attempt is not a branch. It is a work attempt with its own snapshots, evidence, and lifecycle.

### 8.4 Snapshot

A snapshot is an immutable captured file state within an attempt.

Required fields:

- Snapshot ID.
- Attempt ID.
- Parent snapshot ID or base revision.
- Tree snapshot ID.
- Operation ID that created it.
- Actor ID.
- Timestamp.
- Summary.
- Changed path summary.

Snapshots may be explicit or automatic. v0 should implement explicit snapshots first. Automatic snapshots must wait until retention, ignore rules, and storage pressure are solved.

### 8.5 Proposal

A proposal is the reviewable unit: a content delta from a base revision to a proposed tree, plus evidence, check state, and review state.

Required fields:

- Proposal ID.
- Intent ID.
- Source attempt IDs.
- Source snapshot IDs.
- Base revision ID.
- Proposed tree ID.
- Derived diff summary.
- Evidence bundle IDs.
- Check state.
- Review state.
- Decision state.
- Publication state.

Proposals must support revisions. A proposal revision changes when proposed file content changes. Summary and evidence edits produce operation records without changing the proposal revision's content identity.

### 8.6 Evidence Bundle

Evidence is structured support for a proposal or snapshot.

Evidence records include:

- Command invocations run through Forge.
- Exit codes.
- Start/end time.
- Working directory.
- Environment allowlist snapshot, not full environment by default.
- stdout/stderr capture policy.
- Truncated output references.
- Test summaries where parsable.
- Tool and agent metadata.
- Files changed.
- Files read/inspected when detectable.
- Check results.
- Sensitivity classification.
- Visibility.
- Trust level.
- Redaction state.
- Origin and remote state hooks for future sync.

Evidence must be treated as potentially sensitive.

### 8.7 Check Result

Check results record whether a proposal revision satisfied a specific policy version under a specific trust level. Local checks are observed evidence, not proof.

Check result fields:

- Proposal revision ID.
- Policy version.
- Required checks.
- Check results.
- Evidence dependencies.
- Actor or agent that ran the check.
- Trust level.
- Timestamp.
- Final state: `not_run`, `running`, `passed`, `failed`, `missing`, `waived`, `stale`.

Check results become stale when the proposal content changes, base revision changes, policy changes, or attached evidence no longer matches the proposal revision.

### 8.8 Decision

A decision records the human or policy outcome for a proposal revision. A decision does not by itself alter target history or publish anything.

Decision fields:

- Decision ID.
- Proposal revision ID.
- Decision state: `approved`, `rejected`, `needs_changes`, `waived`, `superseded`.
- Check result IDs used.
- Actor.
- Timestamp.
- Rationale.

### 8.9 Publication

Publication exports or synchronizes an accepted proposal. It is separate from decision so hosted review, merge queues, branch protection, and Git export can be modeled without overloading acceptance.

Publication fields:

- Publication ID.
- Decision ID.
- Target line of development.
- Expected previous revision.
- Resulting revision when applied.
- Publication kind: `git_branch`, `git_commit`, `hosted_review`, `hosted_target_update`.
- Timestamp.
- Git export metadata when applicable.

Publication must be transactional. Either the target/export advances and the publication record exists, or neither does.

### 8.10 Operation Log

Forge needs an append-only operation log as a first-class primitive, not only snapshot records.

Every mutating command writes an operation record:

- Operation ID.
- Parent operation ID or IDs.
- Actor.
- Command.
- Input arguments.
- Objects created.
- Resulting view ID.
- Start/end time.
- Result.

The operation log is the transaction root of the repository. It supports recovery, audit, debugging, undo, and future hosted synchronization. Jujutsu's operation log is a strong precedent: operation-level history enables recovery and safe concurrent command behavior.

### 8.11 View

A view is the complete mutable repository state after an operation.

Required fields:

- View ID.
- Parent operation ID.
- Target refs.
- Active attempts.
- Intent states.
- Proposal states.
- Evidence index roots.
- Policy state.
- Workspace heads.

The current repository state is the view referenced by the current operation. Mutable refs are updated only by committing a new operation and view.

## 9. Repository Invariants

Forge must define invariants before implementation. Suggested v0 invariants:

- Immutable objects are content-addressed and never modified in place.
- Mutable references only point to existing immutable objects.
- Every mutating command creates exactly one operation record or a recoverable failed operation record.
- The current repository state is `current_operation -> current_view`.
- A snapshot points to one tree snapshot and one parent lineage.
- A proposal revision's proposed tree and base revision are immutable after creation; edits create a new proposal revision.
- A decision does not alter target history.
- Publication cannot advance a target from an unexpected previous revision without an explicit rebase/merge/apply operation.
- Evidence can be appended but not silently rewritten.
- Schema version is present on every persisted record.
- Every syncable object carries `origin`, `visibility`, `sensitivity`, `trust_level`, `redaction_state`, and `remote_state` fields even if v0 only enforces them locally.
- Unknown future schema versions are read-only unless an explicit upgrade is run.
- A repository can run `forge doctor` after crash and identify orphaned temp files, incomplete operations, and dangling objects.

## 10. Storage Architecture

### 10.1 Storage Strategy

Forge v0 should use a hybrid local storage design:

- SQLite metadata store for repository state, operation/view records, intents, attempts, snapshots, proposals, evidence metadata, check results, decisions, publications, conflict sets, indexes, and schema migrations.
- Content backend abstraction for file-content snapshots. Git-backed tree snapshots remain the default. Native Forge snapshots can be enabled explicitly and are stored as loose content-addressed objects under `.forge/objects`.
- Content-addressed file store for large evidence payloads and optional auxiliary blobs.
- Indexes/caches for fast status, path lookup, changed-file queries, and evidence lookup.

The operation/view state in SQLite is the lifecycle source of truth. Git object IDs and Forge object IDs are content backend references, not Forge domain objects. Indexes are rebuildable.

### 10.2 Object Identity

Forge v0 uses SHA-256 as the canonical object identity algorithm. Additional hashes may be stored as auxiliary checksums but are not canonical unless an explicit repository migration changes canonical identity.

Object IDs must be domain-separated:

`f1:<object-type>:sha256:<digest>`

Hash input must include:

- Object type.
- Schema version.
- Canonical payload length.
- Canonical payload bytes.

This avoids cross-type collisions and allows future hash migrations.

Repositories must include a hash algorithm registry and hash mapping table from v0. BLAKE3 may later be used for fast local chunking or cache keys, but not as canonical identity without an explicit migration. Do not use Rust's `DefaultHasher` or any non-persistent hash API for object identity.

### 10.3 Object Types

Minimum v0 object types:

- `content_ref`: backend-specific reference to file content or a tree.
- `snapshot`: root tree/content ref plus workspace metadata.
- `attempt`: work attempt against an intent.
- `proposal`: reviewable proposal.
- `proposal_revision`: immutable content revision of a proposal.
- `evidence`: command/test/provenance record.
- `check_result`: policy result.
- `decision`: approval/rejection/waiver record.
- `publication`: Git export or future hosted sync record.
- `conflict_set`: structured conflict metadata.
- `operation`: append-only mutation record.
- `view`: resulting repository state after an operation.

### 10.4 Metadata Format

The format must optimize for correctness before convenience.

Recommended v0 approach:

- Metadata is stored in SQLite with explicit migrations, transactions, indexes, and integrity checks.
- Large evidence payloads and optional auxiliary content blobs are stored separately as content-addressed files.
- Diagnostic commands render objects as JSON for humans and agents.
- A small line-based text format may be used for local config and policy if it remains simple and versioned.
- Native snapshot content uses deterministic versioned loose blob/tree payloads with SHA-256 identity. A canonical binary object format, packfiles, compression, and remote sync are deferred until the loose-object boundary is proven.

Human-readable `.forge` is less important than inspectable `.forge`. `forge inspect` can provide human-readable views without making the storage format fragile.

### 10.5 Crash-Safe Writes

Every mutating command must follow a durable write protocol:

1. Write new immutable object to a temp path.
2. Flush file contents.
3. `sync_all` or `sync_data` according to durability needs.
4. Atomically move into final object path on the same filesystem.
5. Sync containing directory where supported.
6. Open a SQLite transaction.
7. Write metadata rows, operation record, and resulting view.
8. Advance `current_operation` with an expected-current-operation check.
9. Commit the SQLite transaction.

Rust std gives some useful primitives, but platform behavior differs. Forge must test crash safety on Linux, macOS, and Windows instead of assuming POSIX semantics everywhere.

### 10.6 Locking and Concurrency

Forge must assume multiple agents and humans may run commands concurrently.

v0 should use conservative repository-level write locking plus append-only operation records. Future versions can move toward finer-grained optimistic concurrency.

Rules:

- Read commands should not block each other.
- Write commands must either serialize safely or detect conflicts and fail with recoverable error codes.
- Long-running evidence commands should not hold global locks while the child process runs.
- Publication must use optimistic target checks to prevent lost updates.
- Stale workspaces must be detectable and recoverable.

### 10.7 Garbage Collection

GC is not optional once snapshots and evidence exist.

v0 must at least define:

- Retention policy for snapshots.
- Retention policy for command output.
- Protection rules for accepted proposals and published refs.
- Protection rules for exported Git commits.
- Dry-run cleanup.
- Reachability check from refs, attempts, proposals, decisions, publications, and operation log.

Without this, six months of agent use will turn `.forge` into an unbounded log dump.

## 11. Working Tree and Path Semantics

Forge must be strict about paths because repository tools fail in edge cases:

- Store paths as raw platform-aware paths internally, not lossy UTF-8 strings where the OS allows non-UTF paths.
- Define normalization rules for `/`, `..`, symlinks, case sensitivity, executable bits, CRLF, and file modes.
- Explicitly handle symlinks.
- Explicitly handle file deletion, rename detection, and directory/file conflicts.
- Respect ignore rules before snapshotting.
- Protect `.forge` from being captured as user content.
- Detect large files and require policy before storing repeated large blobs.
- Treat generated directories and build outputs as storage hazards.

Open question: whether Forge should reuse `.gitignore` syntax in v0, define `.forgeignore`, or support both. Reusing `.gitignore` is pragmatic, but it leaks Git conventions into Forge. A reasonable v0 compromise is: read `.gitignore` when present, support `.forgeignore` for Forge-specific exclusions, and document precedence.

## 12. Attempt Isolation

### 12.1 v0 Isolation

v0 can use normal filesystem workspaces and, in Git-backed repositories, Git worktrees as an implementation detail. The Forge domain model should expose attempts and workspaces, not branches.

Required commands:

- `forge start "intent text"`
- `forge attempt list`
- `forge attempt status <id>`
- `forge attempt attach <id>`
- `forge attempt finish <id>`
- `forge attempt abandon <id>`
- `forge attempt compare <a> <b>`

### 12.2 Future Isolation

Future Forge may support:

- Lazy materialized workspaces.
- Sparse/path-scoped agent workspaces.
- Permissioned code views.
- Remote ephemeral workspaces.
- Workspace snapshots that can run in CI or hosted agents.

The v0 storage model must represent workspace identity and path scope even if enforcement is simple at first.

## 13. Snapshot Semantics

Snapshots are not commits. They are private or semi-private recovery and comparison points within an attempt.

v0 requirements:

- Manual snapshot creation.
- Snapshot listing.
- Snapshot diff against parent/base.
- Restore snapshot to workspace with explicit confirmation unless agent mode supplies `--yes`.
- Snapshot summaries.
- Snapshot reachability from attempt.

Future automatic snapshots must include:

- Trigger policy.
- Storage budget.
- Debounce rules.
- Secret/output exclusion.
- User-visible retention controls.

Automatic snapshots without retention will become operational debt quickly. No automatic snapshots may ship until retention and GC are implemented.

## 14. Proposal Semantics

Proposals are curated change proposals.

v0 requirements:

- Create proposal from current attempt state or snapshot.
- Attach evidence bundles.
- Show human summary.
- Show machine-readable summary.
- Diff against base.
- Check.
- Decide.
- Publish.
- Export to Git branch.

Proposal states:

- `draft`
- `ready_for_check`
- `checked`
- `needs_changes`
- `approved`
- `published`
- `rejected`
- `superseded`

Proposals must support revisions. A proposal revision changes when proposed file content changes. Evidence and summary edits should not require a new proposal ID, but should produce new operation records.

## 15. Conflict and Merge Model

This is one of the largest gaps in IDEA.md.

Forge must define conflicts as data from v0, not only as conflict markers in files.

v0 minimum:

- Detect when proposal base is stale relative to publication target.
- Refuse accept/apply/publication unless the proposal is rebased, merged, or explicitly overridden by policy.
- Use Git or a simple three-way merge through the adapter in Git-backed v0 repositories.
- Persist `ConflictSet` and `PathConflict` records even when merge execution is delegated to Git.
- Record conflict files and conflict status in the proposal/attempt.
- Never silently resolve binary conflicts.

Minimum `ConflictSet` fields:

- Conflict set ID.
- Proposal ID.
- Base tree.
- Ours tree.
- Theirs tree.
- Path conflicts.
- Generated-by operation.
- Resolver backend.
- Status: `unresolved`, `partially_resolved`, `resolved`, `abandoned`.

Minimum `PathConflict` fields:

- Path.
- Kind: `content`, `binary`, `delete_modify`, `rename`, `dir_file`, `mode`, `symlink`.
- Base ref.
- Ours ref.
- Theirs ref.
- Resolution ref when resolved.

If conflict semantics remain vague, Forge will be simple only until two agents edit the same files.

## 16. Evidence and Trust Model

### 16.1 Evidence Is Not Proof

Evidence shows what Forge observed. It does not prove code is correct.

Forge should avoid implying that a command log is a security guarantee. Evidence can be incomplete, stale, forged outside Forge, or generated in an untrusted environment.

### 16.2 Evidence Capture Rules

v0 command:

`forge run -- <command> [args...]`

Captured by default:

- Command and args.
- Exit code.
- Start/end time.
- Working directory.
- Forge repository/attempt/proposal context.
- stdout/stderr policy result.

Default output capture should be conservative:

- Capture command metadata and bounded output excerpts by default.
- Raw stdout/stderr capture is opt-in or policy-required.
- Truncate large output.
- Allow `--no-output`.
- Allow `--sensitive`.
- Allow explicit export policies.
- Never capture full environment by default.
- Evidence is private by default.

### 16.3 Sensitivity Labels

Every evidence bundle needs a sensitivity label:

- `public`
- `internal`
- `sensitive`
- `secret-risk`

Export commands must respect labels. `forge evidence export` should refuse to export `secret-risk` evidence without explicit override.

Visibility labels:

- `private`
- `attempt-participants`
- `repo-members`
- `reviewers`
- `public`

Redaction states:

- `raw`
- `redacted`
- `raw-pruned`
- `blocked`

### 16.4 Provenance

v0 provenance may include self-reported:

- Actor name/email.
- Agent name.
- Model/provider.
- Tool runtime.

But this must be marked as self-reported unless signed or attested.

Trust levels:

- `self_reported`
- `locally_observed`
- `locally_signed`
- `hosted_runner_observed`
- `hosted_runner_signed`
- `third_party_attested`

Future hosted Forge should support:

- Signed evidence records.
- Server-side command runners.
- OIDC or workload identity.
- Agent identity attestations.
- Tamper-evident logs.

## 17. Checks and Policy

Check results are the bridge between evidence, decisions, and publication. They do not prove correctness; they record observed policy results for a specific proposal revision.

v0 should support a simple policy file:

`.forge/policy.forge`

Initial policy capabilities:

- Required commands by path pattern.
- Required successful `forge run` records.
- Required format/lint/test commands.
- Optional human approval marker.
- Maximum allowed stale base age.
- Evidence export requirements.

Check output must include:

- Passed checks.
- Failed checks.
- Missing checks.
- Stale checks.
- Waived checks.
- Exact evidence IDs used.

Do not build a full policy language in v0. Use a constrained declarative format.

### 17.1 Anti-Gaming Requirements

Local checks are gameable. Agents can run fake commands, alter `PATH`, modify tests, hide failing output, or attach evidence from another proposal.

Forge mitigates this by binding every check result to:

- Proposal revision ID.
- Policy version.
- Command path and executable hash when available.
- Environment allowlist hash.
- Evidence IDs.
- Working-tree cleanliness state.
- Trust level.

Agents cannot waive their own required checks unless policy explicitly allows self-waiver.

## 18. Decision and Publication Semantics

Decision and publication split acceptance from side effects.

Decision records:

- Proposal revision ID.
- Decision state: `approved`, `rejected`, `needs_changes`, `waived`, `superseded`.
- Check results considered.
- Actor.
- Timestamp.
- Rationale.

Publication records:

- Decision ID.
- Target line of development or export target.
- Expected previous revision.
- Resulting revision or exported ref.
- Publication kind.
- Timestamp.

Accept command:

`forge accept <proposal-id> --target <line>`

Agent mode:

`forge accept <proposal-id> --target <line> --json --yes`

Accept/apply fails if the target line no longer equals the proposal's expected base unless an explicit rebase, merge, override, or new proposal revision handles the stale base.

Publication should be boring and strict. Ambiguous publication is where data loss and history corruption happen.

## 19. Git Interoperability

### 19.1 Principle

Git is an adapter, not the internal model.

Forge must support existing Git workflows because adoption depends on it. But Git concepts must remain at the boundary.

### 19.2 v0 Git Adapter

Recommended v0 approach:

- Use Git CLI as the compatibility adapter for import/export in Git-backed repositories.
- Store Forge lifecycle state in `.forge`.
- Export accepted proposals to Git branches or commits.
- Generate PR-ready Markdown evidence.
- Record Git object IDs as adapter metadata.
- When native content mode is enabled, synthesize Git trees from Forge-native tree objects only at export time.

The v0 content boundary should be expressed through a `ContentBackend` abstraction:

- Snapshot worktree to a `TreeRef`.
- Diff a base `TreeRef` against a proposed `TreeRef`.
- Materialize a `TreeRef` into a workspace.
- Export a proposal to a Git branch.

This is pragmatic. Reimplementing Git protocol before validating Forge's agent-native workflow is likely a trap. Native mode keeps the boundary intact: Forge can capture and restore snapshots without Git tree objects internally, but accepted work still leaves through the Git adapter in v0.

### 19.3 Future Native Git Adapter

Future versions can replace shelling out with a pure Rust Git adapter. gitoxide proves that serious pure-Rust Git infrastructure is possible, but also demonstrates the size and complexity of the domain: object database, references, transport, CLI behavior, and performance all matter.

Forge should learn from Git's formats:

- Packfiles and indexes exist because loose objects do not scale.
- Commit graphs and path Bloom filters exist because graph traversal gets expensive.
- Partial clone and sparse checkout exist because large repos cannot always be fully materialized.
- Protocol v2 is command-oriented and designed for stateless server behavior.

Forge should not blindly copy these designs, but it should not rediscover their constraints accidentally.

## 20. Hosted Forge Implications

If Forge may become the next GitHub, the local model must prepare for hosted collaboration.

### 20.1 Hosted Objects

Hosted Forge needs first-class remote objects for:

- Repositories.
- Intents.
- Attempts.
- Proposals.
- Reviews.
- Evidence bundles.
- Check results.
- Decisions.
- Publications.
- Comments.
- Checks.
- Policies.
- Actors, teams, and permissions.

Do not model the future hosted product as "Git repo plus PR table." That would throw away Forge's differentiator.

### 20.2 Server Trust

Hosted Forge must not blindly trust local `.forge` evidence. Server-side workflows need:

- Evidence ingestion validation.
- Optional server-side checks.
- Signature/attestation checks.
- Secret scanning on uploaded evidence.
- Policy enforcement independent of local client claims.

### 20.3 Permissions

Future permission design must account for:

- Path-level access.
- Evidence visibility.
- Attempt visibility.
- Agent permissions.
- Contractor/external contributor views.
- Private command output.

This means v0 objects should include sensitivity and visibility hooks even if enforcement is local-only.

Minimum hosted-ready fields for every future-syncable object:

- `origin`
- `visibility`
- `sensitivity`
- `trust_level`
- `redaction_state`
- `remote_state`
- `schema_version`
- `created_by`
- `created_at`

### 20.4 Remote Synchronization

Remote sync must handle:

- Object transfer.
- Missing/lazy objects.
- Concurrent operations.
- Proposal revisions.
- Evidence blobs.
- Policy versions.
- Garbage collection across clients.

Git's partial clone design is a warning: lazy object systems need explicit missing-object semantics, demand fetching, and corruption distinction from intentionally missing promised objects.

## 21. CLI Requirements

### 21.1 CLI Principles

- Every command has human output by default.
- Every command has `--json`.
- `--json` and agent mode never prompt.
- Destructive commands without `--yes` fail with `CONFIRMATION_REQUIRED` instead of prompting.
- Every mutating command accepts `--request-id <client-generated-id>` and returns the existing operation result if retried.
- Every error has a stable machine-readable code.
- Long-running commands expose versioned event streams when needed.
- Commands should be composable and predictable.
- No command is implemented until its JSON schema, error codes, idempotency behavior, and prompt behavior are specified and covered by golden tests.

### 21.2 Core Commands

Default human/agent workflow:

- `forge init`
- `forge start "intent text"`
- `forge save`
- `forge run -- <command>`
- `forge propose`
- `forge check`
- `forge accept`
- `forge reject`
- `forge show`
- `forge doctor`
- `forge export pr-body`
- `forge export branch`

Advanced noun commands may exist later for inspection and automation, but the default workflow should not force humans to learn every internal object.

### 21.3 JSON Output Contract

Every `--json` response should include:

- `schema_version`
- `command`
- `request_id`
- `operation_id`
- `status`
- `data`
- `warnings`
- `errors`
- `retry`

Error objects should include:

- Stable code.
- Human message.
- Recovery suggestion.
- Related object IDs.
- Whether retry is safe.
- Recovery command when one is known.

This contract is critical for AI agents. Changing it casually will break automation.

## 22. MVP Scope

### 22.1 MVP Objective

Build a single Rust CLI that works inside an existing Git repository, creates `.forge`, tracks intents/attempts/snapshots/evidence/proposals, checks proposals with simple local policy, records decisions, and exports accepted proposals to Git.

### 22.2 Must-Haves

- `forge init`
- `.forge` schema versioning.
- SQLite metadata store.
- Operation log and view model.
- Intent/attempt/snapshot/proposal/evidence/check/decision/publication objects.
- Git-backed content snapshots through a `ContentBackend` abstraction.
- Manual snapshot create/list/diff/restore.
- Ignore handling.
- `forge run` bounded evidence capture.
- Evidence sensitivity labels.
- Evidence visibility, redaction state, and trust level.
- Proposal create/show/diff.
- Basic checks from configured commands.
- Decision and publication records.
- Stale base detection.
- Conflict set metadata.
- Git branch export.
- PR body export.
- Human output and JSON output.
- `forge doctor`.
- Basic cleanup/GC with dry-run.
- Agent identity metadata.
- Safe write protocol tests.
- JSON golden tests.

### 22.3 Should-Haves

- Attempt comparison.
- Proposal revision support.
- Git commit export.
- Simple policy waivers.
- Evidence payload pruning.

### 22.4 Explicitly Defer

- Automatic snapshots.
- Hosted remote.
- Independent Forge object store for all file content.
- Native Git protocol.
- Native Git packfile writing.
- Fine-grained permissions.
- Semantic merge.
- Server-side evidence attestation.
- IDE integration.
- Web UI.
- Custom binary metadata format.
- Monorepo sparse/lazy workspace features.

## 23. Six-Month Failure Patterns to Design Against

### 23.1 `.forge` Becomes Huge

Agents create hundreds of snapshots and capture large command outputs. Without retention, compression, and GC, repositories become unusable.

Mitigation: retention policy, evidence truncation, storage budget warnings, dry-run GC, and large-file policy in v0.

### 23.2 Evidence Leaks Secrets

Test output, env dumps, stack traces, API responses, and config files may contain secrets.

Mitigation: conservative capture defaults, sensitivity labels, explicit export gates, no full env capture, secret-scanning hooks later.

### 23.3 Agent JSON Contracts Drift

Agents depend on exact JSON shapes. A minor CLI refactor breaks automation.

Mitigation: version every JSON response, add snapshot tests, maintain compatibility windows, publish schema docs.

### 23.4 Git Adapter Leaks Into the Core

The easiest path is to map everything to branches and commits. Six months later Forge is a Git wrapper with evidence sidecars.

Mitigation: keep Git IDs as adapter metadata only, enforce Forge domain objects in core APIs, and test non-Git-backed repositories early.

### 23.5 Accept and Publication Races Lose Work

Two agents create valid proposals from the same base. One is accepted and published. The other is accepted later and accidentally overwrites or reverts the first.

Mitigation: optimistic target checks, stale base states, explicit rebase/merge before accept/apply/publication.

### 23.6 Conflict Semantics Become Unmaintainable

If conflicts are only text markers in files, hosted review and multi-agent resolution become messy.

Mitigation: record conflict metadata in v0 even if conflict resolution is delegated to Git initially.

### 23.7 No-Dependency Policy Slows the Core

The team spends months hand-writing hash, parser, compression, and Git code instead of validating the workflow.

Mitigation: audited dependency policy, optional feature flags, and no custom crypto/compression.

### 23.8 Human Mental Model Gets Too Heavy

Intent, attempt, snapshot, proposal, evidence, check result, decision, and publication may be too much vocabulary.

Mitigation: default workflows should hide complexity:

- `forge start "fix login bug"`
- `forge save`
- `forge run -- cargo test`
- `forge propose`
- `forge accept`

Advanced objects remain inspectable, not always front-and-center.

### 23.9 Status Is Too Slow

Naive full-tree scans will fail on large repositories.

Mitigation: maintain a working-tree index/cache, path filters, changed-path acceleration, and clear fallback behavior.

### 23.10 Hosted Forge Needs Permissions That Local Objects Cannot Express

If evidence and proposals lack sensitivity/visibility fields, future hosted access control becomes a migration nightmare.

Mitigation: include visibility/sensitivity metadata from v0.

### 23.11 Provenance Is Overtrusted

Agents can claim any model/tool identity if metadata is self-reported.

Mitigation: label provenance as self-reported until signed or server-attested.

### 23.12 Proposal Review Becomes Diff-Only Again

If evidence summaries are poor, humans ignore them and review diffs only.

Mitigation: make evidence summaries concise, structured, and tied to check policy.

## 24. Differentiation

### 24.1 Better Than Git Plus Scripts

Forge wins only if it provides durable primitives scripts cannot reliably provide:

- Structured intent/attempt lifecycle.
- Crash-safe snapshots.
- Proposal identity.
- Evidence bundles linked to exact content.
- Check state.
- Decision and publication records.
- Agent-safe JSON contracts.
- Repository health and GC.

If Forge is only a folder of logs plus Git branches, it is not enough.

### 24.2 Better Than Jujutsu for This Use Case

Jujutsu already has major ideas Forge should respect: operation log, automatic working-copy commits, workspace support, and Git compatibility. Forge must not pretend these do not exist.

Forge's distinct wedge should be:

- Agent-native command contract.
- Intent/attempt abstraction.
- Evidence capture.
- Checks/policy.
- Decision and publication records.
- Hosted review model around proposals and evidence.
- Sensitivity and provenance model.

Forge should learn from Jujutsu's operation safety while focusing on the agent-review lifecycle Jujutsu does not primarily target.

### 24.3 Better Than Future GitHub Extensions

GitHub can add agent metadata and richer PR checks, but it remains Git/PR-centered. Forge should make the proposal/evidence/check/decision/publication lifecycle native locally and remotely.

The hosted platform should not be "GitHub but Rust." It should be "review and safely publish checked change attempts."

## 25. Engineering Architecture

### 25.1 Suggested Crate Layout

- `forge-cli`: CLI parsing and output.
- `forge-protocol`: JSON schemas, error codes, request/response types.
- `forge-core`: domain types and invariants.
- `forge-store`: SQLite metadata, migrations, operations, views, indexes.
- `forge-content`: content backend traits, tree refs, blob refs, diff abstractions.
- `forge-content-git`: Git CLI-backed content backend.
- `forge-content-native`: Forge-native loose object backend for regular-file snapshots.
- `forge-worktree`: path scanning, ignore handling, snapshots, restore.
- `forge-evidence`: command capture and evidence records.
- `forge-policy`: check policy and results.
- `forge-export-git`: Git branch/commit/PR-body export boundary.
- `forge-sync`: reserved for future remote protocol shape; stubbed in v0.
- `forge-test-support`: crash/recovery and fixture helpers.

Keep core domain types independent from the Git adapter.

### 25.2 Dependency Policy

Each dependency must declare:

- Why it is needed.
- Whether it is core or optional.
- License.
- Transitive dependency count.
- Security posture.
- Replacement difficulty.

Initial likely dependency exceptions:

- Cryptographic hash implementation.
- CLI argument parser if std-only parsing becomes noise.
- Serialization support for JSON contracts and structured records.
- SQLite bindings with a controlled bundling/system-library policy.
- Gitignore/path walking semantics.
- Cross-platform time handling.
- Git adapter library only after CLI adapter proves insufficient.
- Compression when pack/GC work begins.

Prefer fewer, boring dependencies over handcrafted infrastructure.

### 25.3 Testing Strategy

Required test categories:

- Unit tests for object encoding/decoding.
- Golden tests for JSON CLI output.
- Property tests for object roundtrips and path normalization.
- Crash simulation tests for write protocol.
- Concurrent command tests.
- Git adapter integration tests.
- Large-repo synthetic performance tests.
- Evidence redaction/export tests.
- Schema migration tests from every released version.

If Forge cannot prove repository safety under tests, it should not ask users to trust it with source code.

## 26. Remaining Open Questions for Deep Review

### 26.1 Product Questions

- Is intent/attempt/snapshot/proposal/evidence/check/decision/publication the right vocabulary, or should any concept collapse?
- What is the simplest CLI workflow that hides the object model without losing power?
- What is the strongest adoption wedge against Git plus scripts, Jujutsu, and Sapling?
- Is Git-backed v0 too conservative, or is it the right way to prove the workflow?

### 26.2 Storage Questions

- Is SQLite metadata plus Git-backed content the right v0 storage cutline?
- What is the minimum safe SQLite schema and migration strategy?
- Is SHA-256 the right canonical ID for v0?
- Is the hash mapping table sufficient for future hash migration?
- What should remain in SQLite versus content-addressed payload files?
- How should indexes be rebuilt and validated?

### 26.3 Concurrency and Reliability Questions

- Is repository-level write locking acceptable for v0?
- What transaction protocol is sufficient across Linux, macOS, and Windows?
- Should Forge copy Jujutsu's operation-log concurrency model more directly?
- How should interrupted commands and stale workspaces recover?

### 26.4 Agent and Evidence Questions

- What evidence should be captured by default?
- What should never be captured by default?
- How should agents declare identity?
- How should Forge prevent agents from gaming local check evidence?
- Should evidence be required for accept/publication in v0?

### 26.5 Git and Hosted Platform Questions

- How long is it acceptable to shell out to Git?
- What Git compatibility boundary avoids contaminating the core model?
- What local object fields are needed now for future hosted permissions?
- Should remote sync follow Git-like pack negotiation, operation-log sync, or a new object protocol?

## 27. Launch Blocker Checklist

v0 cannot ship unless:

- `forge doctor` can recover from interrupted writes.
- Stale-base accept/publication is impossible by default.
- Evidence export blocks `secret-risk` payloads by default.
- JSON golden tests exist for every command.
- Operation/view recovery tests pass.
- GC dry-run exists.
- Git export branch works.
- Proposal evidence binds to an exact proposal revision.
- Conflict set metadata is persisted when stale-base apply or merge conflicts occur.
- Local checks are labeled with their trust level.

## 28. Success Criteria

Forge v0 succeeds if:

- A coding agent can use the CLI without interactive prompts.
- A human can understand the intent, files changed, evidence, and check state.
- An attempt can snapshot work without creating noisy Git commits.
- A proposal can be created, checked, accepted, rejected, and exported.
- The repository survives interrupted commands and validates with `forge doctor`.
- Git export works well enough for current GitHub PR workflows.
- `.forge` remains bounded by retention/GC controls.
- JSON output remains stable under tests.
- The architecture keeps Git at the adapter boundary.

Forge v0 fails if:

- Users still need to manage Git branches/worktrees manually for the core workflow.
- Evidence is too noisy or risky to export.
- Snapshots bloat storage uncontrollably.
- Agent automation breaks due to unstable CLI output.
- The Git adapter becomes the real data model.
- Publication can lose or silently overwrite work.

## 29. External Recon Notes

These sources informed the constraints above:

- Git pack format and indexes show why scalable VCS storage needs more than loose objects: https://git-scm.com/docs/gitformat-pack
- Git index format shows the depth of working-tree state needed for fast status and sparse behavior: https://git-scm.com/docs/gitformat-index
- Git commit-graph shows the need for graph acceleration at scale: https://git-scm.com/docs/gitformat-commit-graph
- Git protocol v2 is command-oriented and stateless by default, which matters for future remote Forge: https://git-scm.com/docs/protocol-v2/2.34.0
- Git partial clone documents lazy object and missing-object semantics that Forge should learn from: https://git-scm.com/docs/partial-clone.html
- Git sparse checkout documents user-facing complexity around partial working trees: https://git-scm.com/docs/sparse-checkout
- Jujutsu operation log demonstrates operation-level recovery and concurrency as a core VCS primitive: https://docs.jj-vcs.dev/latest/operation-log/
- Jujutsu working-copy docs show an alternate model for automatic snapshots, conflicts, and workspaces: https://docs.jj-vcs.dev/latest/working-copy/
- Pijul's patch model is useful prior art for change identity and commutation, but its model is a major commitment: https://pijul.com/manual/why_pijul.html
- gitoxide demonstrates that pure-Rust Git infrastructure is possible, but large in scope: https://github.com/GitoxideLabs/gitoxide
- Rust filesystem docs confirm useful primitives such as atomic create, sync, locks, and rename, but not a full storage engine: https://doc.rust-lang.org/std/fs/struct.File.html and https://doc.rust-lang.org/std/fs/fn.rename.html
- Serde is the Rust ecosystem's standard serialization layer, but adopting it is a dependency-policy decision: https://docs.rs/serde/latest/serde/
