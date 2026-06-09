#!/usr/bin/env bash
# Focused dogfood harness for native path-peer sync.
#
# It creates temporary native Forge repos, drives clean divergent fetch/pull/push
# through native merge commits, and verifies a true divergent edit still records
# conflict-as-data. No network access is required.
#
# Usage:  bash scripts/dogfood-native-sync-peer.sh
# Keep repo/logs: KEEP_DOGFOOD=1 bash scripts/dogfood-native-sync-peer.sh

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
FORGE="$ROOT/target/debug/forge"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/forge-sync-peer-dogfood.XXXXXX")"
OUT="$TMP/out.json"
ERR="$TMP/err.txt"
PASS=0
FAIL=0
declare -a FAILS=()

if [ "${KEEP_DOGFOOD:-0}" != "1" ]; then
  trap 'rm -rf "$TMP"' EXIT
else
  trap 'echo "kept dogfood repos at: '"$TMP"'"' EXIT
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

F() {
  "$FORGE" --json "$@" >"$OUT" 2>"$ERR"
}

make_git_repo() {
  local dir="$1"
  mkdir -p "$dir"
  cd "$dir"
  git init -q
  git config user.email dogfood@example.test
  git config user.name "Forge Sync Dogfood"
  printf 'native peer sync dogfood\n' > README.md
  git add README.md
  git commit -qm "initial"
}

native_init_and_accept() {
  local dir="$1"
  local intent="$2"
  local path="$3"
  local contents="$4"
  cd "$dir"
  F start "$intent" --require "sh -c true"
  printf '%b' "$contents" >"$path"
  F save
  F run -- sh -c true
  F propose
  F check
  F accept
}

base_source() {
  local dir="$1"
  make_git_repo "$dir"
  F init --content-backend native
  native_init_and_accept "$dir" "base native sync state" "base.txt" "base\n"
}

clone_peer() {
  local bundle="$1"
  local dir="$2"
  mkdir -p "$dir"
  cd "$dir"
  F sync clone "$bundle"
}

export_bundle() {
  local dir="$1"
  local bundle="$2"
  cd "$dir"
  F sync export --output "$bundle"
}

conflict_count() {
  local dir="$1"
  cd "$dir"
  F conflict list
  pg "len(d['data']['conflicts'])"
}

native_head() {
  local dir="$1"
  cd "$dir"
  F sync export --output "$TMP/head.json"
  pg "d['data']['native_head']"
}

need git
need python3

echo "=== Building forge (debug) ==="
cargo build -q --bin forge
echo "binary: $FORGE"

echo
echo "=== Native peer sync dogfood ==="

SOURCE="$TMP/source"
BASE_BUNDLE="$TMP/base.json"
base_source "$SOURCE"
export_bundle "$SOURCE" "$BASE_BUNDLE"
native_init_and_accept "$SOURCE" "source clean side" "source-only.txt" "source\n"

FETCH_PEER="$TMP/fetch-peer"
clone_peer "$BASE_BUNDLE" "$FETCH_PEER"
native_init_and_accept "$FETCH_PEER" "peer clean fetch side" "peer-only.txt" "peer\n"
cd "$FETCH_PEER"
F sync fetch "$SOURCE"
FETCH_MERGE="$(pg "d['data']['merge_commit_id']")"
ck "clean fetch status" "$(pg "d['status']")" "success"
ck "clean fetch merged" "$(pg "d['data']['merged']")" "True"
ck "clean fetch not materialized" "$(pg "d['data']['materialized']")" "False"
ck_prefix "clean fetch merge commit id" "$FETCH_MERGE" "f1:commit:"
ck "clean fetch conflict count" "$(conflict_count "$FETCH_PEER")" "0"
ck "clean fetch leaves source file unmaterialized" "$([ ! -e "$FETCH_PEER/source-only.txt" ] && echo yes || echo no)" "yes"
F doctor
ck "doctor after clean fetch" "$(pg "d['data']['ok']")" "True"
ck "clean fetch head survives reconcile" "$(native_head "$FETCH_PEER")" "$FETCH_MERGE"

PULL_PEER="$TMP/pull-peer"
clone_peer "$BASE_BUNDLE" "$PULL_PEER"
native_init_and_accept "$PULL_PEER" "peer clean pull side" "peer-only.txt" "peer\n"
cd "$PULL_PEER"
F sync pull "$SOURCE"
PULL_MERGE="$(pg "d['data']['merge_commit_id']")"
ck "clean pull status" "$(pg "d['status']")" "success"
ck "clean pull merged" "$(pg "d['data']['merged']")" "True"
ck "clean pull materialized" "$(pg "d['data']['materialized']")" "True"
ck_prefix "clean pull merge commit id" "$PULL_MERGE" "f1:commit:"
ck "clean pull materializes source file" "$(cat "$PULL_PEER/source-only.txt")" "source"
ck "clean pull preserves peer file" "$(cat "$PULL_PEER/peer-only.txt")" "peer"
F doctor
ck "doctor after clean pull" "$(pg "d['data']['ok']")" "True"

PUSH_PEER="$TMP/push-peer"
clone_peer "$BASE_BUNDLE" "$PUSH_PEER"
native_init_and_accept "$PUSH_PEER" "peer clean push side" "peer-only.txt" "peer\n"
cd "$PUSH_PEER"
F sync push "$SOURCE"
PUSH_MERGE="$(pg "d['data']['merge_commit_id']")"
ck "clean push status" "$(pg "d['status']")" "success"
ck "clean push merged" "$(pg "d['data']['merged']")" "True"
ck "clean push not materialized remotely" "$(pg "d['data']['materialized']")" "False"
ck_prefix "clean push merge commit id" "$PUSH_MERGE" "f1:commit:"
ck "clean push leaves remote peer file unmaterialized" "$([ ! -e "$SOURCE/peer-only.txt" ] && echo yes || echo no)" "yes"
cd "$SOURCE"
F doctor
ck "doctor after clean push remote" "$(pg "d['data']['ok']")" "True"
ck "clean push remote head survives reconcile" "$(native_head "$SOURCE")" "$PUSH_MERGE"

CONFLICT_SOURCE="$TMP/conflict-source"
CONFLICT_BUNDLE="$TMP/conflict-base.json"
base_source "$CONFLICT_SOURCE"
export_bundle "$CONFLICT_SOURCE" "$CONFLICT_BUNDLE"
native_init_and_accept "$CONFLICT_SOURCE" "source conflict side" "shared.txt" "source\n"
CONFLICT_PEER="$TMP/conflict-peer"
clone_peer "$CONFLICT_BUNDLE" "$CONFLICT_PEER"
native_init_and_accept "$CONFLICT_PEER" "peer conflict side" "shared.txt" "peer\n"
cd "$CONFLICT_PEER"
F sync fetch "$CONFLICT_SOURCE"
ck "conflict fetch status" "$(pg "d['status']")" "success"
ck "conflict fetch merged false" "$(pg "d['data']['merged']")" "False"
ck "conflict fetch records one conflict" "$(conflict_count "$CONFLICT_PEER")" "1"
F doctor
ck "doctor after conflict fetch" "$(pg "d['data']['ok']")" "True"

echo
echo "Native peer sync dogfood: PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failures:\n' >&2
  printf ' - %s\n' "${FAILS[@]}" >&2
  exit 1
fi

echo "Native peer sync dogfood checks passed."
