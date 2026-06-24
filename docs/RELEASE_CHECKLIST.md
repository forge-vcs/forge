# Public Release Checklist

Use this checklist before making the repository public or pushing a release tag.

## Repository State

- `main` is clean and synced with `origin/main`.
- `README.md` describes the current native sync, trust, merge, and storage
  surface.
- `LICENSE` exists and matches workspace package metadata.
- `RELEASE_NOTES.md` describes the release claim and current boundaries.
- `docs/P9_RELEASE_AUDIT.md` maps every Phase 9 exit criterion to executable
  evidence.
- No local `dogfood/*` branches are deleted as part of release prep.

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

## Release Boundary

The public release may claim local/native Forge is release-candidate complete.
Do not claim hosted collaboration, global identity, certificate authority,
revocation, organization policy management, or resumable transport.

## Tagging

After the release gate and GitHub `verify` are green:

```bash
git checkout main
git pull --ff-only origin main
git tag -a v0.1.0-rc5 -m "Forge v0.1.0-rc5"
git push origin v0.1.0-rc5
```

## Publish

- Confirm the pushed tag resolves on GitHub.
- Confirm README, LICENSE, release notes, and P9 audit render correctly.
- Only then switch repository visibility to public.
