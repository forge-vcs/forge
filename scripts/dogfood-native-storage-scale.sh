#!/usr/bin/env bash
# Storage-scale dogfood for native Forge content storage.
#
# Usage:
#   bash scripts/dogfood-native-storage-scale.sh --smoke
#   bash scripts/dogfood-native-storage-scale.sh
# Keep repo/logs: KEEP_DOGFOOD=1 bash scripts/dogfood-native-storage-scale.sh --smoke

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
FORGE="$ROOT/target/debug/forge"
MODE="${1:---full}"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/forge-storage-dogfood.XXXXXX")"
OUT="$TMP/out.json"
ERR="$TMP/err.txt"
PASS=0
FAIL=0
declare -a FAILS=()

if [ "${KEEP_DOGFOOD:-0}" != "1" ]; then
  trap 'rm -rf "$TMP"' EXIT
else
  trap 'echo "kept dogfood repo at: '"$TMP"'"' EXIT
fi

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

ck() {
  local desc="$1"
  local actual="$2"
  local expected="$3"
  if [ "$actual" = "$expected" ]; then
    PASS=$((PASS + 1))
    printf '  \033[32mOK\033[0m %s\n' "$desc"
  else
    FAIL=$((FAIL + 1))
    FAILS+=("$desc -- got [$actual] want [$expected]")
    printf '  \033[31mFAIL\033[0m %s -- got [%s] want [%s]\n' "$desc" "$actual" "$expected"
  fi
}

ck_int_gt() {
  local desc="$1"
  local actual="$2"
  local floor="$3"
  if [ "$actual" -gt "$floor" ]; then
    PASS=$((PASS + 1))
    printf '  \033[32mOK\033[0m %s\n' "$desc"
  else
    FAIL=$((FAIL + 1))
    FAILS+=("$desc -- got [$actual] want > [$floor]")
    printf '  \033[31mFAIL\033[0m %s -- got [%s] want > [%s]\n' "$desc" "$actual" "$floor"
  fi
}

pg() {
  python3 -c "import json,sys
try:
    d=json.load(open(sys.argv[1]))
    print(eval(sys.argv[2]))
except Exception as e:
    print('<ERR:%s>' % e)" "$OUT" "$1"
}

F() {
  "$FORGE" --json "$@" >"$OUT" 2>"$ERR"
}

make_repo() {
  mkdir -p "$TMP/repo"
  cd "$TMP/repo"
  git init -q
  git config user.email dogfood@example.test
  git config user.name "Forge Storage Dogfood"
  printf 'native storage dogfood\n' > README.md
  git add README.md
  git commit -qm "initial"
}

write_corpus_revision() {
  local revision="$1"
  local files="$2"
  local bytes_per_file="$3"
  python3 - "$revision" "$files" "$bytes_per_file" <<'PY'
import sys
from pathlib import Path

revision = int(sys.argv[1])
files = int(sys.argv[2])
bytes_per_file = int(sys.argv[3])
root = Path("corpus")
root.mkdir(exist_ok=True)
base = ("forge native storage dogfood repeated payload\n" * 64).encode()
for i in range(files):
    payload = (base + f"file={i % 24:04d}\nrev={revision % 4:04d}\n".encode())
    repeats = (bytes_per_file // len(payload)) + 1
    data = (payload * repeats)[:bytes_per_file]
    path = root / f"group-{i % 12:02d}" / f"file-{i:05d}.txt"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
PY
}

set_storage_budget_one_byte() {
  python3 - <<'PY'
import sqlite3
conn = sqlite3.connect(".forge/forge.db")
conn.execute("UPDATE storage_policy SET storage_budget_bytes = 1 WHERE singleton = 1")
conn.commit()
conn.close()
PY
}

warning_contains_storage_budget() {
  python3 - "$OUT" <<'PY'
import json
import sys
d = json.load(open(sys.argv[1]))
warnings = d.get("warnings") or []
print(any("storage budget exceeded" in warning for warning in warnings))
PY
}

need git
need python3

case "$MODE" in
  --smoke)
    SNAPSHOTS="${DOGFOOD_SNAPSHOTS:-8}"
    FILES="${DOGFOOD_FILES:-120}"
    BYTES_PER_FILE="${DOGFOOD_BYTES_PER_FILE:-2048}"
    ;;
  --full)
    SNAPSHOTS="${DOGFOOD_SNAPSHOTS:-20}"
    FILES="${DOGFOOD_FILES:-500}"
    BYTES_PER_FILE="${DOGFOOD_BYTES_PER_FILE:-4096}"
    ;;
  *)
    echo "usage: $0 [--smoke]" >&2
    exit 2
    ;;
esac

echo "=== Building forge (debug) ==="
cargo build -q --bin forge
echo "binary: $FORGE"

echo
echo "=== Native storage-scale dogfood ($MODE) ==="
make_repo
F init --content-backend native
ck "native init succeeds" "$(pg "d['status']")" "success"
F start "storage scale dogfood"
ck "native start succeeds" "$(pg "d['status']")" "success"

latest_snapshot=""
latest_content_ref=""
for revision in $(seq 1 "$SNAPSHOTS"); do
  write_corpus_revision "$revision" "$FILES" "$BYTES_PER_FILE"
  F save
  ck "snapshot $revision saves" "$(pg "d['status']")" "success"
  latest_snapshot="$(pg "d['data']['snapshot_id']")"
  latest_content_ref="$(pg "d['data']['content_ref']")"
done
ck_int_gt "created many snapshots" "$SNAPSHOTS" 3
ck "latest content ref is native" "${latest_content_ref%%:*}" "forge-tree"

F run -- true
ck "native run succeeds before pack gc" "$(pg "d['data']['exit_code']")" "0"
F propose
proposal_id="$(pg "d['data']['proposal_id']")"
ck "proposal succeeds before pack gc" "$(pg "d['status']")" "success"
F check --proposal "$proposal_id"
ck "check passes before pack gc" "$(pg "d['data']['status']")" "passed"
F accept --proposal "$proposal_id"
ck "accept succeeds before pack gc" "$(pg "d['status']")" "success"

find .forge/objects/sha256 -type f -exec touch -t 202001010000 {} +
F gc --dry-run
ck "gc dry-run succeeds" "$(pg "d['status']")" "success"
pack_candidates="$(pg "len(d['data']['pack_candidate_native_objects'])")"
ck_int_gt "gc finds pack candidates" "$pack_candidates" 0
digest="$(pg "d['data']['plan_digest']")"
F gc --yes --plan-digest "$digest"
ck "confirmed gc succeeds" "$(pg "d['status']")" "success"
ck_int_gt "confirmed gc creates packs" "$(pg "len(d['data']['created_packs'])")" 0
ck_int_gt "confirmed gc deletes loose duplicates" "$(pg "len(d['data']['deleted'])")" 0

F gc --dry-run
ck "post-gc dry-run succeeds" "$(pg "d['status']")" "success"
ck "post-gc has no loose duplicates" "$(pg "len(d['data']['loose_duplicate_native_objects'])")" "0"

F restore "$latest_snapshot" --yes
ck "restore reads latest snapshot after loose duplicate deletion" "$(pg "d['status']")" "success"
F diff --working --to "$latest_content_ref"
ck "working diff reads packed snapshot cleanly" "$(pg "len(d['data']['files'])")" "0"
F doctor
ck "doctor remains clean after pack gc" "$(pg "d['data']['ok']")" "True"
ck "doctor reports no pack issues" "$(pg "len(d['data']['native_pack_issues'])")" "0"
ck_int_gt "doctor reports pack bytes" "$(pg "d['data']['storage']['packs']['bytes']")" 0

set_storage_budget_one_byte
F start "storage pressure warning"
ck "low-budget mutating command still succeeds" "$(pg "d['status']")" "success"
ck "storage budget warning is visible" "$(warning_contains_storage_budget)" "True"

echo
echo "=== RESULT ==="
echo "PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf '%s\n' "${FAILS[@]}" >&2
  exit 1
fi
echo "Native storage-scale dogfood checks passed."
