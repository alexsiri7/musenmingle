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
`aggregator` for a third-party listing site covering many venues: an
aggregator never takes precedence in a cross-source merge and pages link to
the venue's own site before it.

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
   comfortably under `SOURCE_TIMEOUT_SECS`. A site whose robots.txt forces a
   slow rate can override `Source::fetch_timeout` instead of dropping data
   (see `gasworks`).
7. **Skips are not errors.** Return `Ok(None)` from `normalise` for pages that
   are not in-scope events (online-only, open-ended programmes, out-of-scope
   categories). Report genuine per-page failures with `ctx.report_error`; they
   feed the health checker.
8. **Times:** use `normalise::parse_datetime` (honours offsets) or
   `parse_london_wall_clock` when a site prints local times with a bogus
   offset. Verify against the human-readable time on the page. When the
   site gives a date but no time, set `all_day: true` with `starts_at` at
   London midnight of the first day and `ends_at` London midnight of the
   last day (inclusive), or `None` for a single day; pages then show no
   time (`normalise::is_date_only` / `is_london_midnight` help, see
   `courtauld`). Never set it for an item that has a time. An event that
   is a few sessions over weeks or months (#207) is one event with
   `NewEvent::set_sessions` (it sets the envelope dates): see
   `normalise::session_days` / `weekly_days` / `day_sessions` and
   `the_showroom` / `camden_art_centre`; never store it as one range. These
   London-midnight starts are still what the `when=` filters read as
   "untimed". If the page states late opening hours for an untimed event
   (an exhibition's "late openings" section), tag it `late opening` when
   `normalise::mentions_late_opening` accepts that text, so it counts for
   `when=evening`.
9. **Content policy: set it in the seed migration.** Muse & Mingle links out; it
   does not republish. In the seed row set `display_name` (the name shown
   on pages and in image credits, e.g. `'Barbican'`) and decide
   `store_description` / `store_image` from the site's terms of use, with a
   `policy_note` saying why (terms URL + date checked). Both default to
   true, which is right only when the terms don't forbid it; sites or APIs
   whose terms restrict reproduction (e.g. "personal use only", "no
   caching", "don't use images separately") are **facts + link only**:
   `store_description = FALSE, store_image = FALSE`. Terms that forbid
   reproducing *any information* from the site (not just text and images)
   rule out even facts + link: refuse the site (`terms`) instead, as with
   ArtRabbit (#96), D&AD (#101) and White Cube. Scrapers still emit
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
# 5. add the new file to qa::code::SOURCE_FILES (a test fails until you do)
cargo fmt && cargo clippy --all-targets --all-features -- -D warnings && cargo test
```

## Fixing a "Scraper check" issue

The scraper QA check (README, "Scraper QA") files `Scraper check: <key> — …`
issues when an AI comparison of a source's pages with what we stored finds
wrong fields or events we missed. The issue lists the page URLs, the fetch
date and a table of our stored value, what the page says and a verbatim
quote. The AI can be wrong: check each row on the page first. Then:

1. Save the pages listed (with the MuseNMingleBot UA, as in the workflow
   above) as `tests/fixtures/scrapers/<key>/qa-<YYYY-MM-DD>.html` (suffix
   `-2`, `-3`, … if several).
2. Add a snapshot test on them whose expected values are the page's, i.e.
   the "page says" column, and fix the scraper until it passes. This is the
   regression test.
3. The issue closes itself when a later check (weekly, or the next run after
   the source's file changes) finds no problems.

If the "missed" events are ones the scraper skips on purpose (its
documented, tested `Ok(None)` scope), don't add them. Give the source a
`qa_scope` note instead, or correct its existing one, saying what it leaves
out in the site's own words (e.g. the categories the listing shows). That
changes the source's file, so the next run rechecks it.

## Adding a venue on The Events Calendar

Many venue sites run WordPress with The Events Calendar (TEC). They share
one implementation, `src/sources/tec.rs`, so a new TEC venue is a seed row,
not code: `events.sources.platform` names the shared implementation
(`'tec'`) and `events.sources.config` holds the venue's settings, which
`sources::build` hands to it. Other shared platforms should reuse
`platform`/`config` the same way (e.g. Artlogic, #52).

1. Check robots.txt as above, against the exact URLs the source requests:
   `/wp-json/tribe/events/v1/events?ends_after=…&per_page=50&page=1` and,
   if used, the list view. Sites often disallow `/wp-json/` or `/*?`; then
   the API is out and only the list view can be used.
2. Probe the API once: `curl -A "$UA" 'https://<site>/wp-json/tribe/events/v1/events?per_page=1'`.
   If it is off (404) or disallowed, find the TEC list view (usually
   `/events/`, possibly redirected) and check that it has JSON-LD `Event`s.
3. Note the venue's category slugs (from the API's `categories`) and
   whether events carry a venue (`venue: []` means they don't).
4. Read the terms and decide the content policy as for any other source.
5. Save fixtures under `tests/fixtures/scrapers/tec-<venue>/` (robots.txt
   plus `api-page-1.json` or `list.html`) and add a snapshot test to
   `tests/source_tec.rs`; it normalises with the seed row's `config`.
6. Add the row in a new migration with `platform = 'tec'` and a `config`
   holding only what differs from the defaults:

   | field | default | meaning |
   |---|---|---|
   | `api_path` | `"/wp-json/tribe/events/v1/events"` | `null` reads only the list view |
   | `list_path` | none | list view to read when the API is off |
   | `venue` | none | `{"name", "address"}` for events without a venue; also marks them as in London |
   | `category_map` | `{}` | TEC category slug → category, checked first |
   | `skip_categories` | `[]` | TEC category slugs to skip (films, music, tours, …) |
   | `default_category` | none | category when neither the map nor keywords (title, or for the list view also the description) match |
   | `skip_keywords` | `[]` | title words or phrases whose events are skipped (e.g. yoga) |

   An unknown field or category makes the row a recorded skip
   (`invalid config`), not a crash.

## Adding an Artlogic gallery

Many London commercial galleries run the Artlogic CMS (the page carries
`<meta name="generator" content="Artlogic CMS - https://artlogic.net">` and
images on `static-assets.artlogic.net`). They share `src/sources/artlogic.rs`
(`platform = 'artlogic'`), so a new gallery is a seed row:

1. Fetch `robots.txt` and `/exhibitions/` with the bot UA and follow the
   redirect: seed `base_url`/`domain` with the **post-redirect host**, and
   set `listing_paths` to the page the listing actually lives on
   (`/exhibitions/current-forthcoming/`, `/exhibitions/location/1/`, …). If
   `/exhibitions/` lands on a single show, use
   `["/exhibitions/current/", "/exhibitions/forthcoming/"]`.
2. Check the listing has `#exhibitions-grid-current` / `-forthcoming` grids
   (or classic `section[data-label="Current"]`), and that its cards carry a
   date range the parser reads (unit tests in `artlogic.rs` list the
   formats).
3. Look at the cards' location labels. For a gallery with spaces outside
   London (or several in London), add `locations`: each `match` is a
   case-insensitive substring of the label (`"London"`, `"Cork Street"`),
   optionally with its own `name`/`address`; cards matching none are
   skipped. `unlabelled_at_venue: true` places cards with no label at
   `venue`. Leave out galleries whose current list is mostly off-site shows
   without labels to filter on.
4. Find the address (footer or `/contact/`) for `venue`, and read the terms
   (`/terms-and-conditions/`, often only terms of sale) for the content
   policy.
5. Add the row in a new migration (see
   `20260927952001_seed_artlogic_galleries.sql`) and bump the row count in
   `tests/source_artlogic.rs`.

   | field | default | meaning |
   |---|---|---|
   | `listing_paths` | `["/exhibitions/"]` | listing pages to read |
   | `venue` | required | `{"name", "address"}` of the London space |
   | `locations` | `[]` | `[{"match", "name"?, "address"?}]` London location labels |
   | `unlabelled_at_venue` | `false` | with `locations`: unlabelled cards go to `venue` |
   | `skip_match` | `[]` | extra phrases marking a card as not at the gallery |

Images all come from `static-assets.artlogic.net`, whose robots.txt asks for
`Crawl-delay: 10`; the thumbnailer honours it for all galleries together, so
with its per-host cap new galleries' thumbnails fill in over a few runs.

## Adding a Luma calendar

Luma calendars share `src/sources/luma.rs` (`platform = 'luma'`). Luma's
terms allow reuse only through its publicly supported interfaces, so a
calendar is read **only** through its iCal feed; don't scrape luma.com pages.

1. Find the calendar's id (`cal-…`) from its "Add iCal subscription" link
   and check the feed once:
   `curl -A "$UA" 'https://api2.luma.com/ics/get?entity=calendar&id=cal-…'`.
   It must be a London creative community with upcoming events (not a
   "Personal" calendar, not startup/tech).
2. Save the feed as `tests/fixtures/scrapers/luma/<name>.ics`, add it to
   `calendar_snapshots` in `tests/source_luma.rs` and review the snapshot.
3. Add a row in a new migration with `kind = 'aggregator'`,
   `base_url = 'https://api2.luma.com'`, `store_description = FALSE`,
   `store_image = FALSE`, `platform = 'luma'` and a `config` of
   `calendar_id`, optionally `default_category` and `skip_keywords` (title
   words or phrases to skip). Unknown fields make the row a recorded skip.
4. Add the calendar and a one-line reason to `docs/luma-calendars.md`.

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

## Suggested sites: the `unsafe-change` check

A site suggested through the public form or `POST /v1/suggestions` becomes a
`new-scraper` issue filed by the API. The factory screens it; if screening
passes, the issue gets `archon:auto-approved` and an agent may build the
scraper without the owner looking first. A PR that closes such an issue
(without the owner's `archon:approved`) must pass the `unsafe-change` check
(`.github/workflows/unsafe-change.yml`, policy `.github/unsafe-change.yml`)
before the factory merges it. The check is a short denylist, so a normal
scraper or bug-fix PR passes. It fails a PR that:

- touches CI, deploy, infra or agent instructions (`.github/`, `Dockerfile`,
  `.railway/`, `railway.*`, `ops/`, `.env*`, `build.rs`, `CLAUDE.md`, ...) or
  one of this repo's extra paths (`sqlx.toml`, `src/github.rs`,
  `src/notify.rs`, `src/enrich/requesty.rs`);
- touches a path naming secrets or auth handling (`auth`, `token`, `secret`,
  `credential`, `session`, `security`, `crypto`, `permission`). Fixtures,
  snapshots and Markdown files are exempt;
- adds a new dependency to `Cargo.toml`. Version bumps and `Cargo.lock` are
  fine;
- adds code that runs processes, reads environment variables (except
  `env!("CARGO_...")`) or opens raw sockets;
- adds migration SQL beyond ordinary changes in the `events` schema: no
  `GRANT`/`REVOKE`, roles, extensions, schemas, policies, `EXECUTE` or other
  schemas.

On failure the PR gets `needs-owner-review` and the owner gets one ntfy.
PRs that close no screened issue get "not applicable" and pass. To test the
checker against past PRs without writing anything:
`python3 .github/unsafe-change/check.py --dry-run <pr>...`.

## Checklist for a `new-scraper` issue

The issue template `.github/ISSUE_TEMPLATE/new-scraper.md` contains this list:

- [ ] Site name, events page URL and proposed source key
- [ ] robots.txt checked with the MuseNMingleBot UA; events pages allowed (paste the relevant lines)
- [ ] JSON-LD `Event` markup present? (listing and/or detail pages) — if not, which CSS selectors
- [ ] Pagination / detail pages and a per-run fetch cap
- [ ] Proposed `interval_minutes`
- [ ] Category mapping (exhibition / expo / community / talk / workshop / music, with music subtags in `tags`: see `src/music.rs`) and what to skip
- [ ] Time-zone quirks verified against human-readable times
- [ ] Date-only items (no time given) set `all_day`; timed items never do
- [ ] Fixtures saved under `tests/fixtures/scrapers/<key>/`
- [ ] Snapshot test of normalised output committed and reviewed
- [ ] wiremock fetch test (incl. robots.txt) passes
- [ ] Source registered in `sources::build` + seed migration (new file)
- [ ] Seed sets `display_name` (shown on pages and in "Image: …" credits)
- [ ] Venue type: a single-venue source gets its default in `SOURCE_DEFAULTS` (`src/venue_type.rs`); an aggregator's well-known venues can get `events.venues.venue_type` overrides in a new migration
- [ ] Site's terms of use checked: `store_description` / `store_image` decided (default true only if the terms don't forbid it; restrictive terms → both false, facts + link only) and `policy_note` records the terms URL + date
- [ ] No image hotlinking or image fetching in the source (thumbnails come only from the thumbnailer); no truncation of descriptions in the source (upsert does the excerpt)
- [ ] No LLM parsing; all requests go through `FetchContext`
- [ ] **Or, if the site can't be used** (robots.txt disallows the events pages, the site blocks our bot, no usable event data, terms forbid it, events only render with JavaScript): close the issue as not planned and, in the same PR, add an `events.refused_sources` row via a NEW migration (registrable domain, name, URL, `reason_code`, `reason_text`, `checked_on`, link to this issue)
