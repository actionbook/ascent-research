# ascent-research Roadmap

This roadmap tracks the next harness-engineering improvements after the
`0.3.0` release. The guiding principle is to move workflow discipline out of
skill prose and into stable CLI contracts: session is the durable event log,
harness is the replaceable orchestration layer, and tools are separately
auditable hands.

## Priority 0: Completion Protocol In The CLI

Goal: make "the research is complete" a deterministic CLI protocol instead of
a sequence the outer agent must remember.

- Add `ascent-research finish <slug> [--bilingual] [--open]`.
- Internally run `coverage -> synthesize -> audit`.
- Return non-zero unless coverage passes, synthesize succeeds, and audit is
  complete.
- Keep `coverage`, `synthesize`, and `audit` available as separate inspection
  commands.

Expected effect: Claude Code, Codex, and future harnesses can call one stable
completion interface and stop relying on skill text for the mandatory tail.

Spec: `specs/research-harness-finish-audit.spec.md`

## Priority 1: Audit Revalidates Coverage

Goal: make `audit` the final acceptance projection over both the event log and
current report readiness.

- Make `audit` run the same local coverage gate used by `synthesize`.
- Include `coverage.report_ready` and `coverage.report_ready_blockers` in JSON
  output.
- Mark `audit_status="incomplete"` when coverage fails, even if historical
  synthesis events exist.
- Preserve audit's read-only property: no event append, no render, no network.

Expected effect: if `session.md`, wiki pages, diagrams, or source references
drift after synthesis, final validation catches it.

Spec: `specs/research-harness-finish-audit.spec.md`

## Priority 2: Event Log Diagnostics

Goal: treat `session.jsonl` corruption as a first-class audit finding instead
of a stderr-only warning.

- Add a diagnostic event reader that returns valid events plus malformed line
  counts and parse diagnostics.
- Keep the existing tolerant reader for legacy callers that only need best
  effort.
- Make `audit` expose `event_log.malformed_lines`,
  `event_log.unknown_events`, and `event_log.parse_errors`.
- Block `audit_status="complete"` when the durable evidence log has skipped
  lines.

Expected effect: the session log becomes a trustworthy evidence store; loss of
evidence is visible to humans and harnesses.

Spec: `specs/research-harness-finish-audit.spec.md`

## Priority 3: Action-Level Trace

Goal: close the observability gap between loop-step aggregate counts and actual
mutations to `session.md` / wiki / diagrams.

- Add action-level trace events for loop actions.
- At minimum record action type, target, status, duration, and error summary.
- Cover `write_overview`, `write_section`, `write_aside`, `write_plan`,
  `write_diagram`, `write_wiki_page`, `append_wiki_page`, `digest_source`,
  `fact_check`, `add`, and `batch`.
- Keep summaries safe: no full provider text, credentials, or raw subprocess
  output.

Expected effect: audit can answer "what did the agent actually do?" without
diffing markdown by hand.

Future spec: `specs/research-loop-action-trace.spec.md`

## Priority 4: Claim Inventory Fact Checks

Goal: make dynamic factual validation proportional to the claims in the report,
not merely "at least one fact check exists".

- Add a claim inventory surface, either as loop action or derived session
  artifact.
- Require every high-risk claim in `fact-check` sessions to have a supported
  `FactChecked` event.
- Count unverified, refuted, uncertain, invalid-source, and undigested-source
  claims separately.
- Keep domain policy generic; do not hard-code NBA, Lakers, Rockets, or other
  topic-specific rules.

Expected effect: current-roster, news, market, latest-version, and other live
reports stop passing with only one token fact check.

Future spec: `specs/research-claim-inventory-fact-check.spec.md`

## Priority 5: Explicit Legacy Migration

Goal: keep `~/.actionbook/ascent-research/` as the canonical home while giving
upgraders a deterministic path out of the legacy root.

- Add `ascent-research migrate --from-legacy`.
- Copy or move old sessions and preset overrides from
  `~/.actionbook/research/` into `~/.actionbook/ascent-research/`.
- Detect slug conflicts and require explicit overwrite/skip policy.
- Emit a machine-readable migration summary.
- Keep legacy reads as read-only until the announced removal version.

Expected effect: v0.4 can remove legacy fallback without surprising existing
users.

Future spec: `specs/research-legacy-migration.spec.md`

## Priority 6: Runtime Version Gates

Goal: make `doctor` validate tool contracts, not just tool presence.

- Parse `postagent --version` and `actionbook --version`.
- Enforce minimum versions required by the current route and dry-run contracts.
- Surface version mismatch as required doctor failure with install hints.
- Keep provider feature checks separate from hand/tool version checks.

Expected effect: failures like "postagent 0.3.1 requires token placeholder for
public URL dry-run" become predictable preflight errors instead of runtime
surprises.

Future spec: `specs/research-runtime-version-gates.spec.md`

## Release Grouping

- `0.3.1`: Priority 0, 1, 2. This is the safest patch/minor follow-up because
  it tightens validation without changing the report model.
- `0.4.0`: Priority 3 and 4. These extend the event schema and fact-check
  model, so they deserve a larger release boundary.
- `0.4.x`: Priority 5 and 6. Migration and version gates can ship once the new
  completion protocol is stable.

