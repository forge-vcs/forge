#!/usr/bin/env bash
# One-time pilot environment setup: two clones (arm A stripped, arm B full),
# cold cargo build in each so run wall-clocks measure the agent, not the
# first compile.
set -euo pipefail
SCRATCH="${1:?usage: pilot-setup.sh <scratch-dir>}"
SRC=/Users/skolte/Github-Private/forge
BASE=6238c53

for arm in a b; do
  clone="$SCRATCH/pilot-$arm"
  if [[ ! -d "$clone/.git" ]]; then
    rm -rf "$clone"
    git clone --quiet "$SRC" "$clone"
  fi
  rm -rf "$clone/target"
  git -C "$clone" checkout --quiet "$BASE"
  git -C "$clone" branch -f pilot-run "$BASE"
  git -C "$clone" checkout --quiet pilot-run
  rm -f "$clone/.mcp.json"
done

# Arm A: CLAUDE.md stripped to mechanics (approved: branch/clone-only).
cat > "$SCRATCH/pilot-a/CLAUDE.md" << 'EOF'
# CLAUDE.md

Single Cargo workspace, Rust 1.92.0 (rust-toolchain.toml). The binary is
`forge` (crates/forge-cli). Library crates under crates/: forge-core,
forge-store (SQLite), forge-content, forge-content-git,
forge-content-native, forge-evidence, forge-policy, forge-protocol,
forge-export-git, forge-sync. Integration tests live in
crates/forge-cli/tests/ and use assert_cmd + tempfile against the compiled
binary in temp repos.

Verify before done:
- cargo fmt --all --check
- cargo test --workspace
- cargo clippy --workspace --all-targets -- -D warnings
EOF
git -C "$SCRATCH/pilot-a" add CLAUDE.md
git -C "$SCRATCH/pilot-a" commit --quiet -m "pilot: strip CLAUDE.md to mechanics (arm A)"
git -C "$SCRATCH/pilot-a" branch -f pilot-run HEAD

for arm in a b; do
  echo "=== cargo build pilot-$arm ($(date +%H:%M:%S))"
  (cd "$SCRATCH/pilot-$arm" && cargo build -p forge-cli 2>&1 | tail -1 && cargo test --workspace --no-run 2>&1 | tail -1)
done
echo "SETUP READY $(date +%H:%M:%S)"
