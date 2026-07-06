#!/usr/bin/env bash
# Mechanical acceptance verification (RUBRIC §4): for each run, rebuild its
# exact base (stack), apply its patch, run the task's acceptance commands
# fresh. Records PASS/FAIL per command in runs/<run>/verify.txt.
# Usage: verify-runs.sh <clone-dir> <spec>...   spec = run:stack1,stack2|-
set -uo pipefail
CLONE="${1:?clone}"; shift
CCX="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNS="$CCX/runs"

cmds_for() {
  case "$1" in
    362-1|362-2) echo "cargo test -p forge-content-native provenance::|cargo clippy -p forge-content-native --all-targets -- -D warnings" ;;
    362-3)       echo "cargo test -p forge-cli blame" ;;
    362-4)       echo "cargo test -p forge-store provenance|cargo test -p forge-cli blame" ;;
    362-5)       echo "cargo test -p forge-cli --test forge_blame" ;;
    382-1|382-3) echo "cargo test -p forge-cli --test forge_attempts" ;;
    382-2)       echo "cargo test -p forge-store|cargo test -p forge-cli --test forge_attempts" ;;
  esac
}

for spec in "$@"; do
  run="${spec%%:*}"; stack="${spec#*:}"; [[ "$stack" == "-" ]] && stack=""
  task=$(echo "$run" | sed -E 's/^[AB]-//; s/-r2$//')
  out="$RUNS/$run/verify.txt"; : > "$out"
  echo "=== VERIFY $run ($(date +%H:%M:%S))"
  git -C "$CLONE" reset --hard --quiet pilot-run
  git -C "$CLONE" clean -fdq -e target
  git -C "$CLONE" checkout --quiet --detach pilot-run
  ok=1
  if [[ -n "$stack" ]]; then
    IFS=',' read -ra PARTS <<< "$stack"
    for part in "${PARTS[@]}"; do
      git -C "$CLONE" apply --index --3way "$RUNS/$part/patch.diff" || { echo "STACK-FAIL $part" >> "$out"; ok=0; }
    done
  fi
  if [[ $ok -eq 1 ]]; then
    git -C "$CLONE" apply --index --3way "$RUNS/$run/patch.diff" || { echo "PATCH-FAIL" >> "$out"; ok=0; }
  fi
  if [[ $ok -eq 1 ]]; then
    IFS='|' read -ra CMDS <<< "$(cmds_for "$task")"
    for cmd in "${CMDS[@]}"; do
      if (cd "$CLONE" && eval "$cmd" > /dev/null 2>&1); then
        echo "PASS $cmd" >> "$out"
      else
        echo "FAIL $cmd" >> "$out"
      fi
    done
  fi
  cat "$out" | sed 's/^/    /'
done
echo "VERIFY BATCH COMPLETE $(date +%H:%M:%S)"
