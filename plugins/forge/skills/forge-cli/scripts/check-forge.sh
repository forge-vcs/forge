#!/usr/bin/env bash
set -euo pipefail

if ! command -v forge >/dev/null 2>&1; then
  cat <<'EOF'
forge is not installed or is not on PATH.

Install the current release candidate with:
  cargo install --git https://github.com/freezscholte/forge --tag v0.1.0-rc3 forge-cli
EOF
  exit 1
fi

echo "forge: $(command -v forge)"

if forge schema --json >/dev/null 2>&1; then
  echo "forge schema: ok"
else
  echo "forge schema: unavailable or failed" >&2
  exit 2
fi
