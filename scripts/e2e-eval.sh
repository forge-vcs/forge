#!/usr/bin/env bash
# End-to-end acceptance eval for the real `forge` binary (NER-133 Phase 2 focus).
#
# Complements `cargo test` (which proves logic in-harness) by driving the SHIPPED
# binary through real git repos and asserting observable behavior. Deterministic
# checks only — nondeterministic concurrency races, crash-consistency, and
# real-old-DB upgrade/brick reconciliation stay in the Rust suites
# (forge_concurrency.rs, forge_crash_injection.rs, migrations.rs, migrate.rs).
#
# Usage:  bash scripts/e2e-eval.sh
# Exit:   0 if all checks pass, 1 otherwise.

set -u
ROOT="$(git rev-parse --show-toplevel)"
FORGE="$ROOT/target/debug/forge"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/forge-eval.XXXXXX")"
OUT="$TMP/out.json"; ERR="$TMP/err.txt"
trap 'rm -rf "$TMP"' EXIT

PASS=0; FAIL=0; declare -a FAILS=()
have_sqlite=1; command -v sqlite3 >/dev/null 2>&1 || have_sqlite=0

ck() { # ck "desc" actual expected
  if [ "$2" = "$3" ]; then PASS=$((PASS+1)); printf '  \033[32m✓\033[0m %s\n' "$1"
  else FAIL=$((FAIL+1)); FAILS+=("$1 — got [$2] want [$3]"); printf '  \033[31m✗\033[0m %s — got [%s] want [%s]\n' "$1" "$2" "$3"; fi
}
ckc() { # ckc "desc" haystack needle  (PASS if needle in haystack)
  case "$2" in *"$3"*) PASS=$((PASS+1)); printf '  \033[32m✓\033[0m %s\n' "$1";;
  *) FAIL=$((FAIL+1)); FAILS+=("$1 — [$3] not in output"); printf '  \033[31m✗\033[0m %s — [%s] missing\n' "$1" "$3";; esac
}
pg() { python3 -c "import json,sys
try:
    d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))
except Exception as e:
    print('<ERR:%s>'%e)" "$OUT" "$1"; }
F() { "$FORGE" --json "$@" >"$OUT" 2>"$ERR"; }   # forge --json <args>; envelope -> $OUT
mkrepo() { local d="$TMP/$1"; mkdir -p "$d"; cd "$d" || exit 1
  git init -q; git config user.email e@e.test; git config user.name eval
  echo "# $1" > README.md; git add README.md; git commit -qm init; }
db() { sqlite3 "$1/.forge/forge.db" "$2"; }

echo "=== Building forge (debug) ==="
cargo build -q --bin forge || { echo "BUILD FAILED"; exit 1; }
echo "binary: $FORGE"; [ "$have_sqlite" = 1 ] || echo "(sqlite3 not found — DB-inspection checks will be SKIPPED)"

echo; echo "=== SCHEMA (forge schema) ==="
mkrepo schema-repo >/dev/null; F init >/dev/null
F schema; ck "schema_version is forge.cli.v0 (in repo)" "$(pg "d['schema_version']")" "forge.cli.v0"
ck "contract.schema_version is forge.cli.v0" "$(pg "d['data']['schema_version']")" "forge.cli.v0"
codes="$(pg "sorted(e['code'] for e in d['data']['errors'])")"
for c in STALE_BASE LOCK_TIMEOUT CONFLICT SCHEMA_VERSION_UNSUPPORTED MIGRATION_FAILED DIRTY_WORKTREE NOT_ACCEPTED; do
  ckc "registry contains $c" "$codes" "$c"; done
ck "CONFLICT is retryable" "$(pg "next(e['retryable'] for e in d['data']['errors'] if e['code']=='CONFLICT')")" "True"
ck "LOCK_TIMEOUT is retryable" "$(pg "next(e['retryable'] for e in d['data']['errors'] if e['code']=='LOCK_TIMEOUT')")" "True"
cd "$TMP"; F schema; ck "schema works OUTSIDE a git repo (repo-independent)" "$(pg "d['schema_version']")" "forge.cli.v0"

echo; echo "=== LIFECYCLE (init→start→save→run→propose→check→accept→export) ==="
mkrepo life >/dev/null
F init;    ck "init success" "$(pg "d['status']")" "success"
F doctor;  ck "doctor ok" "$(pg "d['data']['ok']")" "True"
ck "doctor schema_version=4" "$(pg "d['data']['schema_version']")" "4"
F start "build a feature"; ck "start success" "$(pg "d['status']")" "success"
echo "hello" > feature.txt
F save;    ck "save success" "$(pg "d['status']")" "success"
F run -- true; ck "run success" "$(pg "d['status']")" "success"
F propose; ck "propose success" "$(pg "d['status']")" "success"
F check;   ck "check success" "$(pg "d['status']")" "success"
F accept;  ck "accept success" "$(pg "d['status']")" "success"
F export branch eval-branch; ck "export branch success" "$(pg "d['status']")" "success"
git rev-parse --verify eval-branch >/dev/null 2>&1 && b=yes || b=no
ck "exported git branch exists" "$b" "yes"

echo; echo "=== DECLARATIVE CHECK GATES (NER-135) ==="
mkrepo gates >/dev/null
F init >/dev/null
F start "gated feature" --require "cargo test"; ck "start with --require gate" "$(pg "d['status']")" "success"
echo "gate me" > gated.txt
F save >/dev/null
F run -- sh -c true >/dev/null   # a trivial success that does NOT match the named gate
F propose >/dev/null
F check; ck "run -- true cannot satisfy a non-trivial gate" "$(pg "d['data']['status']")" "missing"
ck "check JSON reports a per-gate verdict" "$(pg "d['data']['gates'][0]['verdict']")" "missing"
F accept; ck "accept requires a passing check by default" "$(pg "d['errors'][0]['code']")" "CHECK_NOT_PASSED"
F accept --allow-unverified; ck "accept --allow-unverified bypasses the gate" "$(pg "d['status']")" "success"

echo; echo "=== MIGRATION state (live binary) ==="
if [ "$have_sqlite" = 1 ]; then
  vers="$(db "$TMP/life" "SELECT group_concat(version) FROM schema_migrations ORDER BY version;")"
  ck "schema_migrations has versions 1,2,3,4" "$vers" "1,2,3,4"
  nullck="$(db "$TMP/life" "SELECT count(*) FROM schema_migrations WHERE checksum IS NULL;")"
  ck "all migration rows carry a checksum" "$nullck" "0"
  # HEAD+1 read-only refuse on a separate repo
  mkrepo headplus1 >/dev/null; F init >/dev/null
  db "$TMP/headplus1" "INSERT OR REPLACE INTO schema_migrations(version,name,applied_at_ms,checksum) VALUES (5,'future',0,NULL);"
  F show; ck "DB ahead of binary refuses read-only" "$(pg "d['errors'][0]['code']")" "SCHEMA_VERSION_UNSUPPORTED"
else
  echo "  (skipped: sqlite3 unavailable)"
fi

echo; echo "=== TYPED ERRORS ==="
mkrepo errs >/dev/null; F init >/dev/null
F accept; code="$(pg "d['errors'][0]['code']")"
case "$code" in COMMAND_FAILED) ck "accept with no proposal is typed (not COMMAND_FAILED)" "typed" "COMMAND_FAILED";; *) ck "accept with no proposal is typed: $code" "typed" "typed";; esac
F start "x" >/dev/null
F attempt show bogus-id 2>/dev/null
ckc "unknown selector yields a typed UNKNOWN_* code" "$(pg "d['errors'][0]['code']")" "UNKNOWN"

echo; echo "=== SECRET-EXPORT default-deny ==="
mkrepo secret >/dev/null; F init >/dev/null; F start "secret test" >/dev/null
printf 'API_TOKEN=supersecret\n' > .env
echo "ok" > app.txt
F save >/dev/null; F run -- true >/dev/null; F propose >/dev/null; F check >/dev/null; F accept >/dev/null
F export branch secret-branch; ck "export branch (with .env present) succeeds" "$(pg "d['status']")" "success"
tree="$(git ls-tree -r secret-branch --name-only 2>/dev/null)"
ckc "non-secret file is exported" "$tree" "app.txt"
case "$tree" in *".env"*) FAIL=$((FAIL+1)); FAILS+=(".env LEAKED into exported tree"); printf '  \033[31m✗\033[0m .env must NOT be in the exported branch tree\n';; *) PASS=$((PASS+1)); printf '  \033[32m✓\033[0m .env is absent from the exported branch tree\n';; esac
warns="$(pg "len(d.get('warnings',[]))")"
echo "  (info) export warnings[] count = $warns  [0 expected: .env is stripped at snapshot time before export; see code-review triage]"

echo; echo "=== CONFLICT-SET on stale base ==="
mkrepo conflict >/dev/null; F init >/dev/null; F start "stale test" >/dev/null
echo "work" > w.txt; F save >/dev/null; F run -- true >/dev/null; F propose >/dev/null
echo "moved" >> README.md; git add README.md; git commit -qm "move HEAD"   # base_head now stale
F accept; ck "accept against moved HEAD bails STALE_BASE" "$(pg "d['errors'][0]['code']")" "STALE_BASE"
ckc "STALE_BASE details carry expected_head" "$(pg "list(d['errors'][0]['details'].keys())")" "expected_head"
ckc "STALE_BASE details carry actual_head" "$(pg "list(d['errors'][0]['details'].keys())")" "actual_head"
if [ "$have_sqlite" = 1 ]; then
  ck "a conflict_sets row was persisted" "$(db "$TMP/conflict" "SELECT count(*) FROM conflict_sets WHERE context='stale_base_accept';")" "1"
  ckc "conflict_sets row records the head pair" "$(db "$TMP/conflict" "SELECT paths_json FROM conflict_sets LIMIT 1;")" "expected_head"
else echo "  (conflict_sets row check skipped: sqlite3 unavailable)"; fi

echo; echo "=== TAMPER-EVIDENCE (NER-136) ==="
mkrepo tamper >/dev/null; F init >/dev/null; F start "tamper test" >/dev/null
echo "work" > t.txt; F save >/dev/null; F run -- sh -c true >/dev/null; F propose >/dev/null
F check; ck "untampered check passes" "$(pg "d['data']['status']")" "passed"
if [ "$have_sqlite" = 1 ]; then
  # Edit the persisted excerpt WITHOUT recomputing the content hash.
  db "$TMP/tamper" "UPDATE evidence SET stdout_excerpt='FORGED';"
  F doctor; ck "doctor flags a tampered evidence row" "$(pg "d['data']['ok']")" "False"
  ckc "doctor names the content_edit kind" "$(pg "[r['kind'] for r in d['data']['tampered_rows']]")" "content_edit"
  F check; ck "check refuses tampered evidence" "$(pg "d['errors'][0]['code']")" "EVIDENCE_TAMPERED"
  F accept --allow-unverified; ck "--allow-unverified never bypasses tamper" "$(pg "d['errors'][0]['code']")" "EVIDENCE_TAMPERED"
else
  echo "  (tamper checks skipped: sqlite3 unavailable)"
fi

echo; echo "=== RESULT ==="
echo "PASS=$PASS  FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then printf '\nFailures:\n'; for m in "${FAILS[@]}"; do echo "  - $m"; done; exit 1; fi
echo "All checks passed."
