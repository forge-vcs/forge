#!/usr/bin/env bash
# Focused dogfood harness for the native Forge loop on a temporary TypeScript repo.
#
# It creates a small TS project, drives competing attempts through clean and
# conflicting native merges, and asserts the agent-facing JSON contract around
# conflict suggestions. No network access or package installation is required:
# the test command is `tsc --noEmit` from PATH.
#
# Usage:  bash scripts/dogfood-typescript-native.sh
# Keep repo/logs: KEEP_DOGFOOD=1 bash scripts/dogfood-typescript-native.sh

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
FORGE="$ROOT/target/debug/forge"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/forge-ts-dogfood.XXXXXX")"
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
    printf '  \033[32m✓\033[0m %s\n' "$desc"
  else
    FAIL=$((FAIL + 1))
    FAILS+=("$desc -- got [$actual] want [$expected]")
    printf '  \033[31m✗\033[0m %s -- got [%s] want [%s]\n' "$desc" "$actual" "$expected"
  fi
}

ckc() {
  local desc="$1"
  local haystack="$2"
  local needle="$3"
  case "$haystack" in
    *"$needle"*)
      PASS=$((PASS + 1))
      printf '  \033[32m✓\033[0m %s\n' "$desc"
      ;;
    *)
      FAIL=$((FAIL + 1))
      FAILS+=("$desc -- [$needle] not in output")
      printf '  \033[31m✗\033[0m %s -- [%s] missing\n' "$desc" "$needle"
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

mktsrepo() {
  local name="$1"
  local dir="$TMP/$name"
  mkdir -p "$dir/src"
  cd "$dir"
  git init -q
  git config user.email dogfood@example.test
  git config user.name "Forge Dogfood"
  cat >package.json <<'JSON'
{"scripts":{"test":"tsc --noEmit"},"devDependencies":{}}
JSON
  cat >tsconfig.json <<'JSON'
{"compilerOptions":{"strict":true,"target":"ES2022","module":"ES2022","moduleResolution":"Bundler","noEmit":true},"include":["src/**/*.ts"]}
JSON
  cat >src/calculator.ts <<'TS'
export type ScoreBand = "negative" | "zero" | "positive";

export function scoreBand(value: number): ScoreBand {
  if (value < 0) return "negative";
  if (value === 0) return "zero";
  return "positive";
}

export function total(values: number[]): number {
  return values.reduce((sum, value) => sum + value, 0);
}
TS
  git add package.json tsconfig.json src/calculator.ts
  git commit -qm "initial TypeScript project"
}

need git
need python3
need tsc

echo "=== Building forge (debug) ==="
cargo build -q --bin forge
echo "binary: $FORGE"
echo "typescript: $(tsc --version)"

echo
echo "=== Native clean merge on TypeScript repo ==="
mktsrepo clean
F init --content-backend native
ck "native init succeeds" "$(pg "d['status']")" "success"
F start "clean TypeScript merge"
intent="$(pg "d['data']['intent_id']")"
attempt_a="$(pg "d['data']['attempt_id']")"
F attempt start --intent "$intent"
attempt_b="$(pg "d['data']['attempt_id']")"

F attempt attach "$attempt_a" >/dev/null
python3 - <<'PY'
from pathlib import Path
p = Path("src/calculator.ts")
s = p.read_text()
s = s.replace('return values.reduce((sum, value) => sum + value, 0);',
              'return values.reduce((sum, value) => sum + value, 0);\n}\n\nexport function average(values: number[]): number {\n  return values.length === 0 ? 0 : total(values) / values.length;')
p.write_text(s)
PY
F save --attempt "$attempt_a" >/dev/null
F run --attempt "$attempt_a" -- tsc --noEmit
ck "attempt A TypeScript check passes" "$(pg "d['data']['exit_code']")" "0"
F propose --attempt "$attempt_a"
proposal_a="$(pg "d['data']['proposal_id']")"
F check --attempt "$attempt_a" >/dev/null

F attempt attach "$attempt_b" >/dev/null
cat >src/labels.ts <<'TS'
import type { ScoreBand } from "./calculator";

export function labelFor(band: ScoreBand): string {
  return `score:${band}`;
}
TS
F save --attempt "$attempt_b" >/dev/null
F run --attempt "$attempt_b" -- tsc --noEmit
ck "attempt B TypeScript check passes" "$(pg "d['data']['exit_code']")" "0"
F propose --attempt "$attempt_b"
proposal_b="$(pg "d['data']['proposal_id']")"

F accept --attempt "$attempt_a" --proposal "$proposal_a" >/dev/null
F merge --proposal "$proposal_b"
ck "clean native merge returns merged=true" "$(pg "d['data']['merged']")" "True"
merged_revision="$(pg "d['data']['proposal_revision_id']")"
F run --attempt "$attempt_b" -- tsc --noEmit
ck "clean merged tree typechecks after run" "$(pg "d['data']['exit_code']")" "0"
F check --attempt "$attempt_b" --proposal "$proposal_b"
ck "clean merged revision checks passed" "$(pg "d['data']['status']")" "passed"
ck "check binds to merged revision" "$(pg "d['data']['proposal_revision_id']")" "$merged_revision"
F accept --attempt "$attempt_b" --proposal "$proposal_b"
ck "clean merged proposal accepts" "$(pg "d['status']")" "success"

echo
echo "=== Native conflict suggestion loop on TypeScript repo ==="
mktsrepo conflict
F init --content-backend native >/dev/null
F start "conflicting TypeScript merge"
intent="$(pg "d['data']['intent_id']")"
attempt_a="$(pg "d['data']['attempt_id']")"
F attempt start --intent "$intent"
attempt_b="$(pg "d['data']['attempt_id']")"

F attempt attach "$attempt_a" >/dev/null
python3 - <<'PY'
from pathlib import Path
p = Path("src/calculator.ts")
s = p.read_text().replace('if (value < 0) return "negative";', 'if (value < 0) return "debt" as ScoreBand;')
p.write_text(s)
PY
F save --attempt "$attempt_a" >/dev/null
F run --attempt "$attempt_a" -- tsc --noEmit
ck "conflict attempt A typechecks" "$(pg "d['data']['exit_code']")" "0"
F propose --attempt "$attempt_a"
proposal_a="$(pg "d['data']['proposal_id']")"
F check --attempt "$attempt_a" >/dev/null

F attempt attach "$attempt_b" >/dev/null
python3 - <<'PY'
from pathlib import Path
p = Path("src/calculator.ts")
s = p.read_text().replace('if (value < 0) return "negative";', 'if (value < 0) return "loss" as ScoreBand;')
p.write_text(s)
PY
F save --attempt "$attempt_b" >/dev/null
F run --attempt "$attempt_b" -- tsc --noEmit
ck "conflict attempt B typechecks" "$(pg "d['data']['exit_code']")" "0"
F propose --attempt "$attempt_b"
proposal_b="$(pg "d['data']['proposal_id']")"
F check --attempt "$attempt_b" >/dev/null

F accept --attempt "$attempt_a" --proposal "$proposal_a" >/dev/null
F merge --proposal "$proposal_b"
ck "conflicting native merge returns merged=false" "$(pg "d['data']['merged']")" "False"
conflict_set_id="$(pg "d['data']['conflict_set_id']")"
ck "merge output has no suggestions" "$(pg "'suggestions' in d['data']")" "False"

F conflict show "$conflict_set_id"
ck "plain conflict show has no suggestions" "$(pg "'suggestions' in d['data']")" "False"
ck "conflict remains unresolved before suggestion" "$(pg "d['data']['conflict']['status']")" "unresolved"

F conflict show "$conflict_set_id" --suggest
ck "suggested conflict emits two ranked candidates" "$(pg "len(d['data']['suggestions'])")" "2"
ck "suggestion requires explicit resolve" "$(pg "d['data']['suggestions'][0]['requires_explicit_resolve']")" "True"
ck "suggestion provenance cites proposal" "$(pg "d['data']['suggestions'][0]['provenance']['proposal_id']")" "$proposal_b"
ck "suggestion provenance carries evidence" "$(pg "d['data']['suggestions'][0]['provenance']['evidence_input_status']")" "present"
ckc "suggestion resolution is forge-tree" "$(pg "d['data']['suggestions'][0]['resolution_ref']")" "forge-tree:"
rendered="$(cat "$OUT")"
case "$rendered" in
  *"src/calculator.ts"*|*"debt"*|*"loss"*)
    FAIL=$((FAIL + 1))
    FAILS+=("conflict suggestion leaked raw path or inline TypeScript content")
    printf '  \033[31m✗\033[0m conflict suggestion redaction\n'
    ;;
  *)
    PASS=$((PASS + 1))
    printf '  \033[32m✓\033[0m conflict suggestion redaction\n'
    ;;
esac
resolution_ref="$(pg "d['data']['suggestions'][0]['resolution_ref']")"

F conflict show "$conflict_set_id" >/dev/null
ck "suggestion did not resolve conflict" "$(pg "d['data']['conflict']['status']")" "unresolved"
F conflict resolve "$conflict_set_id" --tree "$resolution_ref"
ck "explicit conflict resolve succeeds" "$(pg "d['status']")" "success"
resolved_revision="$(pg "d['data']['proposal_revision_id']")"
ckc "explicit resolve records evidence" "$(pg "d['data']['evidence_id']")" "evidence_"

F accept --attempt "$attempt_b" --proposal "$proposal_b" || true
ck "accept before fresh re-check is refused" "$(pg "d['errors'][0]['code']")" "CHECK_NOT_PASSED"
F run --attempt "$attempt_b" -- tsc --noEmit
ck "resolved conflict tree typechecks after run" "$(pg "d['data']['exit_code']")" "0"
F check --attempt "$attempt_b" --proposal "$proposal_b"
ck "resolved revision checks passed" "$(pg "d['data']['status']")" "passed"
ck "check binds to resolved revision" "$(pg "d['data']['proposal_revision_id']")" "$resolved_revision"
F accept --attempt "$attempt_b" --proposal "$proposal_b"
ck "resolved conflict proposal accepts" "$(pg "d['status']")" "success"
F doctor
ck "doctor clean after TypeScript dogfood" "$(pg "d['data']['ok']")" "True"
F gc --dry-run
ck "gc dry-run still works after TypeScript dogfood" "$(pg "d['data']['dry_run']")" "True"

echo
echo "=== Native multi-workspace TypeScript loop and confirmed GC ==="
mktsrepo workspaces
F init --content-backend native >/dev/null
echo "TOKEN=dogfood" >.env
F start "workspace TypeScript isolation"
intent="$(pg "d['data']['intent_id']")"
attempt_a="$(pg "d['data']['attempt_id']")"
workspace_a="$(pg "d['data']['workspace_path']")"
F attempt start --intent "$intent"
attempt_b="$(pg "d['data']['attempt_id']")"
workspace_b="$(pg "d['data']['workspace_path']")"

ck "workspace A path surfaces" "$([[ "$workspace_a" == .forge/worktrees/* ]] && echo yes || echo no)" "yes"
ck "workspace B path surfaces" "$([[ "$workspace_b" == .forge/worktrees/* ]] && echo yes || echo no)" "yes"
ck "workspace A has source tree" "$([ -f "$workspace_a/src/calculator.ts" ] && echo yes || echo no)" "yes"
ck "workspace B has source tree" "$([ -f "$workspace_b/src/calculator.ts" ] && echo yes || echo no)" "yes"
ck "workspace A filters .env" "$([ ! -e "$workspace_a/.env" ] && echo yes || echo no)" "yes"

(
  cd "$workspace_a"
  python3 - <<'PY'
from pathlib import Path
p = Path("src/calculator.ts")
s = p.read_text()
s = s.replace('return values.reduce((sum, value) => sum + value, 0);',
              'return values.reduce((sum, value) => sum + value, 0);\n}\n\nexport function max(values: number[]): number {\n  return values.length === 0 ? 0 : Math.max(...values);')
p.write_text(s)
PY
  "$FORGE" --json save >"$OUT" 2>"$ERR"
)
ck "workspace A save binds to attempt A" "$(pg "d['data']['attempt_id']")" "$attempt_a"
(
  cd "$workspace_a"
  "$FORGE" --json run -- tsc --noEmit >"$OUT" 2>"$ERR"
)
ck "workspace A TypeScript check passes" "$(pg "d['data']['exit_code']")" "0"
(
  cd "$workspace_a"
  "$FORGE" --json propose >"$OUT" 2>"$ERR"
)
proposal_a="$(pg "d['data']['proposal_id']")"
F check --attempt "$attempt_a" --proposal "$proposal_a" >/dev/null

(
  cd "$workspace_b"
  cat >src/format.ts <<'TS'
export function formatScore(value: number): string {
  return `${value.toFixed(2)} pts`;
}
TS
  "$FORGE" --json save >"$OUT" 2>"$ERR"
)
ck "workspace B save binds to attempt B" "$(pg "d['data']['attempt_id']")" "$attempt_b"
ck "workspace B stayed isolated" "$([ ! -f "$workspace_a/src/format.ts" ] && echo yes || echo no)" "yes"
ck "repo root stayed isolated from workspace edits" "$([ ! -f src/format.ts ] && echo yes || echo no)" "yes"
(
  cd "$workspace_b"
  "$FORGE" --json run -- tsc --noEmit >"$OUT" 2>"$ERR"
)
ck "workspace B TypeScript check passes" "$(pg "d['data']['exit_code']")" "0"
(
  cd "$workspace_b"
  "$FORGE" --json propose >"$OUT" 2>"$ERR"
)
proposal_b="$(pg "d['data']['proposal_id']")"

F accept --attempt "$attempt_a" --proposal "$proposal_a" >/dev/null
if ! git diff --quiet -- src/calculator.ts; then
  git add src/calculator.ts
  git commit -qm "accept workspace A"
fi
F merge --proposal "$proposal_b"
ck "workspace merge returns merged=true" "$(pg "d['data']['merged']")" "True"

orphan_id="$(python3 - <<'PY'
import hashlib
from pathlib import Path

payload = b"dogfood unreachable orphan"
preimage = b"forge-object\nblob\n1\n" + str(len(payload)).encode() + b"\n" + payload
digest = hashlib.sha256(preimage).hexdigest()
path = Path(".forge/objects/sha256") / digest[:2] / digest
path.parent.mkdir(parents=True, exist_ok=True)
path.write_bytes(preimage)
print(f"blob:sha256:{digest}")
PY
)"
orphan_digest="${orphan_id##*:}"
orphan_path=".forge/objects/sha256/${orphan_digest:0:2}/$orphan_digest"
touch -t 202001010000 "$orphan_path"
F gc --dry-run
gc_digest="$(pg "d['data']['plan_digest']")"
ckc "gc dry-run sees dogfood orphan" "$(pg "d['data']['unreachable_native_objects']")" "$orphan_id"
F gc --yes --plan-digest "$gc_digest"
ck "confirmed gc deletes dogfood orphan" "$([ ! -e "$orphan_path" ] && echo yes || echo no)" "yes"

echo
echo "=== RESULT ==="
echo "PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf '\nFailures:\n'
  printf ' - %s\n' "${FAILS[@]}"
  echo "repo/logs: $TMP"
  exit 1
fi
echo "All TypeScript native dogfood checks passed."
