#!/usr/bin/env bash
set -euo pipefail
TARGET="${1:-$HOME/.claude/skills/active-research/SKILL.md}"
COUNT=$(grep -c -E 'browser +fetch' "$TARGET" || true)
echo "$COUNT occurrences"
if [[ "$COUNT" -ne 0 ]]; then
    grep -n -E 'browser +fetch' "$TARGET" >&2
    exit 1
fi
