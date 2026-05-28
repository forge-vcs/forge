---
name: verify
description: Run the full Forge verification gate (fmt check, tests, clippy with -D warnings) and report what passed or failed. Use before considering a change complete or before committing.
---

Run these three commands in order from the repository root. Run all three even if an earlier one fails, so the user sees the full picture.

```
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Notes:
- `clippy` runs with `-D warnings`, so any warning is a failure.
- If `cargo fmt --all --check` reports diffs, offer to run `cargo fmt --all` to fix formatting.
- Report a concise pass/fail summary for each of the three. For any failure, surface the relevant compiler/clippy/test output so the user can act on it. Do not declare success unless all three pass.
