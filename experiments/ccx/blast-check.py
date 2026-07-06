#!/usr/bin/env python3
"""ccx T2 spike: blast-radius predicate over a Forge proposal's changed paths.

Usage:
    forge propose --json | blast-check.py --allow 'src/**' --forbid 'src/main.ts'
    forge save --json    | blast-check.py --allow 'docs/**'

Reads any forge.cli.v0 envelope whose data carries `changed_paths` and tests
every path against the allow/forbid globs (fnmatch; `**` matches across
separators). Exit 0 = inside blast radius, 2 = violation(s), 1 = usage/parse
error. Deliberately trivial: the point of the spike is that Forge already
persists per-revision changed paths, so the whole blast-radius check is this
predicate plus a place to declare the allowlist.
"""
import argparse
import fnmatch
import json
import sys


def matches(path: str, pattern: str) -> bool:
    # fnmatch's `*` already crosses `/`; normalize `**` so authors can write
    # gitignore-style patterns.
    return fnmatch.fnmatch(path, pattern.replace("**", "*"))


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--allow", action="append", default=[], metavar="GLOB")
    ap.add_argument("--forbid", action="append", default=[], metavar="GLOB")
    args = ap.parse_args()
    if not args.allow:
        print("blast-check: at least one --allow glob required", file=sys.stderr)
        return 1

    try:
        envelope = json.load(sys.stdin)
    except json.JSONDecodeError as err:
        print(f"blast-check: stdin is not JSON: {err}", file=sys.stderr)
        return 1
    data = envelope.get("data", envelope)
    changed = data.get("changed_paths")
    if changed is None:
        print("blast-check: no changed_paths in payload", file=sys.stderr)
        return 1

    violations = []
    for path in changed:
        if any(matches(path, glob) for glob in args.forbid):
            violations.append((path, "forbidden"))
        elif not any(matches(path, glob) for glob in args.allow):
            violations.append((path, "outside allowlist"))

    report = {
        "revision": data.get("proposal_revision_id") or data.get("snapshot_id"),
        "changed_paths": changed,
        "allow": args.allow,
        "forbid": args.forbid,
        "violations": [{"path": p, "kind": k} for p, k in violations],
        "verdict": "violation" if violations else "within_blast_radius",
    }
    json.dump(report, sys.stdout, indent=1)
    print()
    return 2 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
