#!/usr/bin/env bash
# Validates benchmarks/claims.registry.jsonl's structural integrity: every
# id is unique, every supersedes/superseded_by reference resolves to a real
# entry, the supersede relationship is bidirectional (A supersedes B <=> B's
# superseded_by is A), a "superseded" entry has superseded_by set, and
# raw_output_path points at a file that actually exists.
#
# Deliberately does NOT verify that calm_commit/corpus_commit/harness_commit
# are real, reachable git SHAs. Those fields are intentionally free text --
# a real entry in this registry already reads "X plus uncommitted fixes,
# later committed as Y", and `null` with an honest `evidence_gap` note is a
# valid, expected value for a benchmark run that predates this checkout's
# available history -- so a regex-based `git cat-file -e` check would
# either choke on that prose or false-negative on a corpus commit that
# belongs to an entirely different repo. Commit references here are
# reviewer-verified, not machine-verified; don't oversell this script by
# adding that check later without also handling those two real shapes.
#
# Usage:
#   scripts/check-claims-registry.sh   # print problems, exit 1 if any
set -euo pipefail
cd "$(dirname "$0")/.."

reg="benchmarks/claims.registry.jsonl"
if [ ! -f "$reg" ]; then
    echo "check-claims-registry: $reg missing" >&2
    exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "check-claims-registry: jq not found -- install it first" >&2
    exit 1
fi

fail=0
line_no=0
declare -A seen_ids
declare -A status_by_id
declare -A superseded_by_of
declare -A supersedes_of

while IFS= read -r line; do
    line_no=$((line_no + 1))
    [ -n "$line" ] || continue
    if ! echo "$line" | jq -e . >/dev/null 2>&1; then
        echo "check-claims-registry: line $line_no is not valid JSON" >&2
        fail=1
        continue
    fi

    id=$(echo "$line" | jq -r '.id // empty')
    status=$(echo "$line" | jq -r '.status // empty')
    raw_path=$(echo "$line" | jq -r '.raw_output_path // empty')
    superseded_by=$(echo "$line" | jq -r '.superseded_by // empty')

    if [ -z "$id" ]; then
        echo "check-claims-registry: line $line_no has no id" >&2
        fail=1
        continue
    fi
    if [ -n "${seen_ids[$id]:-}" ]; then
        echo "check-claims-registry: duplicate id \"$id\" (line $line_no and ${seen_ids[$id]})" >&2
        fail=1
    fi
    seen_ids[$id]=$line_no
    status_by_id[$id]="$status"
    superseded_by_of[$id]="$superseded_by"

    if [ "$status" != "current" ] && [ "$status" != "superseded" ]; then
        echo "check-claims-registry: $id has unknown status \"$status\" (must be current or superseded)" >&2
        fail=1
    fi
    if [ "$status" = "superseded" ] && [ -z "$superseded_by" ]; then
        echo "check-claims-registry: $id is superseded but has no superseded_by" >&2
        fail=1
    fi
    if [ -n "$raw_path" ] && [ ! -f "$raw_path" ]; then
        echo "check-claims-registry: $id's raw_output_path \"$raw_path\" does not exist" >&2
        fail=1
    fi

    while IFS= read -r sup; do
        [ -n "$sup" ] || continue
        supersedes_of["$id"]+="$sup "
    done < <(echo "$line" | jq -r '.supersedes[]? // empty')
done < "$reg"

# Cross-reference pass: every superseded_by/supersedes must point at a real
# id, and the relationship must be bidirectional.
for id in "${!seen_ids[@]}"; do
    sb="${superseded_by_of[$id]:-}"
    if [ -n "$sb" ]; then
        if [ -z "${seen_ids[$sb]:-}" ]; then
            echo "check-claims-registry: $id's superseded_by \"$sb\" does not resolve to a real id" >&2
            fail=1
        elif [[ " ${supersedes_of[$sb]:-} " != *" $id "* ]]; then
            echo "check-claims-registry: $id says superseded_by=$sb, but $sb's supersedes list doesn't include $id" >&2
            fail=1
        fi
    fi
    for sup in ${supersedes_of[$id]:-}; do
        if [ -z "${seen_ids[$sup]:-}" ]; then
            echo "check-claims-registry: $id's supersedes \"$sup\" does not resolve to a real id" >&2
            fail=1
        elif [ "${superseded_by_of[$sup]:-}" != "$id" ]; then
            echo "check-claims-registry: $id supersedes $sup, but $sup's superseded_by isn't $id" >&2
            fail=1
        fi
    done
done

if [ "$fail" -ne 0 ]; then
    echo "check-claims-registry: fix the problems above" >&2
    exit 1
fi
echo "check-claims-registry: ${#seen_ids[@]} claim(s) structurally consistent."
