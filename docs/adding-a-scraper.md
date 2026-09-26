# Adding a scraper

Every source lives in `src/sources/<name>.rs`, implements the `Source` trait
and is registered in `sources::build` plus a seed row in a **new** migration
(`INSERT INTO events.sources ... ON CONFLICT (key) DO NOTHING`). Migrations are
append-only: never edit an existing one. A seed row looks like:

```sql
INSERT INTO events.sources (key, kind, base_url, domain, interval_minutes, enabled,
                            display_name, store_description, store_image, policy_note)
VALUES ('example-gallery', 'scraper', 'https://www.example.org', 'www.example.org', 1440, TRUE,
        'Example Gallery', TRUE, TRUE,
        'Terms (https://www.example.org/terms, checked 2026-10-01) don''t restrict listings')
ON CONFLICT (key) DO NOTHING;
```

`kind` is `scraper` for a venue's own site, `api` for a third-party API and
`aggregator` for a third-party listing site covering many venues (e.g.
ArtRabbit): an aggregator never takes precedence in a cross-source merge and
pages link to the venue's own site before it.

## Rules (non-negotiable)

1. **Fixture + snapshot test are mandatory.** Save the real HTML you fetched
   (listing and detail pages, and the site's `robots.txt`) under
   `tests/fixtures/scrapers/<source-key>/`, and add a test that parses the
   fixtures and snapshots the *normalised* output with `insta`
   (`insta::assert_json_snapshot!`). Commit the `.snap` files. Review every
   snapshot by hand before committing: it is the spec of what the scraper
   produces. See `tests/source_serpentine.rs`.
2. **JSON-LD first.** Prefer schema.org `Event` markup
   (`sources::jsonld::extract_events`). Fall back to CSS selectors with the
   `scraper` crate only for fields the JSON-LD lacks or gets wrong, and keep
   selectors as narrow as possible (scope to the page's main block, not
   "related events" teasers).
3. **No LLM parsing.** Parsing is deterministic Rust code, full stop.
4. **Only fetch through `FetchContext`.** It enforces the bot User-Agent,
   robots.txt (per origin, RFC 9309 semantics, Crawl-delay honoured) and the
   per-domain rate limit (default 1 request / 2 s). Never build your own
   `reqwest::Client` in a source.
5. **Check robots.txt yourself before starting** with the bot UA:
   `curl -A 'MuseNMingleBot/0.1.0 (+https://musenmingle.interstellarai.net/about#for-venues)' https://<site>/robots.txt`.
   If the events pages are disallowed, do not write the scraper.
6. **Be light.** Cap detail-page fetches per run, pick a sensible
   `interval_minutes` (daily is plenty for most venues) and keep the run
   comfortably under `SOURCE_TIMEOUT_SECS`.
7. **Skips are not errors.** Return `Ok(None)` from `normalise` for pages that
   are not in-scope events (online-only, open-ended programmes, out-of-scope
   categories). Report genuine per-page failures with `ctx.report_error`; they
   feed the health checker.
8. **Times:** use `normalise::parse_datetime` (honours offsets) or
   `parse_london_wall_clock` when a site prints local times with a bogus
   offset. Verify against the human-readable time on the page.
9. **Content policy: set it in the seed migration.** Muse & Mingle links out; it
   does not republish. In the seed row set `display_name` (the name shown
   on pages and in image credits, e.g. `'Barbican'`) and decide
   `store_description` / `store_image` from the site's terms of use, with a
   `policy_note` saying why (terms URL + date checked). Both default to
   true, which is right only when the terms don't forbid it; sites or APIs
   whose terms restrict reproduction (e.g. "personal use only", "no
   caching", "don't use images separately") are **facts + link only**:
   `store_description = FALSE, store_image = FALSE`. Scrapers still emit
   `description`/`image_url` as found — `repo::upsert_event` drops what the
   policy forbids and cuts descriptions to a 300-character excerpt, so don't
   truncate or strip in the scraper (and don't change `clean_description`).
10. **Never hotlink images.** Pages and JSON never use a source's image URL;
    the ingest thumbnailer (`src/thumbs.rs`) fetches each image once via
    `FetchContext`, stores a small credited thumbnail and the pages serve
    that. Don't add image fetching or resizing to a source.

## Workflow

```bash
# 1. fetch fixtures politely (2 s between requests)
UA='MuseNMingleBot/0.1.0 (+https://musenmingle.interstellarai.net/about#for-venues)'
curl -A "$UA" -o tests/fixtures/scrapers/<key>/robots.txt https://<site>/robots.txt
curl -A "$UA" -o tests/fixtures/scrapers/<key>/listing.html https://<site>/<events-page>
# 2. write src/sources/<key>.rs with pure parse_* functions + Source impl
# 3. write tests/source_<key>.rs (snapshot + wiremock fetch test)
INSTA_UPDATE=always cargo test --test source_<key>   # then REVIEW the .snap files
# 4. register in sources::build, add a seed migration (with display_name,
#    store_description, store_image, policy_note), update README's source list
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test
```

## When a site can't be used

If the investigation shows a site must not or cannot be scraped — robots.txt
disallows the events pages (`robots_disallowed`), it blocks the MuseNMingleBot
User-Agent (`bot_blocked`; we never evade blocks), it has no usable event
data (`no_event_data`), its terms forbid it (`terms`), its events only
render with JavaScript (`js_only`; we never run a browser), its owner asked
us not to list it (`owner_request`; see [venue-requests.md](venue-requests.md)
for the full removal procedure), or something else (`other`) — record the
decision instead of leaving it in a closed issue:

1. Add a **new** migration inserting a row into `events.refused_sources`:
   ```sql
   INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
   VALUES ('example.org', 'Example Gallery', 'https://www.example.org/whats-on', 'bot_blocked',
           'it returns 403 to the MuseNMingleBot User-Agent; we don''t evade blocks',
           DATE '2026-10-01', 'https://github.com/alexsiri7/musenmingle/issues/NN');
   ```
   `domain` is the registrable domain (no `www.`); `reason_text` completes
   the sentence "We looked at <name> on <date> and couldn't include it: …".
2. Merge that PR and close the `new-scraper` issue as not planned, linking it.

The site then appears under "Sites we couldn't use" on `/sources` (and in
`refused` of `GET /v1/sources`), and new suggestions for that domain are
answered with the reason (`refused`) instead of filing another issue.

## Checklist for a `new-scraper` issue

The issue template `.github/ISSUE_TEMPLATE/new-scraper.md` contains this list:

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
