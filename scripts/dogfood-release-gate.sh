#!/usr/bin/env bash
# Aggregate release dogfood gate for the native Forge/P9 surface.
#
# Usage: bash scripts/dogfood-release-gate.sh
# Set FORGE_RELEASE_SKIP_WORKSPACE_TESTS=1 to skip the slow full workspace test
# when iterating locally; release runs should leave it unset.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if command -v rtk >/dev/null 2>&1; then
  RUN=(rtk)
else
  RUN=()
fi

step() {
  printf '\n=== %s ===\n' "$*"
}

run() {
  "${RUN[@]}" "$@"
}

cd "$ROOT"

step "format"
run cargo fmt --all --check

step "clippy"
run cargo clippy --workspace --all-targets -- -D warnings

if [ "${FORGE_RELEASE_SKIP_WORKSPACE_TESTS:-0}" != "1" ]; then
  step "workspace tests"
  run cargo test --workspace
else
  step "workspace tests skipped"
fi

step "binary e2e"
run bash scripts/e2e-eval.sh

step "hosted-runner and third-party attestation dogfood"
run bash scripts/dogfood-hosted-runner-attestation.sh

step "native sync release litmus"
run bash scripts/dogfood-native-sync-release-litmus.sh

step "native peer sync"
run bash scripts/dogfood-native-sync-peer.sh

step "native peer sync without git"
run bash scripts/dogfood-native-sync-peer-nogit.sh

step "TypeScript native dogfood"
run bash scripts/dogfood-typescript-native.sh

step "native storage scale smoke"
run bash scripts/dogfood-native-storage-scale.sh --smoke

printf '\nRelease dogfood gate passed.\n'
