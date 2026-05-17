# RFC: Cross-tool session sharing (actionbook ↔ postagent)

**Status**: Draft / open
**Originator**: ascent-research (see `specs/actionbook-v2-mcp-backend.spec.md` for context)
**Affected projects**: actionbook (Chrome extension + edge MCP), postagent (CLI)
**Date**: 2026-05-17

## Why this RFC is in the ascent-research repo

ascent-research is the first downstream consumer that needs both tools to
share identity. The RFC originates here as a design record; the actual
implementation lives in actionbook and postagent. **Both Part A and Part B
must ship before ascent-research can use this** — they will be tracked as
external dependencies in ascent's roadmap, not implemented here.

This document is split into two halves that are intended to be copy-pasted
verbatim as separate GitHub issues in their respective repositories. The
introduction above (this section) provides shared context; readers landing
on a single issue can ignore it.

## Pattern in one diagram

```
   User logs in ONCE in Chrome (actionbook profile):
   github.com, x.com, notion.so, internal-saas.example.com, ...
                          │
                          │  session cookies in Chrome cookie jar
                          ▼
   ┌────────────────────────────────────────────────────────┐
   │ actionbook Chrome extension  (host_permissions: <all>) │
   │ NEW: chrome.cookies.getAll({url}) → JSON via WSS       │
   └────────────────────────┬───────────────────────────────┘
                            │  `actionbook browser export-session <origin>`
                            ▼
   ┌────────────────────────────────────────────────────────┐
   │ postagent CLI                                          │
   │ NEW: `postagent auth <site> --from-cookies actionbook` │
   │   → stores as kind:"cookies" credential                │
   └────────────────────────┬───────────────────────────────┘
                            │  Cookie: <session-jar>
                            ▼
   ascent-research (and any other postagent consumer) makes
   authenticated requests with NO additional OAuth dance
```

Identity flows in one direction: Chrome → actionbook → postagent →
callers. Refresh is manual; see "Out of scope" in each part.

---

# Part A — File at `actionbook/actionbook-cloud` (or the extension repo)

> **Copy from here to the next horizontal rule.**

## Title

`RFC: browser cookie/session export command for sharing logged-in identity across tools`

## Motivation

A typical actionbook user logs into a dozen sites in Chrome exactly once —
GitHub, X, Notion, an internal SaaS dashboard, corporate Jira. Every other
tool that wants to act on those sites today must independently
authenticate: `postagent auth github` runs its own OAuth device flow,
`ascent-research` requires its own `ak_*` API token, ad-hoc Rust processes
need yet another credential file each.

This is wasteful, and worse, it is **impossible** for sites with no public
OAuth flow (Notion's web app, most internal SaaS, most customer dashboards).
The user has already proved their identity to those sites in Chrome; we
just need a way to export that proof to programmatic consumers.

actionbook is uniquely positioned to do this: its extension already holds
`host_permissions: ["<all_urls>"]` and exposes a WSS bridge to the edge
MCP. Adding cookie export is a small, additive change that turns one
browser login into N CLI authentications.

## Proposed command surface

```
actionbook browser export-session <origin> --format <fmt>

  fmt = cookies-json  (default)  → SetCookie-style JSON list
      | netscape                 → Netscape cookie file (curl-compatible)
      | header-line              → "Cookie: a=b; c=d" single header value

  Examples:
    actionbook browser export-session https://github.com --format cookies-json
    actionbook browser export-session https://x.com --format header-line
    actionbook browser export-session https://notion.so --format netscape \
      > ~/.config/postagent/cookies/notion.txt
```

### Output formats

**`cookies-json`** (default, machine-friendly):

```json
[
  {
    "name": "user_session",
    "value": "abc123…",
    "domain": ".github.com",
    "path": "/",
    "expirationDate": 1779062400.0,
    "httpOnly": true,
    "secure": true,
    "sameSite": "lax"
  },
  …
]
```

This is intentionally close to Chrome's own `chrome.cookies.Cookie` shape so
consumers don't have to invent a new schema.

**`netscape`** (compat with `curl --cookie-jar`, `wget`, many scrapers):

```
# Netscape HTTP Cookie File
.github.com	TRUE	/	TRUE	1779062400	user_session	abc123…
```

**`header-line`** (one-shot use in shell pipelines):

```
Cookie: user_session=abc123…; logged_in=yes
```

Header-line drops domain/expiration/flags and is **only safe for a single
request to the origin you exported from**. Intended for
`curl -H "$(actionbook browser export-session https://x.com -f header-line)" …`.

## Security model

- **Auth**: Requires the same MCP user that owns the extension WSS
  connection — already enforced by the existing handshake. No new auth
  surface.
- **Per-origin scoping**: Exactly one `<origin>` per call. No `--all`, no
  wildcards. Exporting "every cookie in the browser" is explicitly out of
  scope.
- **HttpOnly cookies INCLUDED**. This is the whole point — session tokens
  live in HttpOnly cookies. Documented prominently.
- **Audit log**: Each export writes a line (timestamp, origin, requesting
  MCP user) to the extension's existing audit log; popup UI can show
  "recent exports" so the human can spot exfiltration.
- **Expiration metadata**: `cookies-json` includes `expirationDate` so
  consumers can warn the user when re-export is needed.

### Threats considered

- *Malicious MCP client drains cookies*: mitigated by per-origin scoping
  plus audit log — cannot be done silently.
- *Cookies leaked via shell history*: documented; recommend `umask 077` and
  piping to file rather than echoing.
- *Stale cookies persisting in downstream tools*: out of scope here, see
  Part B.

## Extension implementation sketch

`host_permissions: ["<all_urls>"]` already covers `chrome.cookies`. The
new code path is small:

```js
// ServiceWorker (background)
async function exportSession(origin) {
  const cookies = await chrome.cookies.getAll({ url: origin });
  return cookies.map(c => ({
    name: c.name, value: c.value, domain: c.domain, path: c.path,
    expirationDate: c.expirationDate,  // undefined for session cookies
    httpOnly: c.httpOnly, secure: c.secure, sameSite: c.sameSite,
  }));
}
```

Format conversion (cookies-json → netscape / header-line) happens CLI-side,
not in the extension, to keep the manifest-permission surface minimal.

### Why not implement this in the extension's popup UI directly

Popups are *human* workflows — they require a click. The point of this RFC
is that agents (Claude Code, Codex, ascent-research, arbitrary scripts)
need **programmatic** access. A popup button doesn't help an agent running
headless overnight, and it can't compose with shell pipelines like
`actionbook browser export-session … | postagent auth … --from-cookies -`.

## Alternatives considered

- **Run the agent inside the same browser profile**. Agents are
  Rust/Python/Node processes, not extensions. Even browser-driving
  frameworks (Playwright, agent-browser) want raw cookies for the 95% of
  work that doesn't need a full browser.
- **Use Chrome DevTools Protocol from postagent directly**. Requires Chrome
  started with `--remote-debugging-port`, exposes far more than cookies,
  and bypasses the MCP auth boundary the extension already enforces.
- **Have each tool reimplement OAuth**. Status quo; the problem.

## Out of scope for this RFC

- **Re-importing cookies into Chrome** (set-cookie from CLI). One-way only.
- **Session refresh tokens / OAuth refresh flows**. Cookies are exported as
  they are; refresh is the consumer's problem.
- **Cross-profile cookie transfer**. We export from the actionbook profile
  only. If the user has a separate "work Chrome" profile, that's a separate
  install.
- **Per-cookie path filtering**. Export is at origin granularity; if a site
  has cookies under `/admin` and `/`, the consumer gets both.

## Compatibility

Additive only. Existing `actionbook browser` subcommands are unchanged.
There is no new permission to grant in the manifest (the existing
`<all_urls>` host permission already covers `chrome.cookies`).

## Open questions

- Should the JSON output include `storeId` for cookies in incognito /
  container tabs? Probably no for v1 — the actionbook profile is a single
  store by convention.
- Should we offer a `--touch` flag that updates Chrome's "last accessed"
  timestamp on the exported cookies so they don't get garbage-collected?
  Defer to v2 if anyone reports session loss.

---

# Part B — File at `actionbook/postagent`

> **Copy from here to the end of the file.**

## Title

`RFC: import authenticated session from actionbook (or generic cookie source) for a site`

## Motivation

`postagent auth <site>` today does an OAuth dance per site. Two problems:

1. **Tedious when the user already logged in elsewhere**. The user logged
   into GitHub in their browser this morning; making them re-do the device
   flow is bad UX.
2. **Impossible for sites without OAuth**. Notion's web app, internal SaaS
   dashboards, customer portals, intranet Jira — no public OAuth flow.
   Today, postagent simply cannot authenticate against them, even though
   the user clearly *can* authenticate in their browser.

actionbook (see companion RFC, Part A) proposes a
`browser export-session <origin>` command that emits the Chrome cookie jar
for a single origin. This RFC defines the postagent side: how to *consume*
those cookies as a first-class credential kind.

## Proposed command surface

```
postagent auth <site> --from-cookies <source>

  source = actionbook                    → call `actionbook browser export-session`
         | -                              → read cookies-json from stdin
         | <path/to/cookies.json>        → read from file

  Examples:
    postagent auth github   --from-cookies actionbook
    postagent auth notion   --from-cookies actionbook
    cat exported.json | postagent auth jira --from-cookies -
    postagent auth myintranet --from-cookies ~/dump/intranet-cookies.json
```

Companion subcommand:

```
postagent auth <site> status

  Shows: credential kind, source, earliest cookie expiry, action hint.

  Example output:
    site:        github
    kind:        cookies
    source:      actionbook (imported 2026-05-17 09:12 UTC)
    expires:     2026-08-17 (in 92 days)
    action:      none
```

When the earliest cookie expires (or already has), `status` recommends
`postagent auth <site> --from-cookies actionbook` to refresh.

## postagent integration

postagent already has `postagent auth <site>` machinery for OAuth and
static-token credentials. This RFC adds a third `kind`:

```
existing:  kind: "Static"   { token: <string> }
existing:  kind: "OAuth"    { access_token, refresh_token, expires_at }
NEW:       kind: "Cookies"  { cookies: [ … ], imported_from, imported_at }
```

### Request materialization

On each outgoing request, postagent filters stored cookies by
`domain`/`path`/`secure` per RFC 6265, then sends them as a single
`Cookie:` header — in contrast to `Static` (`Authorization: Bearer …`)
and `OAuth` (same plus refresh-on-401). For `Cookies`, a 401/403 is **not**
auto-recovered; postagent surfaces an error pointing at the `status`
subcommand and suggests re-export.

## Coordination contract with actionbook

When `source = actionbook`, postagent shells out:

```
actionbook browser export-session <origin> --format cookies-json
```

…and reads stdout. This requires the actionbook RFC (Part A in the
cross-team RFC document) to be implemented.

### Site ↔ origin mapping

postagent identifies sites by short name (`github`, `notion`, `jira`),
not URL. The mapping to `<origin>` lives in postagent's existing site
registry. For `www.` vs apex ambiguity, postagent applies the same
normalization rule as ascent-research's `smell::normalize_host`: strip
leading `www.` for comparison, preserve the user's choice in the stored
credential entry.

### Fallback when actionbook is not installed

If `source = actionbook` and the `actionbook` binary is not on PATH,
postagent prints:

```
error: --from-cookies actionbook requires the actionbook CLI to be
       installed and connected to a Chrome extension.

       Alternatives:
         • Export cookies manually from your browser and pass via
           --from-cookies <path> or --from-cookies -
         • Install actionbook: https://…
```

…and exits non-zero. We do **not** silently degrade.

## Lifecycle

Cookies expire. postagent's responsibility:

- **Import**: record earliest `expirationDate` across imported cookies.
  Session cookies (no expiration) are treated as `import + 30 days` with a
  warning.
- **`status`**: show expiry and days remaining.
- **Request failure (401/403)**: structured error with the exact re-export
  command. No auto-retry.
- **Background jobs**: postagent does not do background refresh; consumers
  handle their own retry/backoff on 401.

## Security

- **Storage**: Imported cookies go into postagent's existing keychain-style
  secret store (macOS Keychain, Linux Secret Service, Windows Credential
  Manager) — the same place OAuth tokens live today. No new backend.
- **Source attribution**: Stored credential carries
  `imported_from: "actionbook" | "stdin" | "<path>"` and `imported_at:
  <ISO-8601>`, shown in `status` and logged on every use.
- **No re-export**: There is no `postagent auth <site> export-cookies`.
  If you want cookies back out, re-export from actionbook. Keeps the
  blast radius of a postagent compromise no worse than an actionbook
  compromise.
- **Permissions**: Stored credential files are `0600` on POSIX and
  equivalent on Windows; already enforced for OAuth tokens today.

## Alternatives considered

- **Embed a browser via Playwright**. Heavy dependency, requires Chrome at
  runtime, duplicates the auth surface actionbook already has.
- **Standardize on Netscape format only**. Widely supported by curl/wget
  but loses `sameSite` and doesn't round-trip cleanly. Accepted as one of
  three formats; default is cookies-json.
- **Make cookies a flavor of `Static` credential**. Conflates two very
  different request shapes (Cookie vs Authorization header). Cleaner as a
  distinct kind.

## Out of scope

- **Auto-refresh**. User re-runs `--from-cookies actionbook` on expiry; we
  do not drive a browser to renew automatically.
- **Per-cookie path matching at import time**. All origin cookies stored;
  RFC 6265 matching at request time picks the subset.
- **Cookie inspection**. `status` shows expiry and source but not values.
  To inspect, re-export from actionbook.
- **Cross-machine sharing**. Per-machine credentials only, same as today.

## Compatibility

Additive. Existing `postagent auth` flows (OAuth, static token) are
unchanged. The new credential kind `Cookies` is opt-in via the
`--from-cookies` flag.

## Open questions

- Accept `--origin <url>` override when the canonical origin differs from
  the site-registry default? Probably yes; defer to implementation.
- For multi-subdomain auth (`mail.google.com`, `drive.google.com`, …),
  one `postagent auth google` covering all, or one import per subdomain?
  Lean per-subdomain; revisit if painful.
