# actionbook browser hand

## Intent

`actionbook` must behave as a stable browser hand for agent harnesses. A caller
must be able to determine whether the browser daemon is available, start or
restart it intentionally, inspect failure logs, and receive structured fetch
artifacts without guessing from prose errors.

## Stable Interface

### `actionbook browser doctor --json`

Returns a JSON envelope with independent checks:

- `binary_found`
- `daemon_running`
- `daemon_pid`
- `socket_path`
- `cdp_endpoint`
- `browser_binary`
- `profile_dir`
- `proxy`
- `last_error`
- `log_path`
- `recover_command`

It must not require an already-running daemon to report useful diagnostics.

### `actionbook browser doctor --start --json`

If the daemon is not running, start it, then verify:

- daemon process exists
- socket or control endpoint is reachable
- CDP endpoint is reachable
- a blank page can be created and closed

Failure returns a structured `error.code`, `error.details.log_path`, and
`error.details.recover_command`.

### `actionbook browser restart --json`

Stops the current daemon if present, cleans stale control files, starts a fresh
daemon, verifies CDP, and returns the new pid/endpoint/log path. It must not
delete user data outside the browser hand's managed profile/cache directories.

### `actionbook browser logs --tail <N>`

Prints recent daemon/browser logs. JSON mode returns:

- `log_path`
- `lines`
- `truncated`

## Browser Fetch Artifact Contract

Every browser fetch operation consumed by a harness should expose:

- `requested_url`
- `observed_url`
- `status`
- `title`
- `content_bytes`
- `readable_bytes`
- `html_path`
- `screenshot_path`
- `proxy_used`
- `duration_ms`
- `error_code`
- `recover_hint`

## Acceptance Criteria

1. With no daemon running, `actionbook browser doctor --json` succeeds and
   reports `daemon_running=false`.
2. With no daemon running, `actionbook browser doctor --start --json` starts a
   daemon and returns a reachable CDP endpoint.
3. With a stale socket, `actionbook browser restart --json` recovers without
   manual cleanup.
4. On Chrome/profile/proxy/CDP failure, `logs --tail` and the doctor envelope
   contain enough information for an upstream harness to produce a recovery
   hint.
5. Browser fetch output can be persisted directly into a session event log
   without scraping human-readable stderr.
