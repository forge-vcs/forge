# UNKNOWN — ccx-task-362-4-ledger-enrichment (revision 1)

kind: blocking

## What I need to know

The contract requires enriching the output of `forge blame` (both the
`--json` payload lines and the human rendering), but **no blame command
exists anywhere in this worktree**. The blame command is the deliverable of
the neighbor task `ccx-task-362-3-cli-blame`, whose contract is explicitly
marked MISSING in the brief with the instruction "surface as unknown, do
not guess".

To integrate `provenance_detail` into blame output I would need, at
minimum, from the missing neighbor contract (or its landed code):

1. Whether `forge blame` is expected to already exist in this worktree when
   this task runs, or whether the tasks are meant to land together (they
   currently do not: it is absent).
2. The blame data model: how each payload line carries the
   `(intent_id, proposal_revision_id, decision_id)` tuple that
   `provenance_detail` keys on, and the exact JSON field names already
   emitted under `forge.cli.v0` (additive-only means I must not collide
   with or reshape existing fields I cannot see).
3. Where the command lives (`crates/forge-cli/src/commands/` module name,
   `args.rs` subcommand shape) and what its human output format is, so
   "human output gains intent title when available" has a defined anchor.
4. The acceptance command `cargo test -p forge-cli blame` implies existing
   blame tests to extend; there are none (`crates/forge-cli/tests/` has no
   blame test file, and `grep -ri blame crates/` finds zero matches).

## Why the brief does not answer it

- The task contract's `interface` section describes blame enrichment only
  as a delta over an existing command ("payload lines **gain** OPTIONAL
  (additive) fields", "Human output **gains** intent title"), never
  defining the base command.
- The neighbor slot that would define it reads:
  `NEIGHBOR CONTRACT MISSING: ccx-task-362-3-cli-blame (surface as
  unknown, do not guess)`.
- Implementing `forge blame` myself would duplicate the neighbor task's
  scope and force unlicensed decisions (CLI surface, JSON line schema,
  how blame maps native history commits to ledger tuples), violating both
  the unknown_rule and the additive-only JSON rule (I cannot be additive
  against a schema I cannot see).

## Evidence

- `grep -ri "blame" crates/ --include="*.rs"` → no matches (whole
  workspace).
- `crates/forge-cli/src/commands/mod.rs:1-3` — only `core`, `export`,
  `sync` modules; no blame module.
- `crates/forge-cli/src/args.rs`, `crates/forge-cli/src/main.rs` — no
  blame subcommand (grep: zero matches).
- `crates/forge-cli/tests/` directory listing — no blame integration test
  file for `cargo test -p forge-cli blame` to select.
- Brief, neighbors section: `ccx-task-362-3-cli-blame` listed; its
  contract block is the literal "NEIGHBOR CONTRACT MISSING" marker.

## Best guess (not acted on)

The store-side half of the task (`crates/forge-store/src/provenance.rs`
with `ProvenanceDetail` / `provenance_detail()`) is fully specified and
independent of the missing neighbor; it could be implemented and tested
via `cargo test -p forge-store provenance` as written. My best guess is
that task 362-3 was intended to land first (or in the same batch) and this
worktree predates it. But the task instruction says to stop and make no
further edits once an unknown is hit, and the CLI half plus one of the
three acceptance commands cannot be satisfied without guessing the
neighbor's interface — so I have made no code changes.
