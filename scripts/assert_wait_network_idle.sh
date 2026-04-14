#!/usr/bin/env bash
# Fails if any `wait-idle` appears without being preceded by `wait ` (space).
# Good: `browser wait network-idle`
# Bad:  `browser wait-idle`
set -euo pipefail
TARGET="${1:-$HOME/.claude/skills/active-research/SKILL.md}"
COUNT=$(grep -c -E '(^|[^a-zA-Z-])wait-idle' "$TARGET" || true)
echo "$COUNT bare wait-idle occurrences"
if [[ "$COUNT" -ne 0 ]]; then
    grep -n -E '(^|[^a-zA-Z-])wait-idle' "$TARGET" >&2
    exit 1
fi
