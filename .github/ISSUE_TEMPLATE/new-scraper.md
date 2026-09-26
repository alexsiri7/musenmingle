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

- robots.txt (fetched with the MuseNMingleBot UA), relevant lines:
  ```
  ```
- JSON-LD `Event` markup: yes / no / partial (where?)
- Pagination / detail pages:
- Time-zone quirks:
- Terms of use URL and what they allow (excerpt? images?) → `store_description` / `store_image`:
- Display name (for pages and image credits):

### Checklist

- [ ] Site name, events page URL and proposed source key
- [ ] robots.txt checked with the MuseNMingleBot UA; events pages allowed (paste the relevant lines)
- [ ] JSON-LD `Event` markup present? (listing and/or detail pages) — if not, which CSS selectors
- [ ] Pagination / detail pages and a per-run fetch cap
- [ ] Proposed `interval_minutes`
- [ ] Category mapping (exhibition / expo / community / talk / workshop) and what to skip
- [ ] Time-zone quirks verified against human-readable times
- [ ] Fixtures saved under `tests/fixtures/scrapers/<key>/`
- [ ] Snapshot test of normalised output committed and reviewed
- [ ] wiremock fetch test (incl. robots.txt) passes
- [ ] Source registered in `sources::build` + seed migration (new file)
- [ ] Seed sets `display_name` (shown on pages and in "Image: …" credits)
- [ ] Site's terms of use checked: `store_description` / `store_image` decided (default true only if the terms don't forbid it; restrictive terms → both false, facts + link only) and `policy_note` records the terms URL + date
- [ ] No image hotlinking or image fetching in the source (thumbnails come only from the thumbnailer); no truncation of descriptions in the source (upsert does the excerpt)
- [ ] No LLM parsing; all requests go through `FetchContext`
- [ ] **Or, if the site can't be used** (robots.txt disallows the events pages, the site blocks our bot, no usable event data, terms forbid it, events only render with JavaScript): close the issue as not planned and, in the same PR, add an `events.refused_sources` row via a NEW migration (registrable domain, name, URL, `reason_code`, `reason_text`, `checked_on`, link to this issue)
