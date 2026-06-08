#!/usr/bin/env bash
# Benchmark native Forge storage/accounting surfaces on deterministic temporary repos.
#
# Usage:
#   bash scripts/bench-native-storage.sh --smoke
#   bash scripts/bench-native-storage.sh --full
#   bash scripts/bench-native-storage.sh --large-tree-smoke
#   bash scripts/bench-native-storage.sh --large-file-smoke

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
FORGE="$ROOT/target/debug/forge"
MODE="${1:---smoke}"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/forge-native-storage-bench.XXXXXX")"
OUT="$TMP/out.json"
ERR="$TMP/err.txt"

if [ "${KEEP_BENCH:-0}" != "1" ]; then
  trap 'rm -rf "$TMP"' EXIT
else
  trap 'echo "kept benchmark repo at: '"$TMP"'"' EXIT
fi

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

du_bytes() {
  python3 - "$1" <<'PY'
import os
import sys
from pathlib import Path

root = Path(sys.argv[1])
total = 0
if root.exists():
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for name in filenames:
            path = Path(dirpath) / name
            try:
                total += path.lstat().st_size
            except FileNotFoundError:
                pass
print(total)
PY
}

json_get() {
  python3 - "$OUT" "$1" <<'PY'
import json
import sys

data = json.load(open(sys.argv[1]))
value = data
for part in sys.argv[2].split("."):
    value = value[part]
print(value)
PY
}

measure() {
  local label="$1"
  shift
  python3 - "$label" "$@" <<'PY'
import subprocess
import sys
import time

label = sys.argv[1]
cmd = sys.argv[2:]
start = time.perf_counter_ns()
subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
elapsed_ms = (time.perf_counter_ns() - start) // 1_000_000
print(f"{label}_ms={elapsed_ms}")
PY
}

make_repo() {
  local dir="$1"
  local files="$2"
  local bytes_per_file="$3"
  mkdir -p "$dir"
  cd "$dir"
  git init -q
  git config user.email bench@example.test
  git config user.name "Forge Bench"
  python3 - "$files" "$bytes_per_file" <<'PY'
import sys
from pathlib import Path

files = int(sys.argv[1])
bytes_per_file = int(sys.argv[2])
root = Path("corpus")
root.mkdir()
base = ("forge native storage benchmark repeated payload\n" * 128).encode()
for i in range(files):
    payload = (base + f"file={i % 32:04d}\n".encode())
    repeats = (bytes_per_file // len(payload)) + 1
    data = (payload * repeats)[:bytes_per_file]
    path = root / f"group-{i % 16:02d}" / f"file-{i:05d}.txt"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
PY
  git add corpus
  git commit -qm "benchmark corpus"
  git gc -q
}

run_storage_case() {
  local files="$1"
  local bytes_per_file="$2"
  local repo="$TMP/storage"
  make_repo "$repo" "$files" "$bytes_per_file"

  "$FORGE" --json init --content-backend native >"$OUT" 2>"$ERR"
  "$FORGE" --json start "native storage benchmark" >"$OUT" 2>"$ERR"
  measure "native_save" "$FORGE" --json save
  "$FORGE" --json save >"$OUT" 2>"$ERR"
  local content_ref
  local snapshot_id
  content_ref="$(json_get data.content_ref)"
  snapshot_id="$(json_get data.snapshot_id)"
  measure "native_restore_clean" "$FORGE" --json restore "$snapshot_id" --yes

  python3 - <<'PY'
from pathlib import Path
p = Path("corpus/group-00/file-00000.txt")
p.write_text(p.read_text() + "small edit for working diff\n")
PY
  measure "native_diff_working_one_file" "$FORGE" --json diff --working --to "$content_ref"

  "$FORGE" --json doctor >"$OUT" 2>"$ERR"
  echo "mode=$MODE"
  echo "repo=$repo"
  echo "corpus_files=$files"
  echo "corpus_payload_bytes=$((files * bytes_per_file))"
  echo "forge_bytes=$(du_bytes .forge)"
  echo "git_object_bytes=$(du_bytes .git/objects)"
  echo "storage_total_bytes=$(json_get data.storage.total_bytes)"
  echo "storage_loose_object_bytes=$(json_get data.storage.loose_objects.bytes)"
  echo "storage_loose_object_files=$(json_get data.storage.loose_objects.files)"
  echo "storage_pack_bytes=$(json_get data.storage.packs.bytes)"
  echo "storage_database_bytes=$(json_get data.storage.database.bytes)"
  echo "storage_temp_bytes=$(json_get data.storage.temp.bytes)"
  echo "storage_worktree_bytes=$(json_get data.storage.worktrees.bytes)"
  echo "storage_evidence_output_bytes=$(json_get data.storage.evidence_outputs.bytes)"
}

need git
need python3

cd "$ROOT"
cargo build -q --bin forge

case "$MODE" in
  --smoke)
    run_storage_case 80 4096
    ;;
  --full)
    run_storage_case 520 102400
    ;;
  --large-tree-smoke)
    run_storage_case 1000 256
    ;;
  --large-file-smoke)
    run_storage_case 4 1048576
    ;;
  *)
    echo "usage: $0 [--smoke|--full|--large-tree-smoke|--large-file-smoke]" >&2
    exit 2
    ;;
esac
