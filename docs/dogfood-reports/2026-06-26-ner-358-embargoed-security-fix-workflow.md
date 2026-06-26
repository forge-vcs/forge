---
date: 2026-06-26
ticket: NER-358
feature: embargoed-security-fix-workflow
status: passed
repo: /Users/skolte/Github-Private/forge-dogfood
candidate_binary: /Users/skolte/Github-Private/forge/target/debug/forge
run_root: /tmp/forge-dogfood-ner358.LXokXe
---

# NER-358 Embargoed Security-Fix Workflow Dogfood

## Summary

Dogfood passed against a detached temporary worktree of `forge-dogfood` at
`6b24be5` using the locally built NER-358 candidate binary.

The run used the real dogfood app and `npm test` as Forge evidence. The
temporary worktree linked the dogfood repo's existing `node_modules` so the test
runner resolved the same local dependencies as the main dogfood checkout.

## Scenario Covered

- Initialized Forge with the native backend.
- Started an embargoed security-fix attempt in the dogfood app.
- Modified `src/App.tsx`, saved, ran `npm test`, proposed, and checked.
- Verified legacy `visibility set --visibility embargoed` succeeds as an
  embargo workflow alias.
- Marked the proposal with `embargo mark` and verified generic
  `visibility grant` on embargoed work fails with `EMBARGO_WORKFLOW_REQUIRED`.
- Verified `embargo release` before accept fails with `EMBARGO_STATE_INVALID`
  and writes no release bundle.
- Granted `sync_materialize` to `release-bot@example.test`.
- Accepted the proposal under embargo.
- Verified `export branch` before publish fails with `EMBARGO_STATE_INVALID`.
- Verified `embargo release` with only `sync_materialize` fails with
  `VISIBILITY_POLICY_UNMET`, reports missing `publish_reveal`, and writes no
  release bundle.
- Granted `publish_reveal` to `release-bot@example.test`.
- Verified an occupied release output path fails before finalization, preserves
  the existing bytes, leaves no hidden pending output, keeps the workflow in
  `accepted_under_embargo`, and records no release event or release
  authorization.
- Ran `embargo release` and verified the release manifest uses
  `embargo-release.v1`, `embargo_release`, a bundle digest, and future-only
  revocation language.
- Verified the release event records the same bundle digest as the manifest.
- Verified the release manifest is metadata-only for now: no native head, no
  native objects, no native payloads, and no ledger rows.
- Verified `sync inspect` accepts the untampered release manifest.
- Verified `sync import` and `sync clone` refuse the release artifact instead of
  materializing it without local authority validation.
- Tampered the release recipient and verified `sync inspect` rejects the bundle
  with a bundle digest mismatch.
- Ran `embargo reveal` with `sanitized-source`.
- Ran `embargo publish`.
- Verified `export branch` after sanitized publish fails with
  `EMBARGO_STATE_INVALID`, `state=sanitized_source`, and
  `required=full_source`.
- Started a second proposal, revealed it with `full-source`, published it, and
  verified `export branch` succeeds after publish.
- Started a third proposal and verified `embargo close` makes later reveal fail
  with `EMBARGO_STATE_INVALID`.

## Evidence

```text
run_root=/tmp/forge-dogfood-ner358.LXokXe
Preparing worktree (detached HEAD 6b24be5)
HEAD is now at 6b24be5 fix: keep eslint scoped to dogfood source
worktree_head=6b24be5
ok: forge init --content-backend native
ok: forge start NER-358 sanitized embargo dogfood path
ok: forge save
ok: forge run -- npm test
ok: forge propose --summary Sanitized embargo dogfood path
ok: forge check
ok: forge visibility set --visibility embargoed
ok: forge embargo mark
expected EMBARGO_WORKFLOW_REQUIRED: forge visibility grant on embargoed proposal
expected EMBARGO_STATE_INVALID: forge embargo release before accept
ok: forge embargo grant --capability sync_materialize
ok: forge accept
expected EMBARGO_STATE_INVALID: forge export branch dogfood-sanitized-before-publish
expected VISIBILITY_POLICY_UNMET: forge embargo release with sync_materialize only
ok: forge embargo grant --capability publish_reveal
expected message contains sync export output already exists: forge embargo release to occupied path
ok: forge embargo release
ok: forge sync inspect embargo-release.json
expected message contains apply sync bundle: forge sync import embargo-release.json
expected message contains clone sync bundle: forge sync clone embargo-release.json
expected message bundle digest mismatch: forge sync inspect tampered-release.json
ok: forge embargo reveal --mode sanitized-source
ok: forge embargo publish
expected EMBARGO_STATE_INVALID: forge export branch dogfood-sanitized-after-publish
ok: forge start NER-358 full source embargo dogfood path
ok: forge save
ok: forge run -- npm test
ok: forge propose --summary Full source embargo dogfood path
ok: forge check
ok: forge embargo mark
ok: forge embargo grant --capability sync_materialize
ok: forge embargo grant --capability publish_reveal
ok: forge accept
ok: forge embargo release
ok: forge embargo reveal --mode full-source
ok: forge embargo publish
ok: forge export branch dogfood-full-source-after-publish
ok: forge start NER-358 closed embargo dogfood path
ok: forge save
ok: forge run -- npm test
ok: forge propose --summary Closed embargo dogfood path
ok: forge check
ok: forge embargo mark
ok: forge embargo close
expected EMBARGO_STATE_INVALID: forge embargo reveal after close
DOGFOOD PASS
```

## Notes

The real dogfood checkout remained clean. The temporary detached worktree was
left at the run root above for short-term inspection.
