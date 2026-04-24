#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ "${1:-}" == "--self-test-missing-asset" ]]; then
  bad_list="${TMPDIR:-/tmp}/ascent-research-bad-package-list.$$"
  trap 'rm -f "$bad_list"' EXIT
  printf '%s\n' \
    "README.md" \
    "templates/rich-report.html" \
    "templates/rich-report.README.md" \
    "src/route/rules.rs" \
    "src/commands/audit.rs" \
    "src/commands/doctor.rs" >"$bad_list"

  if ASCENT_PACKAGE_LIST_FILE="$bad_list" ASCENT_SKIP_PACKAGE_VERIFY=1 "$0"; then
    echo "expected missing package asset check to fail" >&2
    exit 1
  fi
  exit 0
fi

flags=(-p ascent-research)
if [[ "${STRICT_CLEAN:-0}" != "1" ]]; then
  flags+=(--allow-dirty)
fi
if [[ "${CARGO_ONLINE:-0}" != "1" ]]; then
  flags+=(--offline)
fi

tmp="${TMPDIR:-/tmp}/ascent-research-package-list.$$"
trap 'rm -f "$tmp"' EXIT

if [[ -n "${ASCENT_PACKAGE_LIST_FILE:-}" ]]; then
  cp "$ASCENT_PACKAGE_LIST_FILE" "$tmp"
else
  cargo package "${flags[@]}" --list >"$tmp"
fi

required_files=(
  "README.md"
  "presets/tech.toml"
  "presets/sports.toml"
  "templates/rich-report.html"
  "templates/rich-report.README.md"
  "src/route/rules.rs"
  "src/commands/audit.rs"
  "src/commands/doctor.rs"
)

for file in "${required_files[@]}"; do
  if ! grep -Fxq "$file" "$tmp"; then
    echo "missing packaged file: $file" >&2
    exit 1
  fi
done

grep -Fq 'default = ["autoresearch"]' packages/research/Cargo.toml
grep -Fq 'provider-claude = ["autoresearch", "dep:cc-sdk"]' packages/research/Cargo.toml
grep -Fq 'cargo install ascent-research --features "provider-claude provider-codex"' skills/ascent-research/SKILL.md
grep -Fq 'npm install -g postagent @actionbookdev/cli' skills/ascent-research/SKILL.md
grep -Fq 'ascent-research --json doctor' skills/ascent-research/SKILL.md
grep -Fq 'ascent-research --json audit' skills/ascent-research/SKILL.md

if [[ "${ASCENT_SKIP_PACKAGE_VERIFY:-0}" != "1" ]]; then
  cargo package "${flags[@]}"
fi
