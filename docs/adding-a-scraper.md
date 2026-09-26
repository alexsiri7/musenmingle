# Adding a scraper

Every source lives in `src/sources/<name>.rs`, implements the `Source` trait
and is registered in `sources::build` plus a seed row in a **new** migration
(`INSERT INTO events.sources ... ON CONFLICT (key) DO NOTHING`). Migrations are
append-only: never edit an existing one.

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
   `curl -A 'ThaleiaBot/0.1.0 (+https://github.com/alexsiri7/thaleia; contact via repo issues)' https://<site>/robots.txt`.
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

## Workflow

```bash
# 1. fetch fixtures politely (2 s between requests)
UA='ThaleiaBot/0.1.0 (+https://github.com/alexsiri7/thaleia; contact via repo issues)'
curl -A "$UA" -o tests/fixtures/scrapers/<key>/robots.txt https://<site>/robots.txt
curl -A "$UA" -o tests/fixtures/scrapers/<key>/listing.html https://<site>/<events-page>
# 2. write src/sources/<key>.rs with pure parse_* functions + Source impl
# 3. write tests/source_<key>.rs (snapshot + wiremock fetch test)
INSTA_UPDATE=always cargo test --test source_<key>   # then REVIEW the .snap files
# 4. register in sources::build, add a seed migration, update README's source list
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test
```

## When a site can't be used

If the investigation shows a site must not or cannot be scraped — robots.txt
disallows the events pages (`robots_disallowed`), it blocks the ThaleiaBot
User-Agent (`bot_blocked`; we never evade blocks), it has no usable event
data (`no_event_data`), its terms forbid it (`terms`), its events only
render with JavaScript (`js_only`; we never run a browser), or something else
(`other`) — record the decision instead of leaving it in a closed issue:

1. Add a **new** migration inserting a row into `events.refused_sources`:
   ```sql
   INSERT INTO events.refused_sources (domain, name, url, reason_code, reason_text, checked_on, issue_url)
   VALUES ('example.org', 'Example Gallery', 'https://www.example.org/whats-on', 'bot_blocked',
           'it returns 403 to the ThaleiaBot User-Agent; we don''t evade blocks',
           DATE '2026-10-01', 'https://github.com/alexsiri7/thaleia/issues/NN');
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
- [ ] **Or, if the site can't be used** (robots.txt disallows the events pages, the site blocks our bot, no usable event data, terms forbid it, events only render with JavaScript): close the issue as not planned and, in the same PR, add an `events.refused_sources` row via a NEW migration (registrable domain, name, URL, `reason_code`, `reason_text`, `checked_on`, link to this issue)
