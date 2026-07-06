#!/usr/bin/env bash
# Arm A rerun pipeline (protocol amendment P1, 2026-07-06): dependent tasks
# run on stacked bases. Each spec is "task=stackrun1,stackrun2,..." where
# stackruns are runs/ dirs whose patch.diff is applied + committed first.
# Outputs land in runs/A-<task>-r2/. Aborts if a stack patch fails to apply
# or a run files UNKNOWN (the chain would be built on sand).
set -uo pipefail
SCRATCH="${1:?usage: run-arm-a-stacked.sh <scratch-dir> task=stack,... ...}"; shift
CCX="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLONE="$SCRATCH/pilot-a"
RUNS="$CCX/runs"

for spec in "$@"; do
  task="${spec%%=*}"
  stack="${spec#*=}"; [[ "$stack" == "$task" ]] && stack=""
  out="$RUNS/A-$task-r2"; mkdir -p "$out"
  echo "=== ARM A r2 :: $task :: stack[$stack] :: $(date +%H:%M:%S)"

  git -C "$CLONE" reset --hard --quiet pilot-run
  git -C "$CLONE" clean -fdq -e target
  # Stack commits go on a detached HEAD so the pilot-run base never moves.
  git -C "$CLONE" checkout --quiet --detach pilot-run

  if [[ -n "$stack" ]]; then
    IFS=',' read -ra PARTS <<< "$stack"
    for part in "${PARTS[@]}"; do
      if ! git -C "$CLONE" apply --index --3way "$RUNS/$part/patch.diff"; then
        echo "FATAL: stack patch $part failed to apply for $task"; exit 1
      fi
    done
    git -C "$CLONE" commit --quiet -m "pilot stack: $stack"
  fi

  contract=$(ls "$CCX/contracts/task-$task-"*.yaml 2>/dev/null | head -1)
  if [[ -z "$contract" ]] || ! "$CCX/brief.sh" "$contract" > "$out/brief.txt"; then
    echo "FATAL: no brief for task $task"; exit 1
  fi
  if ! grep -q "NEIGHBOR CONTRACT (normative)" "$out/brief.txt" && grep -q "ccx-task" <(grep -A5 '^neighbors:' "$contract"); then
    echo "WARN: brief for $task resolved no neighbor contracts"
  fi

  {
    cat "$out/brief.txt"
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
  (cd "$CLONE" && claude -p \
      --output-format json --dangerously-skip-permissions \
      < "$out/prompt.txt" > "$out/result.json" 2> "$out/stderr.log")
  status=$?
  end=$(date +%s)
  echo "$status $((end-start))s" > "$out/exit-and-seconds.txt"

  git -C "$CLONE" add -A
  git -C "$CLONE" diff --cached > "$out/patch.diff"
  if [[ -f "$CLONE/UNKNOWN.md" ]]; then
    cp "$CLONE/UNKNOWN.md" "$out/UNKNOWN.md"
    echo "HALT: $task filed UNKNOWN — chain stops here for author triage"
    exit 2
  fi
  echo "    exit=$status wall=$((end-start))s patch=$(wc -l < "$out/patch.diff") lines"
done
echo "ARM A r2 COMPLETE $(date +%H:%M:%S)"
