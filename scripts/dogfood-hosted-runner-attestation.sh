#!/usr/bin/env bash
# Phase 9 hosted-runner trust dogfood.
#
# Exercises the real CLI path:
# - hosted_runner_signed accept policy fails before hosted attestation
# - trust attest hosted-runner signs proposal evidence with a non-local runner key
# - hosted_runner_signed accept policy succeeds after attestation
# - third_party_attested still fails closed

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FORGE="$ROOT/target/debug/forge"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/forge-hosted-attest.XXXXXX")"
OUT="$TMP/out.json"
ERR="$TMP/err.txt"
PASS=0
FAIL=0
declare -a FAILS=()

if [ "${KEEP_DOGFOOD:-0}" != "1" ]; then
  trap 'rm -rf "$TMP"' EXIT
else
  trap 'echo "kept hosted-runner dogfood repo at: '"$TMP"'"' EXIT
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

need cargo
need python3

echo "=== Building forge (debug) ==="
(cd "$ROOT" && cargo build -q --bin forge)
echo "binary: $FORGE"

echo
echo "=== Phase 9 hosted-runner attestation dogfood ==="

python3 - "$TMP/hosted-runner-ed25519.pk8" <<'PY'
import sys
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import ed25519

key = ed25519.Ed25519PrivateKey.generate()
pkcs8 = key.private_bytes(
    encoding=serialization.Encoding.DER,
    format=serialization.PrivateFormat.PKCS8,
    encryption_algorithm=serialization.NoEncryption(),
)
open(sys.argv[1], "wb").write(pkcs8)
PY

cd "$TMP"
F init --content-backend native
ck "native init succeeds" "$(pg "d['status']")" "success"
F start "hosted runner attest dogfood" --require "sh -c true"
ck "start succeeds" "$(pg "d['status']")" "success"
printf 'hosted runner attest\n' > hosted.txt
F save
ck "save succeeds" "$(pg "d['status']")" "success"
F run -- sh -c true
ck "run evidence succeeds" "$(pg "d['status']")" "success"
F propose
ck "proposal succeeds" "$(pg "d['status']")" "success"
F check
ck "check succeeds" "$(pg "d['status']")" "success"

F trust policy --accept hosted_runner_signed
ck "hosted accept policy updates" "$(pg "d['data']['min_accept_trust']")" "hosted_runner_signed"
if F accept; then
  blocked_code="<accepted>"
else
  blocked_code="$(pg "d['errors'][0]['code']")"
fi
ck "hosted policy fails before attestation" "$blocked_code" "TRUST_POLICY_UNMET"
ck "pre-attestation failure names evidence" "$(pg "d['errors'][0]['details']['signature_issues'][0]['subject_kind']")" "evidence"

F trust attest hosted-runner --key "$TMP/hosted-runner-ed25519.pk8" --issuer ci.example/verify
ck "hosted attestation succeeds" "$(pg "d['status']")" "success"
ck "hosted attestation trust level" "$(pg "d['data']['trust_level']")" "hosted_runner_signed"
ck "hosted attestation signs evidence" "$(pg "d['data']['subject_count']")" "1"
ck "hosted attestation inserts signature" "$(pg "d['data']['signature_count']")" "1"

F doctor
ck "doctor remains healthy after hosted attestation" "$(pg "d['data']['ok']")" "True"
ck "doctor reports hosted runner key" "$(pg "len(d['data']['signature_key_summary']['hosted_runner_key_fingerprints'])")" "1"

F accept
ck "accept succeeds after hosted attestation" "$(pg "d['status']")" "success"
ck "accepted decision" "$(pg "d['data']['decision']")" "accepted"

F trust policy --export third_party_attested
ck "third-party export policy updates" "$(pg "d['data']['min_export_trust']")" "third_party_attested"
if F export branch hosted-third-party-should-fail; then
  third_party_code="<exported>"
else
  third_party_code="$(pg "d['errors'][0]['code']")"
fi
ck "third-party policy still fails closed" "$third_party_code" "TRUST_POLICY_UNMET"
ck "third-party failure includes attestation gap" "$(pg "any(i['subject_kind'] == 'attestation' and i['subject_id'] == 'third_party_attested' for i in d['errors'][0]['details']['signature_issues'])")" "True"

echo
echo "Hosted-runner attestation dogfood: PASS=$PASS FAIL=$FAIL"
if [ "$FAIL" -ne 0 ]; then
  printf 'Failures:\n' >&2
  printf ' - %s\n' "${FAILS[@]}" >&2
  exit 1
fi

echo "Hosted-runner attestation dogfood checks passed."
