# Public Release Checklist

Use this checklist before making the repository public or pushing a release tag.

## Repository State

- `main` is clean and synced with `origin/main`.
- `README.md` describes the current native sync, trust, merge, and storage
  surface.
- `LICENSE` exists and matches workspace package metadata.
- `RELEASE_NOTES.md` has a top entry for the exact release tag being prepared,
  including the install command, release validation evidence, and current
  boundaries.
- `docs/P9_RELEASE_AUDIT.md` maps every Phase 9 exit criterion to executable
  evidence.
- No local `dogfood/*` branches are deleted as part of release prep.

## Release Documentation

Before tagging, update and review these files together:

- `README.md` install commands name the release tag.
- `RELEASE_NOTES.md` top entry names the release tag and matches the GitHub
  release body.
- `docs/P9_RELEASE_AUDIT.md` names the audited commit, release tag, and latest
  gate evidence.
- `docs/RELEASE_CHECKLIST.md` tag examples name the release tag.

## Verification

Run:

```bash
rtk bash scripts/dogfood-release-gate.sh
```

Expected evidence:

- workspace tests pass
- e2e eval reports `PASS=95 FAIL=0`
- hosted/third-party attestation dogfood reports `PASS=26 FAIL=0`
- native sync release litmus reports `PASS=32 FAIL=0`
- native peer sync reports `PASS=26 FAIL=0`
- native no-git peer sync reports `PASS=26 FAIL=0`
- TypeScript native dogfood reports `PASS=44 FAIL=0`
- native storage-scale smoke reports `PASS=30 FAIL=0`

## Feature-Specific Validation

For any release that includes a new feature or changed user-visible behavior,
validate that feature explicitly before tagging. The aggregate release gate is a
regression backstop; it is not enough unless it directly exercises the new
behavior.

Record the feature-specific evidence in `RELEASE_NOTES.md` or
`docs/P9_RELEASE_AUDIT.md`:

- feature or behavior changed
- focused tests, e2e scripts, or manual dogfood scenarios run
- expected user-visible outcome
- important negative or boundary cases
- result and remaining known limits

## External Dogfood

Before tagging, run a full product dogfood pass in
`/Users/skolte/Github-Private/forge-dogfood` against the exact candidate Forge
binary from current `main`.

For feature-bearing releases, this dogfood pass must include at least one
scenario that uses the release's headline feature. A generic
`start/save/run/propose/check/accept` lifecycle pass is useful but does not, by
itself, validate an unrelated new feature.

Required setup:

```bash
cargo install --path /Users/skolte/Github-Private/forge/crates/forge-cli --root "$TMP_FORGE_INSTALL"
cd /Users/skolte/Github-Private/forge-dogfood
PATH="$TMP_FORGE_INSTALL/bin:$PATH" forge --version
```

Required checks:

```bash
npm run typecheck
npm test
npm run build
npm run lint
```

Then execute a real Forge workflow from `docs/DOGFOOD_PLAN.md` using the
candidate binary:

- `forge init` or `forge doctor`, depending on the current dogfood repo state.
- `forge start` with the dogfood check commands as requirements.
- Make a small user-visible app or documentation change.
- `forge save`.
- `forge run -- npm run typecheck`.
- `forge run -- npm test`.
- `forge run -- npm run build`.
- `forge run -- npm run lint`.
- `forge propose --summary ...`.
- `forge check`.
- `forge accept`.

Record the candidate commit, Forge binary source, commands, pass/fail results,
and any friction in `RELEASE_NOTES.md` or `docs/P9_RELEASE_AUDIT.md`. Do not
tag if this external dogfood pass fails or exposes release-blocking friction.

## Release Boundary

The public release may claim local/native Forge is release-candidate complete.
Do not claim hosted collaboration, global identity, certificate authority,
revocation, organization policy management, or resumable transport.

## Tagging

After release docs are updated on `main`, the release gate plus GitHub `verify`
are green, and the external dogfood pass is complete:

```bash
git checkout main
git pull --ff-only origin main
git tag -a v0.1.0-rc6 -m "Forge v0.1.0-rc6"
git push origin v0.1.0-rc6
```

## Publish

- Confirm the pushed tag resolves on GitHub.
- Confirm README, LICENSE, release notes, and P9 audit render correctly.
- Only then switch repository visibility to public.
