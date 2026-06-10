# Forge → True, AI-Agent-Native Git Alternative: Sequenced Roadmap

## Where Forge is today

Forge is a well-disciplined **agent control surface and review ledger sitting on top of git** — and it is honest about that (the PRD states v0 is not an independent VCS). The agent-facing contract is genuinely mature and differentiated; the substrate beneath it is thin and quietly git-dependent. The lifecycle/metadata layer is the credible, defensible part. The storage-engine independence and the evidence-verification *depth* the strategy stakes its differentiation on are mostly unbuilt.

- **Strong:** a versioned JSON envelope, durable IDs that double as selectors, `--request-id` idempotent replay with conflict detection, no-prompt `--yes` gating, a real intent→attempt→snapshot→proposal→evidence→check→decision→publication lifecycle, content-addressed SHA-256 objects with domain separation and verify-on-read, an append-only operations/views op-log spine, and behavior-focused tests. The competing-attempts wedge (PR #3) ships green on a single checkout.
- **Weak / load-bearing gaps:** no native commit/DAG/refs/diff/merge/remote; the "check engine" is a single `exit==0` comparison gameable by `run -- true`; evidence is mutable and unsigned; no concurrency model (no WAL/busy_timeout — second writer hard-fails); timestamp-only IDs can collide; a swallowed parent-dir fsync error with no store-before-DB ordering (a committed ref can point at a vanished object); GC is dry-run-only on uncompressed loose JSON; the "native" backend still shells out to `git ls-files`/`git diff` (PRD §23.4 "git adapter leaks into the core" is *already partly realized*); the `conflict_sets` table is declared but never written; the error layer reconstructs codes by substring-matching free text and almost never populates `details`/`warnings`/`retry`.

## Sequencing logic

The order is three movements — **bulletproof the ledger (Phases 1–2), fund the wedge (3–6), earn native-VCS independence (7–9)** — dependency-correct first and value-weighted second. Trust is the product's core invariant: every later feature silently assumes that a committed ref survives a crash, that evidence wasn't tampered with, and that the contract is stable, so substrate correctness must precede scale. But correctness is necessary, not sufficient — the agent-native depth (real check engine, tamper-evident structured evidence, the missing compare/rank API) is where the smallest investment yields the largest differentiation, so the wedge is funded *immediately* after the cheapest substrate fixes it needs land, never starved behind the far larger native-VCS work. Native-VCS independence is real and strategically existential, but it is greenfield and earns its place last, where it inherits durability, concurrency, versioning, a stable contract, and an integrity model for free rather than being rebuilt on sand.

**Three changes I made versus the original draft, on the strength of the critique:** (1) the two PRD §27 launch blockers — secret-export default-deny and conflict-set metadata persistence — are pulled forward out of Phases 5/8 to *before the export path that needs them*; (2) Phase 3's speculative physical worktrees are demoted (the project explicitly deferred them), leaving only the real correctness fix on the critical path; (3) the monolithic Phase 1 is split (1a cheap/unblocking, 1b hardening) and relabeled XL. I deliberately keep the critique's *one* questionable suggestion — eager phase-splitting of 7 and 9 into parallel tracks — as explicit sequencing notes rather than new phases (see "deliberate disagreements").

---

## The roadmap

### Phase 1a — Minimum substrate the wedge needs: durability fix, WAL concurrency, collision-safe IDs, idempotent domain writes
**Goal:** Make the cheapest, highest-leverage substrate fixes the wedge depends on so competing attempts can run in parallel without `database is locked`, ID collisions, or a silently lost object — without waiting on the heavier hardening.

**Features**
- Close the rename-durability hole: propagate (never swallow) the parent-dir fsync error in `forge-content-native` (replace `let _ = file.sync_all()` at lib.rs:507); fsync newly-created ancestor dirs on first creation (lib.rs:155-164).
- Configure the production connection for multi-process use: `PRAGMA journal_mode=WAL`, non-zero `busy_timeout`, explicit `synchronous`, `BEGIN IMMEDIATE` for writers (open_connection lib.rs:1875-1879), closing the record_check/propose/record_evidence read-then-write TOCTOU windows.
- Replace timestamp-only IDs (forge-store `new_id` lib.rs:1788-1797, forge-core `unique_suffix` lib.rs:88-95) with ULID/UUIDv7 (monotonic + entropy, lexically sortable); add a rowid/sequence tiebreak to every `... ORDER BY created_at_ms DESC LIMIT 1` so same-nanosecond mints can't collide and "latest" is deterministic. **No row rewrite is required** — IDs are opaque TEXT keys, so this is a forward-only mint-format change with no schema migration (correcting the original's phantom "migration story delivered in Phase 2" coupling).
- **`request_id`-scoped idempotency covering domain rows** (snapshot/evidence/proposal), not just the operations row: a retried `save` that committed-but-lost-its-ack must not create a second snapshot before the unique index fires. Make the command's domain writes conditional on the operation row not already existing.

**Depends on:** — (nothing)
**Why it beats git:** brings Forge to git's concurrent-access floor (many readers, serialized writers, no hard-fail on the second writer) and collision-proof identity, the table stakes for a substrate rather than a cache.
**Why it's agent-native:** agents fan out in parallel and retry on timeout — exactly the two conditions today's substrate fails (second agent hard-fails; same-nanosecond IDs collide in the compete loop; a retried command silently double-inserts).
**Effort:** M · **Risk:** WAL behavior varies subtly across OSes (test Linux/macOS/Windows; keep write txns short). ID change touches every mint/selector path.
**Exit criteria:** ≥8 concurrent processes complete the compete loop with zero `SQLITE_BUSY` and zero ID collisions across a 10k-mint loop; `grep` proves no `let _ = .*sync` remains on a durability path; a mutating command retried with the same `--request-id` creates **exactly one** domain row.

### Phase 1b — Crash-correctness hardening: store-before-DB ordering, atomic restore, repo write lock
**Goal:** Lock the full durability invariant — if a command returns `Ok`, its effects survive a crash — and make the serialization point explicit and observable. Can proceed in parallel with Phase 2.

**Features**
- Enforce a strict store-before-DB durability **order**: object file + its directory entry durable *before* the SQLite txn that commits the referencing `content_ref`.
- Crash-atomic worktree restore via per-file temp-file+rename rather than in-place `fs::write` (lib.rs:389, 112-119).
- PRD §10.6 repo-level advisory file lock on `.forge` with a typed `LOCK_TIMEOUT` (retryable) error, so serialization is explicit rather than an accidental property of SQLite locking.
- Crash-injection + concurrency harness: kill between object-fsync and DB-commit, and mid-restore; `doctor` must report zero dangling `content_refs` and zero half-applied worktrees.

**Depends on:** Phase 1a
**Why it beats git:** matches git's loose-object atomic-rename + durable-parent-dir + lockfile-ref crash safety, which Forge is strictly weaker than today.
**Why it's agent-native:** agents are killed mid-operation (timeouts, sandbox teardown); without store-before-DB ordering an agent gets `Ok` then loses the object on power loss; the advisory lock turns the second writer's hard-fail into a queued, retryable wait.
**Effort:** L · **Risk:** per-file temp+rename and per-ancestor fsync add latency on large worktrees (fsync ancestors only on first creation; benchmark). Advisory lock + WAL interact subtly across OSes.
**Exit criteria:** crash-injection passes on Linux and macOS at every durability boundary; a committed `content_ref` provably implies a durably-retained object; the locking model and its contention error are documented and golden-tested.

> **Why 1a/1b split (vs the original monolithic "L" Phase 1):** the critique is right that the bundle is realistically XL and that lumping cross-OS advisory locking + a full crash-injection harness with the cheap fixes risks the foundation consuming the runway and starving the wedge. 1a is the minimal substrate the wedge actually needs and unblocks Phases 3–6; 1b runs alongside Phase 2.

### Phase 2 — Migration framework, typed machine-actionable contract, and the launch-blocker gates
**Goal:** Make the schema safely evolvable and the agent-facing contract stable-by-construction — and land the two cheap PRD §27 launch blockers here, before any egress path exists.

**Features**
- Numbered `.sql` migration runner: each migration discrete, ordered, recorded in `schema_migrations`, DDL + version stamp in **one** transaction, gated under the Phase 1b write lock — replacing the imperative apply_migrations sequence (lib.rs:1799-1843) that runs unconditionally on every open.
- Unify fresh-init and upgrade paths so a freshly initialized DB is schema-identical to an upgraded one (today `attached_attempt_id` is an upgrade-only ALTER absent from 001's DDL — a *present-tense* divergence). Per-migration checksums; "unknown future version ⇒ read-only refuse."
- Typed `ForgeError` enum emitted from forge-store (STALE_BASE, DIRTY_WORKTREE, AMBIGUOUS_ATTEMPT, AMBIGUOUS_PROPOSAL, REQUEST_ID_CONFLICT, NOT_ACCEPTED, LOCK_TIMEOUT…), replacing substring-matched `error_code()` (main.rs:739-775) and `bail!("stale base")` string contracts.
- Populate `errors[].details` with structured payloads (AMBIGUOUS_ATTEMPT → candidate IDs as array; STALE_BASE → expected vs actual head; DIRTY_WORKTREE → offending paths); make `retry.retryable` meaningful; populate `warnings[]` for silent behaviors.
- Discoverable `forge schema` / `--capabilities` emitting envelope `schema_version`, per-command shapes, and the full error-code registry as a published JSON Schema; convert tests that pin human wording to assert typed codes + details.
- **Launch blocker A — secret-export default-deny:** gate evidence/content export on secret-risk by default and surface every dropped secret path as a `warnings[]` entry. (Cheap: snapshots already exclude secret-risk paths; the missing piece is the export-time refusal + warning, not the full redactor.)
- **Launch blocker B — conflict-set metadata persistence:** when `current_head != base_head` is detected at accept/export, write a `ConflictSet`/`PathConflict` row recording the divergence (PRD §15 "persist as data even while delegating merge to git"). This is a metadata insert — **no merge engine required.**

**Depends on:** Phase 1a (ID format, idempotency); coordinates with Phase 1b (write lock)
**Why it beats git:** git porcelain is not a stable machine contract and its formats are forward-compatible by explicit design; Forge's versioned self-describing envelope with stable codes + a disciplined migration mechanism makes both real (today both are partly fake).
**Why it's agent-native:** an agent branches on codes + details (re-derive on STALE_BASE with the actual head, disambiguate from candidate IDs, retry on LOCK_TIMEOUT) and runs many binary versions against shared repos; refuse-on-unknown gives a clean "this repo is newer than me" signal.
**Effort:** L · **Risk:** changing codes/shapes is itself a contract break (ride the `schema_version` bump with a compatibility window). Reconciling in-the-wild ad-hoc v1/v2 DBs can brick repos if mis-detected.
**Exit criteria:** adding a schema change requires only a numbered `.sql` file; runs are crash-atomic and idempotent; **before reconciliation the DB is backed up**, and a schema-diff test proves both genesis paths — fresh-init-v2 *and* upgraded-via-ALTER — converge to an identical head; no error code is string-derived (grep proves the enum is the single source); `forge schema` emits a versioned contract; **export refuses secret-risk payloads by default and the refusal is a `warnings[]` entry; a stale-base bail writes a persisted conflict-set row.**

> **Why the two launch blockers moved here (vs original Phases 5/8):** PRD §27 names both as hard "cannot ship v0 without" gates. The original buried the secret-export gate at Phase 5 *behind* Phase 6's git-export egress path — the exact launch-blocker condition the PRD forbids and the §23.12 secret-leak surface — and buried conflict-set persistence at Phase 8 behind the entire native-VCS spine while Phases 1–6 repeatedly hit STALE_BASE and recorded nothing. Both are cheap to decouple from the heavy work (the full entropy/PEM/JSON redactor stays in Phase 5; the 3-way merge engine stays in Phase 8).

### Phase 3 — Close the cross-attempt contamination footgun (write-binding verification)
**Goal:** Make `save --attempt X` provably record attempt X's content, eliminating the silent contamination footgun — the one genuinely-needed isolation fix on the critical path.

**Features**
- Make `save --attempt X` verify the workspace it snapshots actually corresponds to attempt X's binding (record/verify the expected `content_ref` before snapshot) rather than snapshotting whatever is in cwd (main.rs:346) — closing the footgun masked in tests only by always attaching first.
- Keep git as the materialization adapter at this stage, but **isolate worktree management behind the `ContentBackend` boundary** so git-worktree semantics (locking, shared HEAD) cannot leak into the core (guarding the §23.4 risk and leaving a clean seam for the Phase 7 native walker).

**Depends on:** Phase 1a
**Why it beats git:** makes "N attempts at one intent" a first-class, ID-addressable concept rather than branch-juggling + scratch-commits + PR-body archaeology.
**Why it's agent-native:** the v0 wedge is "multiple agents attempt, the best is selected"; a wrong `--attempt` silently recording the wrong files is a trust-destroying autonomous-loop failure.
**Effort:** S · **Risk:** low; the main risk is scope creep back into physical worktrees (resist it).
**Exit criteria:** a test proves `save --attempt X` records X's content even when a different attempt was most recently materialized; worktree management is confined behind `ContentBackend` (no git-worktree calls in core lifecycle code).

> **Why Phase 3 is deflated from L→S (vs the original):** the critique is correct and decisive here. The original made **physical per-attempt worktrees** a blocking precondition of the headline differentiator — but the project's own competing-attempts doc explicitly scopes the wedge as "proposal-level competition in one checkout, NOT parallel physical worktrees" and explicitly defers per-attempt workspace directories. Compare/rank (Phase 6) operates on DB-resolved, attempt-bound snapshot `content_refs`, which are independent regardless of how many physical checkouts exist — so physical isolation is **not** a precondition. The original even admitted these worktrees "depend on Phase 8 GC to reclaim." Physical workspaces are deferred (see "What we defer"); only the real correctness fix stays on the path, unblocking Phase 6 far sooner.

### Phase 4 — Declarative multi-gate check engine
**Goal:** Replace the single `exit==0`-on-latest-evidence policy with a declarative, content-bound, multi-gate engine that aggregates over the proposed snapshot's **full** evidence set, so a green check means "the required, named verifications passed on THIS exact tree."

**Features**
- Declarative check spec per intent/proposal (e.g. requires `cargo test` AND `cargo clippy` to pass on the proposed snapshot), replacing the 24-line `forge_policy::evaluate` (lib.rs:9-24).
- Aggregate evidence over the proposed snapshot, not the single latest row, closing the gap where `run -- echo ok` flips a failing gate green (latest_evidence_for_attempt lib.rs:1471-1496).
- Bind each gate to a specific command identity (path + args, executable hash where available) so a gate names *which* command must pass — defeating the `run -- true` bypass.
- Move snapshot-staleness out of `record_check` into the policy crate so forge-policy is the single source of truth.
- Emit per-gate results (passed/failed/missing/stale) in check JSON; default evidence to REQUIRED for accept (configurable), resolving PRD §26.4 and preventing the §23.12 collapse to diff-only review.

**Depends on:** Phase 2
**Why it beats git:** git + external CI has no first-class, queryable link between a change, the declared verifications required, and a gate decision tied to that exact content — that lives in CI dashboards and PR comments and cannot model evidence staleness or required-gate composition.
**Why it's agent-native:** for an agent to be trusted to self-select, the gate must be un-gameable *by the agent itself*; today any zero-exit command satisfies any intent.
**Honesty note:** this phase delivers a declarative, content-bound, multi-gate engine — but it is **not yet tamper-proof**. Until Phase 5's hash-chaining, anyone with the DB can edit `exit_code 7→0` and the gate re-evaluates green; strong anti-gaming awaits Phase 5 hash-chaining and Phase 9 signing (mirroring PRD §17.1's own concession). The phase claims "content-bound and un-bypassable-by-trivial-command," **not** "un-gameable."
**Effort:** L · **Risk:** over-designing the check-spec into a mini-language (ship a minimal, declarative-but-not-Turing-complete list of command-gates + snapshot-freshness).
**Exit criteria:** a multi-command check fails unless all pass on the proposed snapshot; `run -- true` cannot satisfy any non-trivial gate and failing-test-then-`echo ok` doesn't flip it green; check JSON reports per-gate verdicts; accept requires evidence by default; the policy engine is the single staleness source.

> **Why the reword (vs original "un-gameable"):** the critique is right that the original's headline claim is hollow for one full phase since evidence is plain mutable SQLite until Phase 5. The dependency direction (4 then 5) stays — tamper-evidence is only worth building once a real gate consumes structured results — but the marketing is corrected.

### Phase 5 — Tamper-evident, structured evidence with an actor/identity model
**Goal:** Make evidence trustworthy and comparable and provenance attributable — the integrity model the trust thesis rests on.

**Features**
- Content hash per evidence row (over command, exit_code, stdout/stderr excerpts, timing, snapshot_id) chained into the append-only operations spine, so any post-hoc edit is detectable; replace the hardcoded `trust='locally_observed'` string with a real, verifiable claim tied to how the hash was produced.
- Structured result parsers extracting machine-readable outcomes (test pass/fail counts, lint/clippy findings, coverage) alongside the bounded 4096-byte excerpt; these become the inputs Phase 4 gates evaluate (e.g. "tests: 0 failing").
- Actor/author/decider identity model on attempts, decisions, publications (no actor column exists today) so "an agent proposed, a human accepted" is representable and auditable.
- **Full redaction hardening (corpus work):** detect bare high-entropy tokens, JSON-embedded secrets, PEM bodies, `user:pass@host` URLs before persistence (beyond today's line-oriented key=value, forge-content lib.rs:57-95); every redaction surfaces as a Phase 2 `warnings[]` entry. (The default-deny export *gate* already shipped in Phase 2; this hardens *what* it catches.)
- Hash-chaining preserves the Phase 1 CAS concurrency model; audited crypto/hash crates only (custom crypto banned). Full signing deferred to Phase 9.

**Depends on:** Phase 2, Phase 4
**Why it beats git:** git signing is binary and content-blind; Forge offers a trust ladder (self_reported → locally_observed → locally_signed → attested) bound to exact content and exit codes, plus structured results — git has no concept of evidence.
**Why it's agent-native:** for an agent to be trusted to self-select, a buggy/adversarial agent (or anyone with the DB) must not be able to flip `exit_code` and have the gate re-evaluate green; a reviewing agent must reason over structured outcomes, not a 4KB excerpt.
**Effort:** L · **Risk:** hash-chaining must not break the CAS model; redaction risks false negatives (leaks) and false positives (over-redaction) — use a leak corpus + graceful-degrade-to-raw-excerpt fallback; structured parsers are tool-specific (keep pluggable).
**Exit criteria:** editing any evidence/decision row is detected by `doctor` and causes check re-evaluation to refuse the forged result; every row carries a verifiable hash + real trust claim; a structured gate ("0 failing tests") passes/fails on parsed counts; decisions/publications carry an attributable actor; the hardened redactor catches bare tokens/JSON/PEM/credential-URLs in the corpus.

### Phase 6 — Evidence-based attempt comparison, ranking, and provenance-carrying publication
**Goal:** Ship the headline differentiator — a first-class compare/rank API returning competing attempts' diffs, per-gate outcomes, and structured metrics so a human or agent selects a winner from verified data — and carry a verifiable ledger digest into the published git artifact.

**Features**
- `forge attempt compare`/`forge compare` + a store-level ranking API returning, per attempt: changed paths/diff summary, per-gate check results, structured metrics (test/lint/coverage) — replacing list_attempts' id/intent/base_head/status-only output (lib.rs:1355-1379).
- Deterministic default ranking (all-required-gates-passing first, then structured metrics) **plus** raw per-attempt evidence (ranking advisory, evidence authoritative); stable JSON keyed by `attempt_id`/`proposal_id` so an agent can chain compare → accept headlessly.
- Content-level diff between competing proposals at file/hunk granularity, **produced via the git adapter at this stage** (native diff deferred to Phase 8).
- Replace the constant commit message "Forge accepted proposal" with a structured trailer/note carrying `proposal_id`, `proposal_revision_id`, a content-addressed evidence digest, the deciding actor, and gate outcomes (content-git create_branch_from_git_tree lib.rs:108-111, main.rs:548); a verification step recomputes the digest from the local ledger and confirms the published trailer matches.
- Feed the comparison into PR-body generation (fixing single-latest-evidence under-reporting, pr_body_for lib.rs:1139-1160); export stays one-way to a local git branch — no remote here.

**Depends on:** Phase 3 (write-binding), Phase 5 (structured tamper-evident evidence)
**Why it beats git:** git has no concept of "competing solutions to one intent," and review is diff-centric with identity an unverified `user.name` string. **Scope honestly: at Phase 6 this is local compare/rank + a self-verifying local trailer — NOT cross-machine provenance** (ledger sync is Phase 9). The differentiator is real and single-machine; team-scale trust is explicitly a Phase 9 property.
**Why it's agent-native:** the literal definition of agent self-selection — spawn N attempts, verify each under un-gameable gates, pick the best on tamper-evident evidence with no branch-juggling — the wedge the assessment flags as having "no API at all" today.
**Effort:** L · **Risk:** ranking quality depends entirely on Phase 4/5 richness (always return raw evidence alongside the score; keep the default ranking simple and explainable). The trailer is a self-verifying pointer, not full cross-machine transport, until Phase 9.
**Exit criteria:** an e2e test creates 2+ rival attempts, verifies each, calls compare, and asserts per-attempt diffs + per-gate results + structured metrics + a deterministic ranking; the ranked winner exports headlessly; published commits carry a structured trailer/note that a verification step recomputes and confirms; the PR body cites the competing attempts against the declared intent.

> **Positioning checkpoint:** the honest near-term claim — *an agent-native, trust-anchored review/evidence ledger that interoperates with git* — is **fully delivered at the end of Phase 6**. Everything after is the substrate rewrite that earns the word "alternative."

### Phase 7 — Reverse the git leak: native worktree walker, ignore engine, commit/DAG/refs (+ `forge undo`)
**Goal:** Cut the native backend's dependency on the git binary and give Forge its own history primitives, so the "native" path is real independence and the substrate later VCS primitives operate on exists without git.

**Features**
- Native worktree walker + ignore engine (audited gitignore/path-walk crate) replacing `git ls-files`/`--exclude-standard` shell-outs (forge-content-native lib.rs:427-437), honoring `.gitignore` + `.forgeignore` with documented precedence and preserving `is_secret_risk_path` exclusion exactly (no secret-hygiene regression).
- Native changed-paths computation replacing `git diff --name-only` (lib.rs:449) by diffing prior snapshot tree against the walked tree; make `base_head` backend-agnostic (anchor native repos on a native tree/snapshot id, main.rs:251-258).
- Native Commit/Change `ObjectKind` (today only Blob+Tree) — content-addressed, domain-separated, referencing tree + parent(s) + the proposal_revision/decision/intent that justified it + an evidence digest — making history intent-aware from the first commit; plus a native ref store under `.forge`.
- Navigation through the JSON contract: native `log`, checkout of any historical commit, and **`forge undo`/op-restore surfacing the existing operations/views op-log (001_init.sql:15-48)** — a strong unused seed available today.
- Symlink support (mode 120000) so worktrees round-trip (lib.rs:260-265, 414-417); store each object's kind in its own header to kill the `all_object_ids` double-hash scan (lib.rs:202-207); demote git export to one optional interop adapter.
- Differential test harness asserting the native walker's snapshot set equals the prior git-based set across a corpus before the git calls are deleted.

**Depends on:** Phase 2 (Commit ObjectKind migration), Phase 3 (clean `ContentBackend` boundary). **Not** Phase 6 — see note.
**Why it beats git:** this is where Forge stops being "a git wrapper with evidence sidecars" — its own ignore engine, walker, commit/ref objects, and op-log-backed undo mean the local lifecycle no longer needs the git binary, and intent-aware history nodes carry *why* (intent + evidence + decider) git's commit graph cannot represent.
**Why it's agent-native:** an agent can query "show every change under this intent and the evidence that justified it" from navigable content-addressed history; cutting the hidden git dependency removes a class of environment-dependent, non-deterministic failures hostile to reproducible runs.
**Effort:** XL · **Risk:** the riskiest reversal and first greenfield VCS primitive; a correct ignore engine is deceptively hard and subtle divergence could leak a `.env` or drop a tracked file (differential harness against `git ls-files` before deleting git calls). The commit-object schema is a near-permanent format commitment (reuse the `f1:` versioned tag + a hash-registry/migration table from day one). Stage internally: walker+ignore first, then commit/ref objects.
**Exit criteria:** a native-backend repo completes init → save → run → propose → check → restore, walks its own history, checks out any past commit, and `forge undo` restores a prior operation — **all with git removed from PATH**; the differential test proves snapshot-set equality (including secret-risk exclusion); no `git ls-files`/`git diff` in native paths (grep); symlinks and object-kind headers round-trip; the DAG has no cycles/dangling parents (doctor); git export still works as interop, not a core dependency.

> **Why I keep Phase 7 as a single phase depending on Phase 2/3 (and drop the false Phase 6 edge):** the critique correctly notes the original's Phase 6→7 dependency is sequencing-by-movement, not a technical edge — the walker depends only on durability, the migration framework, and the `ContentBackend` boundary. **I removed the false technical dependency on Phase 6.** I did **not** split the walker and the commit/DAG into parallel tracks as the critique suggested, and this is a deliberate disagreement (see below): the value-weighting "finish and prove the wedge before opening an XL greenfield front" is a real resourcing decision, not a falsehood, and splitting it invites two half-finished fronts. The sequencing is explicit, not encoded as a fake edge.

### Phase 8 — Native diff + intent-aware 3-way merge, operational scale, and physical attempt isolation (completed)
**Goal:** Build the convergence primitives git has and Forge lacks, make the native store survive a heavy agent fleet, and land the deferred physical per-attempt workspaces here where GC can reclaim them.

**Status:** Completed in June 2026 through the Phase 8 slice plans now archived under `docs/plans/completed/`, including conflict-as-data/native merge resolution, real GC plus physical attempt workspaces, and pack/index/retention scale work.

**Features**
- Native content diff (working vs snapshot, snapshot vs snapshot, base vs proposal) at hunk/line granularity with rename detection, exposed through the JSON contract — replacing the Phase 6 git-adapter diff and removing that interop dependency from the core review path.
- 3-way merge engine taking base/ours/theirs trees, producing a merged tree, representing conflicts as first-class re-resolvable typed JSON per PRD §15 (content/binary/rename/dir_file/mode/symlink) — finally *writing into* the `conflict_sets` table beyond Phase 2's stale-base metadata, with a full conflict object an agent reads, resolves via the contract, recorded as evidence and re-checked.
- Intent-aware/evidence-aware resolution: rank/suggest auto-resolutions using Phase 4/5 results and intent scope — shipped as an explicitly-gated, evidence-backed **suggestion** (never silent) layered on conflict-as-data.
- **Physical per-attempt workspaces** (worktree directories under `.forge/`) with non-destructive, concurrency-safe switching — *the work deferred out of Phase 3*, landed here because it depends on the GC/retention that reclaims it. Surface the per-attempt workspace path in attempt show/list JSON.
- Real mark-sweep GC replacing the dry-run-only stub that hard-bails (main.rs:504-505, gc_dry_run lib.rs:1177-1208): reachability from refs + reachable snapshots/proposal_revisions/decisions with a reflog-style safety window, dry-run preview, `--yes` (CONFIRMATION_REQUIRED) for real deletion gated on `doctor` passing.
- Packfile/delta/compression (audited crate; custom compression banned), large-file/streaming to replace whole-file `fs::read` (lib.rs:311,343), a **working-tree index/status cache** for fast diff/status at scale, and a retention policy + storage budget (PRD §10.7).

**Depends on:** Phase 7
**Why it beats git:** brings Forge to git's storage economics and goes beyond git on merge — conflicts as typed JSON an agent resolves programmatically, plus merge ranking using captured intent + evidence to choose among competing attempts; converts the dead `conflict_sets` schema into the headline merge differentiator.
**Why it's agent-native:** closes a loop git cannot (agent receives conflict → proposes resolution → records resolution as evidence → check re-evaluates); autonomous fleets generate orders of magnitude more speculative states than human committers, so real GC + retention + packing is what makes cheap parallel attempts deployable over time.
**Effort:** XL · **Risk:** the riskiest leap — a correct 3-way merge with rename/binary/mode/symlink handling is where most VCS projects stall for years, and intent-aware auto-resolution could produce confidently-wrong merges (ship conflict-as-data + manual/agent resolution FIRST; auto-resolution is a gated, never-silent suggestion). Deleting GC is dangerous — a reachability bug or too-tight window can destroy referenced content including in-flight attempts and the new per-attempt worktrees (long protection window, mandatory dry-run diff, crash-safe deletion ordering, gate on doctor). Packing must not break `f1:` verify-on-read.
**Exit criteria:** native diff is correct with rename detection on a corpus; a 3-way merge of two attempts writes real `conflict_sets`/`PathConflict` rows with correct typed kinds, an agent resolves a conflict object via the contract and it is recorded as evidence and re-checked, symlinks/mode bits round-trip losslessly with crash-atomic restore; `forge gc` reclaims only unreachable objects (reachability fuzz) and never deletes anything reachable from a ref/recent op; the packed+compressed store is measurably smaller than loose and than git; a multi-GB file snapshots/restores without OOM; status/diff stay fast on a large synthetic tree via the index; verify-on-read passes through the pack layer.

### Phase 9 — Distributed sync + tamper-evident provenance: native wire protocol and the signed trust ladder
**Goal:** Deliver the defining distributed property absent today and seal the trust model: a Forge-native protocol exchanging native history, the object store, **and the evidence/decision/op-log ledger**, plus cryptographic signing and the PRD trust ladder.

**Status:** In progress. Local signing, `doctor` verification, the signed Git export bridge, local key status/rotation, versioned `forge-sync` v1 manifest export/inspect/import, and path/file/SSH/HTTPS native clone/fetch/pull/push with ledger-row exchange are complete. True remote-boundary sync conflicts now surface as Phase 8 `native_merge` conflict-as-data instead of thin stale-base records, and clean divergent peer sync now records native merge commits at the receiver boundary. Local trust policy can enforce every PRD trust-ladder rung; `locally_signed` is backed by local signatures, hosted-runner policies can be satisfied by explicit hosted-runner Ed25519 attestations over proposal evidence, and `third_party_attested` can be satisfied by explicit third-party issuer attestations. A focused release litmus now simulates two isolated no-git machines and verifies clone/fetch/pull/push convergence plus conflict-as-data.

**Features**
- Wire protocol with object enumeration + want/have negotiation (or op-log sync) over ssh/https, implementing the stubbed `forge-sync` crate; clone/fetch/push/pull through the JSON contract with the same `--request-id` idempotency and typed-error discipline; protocol versioned from v1, shipping a minimal full-transfer clone before incremental fetch negotiation.
- Sync the **ledger** not just content: intent, attempts, evidence, check results, decisions, and the op-log travel with the objects (today none crosses the boundary), so a reviewing agent/CI bot on another machine receives "did THIS exact tree pass THESE checks, decided by whom" as verifiable provenance.
- Conflict/merge-queue semantics at the remote boundary: re-validate and merge an incoming proposal against a moved target using the Phase 8 engine, surfacing conflict-as-data rather than STALE_BASE-and-bail; git export demoted to optional interop beside the native push.
- Cryptographic signing of native commit objects, decisions, and evidence (audited crypto crate; custom crypto banned), promoting the Phase 5 hash-chain to full signed attestation and realizing the trust ladder self_reported → locally_observed → locally_signed → hosted_runner_signed → third_party_attested as enforced, verifiable claims.
- Policy enforcement on trust level (accept/publish may require ≥ a configured tier), anti-gaming binding of checks to executable hashes + clean-tree state (PRD §17.1) made verifiable, a signed trailer/note at the git-export bridge, and `doctor`/`verify` validating signatures + the hash-chain with explicit "self-signed is not third-party-attested" labeling.

**Depends on:** Phase 8 (merge engine + transferable pack format), Phase 5 (hash-chain), Phase 7 (commit objects)
**Why it beats git:** git transfers commits/trees/blobs and signs binary, commit-only; Forge transfers the full justification graph — intent, content-bound evidence, check state, decision, decider identity — and signs it with a graduated, queryable trust ladder. This is the point where PRD §24.1 ("more than a folder of logs plus git branches") is demonstrably met.
**Why it's agent-native:** an agent's value is the evidence ledger; today it dies in a local SQLite file and a hand-waved PR body. Syncing the ledger lets a reviewing agent on another machine act on verifiable provenance; the actor model makes cross-machine collaboration auditable; signed attestation is what finally makes an autonomous accept/publish decision trustworthy at team scale.
**Effort:** XL · **Risk:** the single largest leap — a correct, secure wire protocol (negotiation, partial/resumable transfer, auth, backpressure, version negotiation) is a category larger than everything before combined; protocol mistakes are extremely costly once peers depend on them (version from v1; full-transfer clone before incremental). Key management/identity binding (where keys live, agent-vs-human identity, rotation, revocation) is the hard part of signing; getting attestation semantics wrong gives false confidence (bind signatures to Phase 5 hashable content; keep the ladder explicit in every envelope; defer hosted/third-party tiers behind clear labeling).
**Sequencing note (signing can land first, internally):** signing depends only on Phase 5 + Phase 7's commit objects + Phase 8's pack-for-transfer — not on the merge engine. Land and locally verify signing via `doctor`/`verify` **before** the wire protocol exists, tightening the trust story earlier. I keep this as one phase with an explicit internal order rather than two numbered phases (deliberate disagreement, below).
**Exit criteria:** two machines with **no git installed** clone/fetch/push/pull and reach byte-identical object stores **and** ledgers with consistent refs; an incoming proposal conflicting with a moved target is merged or surfaced as conflict-as-data; native commits/decisions/evidence are signed and verified by doctor/verify; the trust ladder is enforced (a policy requiring ≥locally_signed rejects self_reported at accept); a git-export consumer verifies a signed trailer back to the ledger; the full litmus test (init → commit history → branch → merge with conflict resolution → clone/push to another machine, all without git) passes end-to-end.

---

## What we deliberately defer (and why)

- **Physical per-attempt worktrees → Phase 8 (not a Phase 3 precondition).** The project explicitly scoped the wedge as single-checkout proposal-level competition and explicitly deferred per-attempt workspace directories; compare/rank works on DB-bound `content_refs` regardless. Building them as a Phase 3 blocker would be XL YAGNI starving the highest-value phase. They land in Phase 8 where GC reclaims them.
- **The full entropy/PEM/JSON redactor → Phase 5.** The cheap launch-blocking *default-deny export gate* ships in Phase 2; the heavier detection corpus is real depth, not a gate, and rides with the evidence-integrity work.
- **The 3-way merge engine → Phase 8.** Only conflict-set *metadata* persistence is a v0 launch blocker (Phase 2); the merge engine itself depends on native history and is correctly the riskiest late phase.
- **Native diff → Phase 8.** Phase 6 uses the pragmatic git-adapter diff; native diff removes that interop dependency but isn't needed for the initial compare surface (rename detection in particular is not needed for ranking).
- **Cross-machine provenance / ledger sync → Phase 9.** Phase 6's trailer is a self-verifying local pointer; team-scale trust requires the wire protocol and is honestly gated to the end.
- **Blame/annotate, bisect, and lifecycle hooks** — VCS table-stakes flagged as missing. **Blame is deferred to a Phase 8+ follow-on** (intent-aware blame — "which intent/attempt/evidence last touched this line" — is a natural agent-native leapfrog once native history and diff exist, but it is additive, not on the trust/wedge critical path). **Bisect is deferred to a post-Phase-7 follow-on** (trivially script-driven over native history; low priority). **Hooks/lifecycle extensibility** is partially served by the Phase 4 check engine; a general hook-dispatch mechanism is deferred but flagged as **load-bearing for the hosted/policy-enforcement future** — it should be added as an explicit Phase 4.5 or Phase 9 companion once the hosted direction is committed, not left implicit. **Stash/shelve** is acceptably implicit in attempts+snapshots.

## The single most important thing to build next

**Phase 1a — the minimum substrate the wedge needs: the swallowed-fsync durability fix, WAL + busy_timeout concurrency, collision-safe ULIDs, and request_id-scoped domain-row idempotency.** It is the only phase that depends on nothing, it is cheap (M, not XL), and it is the precondition for everything: trust is the product, and a system of record that can return `Ok` and then lose the object a ref points at, hard-fail the second concurrent agent, mint colliding IDs in the exact parallel loop that is the v0 wedge, or silently double-insert on a retried `save` is not trustworthy at the most basic level. Crucially, splitting the original monolithic Phase 1 means 1a unblocks the wedge (Phases 3–6) *immediately* while the heavier hardening (1b: store-before-DB ordering, advisory lock, crash-injection harness) proceeds in parallel — so correctness comes first without the foundation devouring the runway and starving the differentiator. Bank the cheapest substrate wins, then fund the wedge.
