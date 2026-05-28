# Code reviews

Triaged outputs from the `/ce-code-review` gate (see `CLAUDE.md` § Engineering workflow). This is the pre-PR review record; GitHub Actions CI and any post-open reviewer are separate passes that do not replace this gate.

## Convention

One file per review: `<YYYY-MM-DD>-<short-name>.md` (or `<YYYY-MM-DD>-pr-<NNN>-<short-branch>.md` for a PR). Each file pins the reviewed range and classifies findings:

- `base-sha` / `head-sha` — the exact diff reviewed.
- **real-actionable** — fix before merge.
- **defer-able** — file as a **Forge** Linear ticket, or track in `docs/ROADMAP.md` / a plan's open-questions section.
- **defense-in-depth** — optional hardening, not blocking.
- **reviewed-and-rejected** — load-bearing: lets `ce-learnings-researcher` skip re-flagging known noise on future runs.

Single-reviewer findings (especially from the adversarial persona) are more variable than cross-corroborated ones — triage with skepticism.
