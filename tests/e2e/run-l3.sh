#!/usr/bin/env bash
# L3 — Full research loop covering BOTH executor paths: API (postagent)
# + Browser (actionbook 3-step). Adds 1 real blog URL to exercise the
# browser subprocess contract end-to-end.
#
# Expect ~2 minutes (browser fetch is the slow part).
set -u

RESEARCH_ROOT=/Users/zhangalex/Work/Projects/actionbook/research-api-adapter
RESEARCH_BIN="$RESEARCH_ROOT/target/release/research"
POSTAGENT_BIN="${POSTAGENT_BIN:-/Users/zhangalex/Work/Projects/actionbook/postagent/packages/postagent-core/target/debug/postagent-core}"
ACTIONBOOK_BIN="${ACTIONBOOK_BIN:-/Users/zhangalex/Work/Projects/actionbook/actionbook/packages/cli/target/release/actionbook}"
JSON_UI_BIN="${JSON_UI_BIN:-json-ui}"

TEST_HOME=$(mktemp -d -t research-l3-XXXXXX)
export ACTIONBOOK_RESEARCH_HOME="$TEST_HOME"
export POSTAGENT_BIN ACTIONBOOK_BIN JSON_UI_BIN SYNTHESIZE_NO_OPEN=1

trap 'rc=$?; [ $rc -eq 0 ] && rm -rf "$TEST_HOME" || echo "artifacts kept in $TEST_HOME"; exit $rc' EXIT

pass() { printf "  \033[32m✅ %s\033[0m\n" "$*"; }
fail() { printf "  \033[31m❌ %s\033[0m\n" "$*"; exit 1; }
section() { printf "\n\033[1m=== %s ===\033[0m\n" "$*"; }

section "Setup"
"$RESEARCH_BIN" new "E2E L3 mixed sources" --slug l3 --preset tech --json >/dev/null || fail "new"
pass "session l3 created"

section "L3.1 — API source (HN item via postagent)"
"$RESEARCH_BIN" add "https://news.ycombinator.com/item?id=1" --slug l3 --json > /tmp/l3_hn.json
jq -e '.ok == true and .data.route_decision.executor == "postagent"' /tmp/l3_hn.json >/dev/null \
  && pass "HN accepted via postagent" || { cat /tmp/l3_hn.json; fail "HN failed"; }

section "L3.2 — Browser source (real blog, actionbook 3-step)"
BLOG_URL="https://corrode.dev/blog/async/"
echo "  fetching $BLOG_URL (may take 15-30s) ..."
"$RESEARCH_BIN" add "$BLOG_URL" --slug l3 --json > /tmp/l3_blog.json 2>&1
ROUTE_EXEC=$(jq -r '.data.route_decision.executor // .error.details.route_decision.executor // "unknown"' /tmp/l3_blog.json)
if [ "$ROUTE_EXEC" != "browser" ]; then
  cat /tmp/l3_blog.json
  fail "expected browser executor, got '$ROUTE_EXEC'"
fi
pass "routed to browser"

if jq -e '.ok == true' /tmp/l3_blog.json >/dev/null; then
  SMELL=$(jq -r '.data.smell_pass' /tmp/l3_blog.json)
  BYTES=$(jq -r '.data.bytes' /tmp/l3_blog.json)
  TRUST=$(jq -r '.data.trust_score' /tmp/l3_blog.json)
  RAW_PATH=$(jq -r '.data.raw_path' /tmp/l3_blog.json)
  pass "browser fetch accepted: $BYTES bytes, trust $TRUST, smell=$SMELL"
  [ -f "$TEST_HOME/l3/$RAW_PATH" ] && pass "raw file exists: $RAW_PATH" || fail "missing raw"
  if [ "$BYTES" -ge 500 ]; then
    pass "article body ≥ 500 bytes (smell threshold ok)"
  else
    fail "article body too short: $BYTES"
  fi
else
  REASON=$(jq -r '.error.details.reject_reason // "none"' /tmp/l3_blog.json)
  cat /tmp/l3_blog.json
  fail "browser path rejected: $REASON"
fi

section "L3.3 — session.jsonl has both executors"
JSONL="$TEST_HOME/l3/session.jsonl"
EXECUTORS=$(grep -o '"executor":"[a-z]*"' "$JSONL" | sort -u)
echo "$EXECUTORS" | grep -q '"executor":"postagent"' && pass "postagent event present" || fail "no postagent event"
echo "$EXECUTORS" | grep -q '"executor":"browser"' && pass "browser event present" || fail "no browser event"

section "L3.4 — sources list shows both"
"$RESEARCH_BIN" sources l3 --json > /tmp/l3_sources.json
COUNT=$(jq '.data.accepted | length' /tmp/l3_sources.json)
[ "$COUNT" = "2" ] && pass "2 accepted sources" || { cat /tmp/l3_sources.json; fail "got $COUNT accepted"; }
EXECUTORS_LIST=$(jq -r '[.data.accepted[].executor] | sort | unique | join(",")' /tmp/l3_sources.json)
[ "$EXECUTORS_LIST" = "browser,postagent" ] && pass "both executors in sources list" \
  || fail "executors list: $EXECUTORS_LIST"

section "L3.5 — synthesize report with Methodology breakdown"
cat > "$TEST_HOME/l3/session.md" <<'MD'
# Research: E2E L3 mixed sources

## Objective
Full stack validation — API + Browser paths in one session.

## Preset
tech

## Sources
<!-- research:sources-start -->
_(auto)_
<!-- research:sources-end -->

## Overview
Proves the research CLI can mix postagent API fetches and actionbook
browser article reads in a single session, with each source getting the
correct trust_score and routing category.

## Findings
### API + Browser coexist
HN item (trust 2.0) and a long-form blog article (trust 1.0-1.5) land
in the same session.jsonl with distinct executor tags.

### Subprocess contract stable
Neither postagent's raw-stdout shape nor actionbook's JSON envelope
shape surprised our parse logic this pass.

## Notes
L3 closes the loop started by the spec suite — every layer from
subcommand dispatch through json-ui render was exercised with the
production binaries.
MD

"$RESEARCH_BIN" synthesize l3 --json > /tmp/l3_syn.json
jq -e '.ok == true' /tmp/l3_syn.json >/dev/null \
  && pass "synthesize ok" || { cat /tmp/l3_syn.json; fail "synthesize failed"; }

METHOD=$(jq -r '[.children[] | select(.props.title == "Methodology") | .children[0].props.data][0]' \
  "$TEST_HOME/l3/report.json")
echo "Methodology data: $METHOD"
ACCEPTED_PA=$(echo "$METHOD" | jq '.accepted_postagent')
ACCEPTED_BR=$(echo "$METHOD" | jq '.accepted_browser')
[ "$ACCEPTED_PA" = "1" ] && pass "methodology shows 1 postagent" || fail "wanted 1 postagent, got $ACCEPTED_PA"
[ "$ACCEPTED_BR" = "1" ] && pass "methodology shows 1 browser" || fail "wanted 1 browser, got $ACCEPTED_BR"

section "L3.6 — report.html produced"
[ -f "$TEST_HOME/l3/report.html" ] && {
  HTML_BYTES=$(wc -c < "$TEST_HOME/l3/report.html" | tr -d ' ')
  pass "report.html $HTML_BYTES bytes"
} || fail "no report.html"

section "L3.7 — INSPECT REAL CONTENT (not just envelopes)"
echo ""
echo "--- HN item 1 body (first 300 chars) ---"
HN_RAW=$(jq -r '.data.accepted[] | select(.kind == "hn-item") | .raw_path' /tmp/l3_sources.json)
head -c 300 "$TEST_HOME/l3/$HN_RAW"
echo ""
echo ""
HN_TITLE=$(jq -r '.title // ""' "$TEST_HOME/l3/$HN_RAW" 2>/dev/null)
HN_AUTHOR=$(jq -r '.by // ""' "$TEST_HOME/l3/$HN_RAW" 2>/dev/null)
HN_SCORE=$(jq -r '.score // ""' "$TEST_HOME/l3/$HN_RAW" 2>/dev/null)
echo "  parsed: by=$HN_AUTHOR, score=$HN_SCORE, title=$HN_TITLE"
[ -n "$HN_TITLE" ] && pass "HN item has title: '$HN_TITLE'" || fail "HN item lacks title"

echo ""
echo "--- Browser article (first 500 chars of extracted text) ---"
BR_RAW=$(jq -r '.data.accepted[] | select(.executor == "browser") | .raw_path' /tmp/l3_sources.json)
# browser raw is the actionbook text --json envelope
jq -r '.data.value // ""' "$TEST_HOME/l3/$BR_RAW" 2>/dev/null | head -c 500
echo "..."
echo ""
OBSERVED_URL=$(jq -r '.context.url' "$TEST_HOME/l3/$BR_RAW")
echo "  observed url: $OBSERVED_URL"
if echo "$OBSERVED_URL" | grep -q "corrode.dev"; then
  pass "browser landed on corrode.dev (host match confirmed)"
else
  fail "browser didn't match requested host: $OBSERVED_URL"
fi

echo ""
echo "--- session.md rendered sources block ---"
awk '/<!-- research:sources-start -->/,/<!-- research:sources-end -->/' "$TEST_HOME/l3/session.md"

echo ""
echo "--- report.html sample (first 1KB of body content) ---"
# show a glimpse of the rendered HTML to prove it has real content
grep -oE '<(h[1-6]|p|li|a)[^>]*>[^<]{10,}' "$TEST_HOME/l3/report.html" | head -10 || true

section "Summary"
printf "\n\033[32mL3 all green — both executor paths validated end-to-end.\033[0m\n"
printf "Real content confirmed:\n"
printf "  • HN item: '%s' by %s, score %s\n" "$HN_TITLE" "$HN_AUTHOR" "$HN_SCORE"
printf "  • Blog: %s byte extracted text from %s\n" \
  "$(jq -r '.bytes' /tmp/l3_blog.json)" \
  "$OBSERVED_URL"
printf "  • Report HTML: %s bytes\n" "$HTML_BYTES"
printf "\nSession kept for inspection (run \`open %s/l3/report.html\`):\n  %s\n\n" \
  "$TEST_HOME" "$TEST_HOME/l3"

# Keep artifacts this run so user can open the HTML
trap - EXIT
printf "\033[33mArtifacts retained for inspection.\033[0m\n"
