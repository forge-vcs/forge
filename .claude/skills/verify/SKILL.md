---
name: verify
description: Run the full Forge verification gate (fmt check, tests, clippy with -D warnings, and shell e2e acceptance) and report what passed or failed. Use before considering a change complete or before committing.
---

Run these four commands in order from the repository root. Run all four even if an earlier one fails, so the user sees the full picture.

```
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/e2e-eval.sh
```

Notes:
- `clippy` runs with `-D warnings`, so any warning is a failure.
- If `cargo fmt --all --check` reports diffs, offer to run `cargo fmt --all` to fix formatting.
- `scripts/e2e-eval.sh` exercises the shipped debug binary and catches shell acceptance drift, including migration-head checks that Rust tests may not cover.
- Report a concise pass/fail summary for each command. For any failure, surface the relevant compiler/clippy/test/e2e output so the user can act on it. Do not declare success unless all four pass.
