# Solutions

Past problems and their fixes, so knowledge compounds instead of being re-solved.

`/ce-compound` writes a file here after anything non-obvious is solved. `ce-learnings-researcher` greps this folder during `/ce-plan`, `/ce-debug`, and `/ce-code-review`, so a documented fix here shapes future planning and review (and stops the same trap from being re-hit).

## Convention

One file per problem: `<YYYY-MM-DD>-<kebab-slug>.md`, optionally grouped in subfolders by class (e.g. `integration-issues/`, `conventions/`). Each file starts with YAML frontmatter so the researcher can match it:

```yaml
---
problem_type: bug | integration-issue | convention | performance
module: forge-store          # crate or area the problem lives in
tags: [sqlite, migrations]
symptoms: ["error text or observable behavior someone would search for"]
---
```

Then prose: what broke, the root cause, the fix, and how to avoid it next time. Keep it specific enough that a future session searching the symptom lands here.
