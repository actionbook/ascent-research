#!/usr/bin/env bash
set -euo pipefail

skill="skills/ascent-research/SKILL.md"

test -f "$skill"
grep -Eq -- "--tag[ =]fact-check|--tag fact-check" "$skill"
grep -Eiq "live|sports|news|current roster|current-roster|current price|current-price" "$skill"
grep -q "fact_checks_total" "$skill"
grep -q "fact_check" "$skill"
