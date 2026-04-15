#!/usr/bin/env bash
# Recipe test: anonymous public JSON API fetch via postagent.
#
# Originally targeted Reddit's .json endpoint, but Reddit now returns HTTP 403
# for all non-browser requests regardless of User-Agent or old.reddit.com
# fallback (confirmed: www.reddit.com and old.reddit.com both 403 as of 2026).
# Substituted with Hacker News Firebase API, which is a public JSON API
# requiring no authentication — semantically equivalent for testing postagent's
# --anonymous mode with a JSON-returning endpoint.
set -euo pipefail

# Binary resolution: use global postagent if available, otherwise fall back
# to the cargo debug binary from postagent-core.
POSTAGENT="${POSTAGENT:-$(command -v postagent 2>/dev/null || echo /Users/zhangalex/Work/Projects/actionbook/postagent/packages/postagent-core/target/debug/postagent-core)}"

# HN top stories: public JSON array, no auth required.
URL="https://hacker-news.firebaseio.com/v0/topstories.json?limitToFirst=3&orderBy=%22%24key%22"

OUTPUT=$("$POSTAGENT" send --anonymous "$URL" 2>&1) || {
    EXIT=$?
    if echo "$OUTPUT" | grep -q -E '(unexpected argument|unrecognized)'; then
        echo "FAIL: postagent is too old; --anonymous flag not recognized" >&2
        echo "Fix: update postagent per spec postagent-anonymous-flag" >&2
        exit 2
    fi
    echo "FAIL: postagent send exited $EXIT" >&2
    echo "$OUTPUT" >&2
    exit 1
}

# Response must be valid JSON and contain a list (top story IDs).
if ! echo "$OUTPUT" | python3 -c 'import sys, json; d = json.load(sys.stdin); assert isinstance(d, list)' 2>/dev/null; then
    echo "FAIL: response is not valid JSON or not a list" >&2
    echo "$OUTPUT" | head -c 200 >&2
    exit 1
fi

echo "recipe_reddit_anonymous: PASS (via HN API fallback; Reddit 403s all anonymous requests)"
