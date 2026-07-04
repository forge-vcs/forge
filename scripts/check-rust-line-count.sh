#!/usr/bin/env bash
# Enforce ADR-0001's Rust source file-size ceiling without pretending the
# remaining pre-existing monoliths have already been split.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

max_lines=3000

allowed_cap() {
  # Known exceptions are allowed to shrink, but not grow. Remove entries as
  # domain-split slices bring them under the global ceiling.
  case "$1" in
    crates/forge-store/src/lib.rs) echo 11776 ;;
    crates/forge-content-native/src/lib.rs) echo 4721 ;;
    crates/forge-cli/tests/forge_sync.rs) echo 4683 ;;
    *) echo "" ;;
  esac
}

failed=0
while IFS= read -r file; do
  lines=$(wc -l < "$file" | tr -d ' ')
  if (( lines <= max_lines )); then
    continue
  fi

  allowed=$(allowed_cap "$file")
  if [[ -n "$allowed" ]]; then
    if (( lines > allowed )); then
      printf 'line-count: %s has %d lines; allowlisted cap is %d\n' "$file" "$lines" "$allowed" >&2
      failed=1
    fi
    continue
  fi

  printf 'line-count: %s has %d lines; max is %d. Split by domain or add an ADR-backed allowlist entry.\n' \
    "$file" "$lines" "$max_lines" >&2
  failed=1
done < <(find crates -type f -name '*.rs' | sort)

if (( failed != 0 )); then
  exit 1
fi

echo "rust line-count check passed"
