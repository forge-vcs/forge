#!/usr/bin/env bash
# Arm A: one fresh headless session per task, brief-only, randomized order.
# Usage: run-arm-a.sh <scratch-dir> [task-id ...]   (default: ORDER-A.txt)
set -uo pipefail
SCRATCH="${1:?usage: run-arm-a.sh <scratch-dir> [tasks...]}"; shift || true
CCX="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLONE="$SCRATCH/pilot-a"
RUNS="$CCX/runs"
mkdir -p "$RUNS"

TASKS=("$@")
[[ ${#TASKS[@]} -eq 0 ]] && mapfile -t TASKS < "$CCX/runs/ORDER-A.txt"

for task in "${TASKS[@]}"; do
  out="$RUNS/A-$task"; mkdir -p "$out"
  echo "=== ARM A :: $task :: $(date +%H:%M:%S)"
  git -C "$CLONE" reset --hard --quiet pilot-run
  git -C "$CLONE" clean -fdq -e target

  {
    "$CCX/brief.sh" "task-$task.yaml"
    cat << 'EOF'

--- TASK INSTRUCTION ---
Implement exactly the task specified in the TASK CONTRACT above, in this
repository (you are at the repo root). Rules:
- Touch only paths inside the contract's allowed_changes.
- If the brief does not license a decision you need to make, STOP: write
  UNKNOWN.md at the repo root (what you need to know, why the brief does
  not answer it, your best guess of kind: blocking/assumption/observation,
  file:line evidence) and end without making further edits.
- When done, run the contract's acceptance commands and make them pass.
- Do not create git commits; leave all changes uncommitted in the worktree.
- Do not read or write anything outside this repository.
EOF
  } > "$out/prompt.txt"

  start=$(date +%s)
  (cd "$CLONE" && claude -p "$(cat "$out/prompt.txt")" \
      --output-format json --dangerously-skip-permissions \
      > "$out/result.json" 2> "$out/stderr.log")
  status=$?
  end=$(date +%s)
  echo "$status $((end-start))s" > "$out/exit-and-seconds.txt"

  git -C "$CLONE" add -A
  git -C "$CLONE" diff --cached > "$out/patch.diff"
  [[ -f "$CLONE/UNKNOWN.md" ]] && cp "$CLONE/UNKNOWN.md" "$out/UNKNOWN.md"
  echo "    exit=$status wall=$((end-start))s patch=$(wc -l < "$out/patch.diff") lines"
done
echo "ARM A COMPLETE $(date +%H:%M:%S)"
