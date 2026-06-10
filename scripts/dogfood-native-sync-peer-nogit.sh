#!/usr/bin/env bash
# Focused dogfood harness for native path-peer sync with git removed from PATH.
#
# It creates plain, non-git directories, initializes native Forge repositories,
# and runs clone/fetch/pull/push while each Forge invocation sees a PATH that
# contains `sh` but no `git`. This pins the Phase 9 release criterion that native
# peer sync is not secretly depending on the git binary.
#
# Usage:  bash scripts/dogfood-native-sync-peer-nogit.sh
# Keep repo/logs: KEEP_DOGFOOD=1 bash scripts/dogfood-native-sync-peer-nogit.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FORGE="$ROOT/target/debug/forge"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/forge-sync-peer-nogit.XXXXXX")"
OUT="$TMP/out.json"
ERR="$TMP/err.txt"
NOGIT_BIN="$TMP/nogit-bin"
PASS=0
FAIL=0
declare -a FAILS=()

if [ "${KEEP_DOGFOOD:-0}" != "1" ]; then
  trap 'rm -rf "$TMP"' EXIT
else
  trap 'echo "kept no-git dogfood repos at: '"$TMP"'"' EXIT
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

ck_prefix() {
  local desc="$1"
  local actual="$2"
  local prefix="$3"
  case "$actual" in
    "$prefix"*)
      PASS=$((PASS + 1))
      printf '  \033[32mOK\033[0m %s\n' "$desc"
      ;;
    *)
      FAIL=$((FAIL + 1))
      FAILS+=("$desc -- got [$actual] want prefix [$prefix]")
      printf '  \033[31mFAIL\033[0m %s -- got [%s] want prefix [%s]\n' "$desc" "$actual" "$prefix"
      ;;
  esac
}

pg() {
  python3 -c "import json,sys
try:
    d=json.load(open(sys.argv[1]))
    print(eval(sys.argv[2]))
except Exception as e:
    print('<ERR:%s>' % e)" "$OUT" "$1"
}

NG() {
  PATH="$NOGIT_BIN" "$FORGE" --json "$@" >"$OUT" 2>"$ERR"
}

native_init_and_accept() {
  local dir="$1"
  local intent="$2"
  local path="$3"
  local contents="$4"
  cd "$dir"
  NG start "$intent" --require "sh -c true"
  printf '%b' "$contents" >"$path"
  NG save
  NG run -- sh -c true
  NG propose
  NG check
  NG accept
}

base_source() {
  local dir="$1"
  mkdir -p "$dir"
  cd "$dir"
  printf 'native peer sync no-git dogfood\n' > README.md
  NG init --content-backend native
  native_init_and_accept "$dir" "base native sync state" "base.txt" "base\n"
}

clone_peer() {
  local bundle="$1"
  local dir="$2"
  mkdir -p "$dir"
  cd "$dir"
  NG sync clone "$bundle"
}

export_bundle() {
  local dir="$1"
  local bundle="$2"
  cd "$dir"
  NG sync export --output "$bundle"
}

conflict_count() {
  local dir="$1"
  cd "$dir"
  NG conflict list
  pg "len(d['data']['conflicts'])"
}

native_head() {
  local dir="$1"
  cd "$dir"
  NG sync export --output "$TMP/head.json"
  pg "d['data']['native_head']"
}

need cargo
need python3

if [ ! -x /bin/sh ]; then
  echo "missing /bin/sh; cannot build no-git PATH harness" >&2
  exit 1
fi
mkdir -p "$NOGIT_BIN"
ln -sf /bin/sh "$NOGIT_BIN/sh"

echo "=== Building forge (debug) ==="
(cd "$ROOT" && cargo build -q --bin forge)
echo "binary: $FORGE"

echo
echo "=== Native peer sync dogfood with git removed from PATH ==="

SOURCE="$TMP/source"
BASE_BUNDLE="$TMP/base.json"
base_source "$SOURCE"
ck "source init uses native backend without git" "$(pg "d['status']")" "success"
export_bundle "$SOURCE" "$BASE_BUNDLE"
ck "base export without git" "$(pg "d['status']")" "success"
native_init_and_accept "$SOURCE" "source fast-forward side" "source-ff.txt" "source\n"

FETCH_PEER="$TMP/fetch-peer"
clone_peer "$BASE_BUNDLE" "$FETCH_PEER"
ck "clone without git" "$(pg "d['status']")" "success"
cd "$FETCH_PEER"
NG sync fetch "$SOURCE"
FETCH_HEAD="$(pg "d['data']['remote_native_head']")"
ck "fast-forward fetch without git" "$(pg "d['status']")" "success"
ck "fast-forward fetch not materialized" "$(pg "d['data']['materialized']")" "False"
ck_prefix "fast-forward fetch remote head" "$FETCH_HEAD" "f1:commit:"
ck "fast-forward fetch leaves source file unmaterialized" "$([ ! -e "$FETCH_PEER/source-ff.txt" ] && echo yes || echo no)" "yes"
ck "fast-forward fetch advances native head" "$(native_head "$FETCH_PEER")" "$FETCH_HEAD"

CLEAN_SOURCE="$TMP/clean-source"
CLEAN_BUNDLE="$TMP/clean-base.json"
base_source "$CLEAN_SOURCE"
export_bundle "$CLEAN_SOURCE" "$CLEAN_BUNDLE"
native_init_and_accept "$CLEAN_SOURCE" "source clean side" "source-only.txt" "source\n"

PULL_PEER="$TMP/pull-peer"
clone_peer "$CLEAN_BUNDLE" "$PULL_PEER"
native_init_and_accept "$PULL_PEER" "peer clean pull side" "peer-only.txt" "peer\n"
cd "$PULL_PEER"
NG sync pull "$CLEAN_SOURCE"
PULL_MERGE="$(pg "d['data']['merge_commit_id']")"
ck "clean pull without git" "$(pg "d['status']")" "success"
ck "clean pull merged" "$(pg "d['data']['merged']")" "True"
ck "clean pull materialized" "$(pg "d['data']['materialized']")" "True"
ck_prefix "clean pull merge commit id" "$PULL_MERGE" "f1:commit:"
ck "clean pull materializes source file" "$(cat "$PULL_PEER/source-only.txt")" "source"
ck "clean pull preserves peer file" "$(cat "$PULL_PEER/peer-only.txt")" "peer"
NG doctor
ck "doctor after clean pull without git" "$(pg "d['data']['ok']")" "True"

PUSH_PEER="$TMP/push-peer"
clone_peer "$CLEAN_BUNDLE" "$PUSH_PEER"
native_init_and_accept "$PUSH_PEER" "peer clean push side" "peer-only.txt" "peer\n"
cd "$PUSH_PEER"
NG sync push "$CLEAN_SOURCE"
PUSH_MERGE="$(pg "d['data']['merge_commit_id']")"
ck "clean push without git" "$(pg "d['status']")" "success"
ck "clean push merged" "$(pg "d['data']['merged']")" "True"
ck "clean push not materialized remotely" "$(pg "d['data']['materialized']")" "False"
ck_prefix "clean push merge commit id" "$PUSH_MERGE" "f1:commit:"
ck "clean push leaves remote peer file unmaterialized" "$([ ! -e "$CLEAN_SOURCE/peer-only.txt" ] && echo yes || echo no)" "yes"
cd "$CLEAN_SOURCE"
NG doctor
ck "doctor after clean push remote without git" "$(pg "d['data']['ok']")" "True"
ck "clean push remote head survives reconcile without git" "$(native_head "$CLEAN_SOURCE")" "$PUSH_MERGE"

CONFLICT_SOURCE="$TMP/conflict-source"
CONFLICT_BUNDLE="$TMP/conflict-base.json"
base_source "$CONFLICT_SOURCE"
export_bundle "$CONFLICT_SOURCE" "$CONFLICT_BUNDLE"
native_init_and_accept "$CONFLICT_SOURCE" "source conflict side" "shared.txt" "source\n"
CONFLICT_PEER="$TMP/conflict-peer"
clone_peer "$CONFLICT_BUNDLE" "$CONFLICT_PEER"
native_init_and_accept "$CONFLICT_PEER" "peer conflict side" "shared.txt" "peer\n"
cd "$CONFLICT_PEER"
NG sync fetch "$CONFLICT_SOURCE"
ck "conflict fetch without git" "$(pg "d['status']")" "success"
ck "conflict fetch merged false" "$(pg "d['data']['merged']")" "False"
ck "conflict fetch records one conflict" "$(conflict_count "$CONFLICT_PEER")" "1"
NG doctor
ck "doctor after conflict fetch without git" "$(pg "d['data']['ok']")" "True"

echo
echo "Native no-git peer sync dogfood: PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failures:\n' >&2
  printf ' - %s\n' "${FAILS[@]}" >&2
  exit 1
fi

echo "Native no-git peer sync dogfood checks passed."
