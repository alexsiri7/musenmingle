---
name: New scraper
about: Propose (or specify for an agent) a new event source
title: "New scraper: <site name>"
labels: new-scraper
---

Read `docs/adding-a-scraper.md` first. Every box must be ticked before the PR is merged.

**Site:** <name>
**Events page URL:** <https://...>
**Proposed source key:** `<kebab-case>`

### Findings

- robots.txt (fetched with the ThaleiaBot UA), relevant lines:
  ```
  ```
- JSON-LD `Event` markup: yes / no / partial (where?)
- Pagination / detail pages:
- Time-zone quirks:

### Checklist

- [ ] Site name, events page URL and proposed source key
- [ ] robots.txt checked with the ThaleiaBot UA; events pages allowed (paste the relevant lines)
- [ ] JSON-LD `Event` markup present? (listing and/or detail pages) — if not, which CSS selectors
- [ ] Pagination / detail pages and a per-run fetch cap
- [ ] Proposed `interval_minutes`
- [ ] Category mapping (exhibition / expo / community / talk / workshop) and what to skip
- [ ] Time-zone quirks verified against human-readable times
- [ ] Fixtures saved under `tests/fixtures/scrapers/<key>/`
- [ ] Snapshot test of normalised output committed and reviewed
- [ ] wiremock fetch test (incl. robots.txt) passes
- [ ] Source registered in `sources::build` + seed migration (new file)
- [ ] No LLM parsing; all requests go through `FetchContext`
- [ ] **Or, if the site can't be used** (robots.txt disallows the events pages, the site blocks our bot, no usable event data, terms forbid it): close the issue as not planned and, in the same PR, add an `events.refused_sources` row via a NEW migration (registrable domain, name, URL, `reason_code`, `reason_text`, `checked_on`, link to this issue)
