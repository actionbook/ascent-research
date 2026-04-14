#!/usr/bin/env bash
# Asserts that the "Navigation Pattern" section contains an innerText note.
set -euo pipefail
TARGET="${1:-$HOME/.claude/skills/active-research/SKILL.md}"
# Extract content from "## Navigation Pattern" up to the next "## " header.
SECTION=$(awk '/^## Navigation Pattern/{flag=1; next} /^## /{flag=0} flag' "$TARGET")
if echo "$SECTION" | grep -q -E 'innerText'; then
    echo "innerText note present"
else
    echo "innerText note missing from Navigation Pattern section" >&2
    exit 1
fi
