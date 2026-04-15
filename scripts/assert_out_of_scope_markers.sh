#!/usr/bin/env bash
set -euo pipefail
TARGET="${1:-$HOME/.claude/skills/active-research/SKILL.md}"

BODY=$(awk '/^## API-First Sources/{flag=1; next} /^## /{flag=0} flag' "$TARGET")

MISSING=()
for kw in Tavily Exa Brave Reddit; do
    if ! echo "$BODY" | grep -q "$kw"; then
        MISSING+=("$kw")
    fi
done

if [[ "${#MISSING[@]}" -eq 0 ]]; then
    echo "all out-of-scope markers present"
else
    echo "FAIL: missing out-of-scope markers: ${MISSING[*]}" >&2
    exit 1
fi
