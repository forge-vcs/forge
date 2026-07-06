#!/usr/bin/env bash
# Arm B (status quo, pinned): ONE continuous session per ticket working its
# tasks sequentially (claude -c continues the same conversation). Inputs:
# full CLAUDE.md, ticket text, task title+definition (no contracts), repo
# search. Branch reset between tasks, same capture as arm A.
# Usage: run-arm-b.sh <scratch-dir> <ticket> <task-id ...>
set -uo pipefail
SCRATCH="${1:?usage}"; TICKET="${2:?ticket}"; shift 2
CCX="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLONE="$SCRATCH/pilot-b"
RUNS="$CCX/runs"
mkdir -p "$RUNS"

first=1
STACKED=""
for task in "$@"; do
  out="$RUNS/B-$task"; mkdir -p "$out"
  echo "=== ARM B :: $TICKET :: $task :: $(date +%H:%M:%S)"
  git -C "$CLONE" reset --hard --quiet pilot-run
  git -C "$CLONE" clean -fdq -e target
  git -C "$CLONE" checkout --quiet --detach pilot-run
  # P1 stacking: this arm's own prior task patches form the base.
  for prev in $STACKED; do
    git -C "$CLONE" apply --index --3way "$RUNS/$prev/patch.diff" || { echo "FATAL: stack $prev failed"; exit 1; }
  done
  [[ -n "$STACKED" ]] && git -C "$CLONE" commit --quiet -m "pilot stack: $STACKED"

  {
    if [[ $first -eq 1 ]]; then
      echo "You are working through ticket $TICKET in this repository, task by task. The full ticket:"
      echo
      cat "$CCX/tickets/$TICKET.md"
      echo
    fi
    echo "--- CURRENT TASK ---"
    grep -A2 "^- \*\*$task" "$CCX/tickets/tasks.md"
    cat << 'EOF'

Rules for this task:
- Implement only this task now (later tasks come next in this session).
- The worktree already contains your previous tasks' changes, applied and
  committed; build this task on top of them.
- Run the repo's verify gates for what you build and make them pass.
- Do not create git commits; leave changes uncommitted.
EOF
  } > "$out/prompt.txt"

  start=$(date +%s)
  if [[ $first -eq 1 ]]; then
    (cd "$CLONE" && claude -p \
        --output-format json --dangerously-skip-permissions \
        < "$out/prompt.txt" > "$out/result.json" 2> "$out/stderr.log")
  else
    (cd "$CLONE" && claude -c -p \
        --output-format json --dangerously-skip-permissions \
        < "$out/prompt.txt" > "$out/result.json" 2> "$out/stderr.log")
  fi
  status=$?
  end=$(date +%s)
  echo "$status $((end-start))s" > "$out/exit-and-seconds.txt"
  first=0

  git -C "$CLONE" add -A
  git -C "$CLONE" diff --cached > "$out/patch.diff"
  echo "    exit=$status wall=$((end-start))s patch=$(wc -l < "$out/patch.diff") lines"
  STACKED="$STACKED B-$task"
done
echo "ARM B $TICKET COMPLETE $(date +%H:%M:%S)"
