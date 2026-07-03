---
date: 2026-07-03
ticket: NER-359
feature: local-review-surface
status: passed
repo: /Users/skolte/Github-Private/forge-dogfood
dogfood_head: 6b24be5
candidate_binary: /Users/skolte/Github-Private/forge/target/debug/forge
---

# NER-359 Local Review Surface Dogfood

## Summary

Dogfood passed against a temporary clone of the real `forge-dogfood` app at
`6b24be5` using the locally built NER-359 candidate binary.

The run created a real Forge proposal for a tracked README change, captured the
dogfood app's typecheck, test, build, and lint commands as evidence, checked the
proposal, reviewed it through `forge review show`, exported static HTML, ran
`forge review open --no-browser`, and completed the decision through
`forge accept`.

## Scenario Covered

- Installed dogfood dependencies with `npm ci` in the temporary clone.
- Initialized Forge in the dogfood app.
- Started a proposal with gates for:
  - `npm run typecheck`
  - `npm test`
  - `npm run build`
  - `npm run lint`
- Made a tracked README change and saved it.
- Captured all four dogfood commands with `forge run`.
- Proposed and checked the change.
- Ran `forge review show --proposal <proposal-id>`.
- Ran `forge review export --proposal <proposal-id> --output review.html`.
- Ran `forge review open --proposal <proposal-id> --output review-open.html --no-browser`.
- Accepted the proposal through `forge accept --proposal <proposal-id>`.

## Evidence

```text
proposal=proposal_019f2880785775628d6a15aa212fe435
npm run typecheck: success, exit=0
npm test: success, exit=0
npm run build: success, exit=0
npm run lint: success, exit=0
forge check: passed
forge review show: readiness=ready
forge review show: handoffs=2
forge review export: readiness=ready
forge review open --no-browser: opened=false, warning=browser launch skipped by --no-browser
forge accept: decision=accepted
```

`forge review show` reported these readiness factors:

```text
check_passed: latest check passed
trust_policy_met: latest evidence trust `locally_observed` satisfies accept policy `self_reported`
```

The generated review page was a local static HTML file and the browser handoff
path was non-mutating. The final trust-bearing action remained the terminal
`forge accept` command.

## Notes

This dogfood proves the local read-only review path on the real dogfood app. It
does not claim hosted accounts, hosted comments, cloud execution, or
browser-triggered accept/reject/reveal/publish/export actions.
