Actionbook MCP tools — three extra actions you can emit when V2 backend is
available. These let you actively probe the curated site catalog and run
custom Playwright-style scripts mid-loop, instead of relying on the
page-blind `add` / `batch` fetch path for sites that need login cookies,
GraphQL hooks, or catalog-curated extraction.

  { "type": "actionbook_search", "query": "tweet timeline", "host": "x.com" }
  { "type": "actionbook_manual", "site": "x_com", "group": "search", "action": "search_timeline" }
  { "type": "actionbook_run_code", "url": "https://x.com/elonmusk",
    "script": "async (page) => ({ text: document.body.innerText.slice(0, 8000) })",
    "timeout_ms": 30000 }

When to use which:
- `actionbook_search` — you don't yet know what the catalog offers for a
  host. Returns up to 5 `{site, group, action, summary}` candidates as a
  compact JSON string in next iteration's `recent_actionbook_results`.
- `actionbook_manual` — you know the catalog triple and want the full
  manual markdown. Double-effect: the manual lands in BOTH this turn's
  `recent_actionbook_results` (for the LLM) AND the session wiki (so
  resume / audit can re-read it later). If a wiki page with that slug
  already exists it's silently kept; LLM context still gets the fresh
  manual body.
- `actionbook_run_code` — you need to drive the page yourself (e.g. wait
  for SPA hydration, scrape a GraphQL XHR, follow a logged-in flow).
  The script runs against the user's real Chrome session via the V2
  extension — it shares cookies, identity, and rate limits with the
  human's everyday browser. DO NOT abuse: no infinite loops, no logging
  into accounts that aren't the user's, no mutating actions
  (post/like/follow) unless the human explicitly asked. The function
  must be an async function expression — V2 wraps as `return (...)` and
  rejects non-function evaluations.

Token budgets — `recent_actionbook_results` enforces these per-call:
- `actionbook_search`   ≤ 2 KB  (top K hits, K trimmed to fit)
- `actionbook_manual`   ≤ 8 KB  (markdown body, truncated with marker)
- `actionbook_run_code` ≤ 16 KB (text field; result_json ≤ 4 KB extra)

When the budget triggers truncation, the value ends with the literal
marker `[…truncated to <N>KB…]`. Treat the marker as a signal that the
underlying content is real and longer — don't conclude the page is
short. Re-issue a tighter `search` / `manual` / `run_code` to pull the
piece you actually need.

Per-iteration caps — anything past these gets rejected this turn:
- `actionbook_search`   ≤ 5 / iter
- `actionbook_manual`   ≤ 5 / iter
- `actionbook_run_code` ≤ 3 / iter

Rejected actions consume your `max_actions` budget and surface as
`actionbook_per_loop_cap_exceeded` warnings. Caps reset each iteration.

Fail-soft semantics — when the backend rejects the call (extension
offline, MCP transport error, EVAL_FAILED, etc.) you get back
`recent_actionbook_results: [{error: "...", recoverable: true,
action_type: "..."}]`. `recoverable: true` is the contract: the loop
keeps running, and YOU pick an alternative action next turn (try a
different search query, fall back to a plain `add`, give up the
exploration). Treat the error message as ground truth, not as something
to override.

`run_code` inner `timeout_ms` is clamped `[5000, 60000]`. Default is
30000. The clamp is tighter than the fetch path's 85 s default because
LLM-authored scripts have higher tail risk; if you genuinely need more
time, split the work across two `run_code` actions (or fall back to a
catalog `manual`).

`--dry-run` mode skips MCP execution entirely. You will see a synthetic
empty `recent_actionbook_results` next turn (NOT an error). This is
by-design: dry-run plans the action vocabulary without paying the
network cost. If your reasoning depends on actionbook output, branch on
"did I just dry-run?" implicitly — the orchestrator will tell you in
the prompt.
