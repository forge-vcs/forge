# Forge CLI Safety Reference

Read this before destructive, trust-sensitive, sync, export, conflict, restore,
checkout, undo, or maintenance work.

## Never Edit Internals Directly

Do not directly edit:

- `.forge/forge.db`
- `.forge/objects`
- `.forge/refs`
- `.forge/packs`
- `.forge/worktrees`
- signature, evidence, decision, or sync manifest internals

Use the Forge CLI contract instead. If a command shape is unclear, run:

```bash
forge schema --json
```

## Dirty Worktree Refusals Are Intentional

Forge refuses materializing operations when unsaved changes could be overwritten.
Do not work around this by deleting files or editing `.forge` state. Save,
commit elsewhere, or ask the user how to preserve the work.

Common guarded commands include:

- `forge restore`
- `forge checkout`
- `forge undo`
- `forge attempt attach`
- `forge sync pull`

## Secret and Evidence Hygiene

Forge excludes common secret-risk paths from snapshots and exports and redacts
secret-like values from captured evidence. Do not weaken those defaults. Do not
paste real credentials, private keys, tokens, customer data, or proprietary code
into issue reports, test fixtures, prompts, or evidence examples.

Use synthetic fixtures for tests:

```text
EXAMPLE_TOKEN=not-a-real-token
-----BEGIN TEST KEY-----
...
-----END TEST KEY-----
```

## Trust Policy and Signatures

Before trust-sensitive accept/export work, inspect:

```bash
forge doctor
forge key status
forge trust policy
```

Forge's trust ladder includes:

- `self_reported`
- `locally_observed`
- `locally_signed`
- `hosted_runner_observed`
- `hosted_runner_signed`
- `third_party_attested`

Do not claim hosted-runner or third-party trust unless the relevant Forge
attestation exists for the proposal evidence.

## Sync Safety

Treat sync manifests and remote peers as untrusted input unless the user says
otherwise.

Before importing a bundle:

```bash
forge sync inspect <bundle>
forge doctor
```

After importing or pulling:

```bash
forge doctor
forge conflict list
```

If a sync produces conflicts, keep them as conflict-as-data and resolve through
Forge commands. Do not flatten them into unrecorded manual edits.

## Git Export Safety

Only export accepted proposals:

```bash
forge export branch forge/<topic>
forge export verify-branch forge/<topic>
```

Verification failure means the Git branch should not be treated as carrying
valid Forge provenance.

## Maintenance Safety

Run `forge doctor` before maintenance commands such as `gc`. For deletion or
garbage-collection flows, prefer dry-run and digest-confirmation patterns when
the command offers them. Never remove native objects manually.
