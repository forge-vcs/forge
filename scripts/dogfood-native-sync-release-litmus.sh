#!/usr/bin/env bash
# Release litmus for Phase 9 native sync with git removed from Forge's PATH.
#
# This is stricter than the focused peer-sync smoke tests:
# - simulates two isolated "machines" as plain non-git directories
# - runs every Forge invocation with a PATH containing `sh` but no `git`
# - proves clone produces an exact exported manifest match for durable sync
#   state; local work-context attachments are intentionally machine-local
# - proves fetch/pull/push convergence reaches equal native object payloads,
#   equal native refs, and equal syncable domain-ledger rows
# - proves a true remote-boundary conflict is persisted as conflict-as-data
#
# Usage:  bash scripts/dogfood-native-sync-release-litmus.sh
# Keep repos/logs: KEEP_DOGFOOD=1 bash scripts/dogfood-native-sync-release-litmus.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FORGE="$ROOT/target/debug/forge"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/forge-sync-release-litmus.XXXXXX")"
OUT="$TMP/out.json"
ERR="$TMP/err.txt"
NOGIT_BIN="$TMP/nogit-bin"
PASS=0
FAIL=0
declare -a FAILS=()

if [ "${KEEP_DOGFOOD:-0}" != "1" ]; then
  trap 'rm -rf "$TMP"' EXIT
else
  trap 'echo "kept release-litmus repos at: '"$TMP"'"' EXIT
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

file_url() {
  printf 'file://%s' "$1"
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

export_manifest() {
  local dir="$1"
  local path="$2"
  cd "$dir"
  NG sync export --output "$path"
}

native_head() {
  local dir="$1"
  local path="$TMP/head-$(basename "$dir").json"
  export_manifest "$dir" "$path" >/dev/null
  python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['native_head'])" "$path"
}

conflict_count() {
  local dir="$1"
  cd "$dir"
  NG conflict list
  pg "len(d['data']['conflicts'])"
}

compare_manifests() {
  local desc="$1"
  local left="$2"
  local right="$3"
  local mode="$4"
  local result
  result="$(
    python3 - "$left" "$right" "$mode" <<'PY'
import json
import sys

left_path, right_path, mode = sys.argv[1:4]
left = json.load(open(left_path))
right = json.load(open(right_path))

def stable(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))

def sorted_rows(rows):
    return sorted(rows, key=stable)

def table_rows(manifest):
    tables = {}
    for table in manifest["ledger_rows"]:
        name = table["table"]
        if mode == "domain" and name in {"operations", "views"}:
            continue
        tables[name] = sorted_rows(table["rows"])
    return tables

checks = [
    ("protocol_version", left["protocol_version"], right["protocol_version"]),
    ("cli_schema_version", left["cli_schema_version"], right["cli_schema_version"]),
    ("repo_id", left["repo_id"], right["repo_id"]),
    ("content_backend", left["content_backend"], right["content_backend"]),
    ("native_head", left["native_head"], right["native_head"]),
    ("native_objects", sorted_rows(left["native_objects"]), sorted_rows(right["native_objects"])),
    ("native_payloads", sorted_rows(left["native_payloads"]), sorted_rows(right["native_payloads"])),
]

if mode == "exact":
    checks.extend([
        ("current_operation_id", left["current_operation_id"], right["current_operation_id"]),
        ("current_view_id", left["current_view_id"], right["current_view_id"]),
        ("expected_content_ref", left["expected_content_ref"], right["expected_content_ref"]),
        ("local_key_fingerprint", left["local_key_fingerprint"], right["local_key_fingerprint"]),
        ("ledger_counts", sorted_rows(left["ledger_counts"]), sorted_rows(right["ledger_counts"])),
    ])

checks.append(("ledger_rows", table_rows(left), table_rows(right)))

for name, a, b in checks:
    if a != b:
        print(f"mismatch:{name}")
        sys.exit(0)

print("equal")
PY
  )"
  ck "$desc" "$result" "equal"
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
echo "=== Phase 9 native sync release litmus (no git in Forge PATH) ==="

MACHINE_A="$TMP/machine-a"
MACHINE_B="$TMP/machine-b"
mkdir -p "$MACHINE_A" "$MACHINE_B"

cd "$MACHINE_A"
printf 'release litmus\n' > README.md
NG init --content-backend native
ck "machine A native init without git" "$(pg "d['status']")" "success"
native_init_and_accept "$MACHINE_A" "base release state" "base.txt" "base\n"
ck_prefix "machine A base accept creates native commit" "$(pg "d['data']['commit_id']")" "f1:commit:"

BASE_MANIFEST="$TMP/base.json"
export_manifest "$MACHINE_A" "$BASE_MANIFEST"
ck "machine A base export" "$(pg "d['status']")" "success"

cd "$MACHINE_B"
NG sync clone "$BASE_MANIFEST"
ck "machine B clone without git" "$(pg "d['status']")" "success"
B_CLONE_MANIFEST="$TMP/machine-b-clone.json"
export_manifest "$MACHINE_B" "$B_CLONE_MANIFEST"
compare_manifests "clone reaches exact manifest equality" "$BASE_MANIFEST" "$B_CLONE_MANIFEST" "exact"

native_init_and_accept "$MACHINE_A" "A fast-forward release state" "a-fast-forward.txt" "a fast-forward\n"
A_FF_HEAD="$(pg "d['data']['commit_id']")"
ck_prefix "machine A fast-forward commit" "$A_FF_HEAD" "f1:commit:"

cd "$MACHINE_B"
NG sync fetch "$(file_url "$MACHINE_A")"
ck "machine B fetches A fast-forward without git" "$(pg "d['status']")" "success"
ck "fetch does not materialize worktree" "$(pg "d['data']['materialized']")" "False"
ck "machine B head equals A fast-forward head" "$(native_head "$MACHINE_B")" "$A_FF_HEAD"
A_AFTER_FF="$TMP/a-after-ff.json"
B_AFTER_FF="$TMP/b-after-ff.json"
export_manifest "$MACHINE_A" "$A_AFTER_FF"
export_manifest "$MACHINE_B" "$B_AFTER_FF"
compare_manifests "fetch convergence matches objects and domain ledger" "$A_AFTER_FF" "$B_AFTER_FF" "domain"

native_init_and_accept "$MACHINE_A" "A clean divergent side" "a-clean.txt" "a clean\n"
A_CLEAN_HEAD="$(pg "d['data']['commit_id']")"
native_init_and_accept "$MACHINE_B" "B clean divergent side" "b-clean.txt" "b clean\n"
B_CLEAN_HEAD="$(pg "d['data']['commit_id']")"
ck_prefix "machine A clean-side commit" "$A_CLEAN_HEAD" "f1:commit:"
ck_prefix "machine B clean-side commit" "$B_CLEAN_HEAD" "f1:commit:"

cd "$MACHINE_B"
NG sync pull "$(file_url "$MACHINE_A")"
PULL_MERGE="$(pg "d['data']['merge_commit_id']")"
ck "clean divergent pull succeeds without git" "$(pg "d['status']")" "success"
ck "clean divergent pull records merge" "$(pg "d['data']['merged']")" "True"
ck "clean divergent pull materializes" "$(pg "d['data']['materialized']")" "True"
ck_prefix "clean divergent pull merge commit" "$PULL_MERGE" "f1:commit:"
ck "pull materialized A file" "$(cat "$MACHINE_B/a-clean.txt")" "a clean"
ck "pull preserved B file" "$(cat "$MACHINE_B/b-clean.txt")" "b clean"
NG doctor
ck "machine B doctor after clean pull" "$(pg "d['data']['ok']")" "True"

cd "$MACHINE_B"
NG sync push "$(file_url "$MACHINE_A")"
ck "machine B pushes clean merge back to A" "$(pg "d['status']")" "success"
ck "machine A head equals B merge after push" "$(native_head "$MACHINE_A")" "$PULL_MERGE"
A_AFTER_PULL_PUSH="$TMP/a-after-pull-push.json"
B_AFTER_PULL_PUSH="$TMP/b-after-pull-push.json"
export_manifest "$MACHINE_A" "$A_AFTER_PULL_PUSH"
export_manifest "$MACHINE_B" "$B_AFTER_PULL_PUSH"
compare_manifests "pull plus push-back converges objects and domain ledger" "$A_AFTER_PULL_PUSH" "$B_AFTER_PULL_PUSH" "domain"

native_init_and_accept "$MACHINE_A" "A clean push side" "a-push.txt" "a push\n"
native_init_and_accept "$MACHINE_B" "B clean push side" "b-push.txt" "b push\n"
cd "$MACHINE_B"
NG sync push "$(file_url "$MACHINE_A")"
PUSH_MERGE="$(pg "d['data']['merge_commit_id']")"
ck "clean divergent push succeeds without git" "$(pg "d['status']")" "success"
ck "clean divergent push records remote merge" "$(pg "d['data']['merged']")" "True"
ck_prefix "clean divergent push merge commit" "$PUSH_MERGE" "f1:commit:"
cd "$MACHINE_B"
NG sync fetch "$(file_url "$MACHINE_A")"
ck "machine B fetches remote push merge" "$(pg "d['status']")" "success"
ck "machine B head equals remote push merge" "$(native_head "$MACHINE_B")" "$PUSH_MERGE"
A_AFTER_PUSH_FETCH="$TMP/a-after-push-fetch.json"
B_AFTER_PUSH_FETCH="$TMP/b-after-push-fetch.json"
export_manifest "$MACHINE_A" "$A_AFTER_PUSH_FETCH"
export_manifest "$MACHINE_B" "$B_AFTER_PUSH_FETCH"
compare_manifests "push plus fetch-back converges objects and domain ledger" "$A_AFTER_PUSH_FETCH" "$B_AFTER_PUSH_FETCH" "domain"

native_init_and_accept "$MACHINE_A" "A conflict side" "shared.txt" "from A\n"
native_init_and_accept "$MACHINE_B" "B conflict side" "shared.txt" "from B\n"
cd "$MACHINE_B"
NG sync fetch "$(file_url "$MACHINE_A")"
ck "conflicting fetch succeeds as conflict-as-data" "$(pg "d['status']")" "success"
ck "conflicting fetch reports merged false" "$(pg "d['data']['merged']")" "False"
ck "conflicting fetch records one conflict" "$(conflict_count "$MACHINE_B")" "1"
NG doctor
ck "machine B doctor after conflict-as-data" "$(pg "d['data']['ok']")" "True"

echo
echo "Native sync release litmus: PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failures:\n' >&2
  printf ' - %s\n' "${FAILS[@]}" >&2
  exit 1
fi

echo "Native sync release litmus checks passed."
