# ascent-research hand fallback

## Intent

`ascent-research` is the research harness. It must not assume its hands
(`postagent`, `actionbook`, local file ingest) are always healthy. When a hand
fails, the session must record what failed, why a fallback was selected, and how
the final source relates to the original URL.

## Doctor Contract

`ascent-research doctor --tool-smoke --json` reports tool health in layers:

- `postagent_version`
- `postagent_send_help`
- `postagent_public_dry_run`
- `actionbook_version`
- `actionbook_browser_doctor`
- `actionbook_browser_doctor_startable`
- `actionbook_cdp_connectable`
- `actionbook_page_fetchable`
- `actionbook_readable_extractable`

Until `actionbook` ships first-class doctor commands, the actionbook layers may
be compatibility checks that surface `unavailable` with a precise recovery
hint. A failed browser hand should not be collapsed into a single opaque
`DAEMON_NOT_RUNNING` check.

## Fallback Provenance

`add-local` supports fallback provenance:

```bash
ascent-research add-local ./cache \
  --slug my-session \
  --original-url https://example.com/source.html \
  --origin-tool curl \
  --origin-note "actionbook daemon unavailable; fetched with curl"
```

For each accepted local source, the session event log records:

- the local `file://` URL
- the original URL, when supplied
- the origin tool, when supplied
- the reason/note for fallback

The report may cite the local file URL for coverage, but audit must be able to
show the original URL provenance.

## Source Note Rules

Source notes are allowed only as explicit fallback artifacts. They must:

- list original URLs
- state why direct ingest failed
- be cited as lower-confidence derived sources in the report
- not be the only evidence for high-risk legal, medical, financial, or current
  fact claims unless the report labels the conclusion limited-confidence

## Acceptance Criteria

1. `doctor --tool-smoke --json` emits layered actionbook checks instead of only
   `actionbook_browser_list_sessions`.
2. `add-local --original-url ... --origin-tool ... --origin-note ...` succeeds
   and appends provenance events to `session.jsonl`.
3. `audit` summarizes fallback provenance events.
4. `coverage` still requires accepted sources to be cited in the body.
5. The ascent-research skill instructs agents to disclose hand failures and
   fallback provenance in the final answer.
