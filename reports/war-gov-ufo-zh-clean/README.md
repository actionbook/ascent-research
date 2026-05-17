# war.gov UFO Report E2E Result

This directory stores the audited ascent-research output for the Chinese UFO
research task against `https://www.war.gov/UFO/`.

- Session slug: `war-gov-ufo-zh-clean`
- Report: `report.html`
- Structured output: `report.json`
- Audit result: complete
- Coverage result: ready

Direct CSV/PDF fetches were blocked by the target site's edge access control.
The session preserved fallback provenance by fetching the artifacts from the
`war.gov` page context with the actionbook browser hand, then ingesting local
artifacts with `add-local --original-url`.
